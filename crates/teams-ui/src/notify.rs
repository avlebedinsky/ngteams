//! Desktop notifications for messages that come (M2 Task 14).
//!
//! **Which messages notify.** The [`Notifier`] listens to the models' [`Feed`](crate::models::Feed): a message newly
//! stored, from someone else, a text message not deleted, in a chat (not one of the account's own streams). Then, as
//! the "notifications" setting says ([`Notifications`]: all, mentions only, off), and only when the chat isn't the
//! one shown in a focused window:
//! - one delivered over the push channel notifies; one the store already had (not newly stored), older history
//!   ([`Source::History`]) and an edit, a delete or a reaction never do;
//! - one a catch-up brought ([`Source::CatchUp`]), or a replayed delivery ([`Source::Replay`]: the service pushing
//!   again on a reconnect what it pushed before, newly stored when the earlier push never reached the app), notifies
//!   only if it is unread (after the account's read horizon) and arrived after the cutoff ([`Notifier::cutoff`]): the
//!   app's start until the push channel first goes down, then the moment it was last known to deliver before it last
//!   went down (when its socket last heard from the service, as the engine tells it: a socket gone silent in a suspend
//!   is noticed only later), less [`CLOCK_SKEW_MS`], since the service's times are compared with this machine's;
//! - in a team's channel only a mention of the account notifies (channels are the sidebar's teams section's, read-only in M2).
//!
//! A mention of the account is marked as such ("… mentioned you"). Messages are gathered for [`GATHER`] before they
//! are shown, so a catch-up's are seen together: up to [`SINGLES`] of one chat are shown one by one; more than that
//! collapse into one notification of the chat ("N new messages"), which the ones shown before give way to (closed)
//! and later ones replace; it says "one or more mentioning you" while any message it stands for (one shown before it
//! included) mentioned the account. A chat's notifications are closed once it is read (its read horizon moves, or a
//! read of the service changes the chat, and nothing in it is unread), opened from one, or shown in a focused window
//! when the next message comes. Whether a chat is shown in a focused window is asked again just before its
//! notifications are shown, and a chat opened or read while its messages were being looked at shows none of them.
//!
//! **Activating one** (its body, the `default` action) opens its chat: [`NotifierEvent::Open`], or for a team's
//! channel (the stored conversation's kind when it was shown) [`NotifierEvent::OpenChannel`], read-only, which the
//! [`Desktop`](crate::desktop::Desktop) acts on.
//!
//! **The path.** GPUI's own system notifications reach the bus, but can't replace or close a notification (their tag
//! replaces nothing on Linux), and each opens a connection of its own with `zbus`'s session-bus constructor, which
//! falls back to `$XDG_RUNTIME_DIR/bus` without an address (`tests/gpui_notification.rs`). So the notifications go
//! through a
//! [`NotificationServer`]: in the app [`BusNotifications`], `org.freedesktop.Notifications` over `zbus` on the
//! connection to the bus the app is given ([`crate::bus`]); in a view test a fake. What a notification says (a name,
//! the message's first line) goes only to that bus, never to the view trace, which records the conversation id and
//! the kind.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt as _;
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, SharedString, Subscription, WeakEntity};
use teams_core::{CLOCK_SKEW_MS, Source};
use teams_store::{Conversation, ConversationKind, Message};
use zbus::Connection;
use zbus::proxy::CacheProperties;
use zbus::zvariant::Value;

use crate::bridge::Core;
use crate::chat_list::{self, LIST_LIMIT};
use crate::models::{FeedEvent, Models};
use crate::people::Shown;
use crate::rich_text::Mentions;
use crate::settings::{Notifications, SettingsModel};
use crate::system_message;
use crate::trace::{TraceLine, ViewTrace};

