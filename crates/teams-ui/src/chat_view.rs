//! The chat view: one conversation's messages, the newest at the bottom.
//!
//! - **From the store only.** The view reads the store's newest page when it opens ([`Core::messages`], [`PAGE`]
//!   messages), and again, down to the oldest message it holds, each time the models say its chat changed (a
//!   delivery, a catch-up, an outbox row). A message's reactions come from the store too, read again only for a new
//!   version of it.
//! - **Older pages** load when fewer than a page of rows ([`PAGE`]) is above the view: the store's next page, and once
//!   the store has none, the service's next older page ([`Core::load_older`]) read into the store first. The view
//!   never holds more than the pages it was scrolled through.
//! - **The list** is a bottom-aligned GPUI `list` that follows the tail while it is at the bottom, so a new message
//!   scrolls in; scrolled up, it stays where it is and a "Jump to newest" pill takes it back. Rows are laid out only
//!   as they come into view.
//! - **Rows**: a day separator before a new day; a "New messages" divider before the first message from someone else
//!   after the account's read horizon as it stood when the chat opened ([`Core::read_horizon`]); a sender's messages
//!   within 5 minutes of each other grouped under one name and time. A message shows its rich text through our own
//!   renderer ([`crate::rich_text`], cached per message id and version), "Edited" once edited, "This message was
//!   deleted" once deleted, its reactions with their counts (the account's own marked), and a thread's event as a
//!   line of its own. A message type this build doesn't show is a placeholder that names no content; `Control/…`
//!   messages (typing) are signals, never shown.
//! - **Typing**: who is typing now ([`crate::Chats::typing`]), under the messages; it goes off with their end of
//!   typing, their message, or [`crate::models::TYPING_TIMEOUT`] after their last signal.
//! - **Actions** on a message, under the pointer or selected with the keyboard (Up and Down, once the list has the
//!   focus): reply and react, and for the account's own message edit and delete. They are told as
//!   [`ChatViewEvent::Action`]; compose carries them out (M2 Task 13).
//!
//! What the view shows is traced (`chat-view shown`: the conversation, its newest message ids and their kinds; `typing
//! shown`/`cleared`), never text or a name.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Datelike as _, Local, NaiveDate, TimeZone};
use gpui::{
    AnyElement, App, AsyncApp, Context, ElementId, EventEmitter, FocusHandle, FollowMode, Global,
    InteractiveElement as _, IntoElement, KeyBinding, ListAlignment, ListState, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, actions, div,
    list, px, relative, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Sizable as _, Size};
use teams_core::EngineError;
use teams_proto::chat::{MessageType, RichText, parse_html, thread_activity};
use teams_store::{Message, PageCursor, Reaction};

use crate::bridge::Core;
use crate::models::Models;
use crate::rich_text::{self, Mentions, Rendered};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

actions!(
    teams_ui,
    [
        SelectPrevious,
        SelectNext,
        ReplyToSelected,
        ReactToSelected,
        EditSelected,
        DeleteSelected,
        ClearSelection
    ]
);

/// The message list's key context: its keys apply only while the list itself has the focus (never while compose's
/// text area has it).
pub const CONTEXT: &str = "ChatMessages";
/// Messages read from the store at a time; older ones load when fewer rows than this are above the view.
pub const PAGE: usize = 50;
/// A sender's messages within this long of the one before are grouped under one name and time, ms.
pub const GROUP_MS: i64 = 5 * 60 * 1_000;
/// How far beyond the view rows are laid out, px.
pub const OVERDRAW: f32 = 400.0;
/// The most message ids a trace line names (the newest).
pub const TRACED: usize = 50;
/// What a deleted message shows.
pub const DELETED: &str = "This message was deleted";
/// What a message of a type this build doesn't show shows.
pub const UNSUPPORTED: &str = "This message can't be shown here";
/// The divider at the read horizon.
pub const NEW_MESSAGES: &str = "New messages";
/// Older loads in a row that may bring nothing before rows near the top stop asking for more by themselves.
pub const MAX_FRUITLESS: u32 = 3;

/// Marks the view's key bindings as installed.
struct Bound;

impl Global for Bound {}

/// Binds the message list's keys, once per app.
pub fn init(cx: &mut App) {
    if cx.has_global::<Bound>() {
        return;
    }
    cx.set_global(Bound);
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("r", ReplyToSelected, context),
        KeyBinding::new("a", ReactToSelected, context),
        KeyBinding::new("e", EditSelected, context),
        KeyBinding::new("delete", DeleteSelected, context),
        KeyBinding::new("escape", ClearSelection, context),
    ]);
}

/// What can be done with a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageAction {
    Reply,
    React,
    /// The account's own message only.
    Edit,
    /// The account's own message only.
    Delete,
}

impl MessageAction {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Reply => "Reply",
            Self::React => "React",
            Self::Edit => "Edit",
            Self::Delete => "Delete",
        }
    }

    /// The name of its button's test selector.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::React => "react",
            Self::Edit => "edit",
            Self::Delete => "delete",
        }
    }
}

/// What the chat view tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatViewEvent {
    /// An action was chosen on a message (M2 Task 13 carries it out).
    Action { action: MessageAction, message: String },
}

/// How far the conversation's older history is loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum History {
    /// More may be older than what the view holds.
    More,
    /// The service has nothing older: the view holds the chat's beginning.
    Complete,
    /// The service's older page couldn't be read.
    Failed,
}

