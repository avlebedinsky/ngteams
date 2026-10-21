//! The chat view: one conversation's messages, the newest at the bottom.
//!
//! - **From the store only.** The view reads the store's newest page when it opens ([`Core::messages`], [`PAGE`]
//!   messages), and again, down to the oldest message it holds, each time the models say its chat changed (a
//!   delivery, a catch-up, an outbox row). A message's reactions come from the store too, read again only for a new
//!   version of it.
//! - **Older pages** load when fewer than a page of rows ([`PAGE`]) is above the view: the store's next page, and once
//!   the store has none, the service's next older page ([`Core::load_older`]) read into the store first. The view
//!   never holds more than the pages it was scrolled through. Nothing older from the service is the chat's beginning
//!   only when the store says its whole history is stored ([`Core::history_complete`]); a chat never synced is asked
//!   again once it changes, and after a few older loads that brought nothing the top offers to load again.
//! - **The list** is a bottom-aligned GPUI `list` that follows the tail while it is at the bottom, so a new message
//!   scrolls in; scrolled up, it stays where it is and a "Jump to newest" pill takes it back; a selection moved with
//!   the keyboard stops following. Rows are laid out only as they come into view. When the rows change, the list is
//!   told only which rows were added, removed or changed, and the message at the top of the view keeps its place:
//!   rows added above it (an older page) or below it never move it.
//! - **Rows**: a day separator before a new day; a "New messages" divider before the first message from someone else
//!   after the account's read horizon as it stood when the chat opened ([`Core::read_horizon`]), as the store counts
//!   unread; a sender's messages within 5 minutes of each other grouped under one name and time, someone else's with
//!   their avatar (M2 Task 12: their picture from the engine, else their initials). The name is their profile's
//!   ([`Core::person`]: Graph's, else the middle tier's profile lookup's), else the one their message carried, else
//!   their newest message's or one the binary knew, else their MRI; a name or a picture read later shows once the
//!   engine tells it (`Event::Profile`). A message shows its
//!   rich text through our own renderer ([`crate::rich_text`], parsed and laid out once per message id and version),
//!   "Edited" once edited, "This message was deleted" once deleted, its reactions with their counts (the account's own
//!   marked), and a thread's event as a line of its own. A message type this build doesn't show is a placeholder
//!   that names no content; `Control/…` messages (typing) are signals, never shown.
//! - **Typing**: who is typing now ([`crate::Chats::typing`]), under the messages; it goes off with their end of
//!   typing, their message, or [`crate::models::TYPING_TIMEOUT`] after their last signal.
//! - **Actions** on a message, under the pointer or selected with the keyboard (Up and Down, once the list has the
//!   focus): reply and react, and for the account's own message edit and delete. Each is told as
//!   [`ChatViewEvent::Action`] and carried out (M2 Task 13): a reply and an edit go to the [`Compose`] box under the
//!   messages; react opens the quick reactions M2 Task 10 captured ([`QUICK_REACTIONS`], keys 1–4), and a reaction
//!   chip of one of them clicked sets or takes back the account's own (a chip of any other key only shows); react and
//!   delete select the message they act on, and moving the selection closes them; delete asks first (Enter or the
//!   button confirms, Escape cancels). Every write goes out through the engine ([`Core::write`]), never from the view.
//! - **The account's writes as they go out** ([`crate::sending`]): a message post shows at once as a pending message of
//!   its own (the engine tells its row as it is made, and the rows are built again then, before any store read),
//!   with its state, until its message is held or found in the store ([`Core::message`], by the id the row was sent
//!   with: a message older than the pages held); an edit, a delete or a reaction shows its state on the message it
//!   acts on, unless a newer write on the same thing makes it out of date ([`sending::superseded`]). "Send again" on
//!   a failed row or an earlier session's is the engine's explicit [`Core::resend`], after a warning when its message
//!   may have arrived, or when it changes a message (a later change may have gone out since).
//! - **Read receipts**: with the "send read receipts" setting on, while the chat is shown ([`ChatView::set_shown`]:
//!   the shell hides it behind the settings), the window is active and the list is at the newest message, the
//!   account's read horizon moves to the newest message from someone else that is after it, [`READ_DEBOUNCE`] after
//!   the view settles, one at a time.
//! - **Opened at a message** ([`ChatView::reveal`], a search's hit, M2 Task 15): the view reads the store back to the
//!   message, scrolls to it and selects it; the list stops following the newest.
//!
//! What the view shows is traced (`chat-view shown`: the conversation, its newest message ids and their kinds;
//! `chat-view revealed`: the message opened at; `typing shown`/`cleared`; `outbox shown`: the rows shown and their
//! states), never text or a name.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike as _, Local, NaiveDate, TimeZone, Utc};
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Context, ElementId, Entity, EventEmitter, FocusHandle, FollowMode,
    Global, InteractiveElement as _, IntoElement, KeyBinding, ListAlignment, ListOffset, ListState, ParentElement as _,
    Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window,
    actions, div, list, px, relative, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Sizable as _, Size};
use teams_core::{Action, Command, EngineError};
use teams_proto::chat::{MessageType, RichText, ThreadActivity, parse_html, thread_activity};
use teams_store::{Message, OutboxEntry, OutboxKind, OutboxState, PageCursor, Reaction};

use crate::bridge::Core;
use crate::compose::{self, Compose, ComposeEvent, ComposeParts, Quoted};
use crate::models::Models;
use crate::people::{Face, Shown};
use crate::rich_text::{self, Mentions, Rendered};
use crate::sending::{self, Again, Note, RowState};
use crate::settings::SettingsModel;
use crate::shell::ShellParts;
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
        ClearSelection,
        ConfirmDelete,
        PickLike,
        PickHeart,
        PickLaugh,
        PickSurprised
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
/// The quick reactions, in the order the web client's hover bar offered them (M2 Task 10, `docs/protocol/chat.md`,
/// "Reactions (chat-shapes)"); keys 1–4 pick them once react is chosen.
pub const QUICK_REACTIONS: [&str; 4] = ["like", "heart", "laugh", "surprised"];
/// How long the view waits, once a read receipt is due, before it sends it (and looks again whether it still is).
pub const READ_DEBOUNCE: Duration = Duration::from_millis(800);
/// What the delete's confirmation asks.
pub const CONFIRM_DELETE: &str = "Delete this message?";
/// What "send again" warns of on a row whose message may have arrived.
pub const MAY_HAVE_ARRIVED: &str = "This message may have arrived already. Send it again anyway?";
/// What "send again" warns of on an edit, a delete or a reaction: sent again, it would undo a later change.
pub const MAY_BE_OUT_OF_DATE: &str =
    "A later change to this message may have gone out since. Send this one again anyway?";

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
        KeyBinding::new("enter", ConfirmDelete, context),
        KeyBinding::new("1", PickLike, context),
        KeyBinding::new("2", PickHeart, context),
        KeyBinding::new("3", PickLaugh, context),
        KeyBinding::new("4", PickSurprised, context),
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
    /// The conversation was never synced, so nothing older can be read yet: asked again once the chat changes.
    Unsynced,
    /// The last [`MAX_FRUITLESS`] older loads brought nothing: asked again only on demand ([`ChatView::load_older`]).
    Stalled,
}