/// How long messages are gathered before they are shown.
pub const GATHER: Duration = Duration::from_millis(250);
/// The most notifications of one chat shown one by one; more collapse into one.
pub const SINGLES: usize = 3;
/// The notification server's bus name and object path, and its interface.
pub const SERVER_NAME: &str = "org.freedesktop.Notifications";
pub const SERVER_PATH: &str = "/org/freedesktop/Notifications";
/// The action of a notification's body.
pub const DEFAULT_ACTION: &str = "default";
/// An incoming call's notification's actions (M3 Task 17): answer it, decline it.
pub const ANSWER_ACTION: &str = "answer";
pub const DECLINE_ACTION: &str = "decline";
/// What a sender without a name is called.
pub const SOMEONE: &str = "Someone";

/// What a notification is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    /// One message.
    Message,
    /// One message that mentions the account.
    Mention,
    /// Several messages of one chat, in one.
    Collapsed,
    /// An incoming call, with Answer and Decline ([`crate::call_alert`]).
    Call,
}

impl NoticeKind {
    /// The trace's name for it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Mention => "mention",
            Self::Collapsed => "collapsed",
            Self::Call => "call",
        }
    }
}

/// One desktop notification, as the notifier asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub conversation: String,
    pub kind: NoticeKind,
    pub summary: String,
    pub body: String,
}

/// The desktop's notification server, as the notifier sees it.
pub trait NotificationServer: Send + Sync + 'static {
    /// Shows `notice`, in place of notification `replaces` when that isn't 0; answers the server's id for it.
    fn show(&self, notice: &Notice, replaces: u32) -> BoxFuture<'static, Result<u32, String>>;
    /// Closes notification `id`; one already gone is no matter.
    fn close(&self, id: u32) -> BoxFuture<'static, ()>;
}

/// A notification server that names the account a notification is for (an app of several accounts): its summary
/// followed by the account's name while a label is set ([`AccountLabel::set`]), the notice as it is otherwise.
pub struct LabelledServer {
    inner: Arc<dyn NotificationServer>,
    label: AccountLabel,
}

impl LabelledServer {
    #[must_use]
    pub fn new(inner: Arc<dyn NotificationServer>, label: AccountLabel) -> Self {
        Self { inner, label }
    }
}

impl NotificationServer for LabelledServer {
    fn show(&self, notice: &Notice, replaces: u32) -> BoxFuture<'static, Result<u32, String>> {
        match self.label.get() {
            Some(label) => {
                let notice = Notice {
                    summary: format!("{} · {label}", notice.summary),
                    ..notice.clone()
                };
                self.inner.show(&notice, replaces)
            }
            None => self.inner.show(notice, replaces),
        }
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        self.inner.close(id)
    }
}

/// The account's name a [`LabelledServer`] adds to its notifications; none while the app runs one account.
#[derive(Clone, Debug, Default)]
pub struct AccountLabel(Arc<std::sync::Mutex<Option<String>>>);

impl AccountLabel {
    /// From now on the notifications name `label`; `None` names none.
    pub fn set(&self, label: Option<String>) {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = label;
    }

    #[must_use]
    pub fn get(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

/// What the notifier tells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotifierEvent {
    /// A notification was activated: open its chat.
    Open(String),
    /// A notification of a team's channel was activated: open the channel, read-only (never in a chat view).
    OpenChannel(String),
}

/// The chat shown in a focused window, if one is: no notification is shown for it.
pub type Viewing = Box<dyn Fn(&mut App) -> Option<String>>;

/// A message that may notify, as it came.
#[derive(Clone, Debug)]
struct Candidate {
    conversation: String,
    message: Message,
    source: Source,
}

/// What is shown for one chat.
#[derive(Debug, Default)]
struct ChatNotices {
    /// The notifications shown one by one, by the server's id.
    singles: Vec<u32>,
    /// The one the chat's messages collapsed into.
    collapsed: Option<u32>,
    /// The messages notified since the chat was last read.
    count: usize,
    /// One of them mentioned the account.
    mentioned: bool,
}

impl ChatNotices {
    fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.singles.iter().copied().chain(self.collapsed)
    }
}

/// What one gathering hands its processing: the messages and what decides about them, as they stood when it began.
struct Batch {
    candidates: Vec<Candidate>,
    level: Notifications,
    viewing: Option<String>,
    cutoff: i64,
    /// The notifier's count of forgotten chats when the batch was taken: a chat forgotten after it shows nothing of it.
    taken: u64,
    core: Arc<dyn Core>,
    server: Arc<dyn NotificationServer>,
}