/// A message's rendered rich text, compared by identity: the cache hands out one per message id and version.
#[derive(Clone, Debug)]
pub struct RenderedText(pub Arc<Rendered>);

impl PartialEq for RenderedText {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// What a message row shows as its body.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    Text(RenderedText),
    Deleted,
    /// A thread's event (members added), as a line of its own.
    Event(SharedString),
    /// A type this build doesn't show, by its name.
    Unsupported(String),
}

impl Body {
    /// The trace's name for it.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Deleted => "deleted",
            Self::Event(_) => "event",
            Self::Unsupported(_) => "unsupported",
        }
    }
}

/// A reaction key on a message: how many reacted with it, and whether the account did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionCount {
    pub key: String,
    pub count: usize,
    pub mine: bool,
}

/// A message's row.
#[derive(Clone, Debug, PartialEq)]
pub struct MessageItem {
    pub id: String,
    pub version: i64,
    pub sender: Option<String>,
    pub sender_name: SharedString,
    pub own: bool,
    /// The first of its group: its name and time are shown.
    pub header: bool,
    pub arrival_time: i64,
    pub body: Body,
    pub edited: bool,
    pub reactions: Vec<ReactionCount>,
}

impl MessageItem {
    /// The actions it offers: none on an event, a deleted message or one this build doesn't show; edit and delete
    /// only on the account's own.
    #[must_use]
    pub fn actions(&self) -> Vec<MessageAction> {
        if !matches!(self.body, Body::Text(_)) {
            return Vec::new();
        }
        let mut actions = vec![MessageAction::Reply, MessageAction::React];
        if self.own {
            actions.extend([MessageAction::Edit, MessageAction::Delete]);
        }
        actions
    }
}

/// One row of the list.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// The top: how far the history is loaded.
    Top(History),
    Day(NaiveDate),
    NewMessages,
    Message(MessageItem),
}

/// The day separator's text.
#[must_use]
pub fn day_label(day: NaiveDate, today: NaiveDate) -> String {
    match (today - day).num_days() {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        _ if day.year() == today.year() => day.format("%A, %-d %B").to_string(),
        _ => day.format("%A, %-d %B %Y").to_string(),
    }
}

/// The local day of `ms` in `tz`.
#[must_use]
pub fn day_of<Tz: TimeZone>(ms: i64, tz: &Tz) -> Option<NaiveDate> {
    DateTime::from_timestamp_millis(ms).map(|at| at.with_timezone(tz).date_naive())
}

/// Whether `item` continues the group of `before`: the same sender, within [`GROUP_MS`], neither an event.
fn continues(before: &MessageItem, item: &MessageItem) -> bool {
    let events = matches!(before.body, Body::Event(_)) || matches!(item.body, Body::Event(_));
    !events
        && before.sender.is_some()
        && before.sender == item.sender
        && (0..=GROUP_MS).contains(&(item.arrival_time - before.arrival_time))
}

/// The rows of `messages` (oldest first): the top, day separators, the "New messages" divider before the first
/// message from someone else after `horizon`, and each message grouped with the one before when it continues it.
#[must_use]
pub fn arrange<Tz: TimeZone>(messages: Vec<MessageItem>, history: History, horizon: Option<i64>, tz: &Tz) -> Vec<Item> {
    let mut items = vec![Item::Top(history)];
    let mut day = None;
    let mut placed = horizon.is_none();
    let mut before: Option<MessageItem> = None;
    for mut item in messages {
        let today = day_of(item.arrival_time, tz);
        let new_day = today.is_some() && today != day;
        if new_day {
            day = today;
            items.extend(today.map(Item::Day));
        }
        // what the store counts unread: someone else's message with text, after the horizon
        let unread =
            !item.own && matches!(item.body, Body::Text(_)) && horizon.is_some_and(|at| item.arrival_time > at);
        let divider = !placed && unread;
        if divider {
            placed = true;
            items.push(Item::NewMessages);
        }
        item.header = new_day || divider || !before.as_ref().is_some_and(|before| continues(before, &item));
        before = Some(item.clone());
        items.push(Item::Message(item));
    }
    items
}

/// The reactions of a message by key, in the order they were first given; the account's own marked.
#[must_use]
pub fn count_reactions(reactions: &[Reaction], me: &str) -> Vec<ReactionCount> {
    let mut first: BTreeMap<&str, i64> = BTreeMap::new();
    for reaction in reactions {
        let at = first.entry(&reaction.key).or_insert(reaction.time);
        *at = (*at).min(reaction.time);
    }
    let mut keys: Vec<(&str, i64)> = first.into_iter().collect();
    keys.sort_by_key(|&(key, at)| (at, key));
    keys.into_iter()
        .map(|(key, _)| {
            let users: Vec<&Reaction> = reactions.iter().filter(|reaction| reaction.key == key).collect();
            ReactionCount {
                key: key.to_owned(),
                count: users.len(),
                mine: users.iter().any(|reaction| reaction.user == me),
            }
        })
        .collect()
}

/// What a reaction key shows: the emoji of the keys Teams names, else the key itself.
#[must_use]
pub fn reaction_label(key: &str) -> String {
    let emoji = match key {
        "like" => "👍",
        "heart" => "❤️",
        "laugh" => "😆",
        "surprised" => "😮",
        "sad" => "😢",
        "angry" => "😠",
        _ => return key.to_owned(),
    };
    emoji.to_owned()
}