impl History {
    /// The top row's text.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::More => "Loading older messages…",
            Self::Complete => "This is the beginning of the chat",
            Self::Failed => "Older messages couldn't be loaded",
            Self::Unsynced => "Older messages aren't synced yet",
            Self::Stalled => "No older messages came",
        }
    }

    /// The top row's button, which asks for older messages again: none while a load may run or at the beginning.
    #[must_use]
    pub fn again(self) -> Option<&'static str> {
        match self {
            Self::More | Self::Complete => None,
            Self::Failed => Some("Try again"),
            Self::Unsynced | Self::Stalled => Some("Load older"),
        }
    }
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
    /// The sender's avatar beside `sender_name`: their picture from the engine, else the initials of their name.
    pub face: Face,
    pub own: bool,
    /// The first of its group: its name and time are shown.
    pub header: bool,
    pub arrival_time: i64,
    pub body: Body,
    /// Its `messagetype`: whether the store counts it unread ([`counts_unread`]), whether or not this build shows it.
    pub message_type: String,
    pub edited: bool,
    pub reactions: Vec<ReactionCount>,
    /// The account's writes on it that aren't sent (an edit, a delete, a reaction) and that no newer write on the same
    /// thing made out of date, as they stand, the oldest first.
    pub outbox: Vec<Note>,
}

/// A message post of the account's whose message isn't in the store yet: its row as it stands, and its text.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingItem {
    pub note: Note,
    pub created_at: i64,
    /// Its text as the row carries it; `None` when the row can't be read back.
    pub body: Option<RenderedText>,
}

