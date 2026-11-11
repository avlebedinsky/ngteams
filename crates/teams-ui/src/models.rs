//! The models the views observe, kept up to date by the bridge's drain ([`Models::apply`], on GPUI's foreground).
//!
//! They hold what the engine told, not what the store holds: a view that shows a conversation asks the store again
//! (through [`Core`](crate::Core)) when its [`Chats::revision`] moves. So every view of the same data (the list, the
//! switcher, search) sees every change, and nothing shown comes from anywhere but the store.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime};

use gpui::{App, AppContext as _, AsyncApp, Entity, EventEmitter, SharedString};
use teams_core::{Event, MessageChange, OwnStatus, READING_TEAMS, RegistrationReport, Source, TransientSent};
use teams_store::{Applied, Message, OutboxEntry};

use crate::calls::Calls;
use crate::presence::Presences;

/// How long someone is shown typing after their last typing signal, unless they stop (`typing: false`) or a message
/// of theirs is delivered first. M2 Task 10's capture (`docs/protocol/chat.md`, "The shapes the UI needs
/// (chat-shapes)"): the web client's indicator went off about 25 s after the last typing delivery, or as soon as the
/// sender's message arrived; the web client never sent `Control/ClearTyping`.
pub const TYPING_TIMEOUT: Duration = Duration::from_secs(25);

/// The push channel, as the account's status shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Not connected yet since the engine started.
    Starting,
    Connected,
    /// Down; it reconnects by itself.
    Down,
    /// Stopped for good.
    Stopped,
}

/// The engine itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Running,
    /// Stopped: its requests answer `Stopped`, nothing more goes out.
    Stopped,
}

/// Whether the account has tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Account {
    SignedIn,
    /// The session can't renew its tokens (an expired or revoked refresh token): the engine is stopped, nothing more
    /// is sent, and `hint` says what to do (the lab build: run `proto-lab auth <alias>`).
    SignedOut {
        hint: SharedString,
    },
}

/// The account's status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub link: Link,
    pub engine: Engine,
    pub account: Account,
    /// Catch-ups that ran on their own since the start.
    pub catch_ups: u64,
    /// What the engine last failed at on its own (a catch-up, …), until a catch-up runs clean.
    pub last_failure: Option<&'static str>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            link: Link::Starting,
            engine: Engine::Running,
            account: Account::SignedIn,
            catch_ups: 0,
            last_failure: None,
        }
    }
}

/// What changed in the account's chats, by conversation.
#[derive(Debug, Default)]
pub struct Chats {
    /// Moves on every change the engine tells; a view asks the store again when the one it read at has moved.
    revision: u64,
    /// Per conversation: the revision of its last change.
    changed: HashMap<String, u64>,
    /// Per person, by MRI: the revision of the last message told from them that carried a display name (which may be
    /// a name the store didn't have, or a new one), or of their profile's last change (`Event::Profile`: a name or a
    /// picture read in the background).
    named: HashMap<String, u64>,
    /// Per conversation: who is typing now, each with the stamp of their last signal (a timeout clears only the
    /// signal it was started for).
    typing: BTreeMap<String, BTreeMap<String, u64>>,
    /// The last typing signal's stamp.
    typing_stamp: u64,
    /// The outbox rows as last told, by id.
    outbox: BTreeMap<i64, OutboxEntry>,
    /// The revision of the stored teams' last change (`Event::Teams`).
    teams: u64,
    /// The revision of the last failed read of the teams (`Event::Failed` during [`READING_TEAMS`]).
    teams_failed: u64,
}

impl Chats {
    /// The revision of the stored teams' last change; 0 while none was told.
    #[must_use]
    pub fn teams_at(&self) -> u64 {
        self.teams
    }

    /// The revision of the last failed read of the teams; 0 while none failed. Later than [`Chats::teams_at`]: the
    /// last read told failed.
    #[must_use]
    pub fn teams_failed_at(&self) -> u64 {
        self.teams_failed
    }

    /// The revision of the last change in any conversation.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The revision of `conversation`'s last change; 0 while none was told.
    #[must_use]
    pub fn changed_at(&self, conversation: &str) -> u64 {
        self.changed.get(conversation).copied().unwrap_or(0)
    }

    /// The revision of the last message told from `mri` that carried a display name, in any conversation, or of their
    /// profile's last change; 0 while none was told.
    #[must_use]
    pub fn named_at(&self, mri: &str) -> u64 {
        self.named.get(mri).copied().unwrap_or(0)
    }