/// A message's position in the store's order (arrival time, then id).
fn position(message: &Message) -> (i64, &str) {
    (message.arrival_time, message.id.as_str())
}

/// The store's messages of a conversation from the newest down to `oldest` (one page when `None`), the newest first,
/// and the cursor for the next older page.
async fn read_window(
    core: &dyn Core,
    conversation: &str,
    oldest: Option<(i64, String)>,
) -> Result<(Vec<Message>, Option<PageCursor>), EngineError> {
    let mut read = Vec::new();
    let mut before = None;
    loop {
        let page = core.messages(conversation.to_owned(), before, PAGE).await?;
        let reached = match (&oldest, page.messages.last()) {
            (Some((at, id)), Some(last)) => position(last) <= (*at, id.as_str()),
            _ => true,
        };
        read.extend(page.messages);
        before = page.next;
        if reached || before.is_none() {
            return Ok((read, before));
        }
    }
}

/// What a load read: the messages it brings (the newest first), the reactions of the versions not held yet, and
/// where the store's next older page starts.
struct Loaded {
    messages: Vec<Message>,
    reactions: HashMap<String, (i64, Vec<Reaction>)>,
    next: Option<PageCursor>,
}

/// The reactions of the messages whose version the view doesn't hold yet.
async fn new_reactions(
    core: &dyn Core,
    conversation: &str,
    messages: &[Message],
    held: &HashMap<String, i64>,
) -> HashMap<String, (i64, Vec<Reaction>)> {
    let mut read = HashMap::new();
    for message in messages {
        if held.get(&message.id) == Some(&message.version) || message.delete_time.is_some() {
            continue;
        }
        let reactions = core
            .reactions(conversation.to_owned(), message.id.clone())
            .await
            .unwrap_or_default();
        read.insert(message.id.clone(), (message.version, reactions));
    }
    read
}

/// Which load runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Load {
    /// The newest messages down to the oldest held.
    Window,
    /// The next older page.
    Older,
}

/// The chat view (see the module docs).
pub struct ChatView {
    core: Arc<dyn Core>,
    models: Models,
    trace: Arc<ViewTrace>,
    conversation: String,
    /// The messages held, the oldest first.
    messages: Vec<Message>,
    /// Per message id: the version its reactions were read at, and them.
    reactions: HashMap<String, (i64, Vec<Reaction>)>,
    /// Where the store's next older page starts; `None` once the store has no older message.
    next: Option<PageCursor>,
    history: History,
    /// The account's read horizon when the chat opened.
    horizon: Option<i64>,
    /// People's names from the store, by MRI (`None`: asked, none stored).
    names: HashMap<String, Option<String>>,
    /// Rendered rich text per message id, with the version it was rendered from.
    rendered: HashMap<String, (i64, Arc<Rendered>)>,
    /// How many times rich text was laid out.
    layouts: u64,
    items: Vec<Item>,
    list: ListState,
    loading: Option<Task<()>>,
    window_due: bool,
    older_due: bool,
    /// Loads that ended, and of those the older pages.
    loads: u64,
    older_loads: u64,
    /// Older loads in a row that brought no message: past [`MAX_FRUITLESS`], rows near the top stop asking for more
    /// until the view is asked again ([`ChatView::load_older`]).
    fruitless: u32,
    /// The models' revision of the chat the view last read at.
    seen: u64,
    /// The rows laid out in the last frame.
    drawn: Vec<usize>,
    hovered: Option<String>,
    selected: Option<String>,
    focus: FocusHandle,
    traced: Option<Vec<(String, &'static str)>>,
    typing_traced: bool,
    _observing: Subscription,
}

impl EventEmitter<ChatViewEvent> for ChatView {}

impl ChatView {
    /// The view of `conversation`, loading from the store at once.
    pub fn new(
        core: Arc<dyn Core>,
        models: Models,
        trace: Arc<ViewTrace>,
        conversation: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(OVERDRAW));
        list.set_follow_mode(FollowMode::Tail);
        let view: WeakEntity<Self> = cx.entity().downgrade();
        list.set_scroll_handler(move |_, _, cx| {
            // the pill follows the scroll
            let _ = view.update(cx, |_, cx| cx.notify());
        });
        let observing = cx.observe(&models.chats, |this: &mut Self, chats, cx| {
            let at = chats.read(cx).changed_at(&this.conversation);
            if at > this.seen {
                this.seen = at;
                this.window_due = true;
                this.kick(cx);
            }
            this.typing_changed(cx);
            cx.notify();
        });
        let seen = models.chats.read(cx).changed_at(&conversation);
        let mut view = Self {
            core,
            models,
            trace,
            conversation,
            messages: Vec::new(),
            reactions: HashMap::new(),
            next: None,
            history: History::More,
            horizon: None,
            names: HashMap::new(),
            rendered: HashMap::new(),
            layouts: 0,
            items: Vec::new(),
            list,
            loading: None,
            window_due: true,
            older_due: false,
            loads: 0,
            older_loads: 0,
            fruitless: 0,
            seen,
            drawn: Vec::new(),
            hovered: None,
            selected: None,
            focus: cx.focus_handle(),
            traced: None,
            typing_traced: false,
            _observing: observing,
        };
        view.kick(cx);
        view.typing_changed(cx);
        view
    }

