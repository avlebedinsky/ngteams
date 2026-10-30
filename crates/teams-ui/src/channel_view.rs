//! A team channel's posts and their reply chains (M2 Task 16), read-only: no composer, no message action, no read
//! receipt. Nothing in this view writes; a chat write aimed at a channel is denied by the fence (a channel's members
//! are never read complete), and no write is mapped to `WriteKind::ChannelPost`.
//!
//! - **From the store only.** The view asks the engine to sync the channel each time it is opened
//!   ([`Core::sync_conversation`], [`ChannelView::sync`]: a channel other than a team's general one is in no
//!   conversation list, so no catch-up reads it, and a sync that failed is tried again only so), then reads the store's newest messages ([`PAGE`] at first, a page more for each "Load older
//!   posts") and the post each belongs to ([`Core::reply_roots`]), again each time the models say the channel changed
//!   or someone it shows was named anew ([`crate::Chats::named_at`]: a profile read in the background).
//!   Older history comes from the service when the store has no more ([`Core::load_older`]), until the store says the
//!   whole history is stored ([`Core::history_complete`]).
//! - **Threads** ([`arrange_threads`]): each post with its replies under it, the oldest post first; a message whose
//!   post isn't recorded (read before posts were kept) is a post of its own; replies whose post the view doesn't hold
//!   show under "Replies to an earlier post". Each message shows its sender's name ([`Core::person`]: their profile's,
//!   else the name the message carried, else their MRI; none for the thread's own message, never its id), its time,
//!   its rich text through our own renderer ([`crate::rich_text`]), "Edited", or "This message was deleted"; a system
//!   message ([`crate::system_message`]: a call's start or end, a recording, a transcript, members added, joined, left
//!   or removed, the name) is a line in Teams' words, a card's title and detail on one line; one Teams draws nothing
//!   for isn't shown, nor its thread when nothing under it is (with replies, a placeholder stands for it); any other
//!   type or shape a placeholder that names no content. A forwarded message is drawn as in a chat
//!   ([`crate::forwarded`]), its original's time a link to it, told as [`ChannelViewEvent::OpenOriginal`].
//!
//! What the view shows is traced (`channel-view shown`: the channel, its messages' ids in the order shown, each `post`
//! or `reply`), never text or a name.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::BuildHasher;
use std::sync::Arc;

use chrono::Local;
use gpui::{
    AnyElement, App, AsyncApp, ClipboardItem, Context, FocusHandle, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Task, WeakEntity, Window, div, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Copy;
use gpui_component::kbd::Kbd;
use teams_proto::chat::{LinkPreview, MessageType, RichText, SharedFile, parse_html};
use teams_store::Message;

use crate::bridge::Core;
use crate::chat_view::{DELETED, UNSHOWN_CONTENT, UNSUPPORTED, files_of, unshown_of};
use crate::clock::{self, HourCycle};
use crate::files;
use crate::forwarded::{self, ForwardOrigin, Original};
use crate::link_preview;
use crate::models::Models;
use crate::people::Shown;
use crate::rich_text::{self, Mentions, Rendered};
use crate::selection::{self, MessageKey, Scope, TextSelection};
use crate::system_message::{self, Naming, Shown as SystemShown};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

/// Messages read from the store at first, and how many more each "Load older posts" reads.
pub const PAGE: usize = 200;
/// What a thread whose post the view doesn't hold says above its replies.
pub const EARLIER_POST: &str = "Replies to an earlier post";
/// The line under the posts: nothing can be written here.
pub const READ_ONLY: &str = "Channels are read-only here";

/// A thread as arranged from the store's messages: its post's id, the post when it is held, and its replies, the
/// oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread<T> {
    pub root: String,
    pub post: Option<T>,
    pub replies: Vec<T>,
}