/// The notifications (see the module docs). A GPUI entity.
pub struct Notifier {
    core: Arc<dyn Core>,
    settings: Entity<SettingsModel>,
    trace: Arc<ViewTrace>,
    server: Option<Arc<dyn NotificationServer>>,
    viewing: Viewing,
    /// A catch-up's message notifies only when it arrived after this, in milliseconds ([`Notifier::cutoff`]).
    cutoff: i64,
    /// Messages gathered, not yet processed.
    pending: Vec<Candidate>,
    /// A gathering is under way (it processes what is pending until nothing is).
    gathering: bool,
    shown: HashMap<String, ChatNotices>,
    /// Every notification shown, by the server's id: its chat.
    ids: HashMap<u32, String>,
    /// The conversations shown that were stored as a team's channel.
    channels: HashSet<String>,
    /// How many times a chat's notifications were forgotten (closed, its count started again), and per chat that
    /// count when it last was.
    forgets: u64,
    forgotten: HashMap<String, u64>,
    _feed: Subscription,
}

impl EventEmitter<NotifierEvent> for Notifier {}

/// Milliseconds since the epoch of `time` (0 before it).
fn millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

/// Whether `message` (newly stored in `conversation`, brought by `source`) may notify `me` at all: someone else's
/// message of a type Teams notifies for ([`system_message::notifies`]: a text message or a card, never a call's, a
/// recording's, a transcript's or a thread's event), never one the thread itself sent. The rest is decided when it is
/// processed.
#[must_use]
pub fn may_notify(me: &str, conversation: &str, message: &Message, source: Source) -> bool {
    matches!(source, Source::Delivery | Source::Replay | Source::CatchUp)
        && message
            .sender
            .as_deref()
            .is_some_and(|sender| sender != me && !system_message::is_thread(sender))
        && system_message::notifies(&message.message_type)
        && message.delete_time.is_none()
        && !conversation.starts_with("48:")
}

impl Notifier {
    /// The notifier over the models; it shows nothing until it is given a server ([`Notifier::set_server`]).
    pub fn new(
        core: Arc<dyn Core>,
        models: &Models,
        settings: Entity<SettingsModel>,
        trace: Arc<ViewTrace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let feed = cx.subscribe(&models.feed, |this: &mut Self, _, event, cx| match event {
            FeedEvent::Arrived {
                conversation,
                message,
                source,
            } => this.arrived(conversation, message, *source, cx),
            FeedEvent::Read { conversation } | FeedEvent::Changed { conversation } => this.read(conversation, cx),
            FeedEvent::Down { heard } => this.went_down(*heard),
            // the account's own presence's, nothing to notify
            FeedEvent::Registered | FeedEvent::PushStopped | FeedEvent::OwnReport { .. } => {}
        });
        Self {
            core,
            settings,
            trace,
            server: None,
            viewing: Box::new(|_| None),
            cutoff: millis(SystemTime::now()),
            pending: Vec::new(),
            gathering: false,
            shown: HashMap::new(),
            ids: HashMap::new(),
            channels: HashSet::new(),
            forgets: 0,
            forgotten: HashMap::new(),
            _feed: feed,
        }
    }

    /// From now on, notifications go to `server`.
    pub fn set_server(&mut self, server: Arc<dyn NotificationServer>) {
        self.server = Some(server);
    }

    /// Whether a server was given.
    #[must_use]
    pub fn has_server(&self) -> bool {
        self.server.is_some()
    }

    /// How to tell the chat shown in a focused window.
    pub fn set_viewing(&mut self, viewing: Viewing) {
        self.viewing = viewing;
    }

    /// A catch-up's message (or a replayed delivery newly stored) notifies only when it arrived after this, in
    /// milliseconds since the epoch: the app's start until the push channel first goes down, then the moment it was
    /// last known to deliver before its last drop less [`CLOCK_SKEW_MS`] (see the module docs).
    #[must_use]
    pub fn cutoff(&self) -> i64 {
        self.cutoff
    }