    #[must_use]
    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    /// The rows, the top first.
    #[must_use]
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The message rows, the oldest first.
    pub fn message_items(&self) -> impl Iterator<Item = &MessageItem> {
        self.items.iter().filter_map(|item| match item {
            Item::Message(message) => Some(message),
            _ => None,
        })
    }

    /// How many messages the view holds.
    #[must_use]
    pub fn held(&self) -> usize {
        self.messages.len()
    }

    #[must_use]
    pub fn history(&self) -> History {
        self.history
    }

    /// Loads that ended, and of those the older pages.
    #[must_use]
    pub fn loads(&self) -> (u64, u64) {
        (self.loads, self.older_loads)
    }

    /// Whether a load is running.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading.is_some()
    }

    /// How many times rich text was laid out (once per message id and version).
    #[must_use]
    pub fn layouts(&self) -> u64 {
        self.layouts
    }

    /// The rows laid out in the last frame.
    #[must_use]
    pub fn drawn(&self) -> &[usize] {
        &self.drawn
    }

    #[must_use]
    pub fn list_state(&self) -> &ListState {
        &self.list
    }

    /// Whether the "Jump to newest" pill shows: the list is scrolled up from the newest.
    #[must_use]
    pub fn scrolled_up(&self) -> bool {
        !self.list.is_following_tail()
    }

    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    #[must_use]
    pub fn hovered(&self) -> Option<&str> {
        self.hovered.as_deref()
    }

    /// The message list's focus.
    #[must_use]
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Who is typing now (not the account), by name where the store has one.
    #[must_use]
    pub fn typing(&self, cx: &App) -> Vec<String> {
        let me = self.core.me();
        self.models
            .chats
            .read(cx)
            .typing(&self.conversation)
            .filter(|mri| *mri != me)
            .map(|mri| self.name_of(mri))
            .collect()
    }

    /// The typing line's text, if anyone is typing.
    #[must_use]
    pub fn typing_text(&self, cx: &App) -> Option<String> {
        let names = self.typing(cx);
        match names.as_slice() {
            [] => None,
            [one] => Some(format!("{one} is typing…")),
            [one, two] => Some(format!("{one} and {two} are typing…")),
            _ => Some("Several people are typing…".to_owned()),
        }
    }

    /// Scrolls to the newest message and follows the tail again.
    pub fn jump_to_newest(&mut self, cx: &mut Context<Self>) {
        self.list.scroll_to_end();
        self.list.set_follow_mode(FollowMode::Tail);
        cx.notify();
    }

    /// Selects a message (the keyboard's actions apply to it).
    pub fn select(&mut self, message: Option<String>, cx: &mut Context<Self>) {
        if self.selected != message {
            self.selected = message;
            cx.notify();
        }
    }

    /// Chooses `action` on `message`, if the message offers it: told as [`ChatViewEvent::Action`].
    pub fn act(&mut self, action: MessageAction, message: &str, cx: &mut Context<Self>) {
        let offered = self
            .message_items()
            .find(|item| item.id == message)
            .is_some_and(|item| item.actions().contains(&action));
        if offered {
            cx.emit(ChatViewEvent::Action {
                action,
                message: message.to_owned(),
            });
        }
    }

    /// Asks for the next older page (once the running load ends), even after loads that brought nothing or one that
    /// failed.
    pub fn load_older(&mut self, cx: &mut Context<Self>) {
        self.fruitless = 0;
        if self.history == History::Failed {
            self.history = History::More;
        }
        self.want_older(cx);
    }

    /// Asks for the next older page while there may be one and the last few loads brought something.
    fn want_older(&mut self, cx: &mut Context<Self>) {
        if self.history == History::More && self.fruitless < MAX_FRUITLESS && !self.older_due {
            self.older_due = true;
            self.kick(cx);
        }
    }

    /// Starts the load that is due, unless one runs: the window first, then an older page.
    fn kick(&mut self, cx: &mut Context<Self>) {
        if self.loading.is_some() {
            return;
        }
        let load = if self.window_due {
            self.window_due = false;
            Load::Window
        } else if std::mem::take(&mut self.older_due) && self.history == History::More {
            Load::Older
        } else {
            return;
        };
        let core = Arc::clone(&self.core);
        let conversation = self.conversation.clone();
        let oldest = self
            .messages
            .first()
            .map(|message| (message.arrival_time, message.id.clone()));
        let next = self.next.clone();
        let first = self.loads == 0;
        let held: HashMap<String, i64> = self
            .reactions
            .iter()
            .map(|(id, (version, _))| (id.clone(), *version))
            .collect();
        self.loading = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let read = match load {
                Load::Window => window(&*core, &conversation, oldest, first, &held).await,
                Load::Older => older(&*core, &conversation, oldest, next, &held).await,
            };
            let _ = this.update(cx, |view, cx| view.loaded(load, read, cx));
        }));
    }

    /// A load ended: what it read is held and shown.
    fn loaded(&mut self, load: Load, read: Result<Read, EngineError>, cx: &mut Context<Self>) {
        self.loading = None;
        self.loads += 1;
        match (load, read) {
            (_, Ok(read)) => self.take(load, read),
            // the engine stopped: what is shown stays
            (Load::Window, Err(_)) => {}
            (Load::Older, Err(_)) => self.history = History::Failed,
        }
        if load == Load::Older {
            self.older_loads += 1;
        }
        self.want_names(cx);
        self.rebuild(cx);
        self.kick(cx);
    }

    fn take(&mut self, load: Load, read: Read) {
        if let Horizon::Read(horizon) = read.horizon {
            self.horizon = horizon;
        }
        if let Some(history) = read.history {
            self.history = history;
        }
        let Some(loaded) = read.loaded else { return };
        self.reactions.extend(loaded.reactions);
        let mut messages = loaded.messages;
        messages.reverse();
        match load {
            Load::Window => {
                // the window read reaches down to the oldest held: anything older stays
                let oldest = messages
                    .first()
                    .map(|message| (message.arrival_time, message.id.clone()));
                let kept: Vec<Message> = match &oldest {
                    Some((at, id)) => self
                        .messages
                        .iter()
                        .filter(|message| position(message) < (*at, id.as_str()))
                        .cloned()
                        .collect(),
                    None => Vec::new(),
                };
                if kept.is_empty() {
                    self.next = loaded.next;
                }
                self.messages = kept.into_iter().chain(messages).collect();
            }
            Load::Older => {
                self.next = loaded.next;
                self.fruitless = if messages.is_empty() { self.fruitless + 1 } else { 0 };
                messages.extend(std::mem::take(&mut self.messages));
                self.messages = messages;
            }
        }
        let held: HashSet<&str> = self.messages.iter().map(|message| message.id.as_str()).collect();
        self.reactions.retain(|id, _| held.contains(id.as_str()));
        self.rendered.retain(|id, _| held.contains(id.as_str()));
    }

    /// Asks the store for the names of the people the rows name and the store's messages didn't (senders without a
    /// name, events' members, who is typing).
    fn want_names(&mut self, cx: &mut Context<Self>) {
        let mut wanted: Vec<String> = Vec::new();
        for message in &self.messages {
            if message.sender_name.as_deref().is_none_or(|name| name.trim().is_empty())
                && let Some(sender) = &message.sender
            {
                wanted.push(sender.clone());
            }
            if let Some(activity) = (message.message_type == MessageType::AddMember.as_str())
                .then(|| thread_activity(&message.content))
                .flatten()
            {
                wanted.extend(activity.initiator);
                wanted.extend(activity.targets);
            }
        }
        let typing: Vec<String> = self
            .models
            .chats
            .read(cx)
            .typing(&self.conversation)
            .map(str::to_owned)
            .collect();
        wanted.extend(typing);
        self.ask_names(wanted, cx);
    }

    fn ask_names(&mut self, wanted: Vec<String>, cx: &mut Context<Self>) {
        let me = self.core.me().to_owned();
        let mut asked = Vec::new();
        for mri in wanted {
            if mri != me && !self.names.contains_key(&mri) {
                self.names.insert(mri.clone(), None);
                asked.push(mri);
            }
        }
        if asked.is_empty() {
            return;
        }
        let core = Arc::clone(&self.core);
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let mut found = Vec::new();
            for mri in asked {
                if let Ok(Some(name)) = core.display_name(mri.clone()).await {
                    found.push((mri, name));
                }
            }
            if found.is_empty() {
                return;
            }
            let _ = this.update(cx, |view, cx| {
                for (mri, name) in found {
                    view.names.insert(mri, Some(name));
                }
                view.rebuild(cx);
            });
        })
        .detach();
    }

    /// Someone started or stopped typing.
    fn typing_changed(&mut self, cx: &mut Context<Self>) {
        let typing: Vec<String> = self
            .models
            .chats
            .read(cx)
            .typing(&self.conversation)
            .map(str::to_owned)
            .collect();
        let me = self.core.me();
        let shown = typing.iter().any(|mri| mri != me);
        if shown != self.typing_traced {
            self.typing_traced = shown;
            let change = if shown { "shown" } else { "cleared" };
            self.trace
                .record(&TraceLine::new("typing", change).conversation(&self.conversation));
        }
        self.ask_names(typing, cx);
    }

    /// A person's name: the store's, else "You" for the account, else their MRI.
    fn name_of(&self, mri: &str) -> String {
        if mri == self.core.me() {
            return "You".to_owned();
        }
        self.names.get(mri).cloned().flatten().unwrap_or_else(|| mri.to_owned())
    }

    /// A message's rich text, laid out once per id and version.
    fn rendered(&mut self, message: &Message, text: &RichText) -> Arc<Rendered> {
        if let Some((version, rendered)) = self.rendered.get(&message.id)
            && *version == message.version
        {
            return Arc::clone(rendered);
        }
        self.layouts += 1;
        let mentions = Mentions::of(message.properties.as_deref(), self.core.me());
        let rendered = Arc::new(rich_text::layout(text, &mentions));
        self.rendered
            .insert(message.id.clone(), (message.version, Arc::clone(&rendered)));
        rendered
    }

    /// What a message shows.
    fn body(&mut self, message: &Message) -> Body {
        if message.delete_time.is_some() {
            return Body::Deleted;
        }
        match MessageType::from(message.message_type.clone()) {
            MessageType::RichTextHtml => {
                Body::Text(RenderedText(self.rendered(message, &parse_html(&message.content))))
            }
            MessageType::Text => Body::Text(RenderedText(self.rendered(message, &RichText::plain(&message.content)))),
            MessageType::AddMember => Body::Event(self.event_text(message).into()),
            other => Body::Unsupported(other.as_str().to_owned()),
        }
    }

    /// A thread event's line: who added whom.
    fn event_text(&self, message: &Message) -> String {
        let Some(activity) = thread_activity(&message.content) else {
            return "Members changed".to_owned();
        };
        let targets: Vec<String> = activity.targets.iter().map(|mri| self.name_of(mri)).collect();
        match activity.initiator {
            Some(initiator) => format!("{} added {}", self.name_of(&initiator), targets.join(", ")),
            None => format!("Added {}", targets.join(", ")),
        }
    }

    /// A message's row, before it is arranged; `None` for a control message (a signal, never shown).
    fn item(&mut self, message: &Message) -> Option<MessageItem> {
        if MessageType::from(message.message_type.clone()).is_control() {
            return None;
        }
        let me = self.core.me().to_owned();
        let body = self.body(message);
        let sender_name = message
            .sender_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .or_else(|| message.sender.as_deref().map(|sender| self.name_of(sender)))
            .unwrap_or_else(|| "Unknown".to_owned());
        let reactions = self
            .reactions
            .get(&message.id)
            .map(|(_, reactions)| count_reactions(reactions, &me))
            .unwrap_or_default();
        Some(MessageItem {
            id: message.id.clone(),
            version: message.version,
            own: message.sender.as_deref() == Some(me.as_str()),
            sender: message.sender.clone(),
            sender_name: sender_name.into(),
            header: true,
            arrival_time: message.arrival_time,
            edited: message.edit_time.is_some() && !matches!(body, Body::Deleted),
            body,
            reactions,
        })
    }

    /// Builds the rows again and tells the list which changed.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let messages = std::mem::take(&mut self.messages);
        let rows: Vec<MessageItem> = messages.iter().filter_map(|message| self.item(message)).collect();
        self.messages = messages;
        let items = arrange(rows, self.history, self.horizon, &Local);
        splice(&self.list, &self.items, &items);
        self.items = items;
        if let Some(selected) = &self.selected
            && !self.message_items().any(|item| &item.id == selected)
        {
            self.selected = None;
        }
        self.trace_items();
        cx.notify();
    }

    /// Traces the rows shown, when they changed.
    fn trace_items(&mut self) {
        let shown: Vec<(String, &'static str)> = self
            .message_items()
            .map(|item| (item.id.clone(), if item.edited { "edited" } else { item.body.kind() }))
            .collect();
        let shown: Vec<(String, &'static str)> = shown[shown.len().saturating_sub(TRACED)..].to_vec();
        if self.traced.as_ref() == Some(&shown) {
            return;
        }
        let line = TraceLine::new("chat-view", "shown")
            .conversation(&self.conversation)
            .messages(shown.iter().map(|(id, _)| id.as_str()));
        let line = shown.iter().fold(line, |line, (_, kind)| line.kind(kind));
        self.trace.record(&line);
        self.traced = Some(shown);
    }

    /// Moves the selection by `step` messages (from the newest when none is selected).
    fn step_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.message_items().map(|item| item.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let last = ids.len() - 1;
        let at = match self
            .selected
            .as_ref()
            .and_then(|selected| ids.iter().position(|id| id == selected))
        {
            Some(at) => at.saturating_add_signed(step).min(last),
            None => last,
        };
        self.select(Some(ids[at].clone()), cx);
        if let Some(index) = self
            .items
            .iter()
            .position(|item| matches!(item, Item::Message(message) if message.id == ids[at]))
        {
            self.list.scroll_to_reveal_item(index);
        }
    }

    fn act_on_selected(&mut self, action: MessageAction, cx: &mut Context<Self>) {
        if let Some(selected) = self.selected.clone() {
            self.act(action, &selected, cx);
        }
    }

    /// The row at `index`; a row near the top asks for the next older page.
    fn render_item(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        self.drawn.push(index);
        if index < PAGE {
            self.want_older(cx);
        }
        let palette = theme::current(cx);
        match self.items.get(index) {
            Some(Item::Top(history)) => top_row(*history, palette, cx),
            Some(Item::Day(day)) => day_row(*day, palette),
            Some(Item::NewMessages) => new_messages_row(palette),
            Some(Item::Message(item)) => {
                let item = item.clone();
                self.message_row(&item, palette, cx)
            }
            None => div().into_any_element(),
        }
    }

    fn message_row(&self, item: &MessageItem, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = item.id.clone();
        let hovered_id = id.clone();
        let clicked_id = id.clone();
        let highlighted = self.selected.as_deref() == Some(id.as_str());
        let actions =
            (highlighted || self.hovered.as_deref() == Some(id.as_str())).then(|| actions_bar(item, palette, cx));
        let row = div()
            .id(ElementId::Name(format!("message-{id}").into()))
            .debug_selector(move || format!("message-{id}"))
            .relative()
            .w_full()
            .px_4()
            .pt(px(if item.header { 8.0 } else { 2.0 }))
            .flex()
            .flex_col();
        // the account's own messages to the right
        let row = if item.own { row.items_end() } else { row.items_start() };
        let row = row
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let now = hovered.then(|| hovered_id.clone());
                if *hovered || this.hovered.as_deref() == Some(hovered_id.as_str()) {
                    this.hovered = now;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus.focus(window, cx);
                this.select(Some(clicked_id.clone()), cx);
            }));
        let row = if matches!(item.body, Body::Event(_)) {
            row.child(event_line(item, palette))
        } else {
            row.children(item.header.then(|| header_line(item, palette)))
                .child(bubble(item, highlighted, palette))
                .children(footer_line(item, palette))
        };
        row.children(actions).into_any_element()
    }
}