/// Whether a message of `message_type` counts unread, as [`teams_store::Store::unread_count`] counts it: `Text` and
/// every `RichText…` type (a card too).
#[must_use]
pub fn counts_unread(message_type: &str) -> bool {
    message_type == MessageType::Text.as_str() || message_type.starts_with("RichText")
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
    /// A message post on its way, after every message.
    Pending(PendingItem),
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
        // what the store counts unread: someone else's text message of any `RichText…` type, not deleted, after the
        // horizon; without a horizon the store counts every one, but nothing marks where reading stopped: no divider
        let unread = !item.own
            && counts_unread(&item.message_type)
            && item.body != Body::Deleted
            && horizon.is_some_and(|at| item.arrival_time > at);
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

/// The reactions of the messages whose version the view doesn't hold yet; a deleted message has none (the store drops
/// them with it), so none is read for it and any held before are replaced.
async fn new_reactions(
    core: &dyn Core,
    conversation: &str,
    messages: &[Message],
    held: &HashMap<String, i64>,
) -> HashMap<String, (i64, Vec<Reaction>)> {
    let mut read = HashMap::new();
    for message in messages {
        if held.get(&message.id) == Some(&message.version) {
            continue;
        }
        let reactions = if message.delete_time.is_some() {
            Vec::new()
        } else {
            core.reactions(conversation.to_owned(), message.id.clone())
                .await
                .unwrap_or_default()
        };
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
    /// People as the engine has them, by MRI: the ask whose answer the entry takes, and that answer (`None`: asked,
    /// no answer yet or none).
    people: HashMap<String, (u64, Option<Shown>)>,
    /// The last ask's number: an answer to an earlier ask of someone asked again since is dropped.
    asks: u64,
    /// The models' revision the people were last asked at: someone renamed since is asked again.
    people_seen: u64,
    /// Rendered rich text per message id, with the version it was rendered from.
    rendered: HashMap<String, (i64, Arc<Rendered>)>,
    /// A thread event's parsed content per message id, with the version it was parsed from.
    activities: HashMap<String, (i64, Option<Arc<ThreadActivity>>)>,
    /// How many times rich text was laid out.
    layouts: u64,
    /// How many times a message's content was parsed (its HTML, or a thread event's XML).
    parses: u64,
    items: Vec<Item>,
    list: ListState,
    loading: Option<Task<()>>,
    window_due: bool,
    older_due: bool,
    /// Loads that ended, and of those the older pages.
    loads: u64,
    older_loads: u64,
    /// Older loads in a row that brought no message: at [`MAX_FRUITLESS`] the history is [`History::Stalled`].
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
    settings: Entity<SettingsModel>,
    compose: Entity<Compose>,
    /// The message the quick reactions are open on.
    reacting: Option<String>,
    /// The message whose delete waits for its confirmation.
    deleting: Option<String>,
    /// The row whose "send again" waits for its confirmation (its message may have arrived).
    resending: Option<i64>,
    /// The conversation's outbox rows that aren't sent, as the store had them at the last load.
    unsettled: Vec<OutboxEntry>,
    /// The rows earlier sessions left, by id.
    earlier: HashSet<i64>,
    /// This session's message posts told sent whose message was found in the store, by row id: never shown pending.
    stored_posts: HashSet<i64>,
    /// Whether the chat is shown (not hidden behind the settings): read receipts go only while it is.
    shown: Visibility,
    /// Rendered text of the pending message posts, per row id.
    pending_rendered: HashMap<i64, Arc<Rendered>>,
    /// The newest message the account has read up to (its arrival time): the store's horizon, or the last receipt.
    read_upto: Option<i64>,
    /// The read receipt's wait, while one is due.
    receipt: Option<Task<()>>,
    /// Read receipts sent.
    receipts: u64,
    /// Whether the window is active (the user's).
    active: Activation,
    outbox_traced: Option<Vec<(String, &'static str)>>,
    /// The message to open the view at (its arrival time and id), until a window load reads back to it.
    reveal: Option<(i64, String)>,
    /// The message the running window load reads back to, shown once it ends.
    revealing: Option<(i64, String)>,
    /// The models' changes, the compose box's events and the window's activation.
    _subscriptions: [Subscription; 3],
}

impl EventEmitter<ChatViewEvent> for ChatView {}

impl ChatView {
    /// The view of `conversation` with its compose box, loading from the store at once.
    pub fn new(parts: ShellParts, conversation: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ShellParts {
            core,
            models,
            settings,
            trace,
        } = parts;
        let list = Self::following_list(cx);
        let compose_parts = ComposeParts {
            core: Arc::clone(&core),
            settings: settings.clone(),
            trace: Arc::clone(&trace),
        };
        let compose_of = conversation.clone();
        let compose = cx.new(|cx| Compose::new(compose_parts, compose_of, window, cx));
        let subscriptions = Self::subscriptions(&models, &compose, window, cx);
        let seen = models.chats.read(cx).changed_at(&conversation);
        let people_seen = models.chats.read(cx).revision();
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
            people: HashMap::new(),
            asks: 0,
            people_seen,
            rendered: HashMap::new(),
            activities: HashMap::new(),
            layouts: 0,
            parses: 0,
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
            settings,
            compose,
            reacting: None,
            deleting: None,
            resending: None,
            unsettled: Vec::new(),
            earlier: HashSet::new(),
            stored_posts: HashSet::new(),
            shown: Visibility::Shown,
            pending_rendered: HashMap::new(),
            read_upto: None,
            receipt: None,
            receipts: 0,
            active: Activation::of(window),
            outbox_traced: None,
            reveal: None,
            revealing: None,
            _subscriptions: subscriptions,
        };
        view.kick(cx);
        view.typing_changed(cx);
        view
    }

    /// The message list: bottom-aligned, following the newest; its scroll moves the pill, and back at the newest a
    /// read receipt may be due.
    fn following_list(cx: &mut Context<Self>) -> ListState {
        let list = ListState::new(0, ListAlignment::Bottom, px(OVERDRAW));
        list.set_follow_mode(FollowMode::Tail);
        let view: WeakEntity<Self> = cx.entity().downgrade();
        list.set_scroll_handler(move |_, _, cx| {
            let _ = view.update(cx, |view, cx| {
                view.want_receipt(cx);
                cx.notify();
            });
        });
        list
    }

    /// What the view follows: the models (its chat's changes, typing, names), its compose box's events and the
    /// window's activation.
    fn subscriptions(
        models: &Models,
        compose: &Entity<Compose>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> [Subscription; 3] {
        let observing = cx.observe(&models.chats, |this: &mut Self, chats, cx| {
            let at = chats.read(cx).changed_at(&this.conversation);
            if at > this.seen {
                this.seen = at;
                this.window_due = true;
                // a row just queued shows at once: the rows are built again from the models before the store is read
                // (the engine answers that read only once the send it is busy with is over)
                this.rebuild(cx);
                this.kick(cx);
            }
            this.typing_changed(cx);
            this.renamed(cx);
            cx.notify();
        });
        let compose_events = cx.subscribe_in(compose, window, |this: &mut Self, _, event, window, cx| match event {
            ComposeEvent::EditLast => this.edit_last(window, cx),
        });
        let activation = cx.observe_window_activation(window, |this: &mut Self, window, cx| {
            this.active = Activation::of(window);
            this.want_receipt(cx);
        });
        [observing, compose_events, activation]
    }

    /// The compose box under the messages.
    #[must_use]
    pub fn compose(&self) -> &Entity<Compose> {
        &self.compose
    }

    /// The message the quick reactions are open on.
    #[must_use]
    pub fn reacting(&self) -> Option<&str> {
        self.reacting.as_deref()
    }

    /// The message whose delete waits for its confirmation.
    #[must_use]
    pub fn deleting(&self) -> Option<&str> {
        self.deleting.as_deref()
    }

    /// The row whose "send again" waits for its confirmation.
    #[must_use]
    pub fn resending(&self) -> Option<i64> {
        self.resending
    }

    /// The message posts on their way, the oldest first.
    pub fn pending_items(&self) -> impl Iterator<Item = &PendingItem> {
        self.items.iter().filter_map(|item| match item {
            Item::Pending(pending) => Some(pending),
            _ => None,
        })
    }

    /// Read receipts sent.
    #[must_use]
    pub fn receipts(&self) -> u64 {
        self.receipts
    }

    /// Whether the chat is shown.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.shown == Visibility::Shown
    }

    /// The chat is shown, or hidden (the shell shows the settings over it): hidden, no read receipt goes, and one
    /// waiting is dropped; shown again, one may be due.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.is_shown() == shown {
            return;
        }
        self.shown = if shown { Visibility::Shown } else { Visibility::Hidden };
        if shown {
            self.want_receipt(cx);
        } else {
            self.receipt = None;
        }
        cx.notify();
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

    /// How many times a message's content was parsed (once per message id and version).
    #[must_use]
    pub fn parses(&self) -> u64 {
        self.parses
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

    /// Opens the view at a message (a search's hit, by its arrival time and id): the next window load reads the store
    /// back to it (once the running load ends), then the list scrolls to it, stops following the newest, and the
    /// message is selected. A message the store no longer has (deleted from it since) leaves the view as it is.
    pub fn reveal(&mut self, arrival_time: i64, message: String, cx: &mut Context<Self>) {
        self.reveal = Some((arrival_time, message));
        self.window_due = true;
        self.kick(cx);
    }

    /// Shows the message a window load read back to ([`ChatView::reveal`]).
    fn revealed(&mut self, (_, message): (i64, String), cx: &mut Context<Self>) {
        let Some(index) = self
            .items
            .iter()
            .position(|item| matches!(item, Item::Message(item) if item.id == message))
        else {
            return;
        };
        self.select(Some(message.clone()), cx);
        self.list.pause_following_tail();
        self.list.scroll_to_reveal_item(index);
        self.trace.record(
            &TraceLine::new("chat-view", "revealed")
                .conversation(&self.conversation)
                .messages([message.as_str()]),
        );
        cx.notify();
    }

    /// Scrolls to the newest message and follows the tail again.
    pub fn jump_to_newest(&mut self, cx: &mut Context<Self>) {
        self.list.scroll_to_end();
        self.list.set_follow_mode(FollowMode::Tail);
        self.want_receipt(cx);
        cx.notify();
    }

    /// Selects a message (the keyboard's actions apply to it).
    pub fn select(&mut self, message: Option<String>, cx: &mut Context<Self>) {
        if self.selected != message {
            // what waits on another message (its quick reactions, its delete's confirmation) closes: a key acts only on
            // the message selected
            if self.reacting.is_some() && self.reacting != message {
                self.reacting = None;
            }
            if self.deleting.is_some() && self.deleting != message {
                self.deleting = None;
            }
            self.selected = message;
            cx.notify();
        }
    }

    /// Chooses `action` on `message`, if the message offers it: told as [`ChatViewEvent::Action`], then carried out (a
    /// reply or an edit in the compose box, the quick reactions opened, the delete's confirmation asked).
    pub fn act(&mut self, action: MessageAction, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let offered = self
            .message_items()
            .find(|item| item.id == message)
            .is_some_and(|item| item.actions().contains(&action));
        if !offered {
            return;
        }
        cx.emit(ChatViewEvent::Action {
            action,
            message: message.to_owned(),
        });
        // another action closes what waited for an answer
        self.reacting = None;
        self.deleting = None;
        match action {
            MessageAction::Reply => self.reply_to(message, window, cx),
            MessageAction::Edit => self.edit(message, window, cx),
            MessageAction::React => {
                // the message the keys 1–4 act on is the one selected
                self.select(Some(message.to_owned()), cx);
                self.reacting = Some(message.to_owned());
                self.focus.focus(window, cx);
            }
            MessageAction::Delete => {
                // the message Enter deletes is the one selected
                self.select(Some(message.to_owned()), cx);
                self.deleting = Some(message.to_owned());
                self.focus.focus(window, cx);
            }
        }
        cx.notify();
    }

    /// A held message, by id.
    fn held_message(&self, id: &str) -> Option<&Message> {
        self.messages.iter().find(|message| message.id == id)
    }

    /// Starts a reply to `message` in the compose box.
    fn reply_to(&mut self, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(held) = self.held_message(message) else { return };
        let (name, _) = self.sender_of(held);
        let Some(quoted) = Quoted::of(held, &name) else { return };
        self.compose
            .update(cx, |compose, cx| compose.reply_to(quoted, window, cx));
    }

    /// Starts editing the account's `message` in the compose box: its text loaded back, its mentions by MRI.
    fn edit(&mut self, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(held) = self.held_message(message) else { return };
        let mentioned = rich_text::mentioned(held.properties.as_deref());
        let loaded = compose::load(&compose::rich_text_of(held), &mentioned);
        let id = held.id.clone();
        self.compose.update(cx, |compose, cx| match loaded {
            Ok(loaded) => compose.edit(id, loaded, window, cx),
            Err(why) => compose.show_problem(format!("This message can't be edited here: {why}"), cx),
        });
    }

    /// Up in an empty compose box: the account's newest message that offers an edit.
    fn edit_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let last = self
            .message_items()
            .filter(|item| item.actions().contains(&MessageAction::Edit))
            .last()
            .map(|item| item.id.clone());
        if let Some(id) = last {
            self.act(MessageAction::Edit, &id, window, cx);
        }
    }

    /// Sets the account's reaction `key` on `message`, or takes it back when it is set: only a key of
    /// [`QUICK_REACTIONS`], the keys the capture showed sent; any other is shown, never sent.
    pub fn toggle_reaction(&mut self, message: &str, key: &str, cx: &mut Context<Self>) {
        self.reacting = None;
        if !QUICK_REACTIONS.contains(&key) {
            return;
        }
        let Some(item) = self.message_items().find(|item| item.id == message) else {
            return;
        };
        if !matches!(item.body, Body::Text(_)) {
            return;
        }
        let mine = item
            .reactions
            .iter()
            .any(|reaction| reaction.key == key && reaction.mine);
        let (conversation, message_id, key) = (self.conversation.clone(), message.to_owned(), key.to_owned());
        let action = if mine {
            Action::Unreact {
                conversation,
                message_id,
                key,
            }
        } else {
            Action::React {
                conversation,
                message_id,
                key,
            }
        };
        // the row comes back as the engine's events; a write it refuses has no row to show
        drop(self.core.write(action));
        cx.notify();
    }

    /// Picks the `index`th quick reaction on the message they are open on.
    fn pick(&mut self, index: usize, cx: &mut Context<Self>) {
        if let (Some(message), Some(key)) = (self.reacting.clone(), QUICK_REACTIONS.get(index)) {
            self.toggle_reaction(&message, key, cx);
        }
    }

    /// Deletes the message whose delete waits for its confirmation.
    pub fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        let Some(message_id) = self.deleting.take() else { return };
        drop(self.core.write(Action::Delete {
            conversation: self.conversation.clone(),
            message_id,
        }));
        cx.notify();
    }

    /// "Send again" on row `row`: the engine's explicit resend, after a warning when its message may have arrived.
    pub fn send_again(&mut self, row: i64, cx: &mut Context<Self>) {
        let again = self.note_of_row(row).map(|note| note.again);
        match again {
            Some(Again::AfterWarning) if self.resending != Some(row) => self.resending = Some(row),
            Some(Again::AfterWarning | Again::Yes) => {
                self.resending = None;
                drop(self.core.resend(row));
            }
            Some(Again::No) | None => {}
        }
        cx.notify();
    }

    /// The note shown for row `row`.
    fn note_of_row(&self, row: i64) -> Option<&Note> {
        self.items.iter().find_map(|item| match item {
            Item::Pending(pending) if pending.note.row == row => Some(&pending.note),
            Item::Message(message) => message.outbox.iter().find(|note| note.row == row),
            _ => None,
        })
    }

    /// Escape in the list: what is open closes first (the quick reactions, a confirmation), then the selection.
    fn escape(&mut self, cx: &mut Context<Self>) {
        let open = self.reacting.take().is_some() | self.deleting.take().is_some() | self.resending.take().is_some();
        if open {
            cx.notify();
        } else {
            self.select(None, cx);
        }
    }

    /// Asks for the next older page (once the running load ends), even after loads that brought nothing, one that
    /// failed, or one that found the chat never synced.
    pub fn load_older(&mut self, cx: &mut Context<Self>) {
        self.fruitless = 0;
        if self.history != History::Complete && self.history != History::More {
            self.history = History::More;
            self.rebuild(cx);
        }
        self.want_older(cx);
    }

    /// Asks for the next older page while there may be one.
    fn want_older(&mut self, cx: &mut Context<Self>) {
        if self.history == History::More && !self.older_due {
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
        let held = self
            .messages
            .first()
            .map(|message| (message.arrival_time, message.id.clone()));
        let oldest = if load == Load::Window {
            // a message to open at, older than what is held, is read back to
            self.revealing = self.reveal.take();
            match (held, &self.revealing) {
                (Some(held), Some(target)) if held <= *target => Some(held),
                (_, Some(target)) => Some(target.clone()),
                (held, None) => held,
            }
        } else {
            held
        };
        let next = self.next.clone();
        let first = self.loads == 0;
        let find = if load == Load::Window {
            self.posts_to_find(cx)
        } else {
            Vec::new()
        };
        let held: HashMap<String, i64> = self
            .reactions
            .iter()
            .map(|(id, (version, _))| (id.clone(), *version))
            .collect();
        self.loading = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let read = match load {
                Load::Window => window(&*core, &conversation, oldest, first, &held, &find).await,
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
        if load == Load::Window
            && let Some(target) = self.revealing.take()
        {
            self.revealed(target, cx);
        }
        self.kick(cx);
        self.want_receipt(cx);
    }

    fn take(&mut self, load: Load, read: Read) {
        if let Horizon::Read(horizon) = read.horizon {
            self.horizon = horizon;
        }
        if let Some(outbox) = read.outbox {
            self.unsettled = outbox.unsettled;
            self.earlier = outbox.earlier;
            self.read_upto = self.read_upto.max(outbox.read_upto);
            self.stored_posts.extend(outbox.stored);
        }
        if let Some(history) = read.history {
            self.history = history;
        }
        if load == Load::Window && self.history == History::Unsynced {
            // the chat changed: a sync may have run since
            self.history = History::More;
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
                    if self.next.is_some() && self.history == History::Complete {
                        // the store has an older page: a catch-up landed after an older load found nothing
                        self.history = History::More;
                    }
                }
                self.messages = kept.into_iter().chain(messages).collect();
            }
            Load::Older => {
                self.next = loaded.next;
                self.fruitless = if messages.is_empty() { self.fruitless + 1 } else { 0 };
                if self.fruitless >= MAX_FRUITLESS && self.history == History::More {
                    self.history = History::Stalled;
                }
                messages.extend(std::mem::take(&mut self.messages));
                self.messages = messages;
            }
        }
        let held: HashSet<&str> = self.messages.iter().map(|message| message.id.as_str()).collect();
        self.reactions.retain(|id, _| held.contains(id.as_str()));
        self.rendered.retain(|id, _| held.contains(id.as_str()));
        self.activities.retain(|id, _| held.contains(id.as_str()));
    }

    /// Asks the engine for the people the rows name: the senders (their profile's name and their picture), events'
    /// members and who is typing.
    fn want_names(&mut self, cx: &mut Context<Self>) {
        let mut wanted: Vec<String> = Vec::new();
        let messages = std::mem::take(&mut self.messages);
        for message in &messages {
            if let Some(sender) = &message.sender {
                wanted.push(sender.clone());
            }
            if message.message_type == MessageType::AddMember.as_str()
                && let Some(activity) = self.activity(message)
            {
                wanted.extend(activity.initiator.iter().cloned());
                wanted.extend(activity.targets.iter().cloned());
            }
        }
        self.messages = messages;
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
        let ask = self.asks + 1;
        let mut asked = Vec::new();
        for mri in wanted {
            if mri != me && !self.people.contains_key(&mri) {
                self.people.insert(mri.clone(), (ask, None));
                asked.push(mri);
            }
        }
        if asked.is_empty() {
            return;
        }
        self.asks = ask;
        let core = Arc::clone(&self.core);
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let mut found = Vec::new();
            for mri in asked {
                if let Ok(person) = core.person(mri.clone()).await {
                    found.push((mri, Shown::of(person)));
                }
            }
            if found.is_empty() {
                return;
            }
            let _ = this.update(cx, |view, cx| view.answered(ask, found, cx));
        })
        .detach();
    }

    /// The engine answered ask `ask` for these people: the rows are built again when anyone changed.
    fn answered(&mut self, ask: u64, found: Vec<(String, Shown)>, cx: &mut Context<Self>) {
        let mut changed = false;
        for (mri, shown) in found {
            // someone the view dropped meanwhile isn't added back, and someone asked again since takes only the later
            // ask's answer, whichever comes first
            let Some((asked, entry)) = self.people.get_mut(&mri) else {
                continue;
            };
            if *asked == ask && entry.as_ref() != Some(&shown) {
                *entry = Some(shown);
                changed = true;
            }
        }
        if changed {
            self.rebuild(cx);
        }
    }

    /// Someone the view asked about may have a new name or picture: they are asked again.
    fn renamed(&mut self, cx: &mut Context<Self>) {
        let chats = self.models.chats.read(cx);
        let since = self.people_seen;
        let renamed: Vec<String> = self
            .people
            .keys()
            .filter(|mri| chats.named_at(mri) > since)
            .cloned()
            .collect();
        self.people_seen = chats.revision();
        for mri in &renamed {
            self.people.remove(mri);
        }
        self.ask_names(renamed, cx);
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

    /// A person's name: "You" for the account, else the engine's, else their MRI.
    fn name_of(&self, mri: &str) -> String {
        if mri == self.core.me() {
            return "You".to_owned();
        }
        self.shown(mri)
            .and_then(|shown| shown.name.as_ref())
            .map_or_else(|| mri.to_owned(), ToString::to_string)
    }

    /// A person as the engine last answered, if it has.
    fn shown(&self, mri: &str) -> Option<&Shown> {
        self.people.get(mri).and_then(|(_, shown)| shown.as_ref())
    }

    /// A message's sender's name and whether it is a name (not an MRI): their profile's, else the message's own,
    /// else the engine's other names, else their MRI.
    fn sender_of(&self, message: &Message) -> (String, bool) {
        let Some(sender) = message.sender.as_deref() else {
            return ("Unknown".to_owned(), false);
        };
        let shown = self.shown(sender);
        let carried = message
            .sender_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let name = match (
            shown
                .filter(|shown| shown.profiled())
                .and_then(|shown| shown.name.as_ref()),
            carried,
        ) {
            (Some(profiled), _) => Some(profiled.to_string()),
            (None, Some(carried)) => Some(carried.to_owned()),
            (None, None) => shown.and_then(|shown| shown.name.as_ref()).map(ToString::to_string),
        };
        match name {
            Some(name) => (name, true),
            None => (sender.to_owned(), false),
        }
    }

    /// A text message's rich text, parsed and laid out once per id and version: the cache is asked before anything
    /// is parsed.
    fn rendered(&mut self, message: &Message) -> Arc<Rendered> {
        if let Some((version, rendered)) = self.rendered.get(&message.id)
            && *version == message.version
        {
            return Arc::clone(rendered);
        }
        let text = self.parse(message);
        self.layouts += 1;
        let mentions = Mentions::of(message.properties.as_deref(), self.core.me());
        let rendered = Arc::new(rich_text::layout(&text, &mentions));
        self.rendered
            .insert(message.id.clone(), (message.version, Arc::clone(&rendered)));
        rendered
    }

    /// A text message's content as rich text (counted in [`Self::parses`]).
    fn parse(&mut self, message: &Message) -> RichText {
        self.parses += 1;
        if message.message_type == MessageType::RichTextHtml.as_str() {
            parse_html(&message.content)
        } else {
            RichText::plain(&message.content)
        }
    }

    /// A thread event's content, parsed once per id and version (counted in [`Self::parses`]).
    fn activity(&mut self, message: &Message) -> Option<Arc<ThreadActivity>> {
        if let Some((version, activity)) = self.activities.get(&message.id)
            && *version == message.version
        {
            return activity.clone();
        }
        self.parses += 1;
        let activity = thread_activity(&message.content).map(Arc::new);
        self.activities
            .insert(message.id.clone(), (message.version, activity.clone()));
        activity
    }

    /// What a message shows.
    fn body(&mut self, message: &Message) -> Body {
        if message.delete_time.is_some() {
            return Body::Deleted;
        }
        match MessageType::from(message.message_type.clone()) {
            MessageType::RichTextHtml | MessageType::Text => Body::Text(RenderedText(self.rendered(message))),
            MessageType::AddMember => {
                let activity = self.activity(message);
                Body::Event(self.event_text(activity.as_deref()).into())
            }
            other => Body::Unsupported(other.as_str().to_owned()),
        }
    }

    /// A thread event's line: who added whom.
    fn event_text(&self, activity: Option<&ThreadActivity>) -> String {
        let Some(activity) = activity else {
            return "Members changed".to_owned();
        };
        let targets: Vec<String> = activity.targets.iter().map(|mri| self.name_of(mri)).collect();
        match &activity.initiator {
            Some(initiator) => format!("{} added {}", self.name_of(initiator), targets.join(", ")),
            None => format!("Added {}", targets.join(", ")),
        }
    }

    /// A message's row, before it is arranged, with the note `notes` has for it; `None` for a control message (a
    /// signal, never shown).
    fn item(&mut self, message: &Message, notes: &HashMap<String, Vec<Note>>) -> Option<MessageItem> {
        if MessageType::from(message.message_type.clone()).is_control() {
            return None;
        }
        let me = self.core.me().to_owned();
        let body = self.body(message);
        let (sender_name, named) = self.sender_of(message);
        let picture = message
            .sender
            .as_deref()
            .and_then(|sender| self.shown(sender))
            .and_then(|shown| shown.picture.clone());
        let reactions = self
            .reactions
            .get(&message.id)
            .filter(|_| body != Body::Deleted)
            .map(|(_, reactions)| count_reactions(reactions, &me))
            .unwrap_or_default();
        Some(MessageItem {
            id: message.id.clone(),
            version: message.version,
            own: message.sender.as_deref() == Some(me.as_str()),
            sender: message.sender.clone(),
            face: Face { named, picture },
            sender_name: sender_name.into(),
            header: true,
            arrival_time: message.arrival_time,
            message_type: message.message_type.clone(),
            edited: message.edit_time.is_some() && !matches!(body, Body::Deleted),
            body,
            reactions,
            outbox: notes.get(&message.id).cloned().unwrap_or_default(),
        })
    }

    /// The conversation's outbox rows the view knows, by id: this session's as last told and the store's that aren't
    /// sent (a row told sent stays sent).
    fn known_rows(&self, cx: &App) -> BTreeMap<i64, OutboxEntry> {
        let mut rows: BTreeMap<i64, OutboxEntry> = self
            .models
            .chats
            .read(cx)
            .outbox_rows(&self.conversation)
            .map(|row| (row.id, row.clone()))
            .collect();
        for row in &self.unsettled {
            let told_sent = rows.get(&row.id).is_some_and(|told| told.state == OutboxState::Sent);
            if !told_sent {
                rows.insert(row.id, row.clone());
            }
        }
        rows
    }

    /// The notes shown on messages: per message id, the account's edits, deletes and reactions on it that aren't sent
    /// and that no newer write on the same thing made out of date, the oldest first.
    fn message_notes(&self, rows: &BTreeMap<i64, OutboxEntry>, now_ms: i64) -> HashMap<String, Vec<Note>> {
        let superseded = sending::superseded(rows.values());
        let mut notes: HashMap<String, Vec<Note>> = HashMap::new();
        let shown = rows.values().filter(|row| {
            sending::shown_on_message(row.kind) && row.state != OutboxState::Sent && !superseded.contains(&row.id)
        });
        for row in shown {
            let target = sending::command_of(row).and_then(|command| command.message_id().map(str::to_owned));
            if let Some(target) = target {
                notes
                    .entry(target)
                    .or_default()
                    .push(Note::of(row, self.earlier.contains(&row.id), now_ms));
            }
        }
        notes
    }

    /// This session's message posts told sent whose message the view doesn't hold and that weren't found in the store
    /// yet: their row ids and the message ids they were sent as, for the window load to look up.
    fn posts_to_find(&self, cx: &App) -> Vec<(i64, String)> {
        let held: HashSet<&str> = self
            .messages
            .iter()
            .filter_map(|message| message.client_message_id.as_deref())
            .collect();
        self.models
            .chats
            .read(cx)
            .outbox_rows(&self.conversation)
            .filter(|row| {
                row.kind == OutboxKind::Send
                    && row.state == OutboxState::Sent
                    && !self.stored_posts.contains(&row.id)
                    && !held.contains(row.client_message_id.as_str())
            })
            .filter_map(|row| Some((row.id, row.message_id.clone()?)))
            .collect()
    }

    /// The message posts on their way: the rows known ([`Self::known_rows`]), each until its message is held or was
    /// found in the store (a message older than the pages held).
    fn pending(&mut self, rows: &BTreeMap<i64, OutboxEntry>, now_ms: i64) -> Vec<PendingItem> {
        let held: HashSet<&str> = self
            .messages
            .iter()
            .filter_map(|message| message.client_message_id.as_deref())
            .collect();
        let shown: Vec<&OutboxEntry> = rows
            .values()
            .filter(|row| {
                row.kind == OutboxKind::Send
                    && !held.contains(row.client_message_id.as_str())
                    && !self.stored_posts.contains(&row.id)
            })
            .collect();
        self.pending_rendered
            .retain(|id, _| shown.iter().any(|row| row.id == *id));
        shown
            .into_iter()
            .map(|row| PendingItem {
                note: Note::of(row, self.earlier.contains(&row.id), now_ms),
                created_at: row.created_at,
                body: self.pending_body(row),
            })
            .collect()
    }

    /// A message post's text as its row carries it, laid out once per row.
    fn pending_body(&mut self, row: &OutboxEntry) -> Option<RenderedText> {
        if let Some(rendered) = self.pending_rendered.get(&row.id) {
            return Some(RenderedText(Arc::clone(rendered)));
        }
        let Some(Command::Send { text, .. }) = sending::command_of(row) else {
            return None;
        };
        let rendered = Arc::new(rich_text::layout(text.rich_text(), &Mentions::default()));
        self.pending_rendered.insert(row.id, Arc::clone(&rendered));
        Some(RenderedText(rendered))
    }

    /// Builds the rows again and tells the list which changed.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let now_ms = Utc::now().timestamp_millis();
        let known = self.known_rows(cx);
        let notes = self.message_notes(&known, now_ms);
        let messages = std::mem::take(&mut self.messages);
        let rows: Vec<MessageItem> = messages
            .iter()
            .filter_map(|message| self.item(message, &notes))
            .collect();
        self.messages = messages;
        let mut items = arrange(rows, self.history, self.horizon, &Local);
        items.extend(self.pending(&known, now_ms).into_iter().map(Item::Pending));
        let anchor = Anchor::of(&self.list, &self.items);
        splice(&self.list, &self.items, &items);
        if let Some(anchor) = anchor {
            anchor.restore(&self.list, &items);
        }
        self.items = items;
        if let Some(selected) = &self.selected
            && !self.message_items().any(|item| &item.id == selected)
        {
            self.selected = None;
        }
        self.close_gone();
        self.trace_items();
        self.trace_outbox();
        cx.notify();
    }

    /// Closes what waits on a message that no longer offers it (gone from the rows, or deleted meanwhile): its quick
    /// reactions, its delete's confirmation.
    fn close_gone(&mut self) {
        let offers = |view: &Self, id: &str, action: MessageAction| {
            view.message_items()
                .any(|item| item.id == id && item.actions().contains(&action))
        };
        if let Some(id) = self.reacting.clone()
            && !offers(self, &id, MessageAction::React)
        {
            self.reacting = None;
        }
        if let Some(id) = self.deleting.clone()
            && !offers(self, &id, MessageAction::Delete)
        {
            self.deleting = None;
        }
    }

    /// Traces the outbox rows shown and their states, when they changed (`outbox shown`: the rows' ids as
    /// `messages`, their states as `kinds`).
    fn trace_outbox(&mut self) {
        let shown: Vec<(String, &'static str)> = self
            .items
            .iter()
            .flat_map(|item| match item {
                Item::Pending(pending) => std::slice::from_ref(&pending.note),
                Item::Message(message) => message.outbox.as_slice(),
                _ => &[],
            })
            .map(|note| (note.row.to_string(), note.state.name()))
            .collect();
        let unchanged = match &self.outbox_traced {
            Some(traced) => *traced == shown,
            None => shown.is_empty(),
        };
        if unchanged {
            return;
        }
        let line = TraceLine::new("outbox", "shown")
            .conversation(&self.conversation)
            .messages(shown.iter().map(|(id, _)| id.as_str()));
        let line = shown.iter().fold(line, |line, (_, kind)| line.kind(kind));
        self.trace.record(&line);
        self.outbox_traced = Some(shown);
    }

    /// The newest message a read receipt would move the account's horizon to now, with its arrival time, if one is
    /// due: the setting on, the chat shown, the window active, the list at the newest message, and someone else's
    /// message after the horizon that the store counts unread (and that carries the `clientmessageid` a receipt names).
    fn receipt_target(&self, cx: &App) -> Option<(String, i64)> {
        let wanted = self.shown == Visibility::Shown
            && self.active == Activation::Active
            && self.list.is_following_tail()
            && self.settings.read(cx).settings().send_read_receipts;
        if !wanted {
            return None;
        }
        let me = self.core.me();
        let newest = self.messages.iter().rev().find(|message| {
            message.sender.as_deref().is_some_and(|sender| sender != me)
                && counts_unread(&message.message_type)
                && message.delete_time.is_none()
                && message.client_message_id.is_some()
        })?;
        self.read_upto
            .is_none_or(|upto| newest.arrival_time > upto)
            .then(|| (newest.id.clone(), newest.arrival_time))
    }

    /// A read receipt goes out [`READ_DEBOUNCE`] from now when one is due, unless one waits already.
    fn want_receipt(&mut self, cx: &mut Context<Self>) {
        if self.receipt.is_some() || self.receipt_target(cx).is_none() {
            return;
        }
        self.receipt = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(READ_DEBOUNCE).await;
            let _ = this.update(cx, ChatView::send_receipt);
        }));
    }

    /// Sends the read receipt that is due now, if one still is.
    fn send_receipt(&mut self, cx: &mut Context<Self>) {
        self.receipt = None;
        let Some((message_id, arrival)) = self.receipt_target(cx) else {
            return;
        };
        self.read_upto = Some(arrival);
        self.receipts += 1;
        // the row comes back as the engine's events
        drop(self.core.write(Action::Read {
            conversation: self.conversation.clone(),
            message_id,
        }));
        self.want_receipt(cx);
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
            // a selection moved off the newest stops following the tail, or the next frame would scroll it away
            self.list.pause_following_tail();
            self.list.scroll_to_reveal_item(index);
        }
    }

    fn act_on_selected(&mut self, action: MessageAction, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(selected) = self.selected.clone() {
            self.act(action, &selected, window, cx);
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
            Some(Item::Pending(pending)) => pending_row(pending, palette, cx),
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
            let picker = (self.reacting.as_deref() == Some(item.id.as_str())).then(|| reaction_picker(item, cx));
            row.children(item.header.then(|| header_line(item, palette)))
                .child(bubble(item, highlighted, palette))
                .children(footer_line(item, palette, cx))
                .children(
                    item.outbox
                        .iter()
                        .map(|note| note_line(note, palette, cx))
                        .collect::<Vec<_>>(),
                )
                .children(picker)
        };
        row.children(actions).into_any_element()
    }

    /// What waits for the user's answer under the messages: the delete's confirmation, or the warning before "send
    /// again" on a row whose message may have arrived.
    fn confirmation(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Ask {
            question,
            yes,
            label,
            on_yes,
        } = if self.deleting.is_some() {
            Ask {
                question: CONFIRM_DELETE,
                yes: "confirm-delete",
                label: "Delete",
                on_yes: Box::new(ChatView::confirm_delete),
            }
        } else {
            let row = self.resending?;
            let post = self.note_of_row(row).is_none_or(|note| note.kind == OutboxKind::Send);
            Ask {
                question: if post { MAY_HAVE_ARRIVED } else { MAY_BE_OUT_OF_DATE },
                yes: "resend-anyway",
                label: "Send again",
                on_yes: Box::new(move |this, cx| this.send_again(row, cx)),
            }
        };
        let confirm = Button::new(yes)
            .label(label)
            .primary()
            .with_size(Size::XSmall)
            .on_click(cx.listener(move |this, _, _, cx| on_yes(this, cx)));
        let cancel = Button::new("confirm-cancel")
            .label("Cancel")
            .ghost()
            .with_size(Size::XSmall)
            .on_click(cx.listener(|this, _, _, cx| this.escape(cx)));
        Some(
            div()
                .debug_selector(|| "confirmation".to_owned())
                .flex()
                .items_center()
                .gap_2()
                .px_4()
                .py_1()
                .text_sm()
                .border_t_1()
                .border_color(rgb(palette.border))
                .child(question)
                .child(div().debug_selector(move || yes.to_owned()).child(confirm))
                .child(div().debug_selector(|| "confirm-cancel".to_owned()).child(cancel))
                .into_any_element(),
        )
    }
}