    /// The push channel went down, last heard at `heard`: the cutoff moves there. One that never connected moves
    /// nothing (what it would have delivered is still missed since the last drop, or the start).
    fn went_down(&mut self, heard: Option<SystemTime>) {
        if let Some(heard) = heard {
            self.cutoff = millis(heard).saturating_sub(CLOCK_SKEW_MS);
        }
    }

    /// The notifications shown now, by the server's id, with their chats.
    #[must_use]
    pub fn shown(&self) -> Vec<(u32, String)> {
        let mut shown: Vec<(u32, String)> = self.ids.iter().map(|(id, chat)| (*id, chat.clone())).collect();
        shown.sort_unstable();
        shown
    }

    /// Notification `id` was activated (its body): its chat opens, and the chat's notifications close.
    pub fn activated(&mut self, id: u32, cx: &mut Context<Self>) {
        let Some(conversation) = self.ids.get(&id).cloned() else {
            return;
        };
        self.trace
            .record(&TraceLine::new("notification", "opened").conversation(&conversation));
        cx.emit(if self.channels.contains(&conversation) {
            NotifierEvent::OpenChannel(conversation.clone())
        } else {
            NotifierEvent::Open(conversation.clone())
        });
        self.forget(&conversation, cx);
    }

    fn arrived(&mut self, conversation: &str, message: &Message, source: Source, cx: &mut Context<Self>) {
        if self.settings.read(cx).settings().notifications == Notifications::Off
            || !may_notify(self.core.me(), conversation, message, source)
        {
            return;
        }
        self.pending.push(Candidate {
            conversation: conversation.to_owned(),
            message: message.clone(),
            source,
        });
        if !self.gathering {
            self.gathering = true;
            cx.spawn(async move |this, cx: &mut AsyncApp| gather(this, cx).await)
                .detach();
        }
    }

    /// The account's read horizon in `conversation` moved: once nothing in it is unread, its notifications close.
    fn read(&mut self, conversation: &str, cx: &mut Context<Self>) {
        if !self.shown.contains_key(conversation) {
            return;
        }
        let unread = self.core.unread_count(conversation.to_owned());
        let conversation = conversation.to_owned();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            if unread.await.is_ok_and(|unread| unread == 0) {
                // a notifier that is gone has nothing left to close
                let _ = this.update(cx, |this, cx| this.forget(&conversation, cx));
            }
        })
        .detach();
    }

    /// Closes `conversation`'s notifications and starts its count again; a batch taken before shows none of it.
    fn forget(&mut self, conversation: &str, cx: &mut Context<Self>) {
        self.forgets += 1;
        self.forgotten.insert(conversation.to_owned(), self.forgets);
        let Some(notices) = self.shown.remove(conversation) else {
            return;
        };
        let ids: Vec<u32> = notices.ids().collect();
        for id in &ids {
            self.ids.remove(id);
        }
        self.trace
            .record(&TraceLine::new("notifications", "closed").conversation(conversation));
        if let Some(server) = self.server.clone() {
            cx.background_spawn(async move {
                for id in ids {
                    server.close(id).await;
                }
            })
            .detach();
        }
    }

    /// The messages gathered and what decides about them; `None` (and the gathering over) when none are left.
    fn take_batch(&mut self, cx: &mut Context<Self>) -> Option<Batch> {
        let server = self.server.clone().filter(|_| !self.pending.is_empty());
        let Some(server) = server else {
            // nothing left, or nowhere to show it yet
            self.pending.clear();
            self.gathering = false;
            return None;
        };
        let candidates = std::mem::take(&mut self.pending);
        Some(Batch {
            candidates,
            level: self.settings.read(cx).settings().notifications,
            viewing: (self.viewing)(cx),
            cutoff: self.cutoff,
            taken: self.forgets,
            core: Arc::clone(&self.core),
            server,
        })
    }

    /// One notification shown one by one for `conversation`.
    fn shown_single(&mut self, conversation: &str, kind: NoticeKind, id: u32) {
        let notices = self.shown.entry(conversation.to_owned()).or_default();
        notices.singles.push(id);
        notices.count += 1;
        notices.mentioned |= kind == NoticeKind::Mention;
        self.ids.insert(id, conversation.to_owned());
        self.trace.record(
            &TraceLine::new("notification", "shown")
                .conversation(conversation)
                .kind(kind.name()),
        );
    }

    /// `conversation`'s notifications collapsed into `id`, for `count` messages (`mentioned`: one mentioned the
    /// account); the singles it replaced are gone.
    fn shown_collapsed(&mut self, conversation: &str, id: u32, count: usize, mentioned: bool) {
        let notices = self.shown.entry(conversation.to_owned()).or_default();
        for single in notices.singles.drain(..) {
            self.ids.remove(&single);
        }
        if let Some(old) = notices.collapsed.replace(id) {
            self.ids.remove(&old);
        }
        notices.count = count;
        notices.mentioned = mentioned;
        self.ids.insert(id, conversation.to_owned());
        self.trace.record(
            &TraceLine::new("notification", "shown")
                .conversation(conversation)
                .kind(NoticeKind::Collapsed.name()),
        );
    }
}