/// The actions a message offers, as buttons over its row's top right corner.
fn actions_bar(item: &MessageItem, palette: Palette, cx: &mut Context<ChatView>) -> AnyElement {
    let buttons = item.actions().into_iter().map(|action| {
        let message = item.id.clone();
        let name = action.name();
        div().debug_selector(move || format!("action-{name}")).child(
            Button::new(ElementId::Name(format!("action-{name}-{message}").into()))
                .label(action.label())
                .ghost()
                .with_size(Size::XSmall)
                .on_click(cx.listener(move |this, _, _, cx| this.act(action, &message, cx))),
        )
    });
    div()
        .debug_selector(|| "message-actions".to_owned())
        .absolute()
        .top_0()
        .right(px(16.0))
        .flex()
        .gap_1()
        .px_1()
        .rounded_md()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .children(buttons)
        .into_any_element()
}

/// Whether a load read the account's read horizon, and what it was.
enum Horizon {
    NotRead,
    /// The last read message's arrival time; `None` when the account has none in the chat.
    Read(Option<i64>),
}

/// What a load read.
struct Read {
    loaded: Option<Loaded>,
    /// Read on the first load.
    horizon: Horizon,
    /// The history's new state, when the load learnt it.
    history: Option<History>,
}

/// The window load: the newest messages down to `oldest`, and on the first load the account's read horizon.
async fn window(
    core: &dyn Core,
    conversation: &str,
    oldest: Option<(i64, String)>,
    first: bool,
    held: &HashMap<String, i64>,
) -> Result<Read, EngineError> {
    let horizon = if first {
        let horizon = core.read_horizon(conversation.to_owned()).await.ok().flatten();
        Horizon::Read(horizon.map(|horizon| horizon.last_read_arrival))
    } else {
        Horizon::NotRead
    };
    let (messages, next) = read_window(core, conversation, oldest).await?;
    let reactions = new_reactions(core, conversation, &messages, held).await;
    Ok(Read {
        loaded: Some(Loaded {
            messages,
            reactions,
            next,
        }),
        horizon,
        history: None,
    })
}