/// What a confirming button does.
type OnYes = Box<dyn Fn(&mut ChatView, &mut Context<ChatView>)>;

/// A question the view asks before it writes: its text, its confirming button's id and label, and what that does.
struct Ask {
    question: &'static str,
    yes: &'static str,
    label: &'static str,
    on_yes: OnYes,
}

/// Whether the chat is shown in its window: read receipts go out only while it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Visibility {
    Shown,
    /// Behind the settings.
    Hidden,
}

/// Whether the window is the one the user is using: read receipts go out only while it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Activation {
    Active,
    Inactive,
}

impl Activation {
    fn of(window: &Window) -> Self {
        if window.is_window_active() {
            Self::Active
        } else {
            Self::Inactive
        }
    }
}

/// The quick reactions over a message: each sets the account's own, or takes it back when it is set.
fn reaction_picker(item: &MessageItem, cx: &mut Context<ChatView>) -> AnyElement {
    let buttons = QUICK_REACTIONS.iter().enumerate().map(|(index, key)| {
        let message = item.id.clone();
        let mine = item
            .reactions
            .iter()
            .any(|reaction| reaction.key == *key && reaction.mine);
        let button = Button::new(ElementId::Name(format!("pick-{key}-{message}").into()))
            .label(format!("{} {}", index + 1, reaction_label(key)))
            .with_size(Size::XSmall)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_reaction(&message, key, cx)));
        let button = if mine { button.primary() } else { button.ghost() };
        div().debug_selector(move || format!("pick-{key}")).child(button)
    });
    div()
        .debug_selector(|| "reaction-picker".to_owned())
        .flex()
        .gap_1()
        .pt_1()
        .children(buttons)
        .into_any_element()
}