/// Arranges a channel's `messages` into threads by `roots` (message id → its post's id; a message without one is its
/// own post): each post with its replies, the threads by their post's arrival (or, without the post, their oldest
/// reply's), the replies by arrival. Control messages are left out.
#[must_use]
pub fn arrange_threads<S: BuildHasher>(
    messages: Vec<Message>,
    roots: &HashMap<String, String, S>,
) -> Vec<Thread<Message>> {
    let mut threads: BTreeMap<String, Thread<Message>> = BTreeMap::new();
    for message in messages {
        if MessageType::from(message.message_type.clone()).is_control() {
            continue;
        }
        let root = roots.get(&message.id).cloned().unwrap_or_else(|| message.id.clone());
        let thread = threads.entry(root.clone()).or_insert_with(|| Thread {
            root: root.clone(),
            post: None,
            replies: Vec::new(),
        });
        if message.id == root {
            thread.post = Some(message);
        } else {
            thread.replies.push(message);
        }
    }
    let mut threads: Vec<Thread<Message>> = threads.into_values().collect();
    for thread in &mut threads {
        thread
            .replies
            .sort_by(|one, other| (one.arrival_time, &one.id).cmp(&(other.arrival_time, &other.id)));
    }
    threads.sort_by_key(|thread| {
        thread
            .post
            .as_ref()
            .or_else(|| thread.replies.first())
            .map(|first| (first.arrival_time, first.id.clone()))
    });
    threads
}

/// What a message shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Text(Arc<Rendered>),
    Deleted,
    /// A system message's line, in Teams' words ([`crate::system_message`]): a thread's event, a call's or a
    /// recording's state, or a card's title and detail.
    Event(SharedString),
    Unsupported,
}

/// What the channel view tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelViewEvent {
    /// A forwarded message's original time was clicked: the original is opened in the app when the account has its
    /// chat, else in Teams on the web (the shell's to decide, as for a chat's forward).
    OpenOriginal(Original),
}

/// A message as the view shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub sender_name: SharedString,
    pub arrival_time: i64,
    pub edited: bool,
    pub body: Body,
    /// The files a text carries (`chat_view::files_of`), each drawn as its card under it (`crate::files`).
    pub files: Vec<SharedFile>,
    /// The previews of the links a text carries (`crate::link_preview::previews_of`), each drawn as its card under it.
    pub links: Vec<LinkPreview>,
    /// A text that carries a part this build can't show (`chat_view::unshown_of`: a file entry of a shape no message
    /// showed, a card): shown with
    /// [`UNSHOWN_CONTENT`] under it, never a guess at the part. No web button: no channel deep link was captured.
    pub unshown: bool,
    /// Where a forwarded message came from ([`crate::forwarded`]): its forward card's header, the original's time a
    /// link to it ([`ChannelViewEvent::OpenOriginal`]).
    pub forwarded: Option<ForwardOrigin>,
}

/// What one load read from the store.
struct Read {
    messages: Vec<Message>,
    roots: HashMap<String, String>,
    /// More messages are stored than were read.
    more_stored: bool,
    history_complete: Option<bool>,
    people: HashMap<String, Shown>,
}