/// Waits [`GATHER`], then processes what was gathered until nothing is left.
async fn gather(this: WeakEntity<Notifier>, cx: &mut AsyncApp) {
    cx.background_executor().timer(GATHER).await;
    while let Ok(Some(batch)) = this.update(cx, Notifier::take_batch) {
        process(&this, batch, cx).await;
    }
}

/// The candidates by chat, each chat's in the order they came, the chats in the order of their first.
fn by_chat(candidates: Vec<Candidate>) -> Vec<(String, Vec<Candidate>)> {
    let mut chats: Vec<(String, Vec<Candidate>)> = Vec::new();
    for candidate in candidates {
        match chats.iter_mut().find(|(chat, _)| *chat == candidate.conversation) {
            Some((_, list)) => list.push(candidate),
            None => chats.push((candidate.conversation.clone(), vec![candidate])),
        }
    }
    chats
}

/// Decides about a batch and shows what notifies (see the module docs).
async fn process(this: &WeakEntity<Notifier>, batch: Batch, cx: &mut AsyncApp) {
    let Batch {
        candidates,
        level,
        viewing,
        cutoff,
        taken,
        core,
        server,
    } = batch;
    if level == Notifications::Off || candidates.is_empty() {
        return;
    }
    let conversations = core.conversations(LIST_LIMIT).await.unwrap_or_default();
    for (conversation, candidates) in by_chat(candidates) {
        if viewing.as_deref() == Some(conversation.as_str()) {
            // read as it comes: what was shown for it is done with
            let _ = this.update(cx, |this, cx| this.forget(&conversation, cx));
            continue;
        }
        let stored = conversations.iter().find(|stored| stored.id == conversation);
        let chosen = choose(&*core, stored, candidates, level, cutoff).await;
        if chosen.is_empty() {
            continue;
        }
        let (chat, notices) = describe(&*core, &conversation, stored, chosen).await;
        // asked again after the reads above, right before what is shown is read (nothing waits in between)
        let channel = stored.is_some_and(|stored| stored.kind == ConversationKind::Channel);
        let due = this.update(cx, |this, cx| {
            let due = still_due(this, &conversation, taken, cx);
            if due && channel {
                this.channels.insert(conversation.clone());
            }
            due
        });
        if due.is_ok_and(|due| due) {
            show(this, &*server, &conversation, &chat, notices, cx).await;
        }
    }
}

/// Whether `conversation`'s notices of a batch taken at `taken` may still be shown: it isn't shown in a focused window
/// now (if it is, what was shown for it is done with), and it wasn't opened or read since the batch was taken.
fn still_due(this: &mut Notifier, conversation: &str, taken: u64, cx: &mut Context<Notifier>) -> bool {
    if (this.viewing)(cx).as_deref() == Some(conversation) {
        this.forget(conversation, cx);
        return false;
    }
    this.forgotten.get(conversation).is_none_or(|at| *at <= taken)
}