/// An outbox row's state under the message it concerns, with "send again" when it is offered.
fn note_line(note: &Note, palette: Palette, cx: &mut Context<ChatView>) -> AnyElement {
    let row = note.row;
    let again = (note.again != Again::No).then(|| {
        div().debug_selector(move || format!("send-again-{row}")).child(
            Button::new(ElementId::Name(format!("send-again-{row}").into()))
                .label("Send again")
                .ghost()
                .with_size(Size::XSmall)
                .on_click(cx.listener(move |this, _, _, cx| this.send_again(row, cx))),
        )
    });
    let colour = match note.state {
        RowState::Denied | RowState::Failed => palette.foreground,
        _ => palette.muted_foreground,
    };
    div()
        .debug_selector(move || format!("outbox-{row}"))
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .text_color(rgb(colour))
        .child(note.text())
        .children(again)
        .into_any_element()
}

/// A message post on its way: the account's bubble, muted, with the row's state.
fn pending_row(pending: &PendingItem, palette: Palette, cx: &mut Context<ChatView>) -> AnyElement {
    let row = pending.note.row;
    let id: SharedString = format!("pending-text-{row}").into();
    let content = match &pending.body {
        Some(text) => rich_text::render(&id, &text.0, palette),
        None => muted(UNSUPPORTED, palette),
    };
    div()
        .debug_selector(move || format!("pending-{row}"))
        .w_full()
        .px_4()
        .pt(px(2.0))
        .flex()
        .flex_col()
        .items_end()
        .child(
            div()
                .max_w(relative(0.8))
                .px_3()
                .py_1()
                .rounded_md()
                .opacity(0.7)
                .bg(rgb(palette.selected))
                .child(content),
        )
        .child(note_line(&pending.note, palette, cx))
        .into_any_element()
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
                .on_click(cx.listener(move |this, _, window, cx| this.act(action, &message, window, cx))),
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
    /// The account's writes and read horizon as the store has them now: read by every window load.
    outbox: Option<OutboxRead>,
}