/// The channel view (see the module docs).
pub struct ChannelView {
    core: Arc<dyn Core>,
    models: Models,
    trace: Arc<ViewTrace>,
    channel: String,
    limit: usize,
    threads: Vec<Thread<Item>>,
    more_stored: bool,
    history_complete: Option<bool>,
    /// A text message's rich text, laid out once per id and version.
    rendered: HashMap<String, (i64, Arc<Rendered>)>,
    /// The channel's change the last load started at.
    read_at: u64,
    /// The models' revision the last load started at: a person shown named after it is read again.
    people_at: u64,
    /// The people the last load named (senders, and those a member event names).
    people: Vec<String>,
    loading: Option<Task<()>>,
    dirty: bool,
    loads: u64,
    /// The sync asked for on open, while it runs.
    syncing: Option<Task<()>>,
    older: Option<Task<()>>,
    scroll: ScrollHandle,
    /// The view went to the newest posts after its first load with any.
    placed: bool,
    traced: Option<Vec<(String, &'static str)>>,
    /// The text selected with the mouse, in any of the posts and replies ([`crate::selection`]).
    text_selection: TextSelection,
    /// Takes the focus on a press in the posts, so Ctrl+C reaches the view.
    focus: FocusHandle,
    _observing: Subscription,
}

impl ChannelView {
    /// The view of `channel`: synced from the service at once, read from the store.
    pub fn new(
        core: Arc<dyn Core>,
        models: Models,
        trace: Arc<ViewTrace>,
        channel: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let observing = cx.observe(&models.chats, |this: &mut Self, chats, cx| {
            let due = {
                let chats = chats.read(cx);
                chats.changed_at(&this.channel) != this.read_at
                    || this.people.iter().any(|mri| chats.named_at(mri) > this.people_at)
            };
            if due {
                this.reload(cx);
            }
        });
        let mut view = Self {
            core,
            models,
            trace,
            channel,
            limit: PAGE,
            threads: Vec::new(),
            more_stored: false,
            history_complete: None,
            rendered: HashMap::new(),
            read_at: 0,
            people_at: 0,
            people: Vec::new(),
            loading: None,
            dirty: false,
            loads: 0,
            syncing: None,
            older: None,
            scroll: ScrollHandle::new(),
            placed: false,
            traced: None,
            text_selection: TextSelection::default(),
            focus: cx.focus_handle(),
            _observing: observing,
        };
        view.sync(cx);
        view.reload(cx);
        view
    }