/// The candidates of one chat that notify, each with whether it mentions the account.
async fn choose(
    core: &dyn Core,
    stored: Option<&Conversation>,
    candidates: Vec<Candidate>,
    level: Notifications,
    cutoff: i64,
) -> Vec<(Candidate, bool)> {
    let channel = stored.is_some_and(|stored| stored.kind == ConversationKind::Channel);
    let mut horizon = None;
    let mut chosen = Vec::new();
    for candidate in candidates {
        let mention = Mentions::of(candidate.message.properties.as_deref(), core.me()).names_me();
        if (level == Notifications::Mentions || channel) && !mention {
            continue;
        }
        if matches!(candidate.source, Source::CatchUp | Source::Replay) {
            let arrival = candidate.message.arrival_time;
            if arrival <= cutoff {
                continue;
            }
            if horizon.is_none() {
                let read = core.read_horizon(candidate.conversation.clone()).await;
                horizon = Some(read.ok().flatten().map(|horizon| horizon.last_read_arrival));
            }
            if horizon.flatten().is_some_and(|read| arrival <= read) {
                continue;
            }
        }
        chosen.push((candidate, mention));
    }
    chosen
}

/// The chat's name and the notices for its messages that notify.
async fn describe(
    core: &dyn Core,
    conversation: &str,
    stored: Option<&Conversation>,
    chosen: Vec<(Candidate, bool)>,
) -> (SharedString, Vec<Notice>) {
    let one_to_one = stored.is_none_or(|stored| stored.kind == ConversationKind::OneToOne);
    let chat: SharedString = match stored {
        Some(stored) => chat_list::chat_name(core, stored).await,
        None => "Chat".into(),
    };
    let mut notices = Vec::with_capacity(chosen.len());
    for (candidate, mention) in chosen {
        let sender = sender_name(core, &candidate.message).await;
        let summary = match (one_to_one, mention) {
            (true, false) => sender,
            (true, true) => format!("{sender} mentioned you"),
            (false, false) => format!("{sender} in {chat}"),
            (false, true) => format!("{sender} mentioned you in {chat}"),
        };
        notices.push(Notice {
            conversation: conversation.to_owned(),
            kind: if mention {
                NoticeKind::Mention
            } else {
                NoticeKind::Message
            },
            summary,
            body: chat_list::message_line(&candidate.message),
        });
    }
    (chat, notices)
}

/// A sender's name: as the engine has it, else as the message carried it, else [`SOMEONE`].
async fn sender_name(core: &dyn Core, message: &Message) -> String {
    let Some(sender) = message.sender.clone() else {
        return SOMEONE.to_owned();
    };
    if let Ok(person) = core.person(sender).await
        && let Some(name) = Shown::of(person).name
    {
        return name.to_string();
    }
    message
        .sender_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(SOMEONE)
        .to_owned()
}

/// What a collapsed notification of `count` messages says.
#[must_use]
pub fn collapsed_body(count: usize, mention: bool) -> String {
    if mention {
        format!("{count} new messages, one or more mentioning you")
    } else {
        format!("{count} new messages")
    }
}

/// Shows one chat's notices: one by one while the chat has at most [`SINGLES`], else collapsed into one.
async fn show(
    this: &WeakEntity<Notifier>,
    server: &dyn NotificationServer,
    conversation: &str,
    chat: &str,
    notices: Vec<Notice>,
    cx: &mut AsyncApp,
) {
    let Ok((count, singles, collapsed, mentioned)) = this.update(cx, |this, _| {
        this.shown
            .get(conversation)
            .map_or((0, Vec::new(), None, false), |notices| {
                (
                    notices.count,
                    notices.singles.clone(),
                    notices.collapsed,
                    notices.mentioned,
                )
            })
    }) else {
        return;
    };
    let total = count + notices.len();
    if total <= SINGLES {
        for notice in notices {
            if let Ok(id) = server.show(&notice, 0).await {
                let _ = this.update(cx, |this, _| this.shown_single(conversation, notice.kind, id));
            }
        }
        return;
    }
    for id in singles {
        server.close(id).await;
    }
    // a mention shown before, one by one or in the collapsed one this replaces, is still among the messages
    let mention = mentioned || notices.iter().any(|notice| notice.kind == NoticeKind::Mention);
    let notice = Notice {
        conversation: conversation.to_owned(),
        kind: NoticeKind::Collapsed,
        summary: chat.to_owned(),
        body: collapsed_body(total, mention),
    };
    if let Ok(id) = server.show(&notice, collapsed.unwrap_or(0)).await {
        let _ = this.update(cx, |this, _| this.shown_collapsed(conversation, id, total, mention));
    }
}