/// The account's writes in the conversation and its read horizon, as a window load read them.
struct OutboxRead {
    unsettled: Vec<OutboxEntry>,
    earlier: HashSet<i64>,
    /// The arrival time of the last message the account has read.
    read_upto: Option<i64>,
    /// The message posts looked up whose message the store has, by row id.
    stored: Vec<i64>,
}

/// The conversation's rows that aren't sent, the rows earlier sessions left, and the account's read horizon now; what
/// can't be read is none.
async fn outbox_read(core: &dyn Core, conversation: &str) -> OutboxRead {
    let unsettled = core.outbox_unsettled(conversation.to_owned()).await.unwrap_or_default();
    let earlier = core
        .left_by_earlier_runs()
        .await
        .map(|rows| rows.into_iter().map(|row| row.id).collect())
        .unwrap_or_default();
    let read_upto = core
        .read_horizon(conversation.to_owned())
        .await
        .ok()
        .flatten()
        .map(|horizon| horizon.last_read_arrival);
    OutboxRead {
        unsettled,
        earlier,
        read_upto,
        stored: Vec::new(),
    }
}

/// The posts of `find` (row ids, the message ids they were sent as) whose message the store has.
async fn stored_posts(core: &dyn Core, conversation: &str, find: &[(i64, String)]) -> Vec<i64> {
    let mut stored = Vec::new();
    for (row, message_id) in find {
        let found = core.message(conversation.to_owned(), message_id.clone()).await;
        if matches!(found, Ok(Some(_))) {
            stored.push(*row);
        }
    }
    stored
}