    /// Who is typing in `conversation` now, by MRI.
    pub fn typing(&self, conversation: &str) -> impl Iterator<Item = &str> {
        self.typing
            .get(conversation)
            .into_iter()
            .flat_map(BTreeMap::keys)
            .map(String::as_str)
    }

    /// An outbox row as last told.
    #[must_use]
    pub fn outbox_row(&self, id: i64) -> Option<&OutboxEntry> {
        self.outbox.get(&id)
    }

    /// The outbox rows of `conversation` told since the start, as last told, the oldest first.
    pub fn outbox_rows<'a>(&'a self, conversation: &'a str) -> impl Iterator<Item = &'a OutboxEntry> {
        self.outbox
            .values()
            .filter(move |row| row.conversation_id.as_deref() == Some(conversation))
    }

    fn change(&mut self, conversation: &str) {
        self.revision += 1;
        self.changed.insert(conversation.to_owned(), self.revision);
    }

    /// Someone's name or picture may have changed: every view that shows them asks again.
    fn renamed(&mut self, mri: &str) {
        self.revision += 1;
        self.named.insert(mri.to_owned(), self.revision);
    }

    /// Someone started typing: the stamp of this signal, and whether they weren't shown typing before.
    fn start_typing(&mut self, conversation: &str, from: &str) -> (u64, bool) {
        self.typing_stamp += 1;
        let stamp = self.typing_stamp;
        let shown = self
            .typing
            .entry(conversation.to_owned())
            .or_default()
            .insert(from.to_owned(), stamp)
            .is_none();
        (stamp, shown)
    }

    /// Someone stopped typing (with `stamp`: only if their last signal is that one); whether they were shown typing.
    fn stop_typing(&mut self, conversation: &str, from: &str, stamp: Option<u64>) -> bool {
        let Some(who) = self.typing.get_mut(conversation) else {
            return false;
        };
        if stamp.is_some_and(|stamp| who.get(from) != Some(&stamp)) {
            return false;
        }
        let stopped = who.remove(from).is_some();
        if who.is_empty() {
            self.typing.remove(conversation);
        }
        stopped
    }
}

/// What the [`Feed`] tells as it comes: for what acts on single events (the desktop's notifications), not for what a
/// view shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedEvent {
    /// A message newly stored (a delivery the store didn't have, or one a read of the service brought), with what
    /// brought it. An edit, a delete, a reaction or a newer version of a stored message isn't one.
    Arrived {
        conversation: String,
        message: Box<Message>,
        source: Source,
    },
    /// The account's own read horizon in a conversation moved.
    Read { conversation: String },
    /// A conversation changed (a delivery said so, or a read of the service changed it in the store: a catch-up tells
    /// a read horizon it stored this way, not as [`FeedEvent::Read`]).
    Changed { conversation: String },
    /// The push channel went down: `heard` is when its socket last read a frame once connected, the last moment it
    /// was known to deliver; `None` when it never connected.
    Down { heard: Option<SystemTime> },
    /// A registration of the push channel's endpoint went through: the account's own availability can be reported
    /// from it (M2 Task 20).
    Registered,
    /// The push channel stopped for good: no own availability can be reported any more.
    PushStopped,
    /// A report of the account's own availability that waited out a `Retry-After` went as `sent` (or was dropped while
    /// it waited).
    OwnReport { status: OwnStatus, sent: TransientSent },
}

/// The engine's events that single things act on, told one by one ([`FeedEvent`]); it holds nothing.
#[derive(Debug, Default)]
pub struct Feed;

impl EventEmitter<FeedEvent> for Feed {}

/// The models, shared by every view (cheap to clone: entity handles).
#[derive(Clone, Debug)]
pub struct Models {
    pub status: Entity<Status>,
    pub chats: Entity<Chats>,
    pub feed: Entity<Feed>,
    /// Others' presence as the engine told it (`Event::Presence`).
    pub presence: Entity<Presences>,
    /// The account's calls as the engine told them ([`crate::calls`]); shown only by a build that asked for them
    /// ([`Models::with_calls`]).
    pub calls: Entity<Calls>,
    /// The views' asks for a person's profile card ([`crate::profile_card`]), which the shell of their window opens.
    pub cards: Entity<crate::profile_card::CardAsks>,
}

impl Models {
    /// The models of a build or a run that shows no call (an engine without calling, such as the personal build's
    /// when its devices couldn't be opened): whatever the engine tells of calls is left out.
    #[must_use]
    pub fn new(cx: &mut App) -> Self {
        Self::with(Calls::default(), cx)
    }

    /// The models of a build that shows calls (the lab app, the personal build), its test call to `echo` (the echo
    /// service's MRI, when known).
    #[must_use]
    pub fn with_calls(echo: Option<teams_proto::Mri>, cx: &mut App) -> Self {
        Self::with(Calls::shown(echo), cx)
    }