/// The older load: the store's next page; once the store has none, the service's next older page read into the store
/// first.
async fn older(
    core: &dyn Core,
    conversation: &str,
    oldest: Option<(i64, String)>,
    next: Option<PageCursor>,
    held: &HashMap<String, i64>,
) -> Result<Read, EngineError> {
    let before = if let Some(next) = next {
        next
    } else {
        if core.load_older(conversation.to_owned()).await?.is_none() {
            return Ok(Read {
                loaded: None,
                horizon: Horizon::NotRead,
                history: Some(History::Complete),
            });
        }
        let Some((arrival_time, id)) = oldest else {
            // nothing was held: the newest page now
            return window(core, conversation, None, false, held).await;
        };
        PageCursor { arrival_time, id }
    };
    let page = core.messages(conversation.to_owned(), Some(before), PAGE).await?;
    let reactions = new_reactions(core, conversation, &page.messages, held).await;
    Ok(Read {
        loaded: Some(Loaded {
            messages: page.messages,
            reactions,
            next: page.next,
        }),
        horizon: Horizon::NotRead,
        history: None,
    })
}

/// A row's identity in the list, for telling which rows changed.
fn key(item: &Item) -> (u8, String) {
    match item {
        Item::Top(_) => (0, String::new()),
        Item::Day(day) => (1, day.to_string()),
        Item::NewMessages => (2, String::new()),
        Item::Message(message) => (3, message.id.clone()),
    }
}