    /// Asks the engine to sync the channel from the service, unless a sync it asked for still runs; the store is read
    /// again once it is over. Each opening of the channel asks it, the view's first and any later one.
    pub fn sync(&mut self, cx: &mut Context<Self>) {
        if self.syncing.is_some() {
            return;
        }
        let (core, channel) = (Arc::clone(&self.core), self.channel.clone());
        self.syncing = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // what it stores comes as the models' change too; a failed sync leaves what the store holds shown
            let _ = core.sync_conversation(channel).await;
            let _ = this.update(cx, |view, cx| {
                view.syncing = None;
                view.reload(cx);
            });
        }));
    }

    #[must_use]
    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// The threads shown, the oldest first.
    #[must_use]
    pub fn threads(&self) -> &[Thread<Item>] {
        &self.threads
    }

    /// How many loads from the store have ended.
    #[must_use]
    pub fn loads(&self) -> u64 {
        self.loads
    }

    /// Whether the sync asked for on open still runs.
    #[must_use]
    pub fn syncing(&self) -> bool {
        self.syncing.is_some()
    }

    /// Whether older posts may be loaded: the store holds more, or it doesn't say it holds the whole history.
    #[must_use]
    pub fn has_older(&self) -> bool {
        self.more_stored || self.history_complete != Some(true)
    }

    /// Reads the store again (or once the running load ends).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.loading.is_some() {
            self.dirty = true;
            return;
        }
        self.dirty = false;
        let chats = self.models.chats.read(cx);
        (self.read_at, self.people_at) = (chats.changed_at(&self.channel), chats.revision());
        let (core, channel, limit) = (Arc::clone(&self.core), self.channel.clone(), self.limit);
        self.loading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let read = read(&*core, &channel, limit).await;
            let _ = this.update(cx, |view, cx| view.loaded(read, cx));
        }));
    }

    /// Shows a page more: the store's when it holds more, else the service's next older page read into it first.
    pub fn load_older(&mut self, cx: &mut Context<Self>) {
        if self.older.is_some() || !self.has_older() {
            return;
        }
        self.limit += PAGE;
        if self.more_stored {
            self.reload(cx);
            return;
        }
        let (core, channel) = (Arc::clone(&self.core), self.channel.clone());
        self.older = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let _ = core.load_older(channel).await;
            let _ = this.update(cx, |view, cx| {
                view.older = None;
                view.reload(cx);
            });
        }));
    }

    /// A load ended: its threads shown (a stopped engine's keep what is shown).
    fn loaded(&mut self, read: Option<Read>, cx: &mut Context<Self>) {
        self.loading = None;
        self.loads += 1;
        if let Some(read) = read {
            self.more_stored = read.more_stored;
            self.history_complete = read.history_complete;
            self.people = read.people.keys().cloned().collect();
            let threads = arrange_threads(read.messages, &read.roots);
            self.threads = threads
                .into_iter()
                .filter_map(|thread| self.thread(thread, &read.people))
                .collect();
            self.trace_threads();
            if !self.placed && !self.threads.is_empty() {
                self.placed = true;
                self.scroll.scroll_to_bottom();
            }
        }
        cx.notify();
        if self.dirty {
            self.reload(cx);
        }
    }

    /// A thread as shown; `None` when nothing of it is. A post Teams draws nothing for (a lone bot added, a call's end
    /// with no length) takes its thread with it; with replies shown under it, its placeholder stands in its place, never
    /// "Replies to an earlier post" (that is for a post the view doesn't hold).
    fn thread(&mut self, thread: Thread<Message>, people: &HashMap<String, Shown>) -> Option<Thread<Item>> {
        let replies: Vec<Item> = thread
            .replies
            .iter()
            .filter_map(|reply| self.item(reply, people))
            .collect();
        let post = match thread.post {
            None if replies.is_empty() => return None,
            None => None,
            Some(post) => match self.item(&post, people) {
                Some(item) => Some(item),
                None if replies.is_empty() => return None,
                None => Some(Self::item_with(&post, Body::Unsupported, people, self.core.me())),
            },
        };
        Some(Thread {
            root: thread.root,
            post,
            replies,
        })
    }

    /// A message as shown.
    /// `None` for a system message Teams draws nothing for.
    fn item(&mut self, message: &Message, people: &HashMap<String, Shown>) -> Option<Item> {
        let body = self.body(message, people)?;
        Some(Self::item_with(message, body, people, self.core.me()))
    }

    /// `message` showing `body`.
    fn item_with(message: &Message, body: Body, people: &HashMap<String, Shown>, me: &str) -> Item {
        let text = matches!(body, Body::Text(_));
        let unshown = text && unshown_of(message).any();
        let files = if text { files_of(message) } else { Vec::new() };
        let links = if text {
            link_preview::previews_of(message.properties.as_deref())
        } else {
            Vec::new()
        };
        let forwarded = text
            .then(|| forwarded::forwarded_from(message.properties.as_deref()))
            .flatten()
            .map(|from| ForwardOrigin::of(&from, from.sender.as_deref().and_then(|sender| people.get(sender))));
        // a thread's own message has no sender to name: never the thread's id
        let from_thread = message.sender.as_deref().is_some_and(system_message::is_thread);
        Item {
            id: message.id.clone(),
            sender_name: if from_thread {
                SharedString::default()
            } else {
                sender_name(message, people, me)
            },
            arrival_time: message.arrival_time,
            edited: message.edit_time.is_some() && message.delete_time.is_none(),
            body,
            files,
            links,
            unshown,
            forwarded,
        }
    }

    /// What a message shows: a system message as its line (a card's title and detail on one); `None` for one Teams
    /// draws nothing for.
    fn body(&mut self, message: &Message, people: &HashMap<String, Shown>) -> Option<Body> {
        if message.delete_time.is_some() {
            return Some(Body::Deleted);
        }
        match MessageType::from(message.message_type.clone()) {
            MessageType::RichTextHtml | MessageType::Text => return Some(Body::Text(self.rendered(message))),
            _ => {}
        }
        let names = ChannelNames {
            people,
            me: self.core.me(),
        };
        let shown = system_message::shown(
            &message.message_type,
            &message.content,
            &self.channel,
            message.sender.as_deref(),
            &names,
        );
        Some(match shown {
            Some(SystemShown::Line(text) | SystemShown::TimedLine(text)) => Body::Event(text.into()),
            Some(SystemShown::Card { title, detail, .. }) => Body::Event(
                title
                    .map_or_else(|| detail.clone(), |title| format!("{title}: {detail}"))
                    .into(),
            ),
            Some(SystemShown::Nothing) => return None,
            None => Body::Unsupported,
        })
    }

    /// A text message's rich text, laid out once per id and version.
    fn rendered(&mut self, message: &Message) -> Arc<Rendered> {
        if let Some((version, rendered)) = self.rendered.get(&message.id)
            && *version == message.version
        {
            return Arc::clone(rendered);
        }
        let text = if message.message_type == MessageType::RichTextHtml.as_str() {
            parse_html(&message.content)
        } else {
            RichText::plain(&message.content)
        };
        let mentions = Mentions::of_message(message, self.core.me());
        let rendered = Arc::new(
            rich_text::layout(&text, &mentions).with_source(rich_text::PictureSource::new(Arc::clone(&self.core))),
        );
        self.rendered
            .insert(message.id.clone(), (message.version, Arc::clone(&rendered)));
        rendered
    }

    /// Traces what the view shows, when it changed.
    fn trace_threads(&mut self) {
        let shown: Vec<(String, &'static str)> = self
            .threads
            .iter()
            .flat_map(|thread| {
                let post = thread.post.iter().map(|post| {
                    let kind = if post.unshown { "unshown-post" } else { "post" };
                    (post.id.clone(), kind)
                });
                let replies = thread.replies.iter().map(|reply| {
                    let kind = if reply.unshown { "unshown-reply" } else { "reply" };
                    (reply.id.clone(), kind)
                });
                post.chain(replies)
            })
            .collect();
        if self.traced.as_ref() == Some(&shown) {
            return;
        }
        let mut line = TraceLine::new("channel-view", "shown")
            .conversation(&self.channel)
            .messages(shown.iter().map(|(id, _)| id.as_str()));
        for (_, kind) in &shown {
            line = line.kind(kind);
        }
        self.trace.record(&line);
        self.traced = Some(shown);
    }

    fn thread_view(
        index: usize,
        thread: &Thread<Item>,
        selection: &TextSelection,
        palette: Palette,
        cycle: HourCycle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let post = match &thread.post {
            Some(post) => message_view(post, &Scope::new(selection, post_key(index, post)), palette, cycle, cx),
            None => div()
                .debug_selector(move || format!("earlier-post-{index}"))
                .italic()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(EARLIER_POST)
                .into_any_element(),
        };
        let replies: Vec<AnyElement> = thread
            .replies
            .iter()
            .enumerate()
            .map(|(at, reply)| {
                let scope = Scope::new(selection, reply_key(index, at, reply));
                div()
                    .pl_6()
                    .pt_1()
                    .border_t_1()
                    .border_color(rgb(palette.border))
                    .child(message_view(reply, &scope, palette, cycle, cx))
                    .into_any_element()
            })
            .collect();
        div()
            .debug_selector(move || format!("thread-{index}"))
            .mx_3()
            .my_1()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .flex()
            .flex_col()
            .gap_1()
            .child(post)
            .children(replies)
            .into_any_element()
    }
}