    /// The models of a build that shows calls with their video (the lab app or the personal build, when its calls have
    /// video), its test call to `echo` and its calls' video shown on `feed`.
    #[must_use]
    pub fn with_video_calls(echo: Option<teams_proto::Mri>, feed: crate::video::VideoFeed, cx: &mut App) -> Self {
        Self::with(Calls::shown(echo).with_video(feed), cx)
    }

    fn with(calls: Calls, cx: &mut App) -> Self {
        Self {
            status: cx.new(|_| Status::default()),
            chats: cx.new(|_| Chats::default()),
            feed: cx.new(|_| Feed),
            presence: cx.new(|_| Presences::default()),
            calls: cx.new(|_| calls),
            cards: cx.new(|_| crate::profile_card::CardAsks),
        }
    }

    /// Applies one of the engine's events; the models it changed notify their observers, and the [`Feed`] tells what
    /// came.
    pub fn apply(&self, event: Event, cx: &mut App) {
        self.feed(&event, cx);
        match event {
            Event::Connected { .. } => self.set_link(Link::Connected, cx),
            Event::Disconnected { .. } => self.set_link(Link::Down, cx),
            Event::PushStopped(_) => self.set_link(Link::Stopped, cx),
            Event::CaughtUp { .. } => self.status.update(cx, |status, cx| {
                status.catch_ups += 1;
                status.last_failure = None;
                cx.notify();
            }),
            Event::CatchUpFailed { .. } => self.failed("catch-up", cx),
            Event::Failed { during, .. } => self.failed(during, cx),
            Event::Message {
                conversation,
                message: Some(message),
                change: MessageChange::New,
                source: Source::Delivery,
                ..
            } => {
                // a new message of theirs, delivered, ends their typing; a reaction, an edit or a delete on one of
                // their messages doesn't (anyone may react, and it isn't the message they were typing)
                if let Some(from) = &message.sender {
                    self.stop_typing(&conversation, from, None, cx);
                }
                self.message(&conversation, Some(&*message), cx);
            }
            Event::Message {
                conversation, message, ..
            } => self.message(&conversation, message.as_deref(), cx),
            Event::ReadHorizon { conversation } | Event::ConversationChanged { conversation } => {
                self.changed(&conversation, cx);
            }
            Event::Typing {
                conversation,
                from,
                typing,
            } => self.typing(&conversation, from.as_deref(), typing, cx),
            // a row just made shows at once as sending, then as each attempt leaves it
            Event::Queued(entry) | Event::Outbox(entry) => self.outbox(entry, cx),
            Event::Profile { mri } => self.chats.update(cx, |chats, cx| {
                chats.renamed(&mri);
                cx.notify();
            }),
            Event::Presence { mri, availability } => self.presence.update(cx, |presence, cx| {
                if presence.set(mri, availability) {
                    cx.notify();
                }
            }),
            Event::Teams => self.chats.update(cx, |chats, cx| {
                chats.revision += 1;
                chats.teams = chats.revision;
                cx.notify();
            }),
            // the calls: their own model, which leaves them out in a build that shows none
            Event::CallingRegistration(_)
            | Event::IncomingCall(_)
            | Event::RingEnded { .. }
            | Event::Call { .. }
            | Event::Voicemail { .. }
            | Event::CameraOff { .. }
            | Event::AudioClosed { .. }
            | Event::CallCaptions { .. }
            | Event::CallRecording { .. }
            | Event::CallParticipants { .. }
            | Event::Lobby { .. }
            | Event::CallOwn { .. }
            | Event::CallHeld { .. }
            | Event::CallHeldByFarEnd { .. }
            | Event::CallMeeting { .. }
            | Event::CallSharing { .. }
            | Event::CallRemoteShare { .. }
            | Event::CallGroupChat { .. }
            | Event::CallAddFailed { .. }
            | Event::CallTransferFailed { .. }
            | Event::CallTransferred { .. }
            | Event::CallReaction { .. } => {
                self.calls.update(cx, |calls, cx| calls.apply(&event, cx));
            }
            // how the push channel's registration went and a waiting transient's send (told on the feed, for the own
            // presence's policy), the search backfill over: nothing a view shows
            Event::Registration(_) | Event::Transient(..) | Event::Indexed { .. } => {}
        }
    }