/// The window load: the newest messages down to `oldest`, the account's writes and read horizon now, which of the
/// posts of `find` the store has, and on the first load that horizon for the "New messages" divider.
async fn window(
    core: &dyn Core,
    conversation: &str,
    oldest: Option<(i64, String)>,
    first: bool,
    held: &HashMap<String, i64>,
    find: &[(i64, String)],
) -> Result<Read, EngineError> {
    let mut outbox = outbox_read(core, conversation).await;
    outbox.stored = stored_posts(core, conversation, find).await;
    let horizon = if first {
        Horizon::Read(outbox.read_upto)
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
        outbox: Some(outbox),
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
            // nothing to read: the beginning only once the store says the whole history is stored
            let history = match core.history_complete(conversation.to_owned()).await? {
                Some(true) => History::Complete,
                Some(false) | None => History::Unsynced,
            };
            return Ok(Read {
                loaded: None,
                horizon: Horizon::NotRead,
                history: Some(history),
                outbox: None,
            });
        }
        let Some((arrival_time, id)) = oldest else {
            // nothing was held: the newest page now
            return window(core, conversation, None, false, held, &[]).await;
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
        outbox: None,
    })
}

/// A row's identity in the list, for telling which rows changed.
type Key = (u8, String);

fn key(item: &Item) -> Key {
    match item {
        Item::Top(_) => (0, String::new()),
        Item::Day(day) => (1, day.to_string()),
        Item::NewMessages => (2, String::new()),
        Item::Message(message) => (3, message.id.clone()),
        Item::Pending(pending) => (4, pending.note.row.to_string()),
    }
}