/// A message's sender as shown: "You" for the account, their profile's name, else the name the message carried, else
/// the engine's other names, else their MRI.
fn sender_name(message: &Message, people: &HashMap<String, Shown>, me: &str) -> SharedString {
    let Some(sender) = message.sender.as_deref() else {
        return "Unknown".into();
    };
    if sender == me {
        return "You".into();
    }
    let shown = people.get(sender);
    let carried = message
        .sender_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let profiled = shown
        .filter(|shown| shown.profiled())
        .and_then(|shown| shown.name.clone());
    profiled
        .or_else(|| carried.map(SharedString::from))
        .or_else(|| shown.and_then(|shown| shown.name.clone()))
        .unwrap_or_else(|| sender.to_owned().into())
}

/// How a system message names people in a channel: their name as the engine has it, else the one the event gives,
/// else a placeholder; never an MRI.
struct ChannelNames<'p> {
    people: &'p HashMap<String, Shown>,
    me: &'p str,
}

impl Naming for ChannelNames<'_> {
    fn is_me(&self, mri: &str) -> bool {
        mri == self.me
    }

    fn name(&self, mri: &str, carried: Option<&str>) -> String {
        self.people
            .get(mri)
            .and_then(|shown| shown.name.as_ref())
            .map(ToString::to_string)
            .or_else(|| carried.map(str::to_owned))
            .unwrap_or_else(|| crate::chat_list::unnamed(mri).to_owned())
    }
}