/// Tells `list` how `old` became `new`: the rows between the unchanged start and end are replaced, so rows added
/// above or below keep the scroll where it is; a row changed in place is measured again.
fn splice(list: &ListState, old: &[Item], new: &[Item]) {
    let same = old.iter().zip(new).take_while(|(old, new)| old == new).count();
    let max_tail = old.len().min(new.len()) - same;
    let tail = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(max_tail)
        .take_while(|(old, new)| old == new)
        .count();
    let old_middle = same..old.len() - tail;
    let new_middle = same..new.len() - tail;
    if old_middle.is_empty() && new_middle.is_empty() {
        return;
    }
    let old_keys: Vec<(u8, String)> = old[old_middle.clone()].iter().map(key).collect();
    let new_keys: Vec<(u8, String)> = new[new_middle.clone()].iter().map(key).collect();
    if old_keys == new_keys {
        list.remeasure_items(old_middle);
    } else {
        list.splice(old_middle, new_middle.len());
    }
}

fn centered_line(text: impl Into<SharedString>, palette: Palette) -> gpui::Div {
    div()
        .w_full()
        .py_2()
        .flex()
        .justify_center()
        .text_xs()
        .text_color(rgb(palette.muted_foreground))
        .child(text.into())
}

/// The top row: how far the history is loaded; after a failure, a button to try again.
fn top_row(history: History, palette: Palette, cx: &mut Context<ChatView>) -> AnyElement {
    let text = match history {
        History::More => "Loading older messages…",
        History::Complete => "This is the beginning of the chat",
        History::Failed => "Older messages couldn't be loaded",
    };
    let again = (history == History::Failed).then(|| {
        Button::new("load-older-again")
            .label("Try again")
            .ghost()
            .with_size(Size::XSmall)
            .on_click(cx.listener(|this, _, _, cx| this.load_older(cx)))
    });
    centered_line(text, palette)
        .gap_2()
        .items_center()
        .debug_selector(|| "history-top".to_owned())
        .children(again)
        .into_any_element()
}