/// Tells `list` how `old` became `new`, row by row: rows only in `old` are removed and rows only in `new` added where
/// they stand, and a row in both that changed is measured again. Every other row keeps its place and its measured
/// height. The rows in both keep their order (the messages' order); should they not, the rest is replaced.
fn splice(list: &ListState, old: &[Item], new: &[Item]) {
    let old_keys: Vec<Key> = old.iter().map(key).collect();
    let new_keys: Vec<Key> = new.iter().map(key).collect();
    let in_old: HashSet<&Key> = old_keys.iter().collect();
    let in_new: HashSet<&Key> = new_keys.iter().collect();
    // `at_new` is where `old[at_old]` stands in the list: the rows above it are told already
    let (mut at_old, mut at_new) = (0, 0);
    while at_old < old.len() || at_new < new.len() {
        // a run of rows gone and a run of rows come, replaced together
        let gone = old_keys[at_old..]
            .iter()
            .take_while(|key| !in_new.contains(key))
            .count();
        let came = new_keys[at_new..]
            .iter()
            .take_while(|key| !in_old.contains(key))
            .count();
        if gone + came > 0 {
            list.splice(at_new..at_new + gone, came);
            at_old += gone;
            at_new += came;
            continue;
        }
        if at_old == old.len() || at_new == new.len() || old_keys[at_old] != new_keys[at_new] {
            list.splice(at_new..at_new + (old.len() - at_old), new.len() - at_new);
            return;
        }
        // a run of the same rows: those that changed are measured again
        let run = old_keys[at_old..]
            .iter()
            .zip(&new_keys[at_new..])
            .take_while(|(old, new)| old == new)
            .count();
        remeasure_changed(list, &old[at_old..at_old + run], &new[at_new..at_new + run], at_new);
        at_old += run;
        at_new += run;
    }
}

/// Measures again the rows of `old` that changed in `new` (the same rows, standing in the list from `at`), a run of
/// them at a time.
fn remeasure_changed(list: &ListState, old: &[Item], new: &[Item], at: usize) {
    let mut changed: Option<usize> = None;
    for (offset, (old, new)) in old.iter().zip(new).enumerate() {
        match (old != new, changed) {
            (true, None) => changed = Some(offset),
            (false, Some(from)) => {
                list.remeasure_items(at + from..at + offset);
                changed = None;
            }
            _ => {}
        }
    }
    if let Some(from) = changed {
        list.remeasure_items(at + from..at + old.len());
    }
}

/// Where the view is read from, kept across a change of the rows by the rows' identity, never their index: the first
/// message row at or below the list's top, and the rows above it in the view (the top, a day, the divider).
struct Anchor {
    message: Key,
    /// The rows from the list's top row down to the message row (none when the message row is the top row).
    lead: Vec<Key>,
    /// How far into the list's top row the view starts.
    offset: Pixels,
}

impl Anchor {
    /// Where `list`, showing `items`, is; `None` while it follows the tail (a new row scrolls in) or shows no message.
    fn of(list: &ListState, items: &[Item]) -> Option<Self> {
        if list.is_following_tail() {
            return None;
        }
        let top = list.logical_scroll_top();
        let (at, message) = items
            .iter()
            .enumerate()
            .skip(top.item_ix)
            .find(|(_, item)| matches!(item, Item::Message(_)))?;
        Some(Self {
            message: key(message),
            lead: items[top.item_ix..at].iter().map(key).collect(),
            offset: top.offset_in_item,
        })
    }

    /// Scrolls `list`, now showing `items`, back to the anchor: the same rows at the top, from the same offset. When
    /// the rows above the message row changed (an older page came in below the top row), the message row is put at
    /// the top, so what was read stays in view and the new rows are above it.
    fn restore(self, list: &ListState, items: &[Item]) {
        let Some(at) = items.iter().position(|item| key(item) == self.message) else {
            return;
        };
        let lead = at
            .checked_sub(self.lead.len())
            .filter(|&from| items[from..at].iter().map(key).eq(self.lead.iter().cloned()));
        list.scroll_to(match lead {
            Some(from) => ListOffset {
                item_ix: from,
                offset_in_item: self.offset,
            },
            None => ListOffset {
                item_ix: at,
                offset_in_item: px(0.0),
            },
        });
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

/// The top row: how far the history is loaded; when no load will run by itself, a button to load again.
fn top_row(history: History, palette: Palette, cx: &mut Context<ChatView>) -> AnyElement {
    let again = history.again().map(|label| {
        div().debug_selector(|| "load-older".to_owned()).child(
            Button::new("load-older-again")
                .label(label)
                .ghost()
                .with_size(Size::XSmall)
                .on_click(cx.listener(|this, _, _, cx| this.load_older(cx))),
        )
    });
    centered_line(history.label(), palette)
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
    let avatar = (!item.own).then(|| {
        div()
            .debug_selector({
                let id = item.id.clone();
                move || format!("avatar-{id}")
            })
            .child(item.face.avatar(&item.sender_name, Size::XSmall))
    });
    div()
        .flex()
        .gap_2()
        .items_center()
        .text_sm()
        .children(avatar)
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

/// "Edited" and the reactions, if any; a reaction clicked sets the account's own, or takes it back.
fn footer_line(item: &MessageItem, palette: Palette, cx: &mut Context<ChatView>) -> Option<AnyElement> {
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
        let (message, toggled) = (item.id.clone(), reaction.key.clone());
        let chip = div()
            .id(ElementId::Name(format!("reaction-{}-{key}", item.id).into()))
            .debug_selector(move || format!("reaction-{key}"))
            .px_1()
            .rounded_md()
            .border_1()
            .text_xs()
            .child(format!("{} {}", reaction_label(&reaction.key), reaction.count));
        // a chip of a quick reaction sets or takes back the account's own; any other key only shows
        let chip = if QUICK_REACTIONS.contains(&reaction.key.as_str()) {
            chip.on_click(cx.listener(move |this, _, _, cx| this.toggle_reaction(&message, &toggled, cx)))
        } else {
            chip
        };
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
            .on_action(cx.listener(|this, _: &ReplyToSelected, window, cx| {
                this.act_on_selected(MessageAction::Reply, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ReactToSelected, window, cx| {
                this.act_on_selected(MessageAction::React, window, cx);
            }))
            .on_action(cx.listener(|this, _: &EditSelected, window, cx| {
                this.act_on_selected(MessageAction::Edit, window, cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteSelected, window, cx| {
                this.act_on_selected(MessageAction::Delete, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| this.escape(cx)))
            .on_action(cx.listener(|this, _: &ConfirmDelete, _, cx| this.confirm_delete(cx)))
            .on_action(cx.listener(|this, _: &PickLike, _, cx| this.pick(0, cx)))
            .on_action(cx.listener(|this, _: &PickHeart, _, cx| this.pick(1, cx)))
            .on_action(cx.listener(|this, _: &PickLaugh, _, cx| this.pick(2, cx)))
            .on_action(cx.listener(|this, _: &PickSurprised, _, cx| this.pick(3, cx)))
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
            .children(self.confirmation(palette, cx))
            .child(self.compose.clone())
    }
}