/// One load: the newest `limit` messages, their posts, whether the history is whole, and the people they name.
async fn read(core: &dyn Core, channel: &str, limit: usize) -> Option<Read> {
    let page = core.messages(channel.to_owned(), None, limit).await.ok()?;
    let roots = core.reply_roots(channel.to_owned()).await.ok()?;
    let history_complete = core.history_complete(channel.to_owned()).await.ok()?;
    let mut wanted: HashSet<String> = HashSet::new();
    for message in &page.messages {
        wanted.extend(message.sender.iter().cloned());
        let text = message.message_type == MessageType::RichTextHtml.as_str()
            || message.message_type == MessageType::Text.as_str();
        if !text && message.delete_time.is_none() {
            wanted.extend(system_message::people(&message.message_type, &message.content));
        }
    }
    wanted.remove(core.me());
    // a forward's original sender, the account too: a forward names it by its name
    wanted.extend(
        page.messages
            .iter()
            .filter_map(|message| forwarded::forwarded_from(message.properties.as_deref())?.sender),
    );
    let mut people = HashMap::new();
    for mri in wanted {
        if let Ok(person) = core.person(mri.clone()).await {
            people.insert(mri, Shown::of(person));
        }
    }
    Some(Read {
        more_stored: page.next.is_some(),
        messages: page.messages,
        roots: roots.into_iter().collect(),
        history_complete,
        people,
    })
}

/// A thread's post's place among the view's messages for the text selection: as the threads are shown.
fn post_key(thread: usize, post: &Item) -> MessageKey {
    MessageKey::new((i64::try_from(thread).unwrap_or(i64::MAX), 0), post.id.clone())
}

/// A reply's place among the view's messages for the text selection: under its thread's post, in order.
fn reply_key(thread: usize, at: usize, reply: &Item) -> MessageKey {
    let at = i64::try_from(at).unwrap_or(i64::MAX - 1);
    MessageKey::new((i64::try_from(thread).unwrap_or(i64::MAX), at + 1), reply.id.clone())
}

/// A message: its sender's name and time (in Teams' words, on `cycle`'s clock) over its body, its text selectable in
/// `scope`.
fn message_view(
    item: &Item,
    scope: &Scope,
    palette: Palette,
    cycle: HourCycle,
    cx: &mut Context<ChannelView>,
) -> AnyElement {
    let time = clock::message_time(item.arrival_time, &Local::now(), cycle);
    let header = div()
        .flex()
        .gap_2()
        .items_center()
        .text_sm()
        // a thread's own message names no sender
        .children((!item.sender_name.is_empty()).then(|| {
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(item.sender_name.clone())
        }))
        .child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(time))
        .children(item.edited.then(|| {
            div()
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .child("Edited")
        }));
    let muted = |text: SharedString| {
        div()
            .italic()
            .text_color(rgb(palette.muted_foreground))
            .child(text)
            .into_any_element()
    };
    let forward = item.forwarded.as_ref().map(|origin| {
        let open = origin.original.clone().map(|original| {
            Box::new(cx.listener(move |_, _: &gpui::ClickEvent, _, cx| {
                cx.stop_propagation();
                cx.emit(ChannelViewEvent::OpenOriginal(original.clone()));
            })) as forwarded::OnOpen
        });
        forwarded::header(&item.id, origin, palette, cycle, open)
    });
    let body = match &item.body {
        Body::Text(rendered) => {
            let id = format!("channel-text-{}", item.id).into();
            rich_text::render_selectable(&id, rendered, palette, forward, Some(scope))
        }
        Body::Deleted => muted(DELETED.into()),
        Body::Event(text) => muted(text.clone()),
        Body::Unsupported => muted(UNSUPPORTED.into()),
    };
    let (files, links) = match &item.body {
        Body::Text(rendered) => (
            files::file_cards(&item.id, &item.files, rendered.source.as_ref(), palette),
            link_preview::cards(&item.id, &item.links, rendered.source.as_ref(), palette),
        ),
        _ => (None, None),
    };
    let unshown = item.unshown.then(|| {
        let id = item.id.clone();
        div()
            .debug_selector(move || format!("channel-unshown-{id}"))
            .text_xs()
            .child(muted(UNSHOWN_CONTENT.into()))
    });
    let id = item.id.clone();
    div()
        .debug_selector(move || format!("channel-message-{id}"))
        .flex()
        .flex_col()
        .gap_1()
        .child(header)
        .child(body)
        .children(links)
        .children(files)
        .children(unshown)
        .into_any_element()
}