/// Text for a server that reads markup in a body (`body-markup`): `&`, `<` and `>` as entities.
#[must_use]
pub fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `org.freedesktop.Notifications` on the bus the app is given (see the module docs).
pub struct BusNotifications {
    proxy: zbus::Proxy<'static>,
    app_name: String,
    /// The server reads markup in a body: the text is escaped.
    markup: bool,
}

impl BusNotifications {
    /// The server on `connection`, notifying as `app_name`, and its activations: each notification's id with the
    /// action invoked on it (its body's is [`DEFAULT_ACTION`]).
    ///
    /// # Errors
    /// The server isn't on the bus, or the subscription to its signals is refused.
    pub async fn start(
        connection: &Connection,
        app_name: &str,
    ) -> zbus::Result<(Self, BoxStream<'static, (u32, String)>)> {
        let proxy: zbus::Proxy<'static> = zbus::proxy::Builder::new(connection)
            .destination(SERVER_NAME)?
            .path(SERVER_PATH)?
            .interface(SERVER_NAME)?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let capabilities: Vec<String> = proxy.call("GetCapabilities", &()).await?;
        let invoked = proxy.receive_signal("ActionInvoked").await?;
        let activations = invoked
            .filter_map(|signal| async move { signal.body().deserialize::<(u32, String)>().ok() })
            .boxed();
        let server = Self {
            proxy,
            app_name: app_name.to_owned(),
            markup: capabilities.iter().any(|capability| capability == "body-markup"),
        };
        Ok((server, activations))
    }
}

impl NotificationServer for BusNotifications {
    fn show(&self, notice: &Notice, replaces: u32) -> BoxFuture<'static, Result<u32, String>> {
        let proxy = self.proxy.clone();
        let app_name = self.app_name.clone();
        let summary = notice.summary.clone();
        let body = if self.markup {
            escape_markup(&notice.body)
        } else {
            notice.body.clone()
        };
        let call_notice = notice.kind == NoticeKind::Call;
        Box::pin(async move {
            // an incoming call's: Answer and Decline, critical, shown until it is closed (its ring ended)
            let (actions, hints, expires) = if call_notice {
                let actions = vec![
                    DEFAULT_ACTION,
                    "Open",
                    ANSWER_ACTION,
                    "Answer",
                    DECLINE_ACTION,
                    "Decline",
                ];
                let hints: HashMap<&str, Value<'_>> = HashMap::from([("urgency", Value::U8(2))]);
                (actions, hints, 0_i32)
            } else {
                let hints: HashMap<&str, Value<'_>> =
                    HashMap::from([("category", Value::from("im.received")), ("urgency", Value::U8(1))]);
                (vec![DEFAULT_ACTION, "Open"], hints, -1_i32)
            };
            let call = (
                app_name.as_str(),
                replaces,
                "",
                summary.as_str(),
                body.as_str(),
                actions,
                hints,
                expires,
            );
            proxy
                .call::<_, _, u32>("Notify", &call)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        let proxy = self.proxy.clone();
        Box::pin(async move {
            // one the server already closed is no matter
            let _ = proxy.call::<_, _, ()>("CloseNotification", &(id,)).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_escaped() {
        assert_eq!(escape_markup("a <b> & c"), "a &lt;b&gt; &amp; c");
    }

    #[test]
    fn a_collapsed_notification_counts_its_messages() {
        assert_eq!(collapsed_body(4, false), "4 new messages");
        assert_eq!(collapsed_body(5, true), "5 new messages, one or more mentioning you");
    }
}
