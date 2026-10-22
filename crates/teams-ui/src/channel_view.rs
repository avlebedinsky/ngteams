//! A team channel's posts and their reply chains (M2 Task 16), read-only: no composer, no message action, no read
//! receipt. Nothing in this view writes, and the engine has no write a channel could take.
//!
//! - **From the store only.** The view asks the engine to sync the channel when it opens
//!   ([`Core::sync_conversation`]: a channel other than a team's general one is in no conversation list, so no
//!   catch-up reads it), then reads the store's newest messages ([`PAGE`] at first, a page more for each "Load older
//!   posts") and the post each belongs to ([`Core::reply_roots`]), again each time the models say the channel changed.
//!   Older history comes from the service when the store has no more ([`Core::load_older`]), until the store says the
//!   whole history is stored ([`Core::history_complete`]).
//! - **Threads** ([`arrange_threads`]): each post with its replies under it, the oldest post first; a message whose
//!   post isn't recorded (read before posts were kept) is a post of its own; replies whose post the view doesn't hold
//!   show under "Replies to an earlier post". Each message shows its sender's name ([`Core::person`]: their profile's,
//!   else the name the message carried, else their MRI), its time, its rich text through our own renderer
//!   ([`crate::rich_text`]), "Edited", or "This message was deleted"; a member's addition is a line naming who added
//!   whom; any other event or type a placeholder that names no content.
//!
//! What the view shows is traced (`channel-view shown`: the channel, its messages' ids in the order shown, each `post`
//! or `reply`), never text or a name.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::BuildHasher;
use std::sync::Arc;

use chrono::{DateTime, Local};
use gpui::{
    AnyElement, AsyncApp, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use teams_proto::chat::{MessageType, RichText, parse_html, thread_activity};
use teams_store::Message;

use crate::bridge::Core;
use crate::chat_view::{DELETED, UNSHOWN_CONTENT, UNSUPPORTED, unshown_of};
use crate::models::Models;
use crate::people::Shown;
use crate::rich_text::{self, Mentions, Rendered};
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
    /// A thread event's line.
    Event(SharedString),
    Unsupported,
}

/// A message as the view shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub sender_name: SharedString,
    pub arrival_time: i64,
    pub edited: bool,
    pub body: Body,
    /// A text that carries a part no capture showed (`chat_view::unshown_of`: a received file, a card): shown with
    /// [`UNSHOWN_CONTENT`] under it, never a guess at the part. No web button: no channel deep link was captured.
    pub unshown: bool,
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
            if chats.read(cx).changed_at(&this.channel) != this.read_at {
                this.reload(cx);
            }
        });
        let sync = Arc::clone(&core);
        let synced = channel.clone();
        let syncing = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // what it stores comes as the models' change too; a failed sync leaves what the store holds shown
            let _ = sync.sync_conversation(synced).await;
            let _ = this.update(cx, |view, cx| {
                view.syncing = None;
                view.reload(cx);
            });
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
            loading: None,
            dirty: false,
            loads: 0,
            syncing: Some(syncing),
            older: None,
            scroll: ScrollHandle::new(),
            placed: false,
            traced: None,
            _observing: observing,
        };
        view.reload(cx);
        view
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
        self.read_at = self.models.chats.read(cx).changed_at(&self.channel);
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
            let threads = arrange_threads(read.messages, &read.roots);
            self.threads = threads
                .into_iter()
                .map(|thread| Thread {
                    root: thread.root,
                    post: thread.post.map(|post| self.item(&post, &read.people)),
                    replies: thread
                        .replies
                        .iter()
                        .map(|reply| self.item(reply, &read.people))
                        .collect(),
                })
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

    /// A message as shown.
    fn item(&mut self, message: &Message, people: &HashMap<String, Shown>) -> Item {
        let body = self.body(message, people);
        let unshown = matches!(body, Body::Text(_)) && unshown_of(message).any();
        Item {
            id: message.id.clone(),
            sender_name: sender_name(message, people, self.core.me()),
            arrival_time: message.arrival_time,
            edited: message.edit_time.is_some() && message.delete_time.is_none(),
            body,
            unshown,
        }
    }

    fn body(&mut self, message: &Message, people: &HashMap<String, Shown>) -> Body {
        if message.delete_time.is_some() {
            return Body::Deleted;
        }
        match MessageType::from(message.message_type.clone()) {
            MessageType::RichTextHtml | MessageType::Text => Body::Text(self.rendered(message)),
            MessageType::AddMember => Body::Event(added(message, people, self.core.me()).into()),
            _ => Body::Unsupported,
        }
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
        let mentions = Mentions::of(message.properties.as_deref(), self.core.me());
        let rendered = Arc::new(rich_text::layout(&text, &mentions));
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

    fn thread_view(index: usize, thread: &Thread<Item>, palette: Palette) -> AnyElement {
        let post = match &thread.post {
            Some(post) => message_view(post, palette),
            None => div()
                .debug_selector(move || format!("earlier-post-{index}"))
                .italic()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(EARLIER_POST)
                .into_any_element(),
        };
        let replies = thread.replies.iter().map(|reply| {
            div()
                .pl_6()
                .pt_1()
                .border_t_1()
                .border_color(rgb(palette.border))
                .child(message_view(reply, palette))
                .into_any_element()
        });
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

/// A member's addition: who added whom.
fn added(message: &Message, people: &HashMap<String, Shown>, me: &str) -> String {
    let Some(activity) = thread_activity(&message.content) else {
        return "Members changed".to_owned();
    };
    let name = |mri: &str| -> String {
        if mri == me {
            return "You".to_owned();
        }
        people
            .get(mri)
            .and_then(|shown| shown.name.as_ref())
            .map_or_else(|| mri.to_owned(), ToString::to_string)
    };
    let targets: Vec<String> = activity.targets.iter().map(|mri| name(mri)).collect();
    match &activity.initiator {
        Some(initiator) => format!("{} added {}", name(initiator), targets.join(", ")),
        None => format!("Added {}", targets.join(", ")),
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
        if message.message_type == MessageType::AddMember.as_str()
            && let Some(activity) = thread_activity(&message.content)
        {
            wanted.extend(activity.initiator);
            wanted.extend(activity.targets);
        }
    }
    wanted.remove(core.me());
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

/// A message: its sender's name and time over its body.
fn message_view(item: &Item, palette: Palette) -> AnyElement {
    let time = DateTime::from_timestamp_millis(item.arrival_time)
        .map(|at| at.with_timezone(&Local).format("%d %b %H:%M").to_string())
        .unwrap_or_default();
    let header = div()
        .flex()
        .gap_2()
        .items_center()
        .text_sm()
        .child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(item.sender_name.clone()),
        )
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
    let body = match &item.body {
        Body::Text(rendered) => rich_text::render(&format!("channel-text-{}", item.id).into(), rendered, palette),
        Body::Deleted => muted(DELETED.into()),
        Body::Event(text) => muted(text.clone()),
        Body::Unsupported => muted(UNSUPPORTED.into()),
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
        .children(unshown)
        .into_any_element()
}

impl Render for ChannelView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
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
        let threads: Vec<AnyElement> = self
            .threads
            .iter()
            .enumerate()
            .map(|(index, thread)| Self::thread_view(index, thread, palette))
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
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .child(posts)
            .child(note)
    }
}