impl gpui::EventEmitter<ChannelViewEvent> for ChannelView {}

impl ChannelView {
    /// Puts the text selected with the mouse on the clipboard, as plain text; whether there was any.
    pub fn copy_selection(&self, cx: &mut App) -> bool {
        let messages = self.threads.iter().enumerate().flat_map(|(index, thread)| {
            let post = thread.post.iter().map(move |post| (post_key(index, post), post));
            let replies = thread
                .replies
                .iter()
                .enumerate()
                .map(move |(at, reply)| (reply_key(index, at, reply), reply));
            post.chain(replies).filter_map(|(key, item)| match &item.body {
                Body::Text(rendered) => Some((key, &**rendered)),
                _ => None,
            })
        });
        let Some(text) = self.text_selection.text(messages) else {
            return false;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// The text selected with the mouse.
    #[must_use]
    pub fn text_selection(&self) -> &TextSelection {
        &self.text_selection
    }

    /// The right-click menu over the text selected with the mouse, while it is open: "Copy", with its key.
    fn selection_menu(&self, palette: Palette, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let key = Kbd::binding_for_action_in(&Copy, &self.focus, window).map(Kbd::outline);
        let view = cx.entity().downgrade();
        selection::menu(&self.text_selection, palette, key, move |_, cx| {
            let _ = view.update(cx, |this, cx| this.copy_selection(cx));
        })
    }
}

impl Render for ChannelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let selection_menu = self.selection_menu(palette, window, cx);
        let older = self.has_older().then(|| {
            div().p_2().flex().justify_center().child(
                Button::new("channel-older")
                    .label("Load older posts")
                    .ghost()
                    .loading(self.older.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.load_older(cx))),
            )
        });
        let empty = (self.threads.is_empty() && self.loads > 0 && !self.syncing()).then(|| {
            div()
                .p_3()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child("No posts to show")
        });
        let cycle = clock::cycle(cx);
        let threads: Vec<AnyElement> = self
            .threads
            .iter()
            .enumerate()
            .map(|(index, thread)| Self::thread_view(index, thread, &self.text_selection, palette, cycle, cx))
            .collect();
        let posts = div()
            .id("channel-posts")
            .track_scroll(&self.scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .children(older)
            .children(empty)
            .children(threads);
        let note = div()
            .debug_selector(|| "channel-read-only".to_owned())
            .flex_none()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(rgb(palette.border))
            .text_xs()
            .text_color(rgb(palette.muted_foreground))
            .child(READ_ONLY);
        div()
            .debug_selector(|| "channel-view".to_owned())
            .track_focus(&self.focus)
            // a press anywhere in the view lets the selected text go (a press on text then starts a new selection;
            // Shift+click extends it) and takes the focus, so Ctrl+C reaches the view
            .capture_any_mouse_down(cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                if event.button != MouseButton::Left {
                    return;
                }
                this.focus.focus(window, cx);
                if !event.modifiers.shift && this.text_selection.clear() {
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &Copy, _, cx| {
                if !this.copy_selection(cx) {
                    cx.propagate();
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .child(posts)
            .child(note)
            .children(selection_menu)
    }
}