    /// Tells a message newly stored, a read horizon that moved, a conversation that changed, the push channel gone
    /// down, registered or stopped, or how a waiting report of the account's own availability went, on the [`Feed`].
    fn feed(&self, event: &Event, cx: &mut App) {
        let told = match event {
            Event::Message {
                conversation,
                message: Some(message),
                change: MessageChange::New,
                applied: Applied::Inserted,
                source,
                ..
            } => FeedEvent::Arrived {
                conversation: conversation.clone(),
                message: message.clone(),
                source: *source,
            },
            Event::ReadHorizon { conversation } => FeedEvent::Read {
                conversation: conversation.clone(),
            },
            Event::ConversationChanged { conversation } => FeedEvent::Changed {
                conversation: conversation.clone(),
            },
            Event::Disconnected { heard, .. } => FeedEvent::Down { heard: *heard },
            Event::Registration(RegistrationReport::Registered) => FeedEvent::Registered,
            Event::PushStopped(_) => FeedEvent::PushStopped,
            Event::Transient(transient, sent) => match transient.own_status() {
                Some(status) => FeedEvent::OwnReport {
                    status,
                    sent: sent.clone(),
                },
                None => return,
            },
            _ => return,
        };
        self.feed.update(cx, |_, cx| cx.emit(told));
    }

    /// The engine stopped (its events ended, or it was shut down): no call rings or is up any more.
    pub fn engine_stopped(&self, cx: &mut App) {
        self.calls.update(cx, Calls::stopped);
        self.status.update(cx, |status, cx| {
            if status.engine != Engine::Stopped {
                status.engine = Engine::Stopped;
                cx.notify();
            }
        });
    }

    /// The account is signed out (see [`Account::SignedOut`]): the engine is shut down next, and no call event comes
    /// any more, so no call rings or is up.
    pub fn signed_out(&self, hint: SharedString, cx: &mut App) {
        self.calls.update(cx, Calls::stopped);
        self.status.update(cx, |status, cx| {
            status.account = Account::SignedOut { hint };
            cx.notify();
        });
    }

    fn set_link(&self, link: Link, cx: &mut App) {
        self.status.update(cx, |status, cx| {
            if status.link != link {
                status.link = link;
                cx.notify();
            }
        });
    }

    /// Something failed: the status says what last did, and a failed read of the teams is told to the Teams list.
    fn failed(&self, during: &'static str, cx: &mut App) {
        if during == READING_TEAMS {
            self.chats.update(cx, |chats, cx| {
                chats.revision += 1;
                chats.teams_failed = chats.revision;
                cx.notify();
            });
        }
        self.status.update(cx, |status, cx| {
            status.last_failure = Some(during);
            cx.notify();
        });
    }

    fn changed(&self, conversation: &str, cx: &mut App) {
        self.chats.update(cx, |chats, cx| {
            chats.change(conversation);
            cx.notify();
        });
    }

    /// A message stored or changed: its conversation changed, and its sender's name may have.
    fn message(&self, conversation: &str, message: Option<&Message>, cx: &mut App) {
        self.chats.update(cx, |chats, cx| {
            chats.change(conversation);
            let named = message.and_then(|message| {
                let name = message.sender_name.as_deref().map(str::trim)?;
                message.sender.as_ref().filter(|_| !name.is_empty())
            });
            if let Some(sender) = named {
                chats.named.insert(sender.clone(), chats.revision);
            }
            cx.notify();
        });
    }

    fn typing(&self, conversation: &str, from: Option<&str>, typing: bool, cx: &mut App) {
        // a typing signal that names no one can't be shown against anyone
        let Some(from) = from else { return };
        if !typing {
            self.stop_typing(conversation, from, None, cx);
            return;
        }
        let stamp = self.chats.update(cx, |chats, cx| {
            let (stamp, shown) = chats.start_typing(conversation, from);
            if shown {
                cx.notify();
            }
            stamp
        });
        let (models, conversation, from) = (self.clone(), conversation.to_owned(), from.to_owned());
        cx.spawn(async move |cx: &mut AsyncApp| {
            cx.background_executor().timer(TYPING_TIMEOUT).await;
            cx.update(|cx| models.stop_typing(&conversation, &from, Some(stamp), cx));
        })
        .detach();
    }

    fn stop_typing(&self, conversation: &str, from: &str, stamp: Option<u64>, cx: &mut App) {
        self.chats.update(cx, |chats, cx| {
            if chats.stop_typing(conversation, from, stamp) {
                cx.notify();
            }
        });
    }

    fn outbox(&self, entry: OutboxEntry, cx: &mut App) {
        self.chats.update(cx, |chats, cx| {
            if let Some(conversation) = &entry.conversation_id {
                chats.change(conversation);
            }
            chats.outbox.insert(entry.id, entry);
            cx.notify();
        });
    }
}