fn day_row(day: NaiveDate, palette: Palette) -> AnyElement {
    let label = day_label(day, Local::now().date_naive());
    centered_line(label, palette)
        .debug_selector(move || format!("day-{day}"))
        .into_any_element()
}

fn new_messages_row(palette: Palette) -> AnyElement {
    let rule = || div().flex_1().h(px(1.0)).bg(rgb(palette.brand));
    div()
        .debug_selector(|| "new-messages".to_owned())
        .w_full()
        .px_4()
        .py_1()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(rgb(palette.brand))
        .child(rule())
        .child(NEW_MESSAGES)
        .child(rule())
        .into_any_element()
}

fn event_line(item: &MessageItem, palette: Palette) -> AnyElement {
    let text = match &item.body {
        Body::Event(text) => text.clone(),
        _ => SharedString::default(),
    };
    centered_line(text, palette).into_any_element()
}

fn header_line(item: &MessageItem, palette: Palette) -> AnyElement {
    let time = DateTime::from_timestamp_millis(item.arrival_time)
        .map(|at| at.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_default();
    let name = (!item.own).then(|| {
        div()
            .font_weight(gpui::FontWeight::BOLD)
            .child(item.sender_name.clone())
    });
    div()
        .flex()
        .gap_2()
        .items_baseline()
        .text_sm()
        .children(name)
        .child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(time))
        .into_any_element()
}

fn bubble(item: &MessageItem, highlighted: bool, palette: Palette) -> AnyElement {
    let background = match (highlighted, item.own) {
        (true, _) => palette.selection,
        (false, true) => palette.selected,
        (false, false) => palette.surface,
    };
    let id: SharedString = format!("message-text-{}", item.id).into();
    let content = match &item.body {
        Body::Text(text) => rich_text::render(&id, &text.0, palette),
        Body::Deleted => muted(DELETED, palette),
        Body::Unsupported(_) => muted(UNSUPPORTED, palette),
        Body::Event(_) => div().into_any_element(),
    };
    div()
        .max_w(relative(0.8))
        .px_3()
        .py_1()
        .rounded_md()
        .bg(rgb(background))
        .child(content)
        .into_any_element()
}

fn muted(text: &'static str, palette: Palette) -> AnyElement {
    div()
        .italic()
        .text_color(rgb(palette.muted_foreground))
        .child(text)
        .into_any_element()
}

/// "Edited" and the reactions, if any.
fn footer_line(item: &MessageItem, palette: Palette) -> Option<AnyElement> {
    if !item.edited && item.reactions.is_empty() {
        return None;
    }
    let edited = item.edited.then(|| {
        div()
            .debug_selector(|| "edited".to_owned())
            .text_xs()
            .text_color(rgb(palette.muted_foreground))
            .child("Edited")
    });
    let reactions = item.reactions.iter().map(|reaction| {
        let key = reaction.key.clone();
        let chip = div()
            .debug_selector(move || format!("reaction-{key}"))
            .px_1()
            .rounded_md()
            .border_1()
            .text_xs()
            .child(format!("{} {}", reaction_label(&reaction.key), reaction.count));
        if reaction.mine {
            chip.border_color(rgb(palette.brand)).bg(rgb(palette.selected))
        } else {
            chip.border_color(rgb(palette.border))
        }
    });
    Some(
        div()
            .flex()
            .gap_1()
            .items_center()
            .pt_1()
            .children(edited)
            .children(reactions)
            .into_any_element(),
    )
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        self.drawn.clear();
        let rows = list(
            self.list.clone(),
            cx.processor(|this: &mut Self, index: usize, _window, cx| this.render_item(index, cx)),
        )
        .size_full()
        .pb_3();
        let pill = (self.scrolled_up() && !self.items.is_empty()).then(|| {
            div()
                .debug_selector(|| "jump-to-newest".to_owned())
                .absolute()
                .bottom(px(12.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    Button::new("jump-to-newest")
                        .label("Jump to newest")
                        .primary()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.jump_to_newest(cx))),
                )
        });
        let typing = self.typing_text(cx).map(|text| {
            div()
                .debug_selector(|| "typing".to_owned())
                .px_4()
                .h(px(20.0))
                .text_xs()
                .italic()
                .text_color(rgb(palette.muted_foreground))
                .child(text)
        });
        let messages = div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step_selection(1, cx)))
            .on_action(cx.listener(|this, _: &ReplyToSelected, _, cx| this.act_on_selected(MessageAction::Reply, cx)))
            .on_action(cx.listener(|this, _: &ReactToSelected, _, cx| this.act_on_selected(MessageAction::React, cx)))
            .on_action(cx.listener(|this, _: &EditSelected, _, cx| this.act_on_selected(MessageAction::Edit, cx)))
            .on_action(cx.listener(|this, _: &DeleteSelected, _, cx| this.act_on_selected(MessageAction::Delete, cx)))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| this.select(None, cx)))
            .debug_selector(|| "chat-messages".to_owned())
            .relative()
            .flex_1()
            .min_h_0()
            .child(rows)
            .children(pill);
        div()
            .debug_selector(|| "chat-view".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .child(messages)
            .children(typing)
    }
}
