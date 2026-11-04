//! The engine: one account's client core on a tokio runtime and a thread of its own, reached only through channels,
//! the shape M2's GPUI bridge needs. A UI thread hands it [`Action`]s and queries and awaits each [`Reply`] on its own
//! executor (a CLI blocks on it, [`Reply::blocking_wait`]), and takes its [`Event`]s as they come.
//!
//! - **Commands** ([`EngineRequests::write`]): an [`Action`] (send, edit, delete, react, take a reaction back, read,
//!   create a chat) is checked against the store first: an edit or a delete only of the account's own message that
//!   isn't deleted, a reaction only on a message that isn't, a read receipt only up to a stored message. It becomes an
//!   outbox row ([`enqueue`]) and is sent at once ([`Outbox::send`]) through the transport. The reply is the row as
//!   that send left it, an error only when no row was made (or it can't be read back): a row that waits (a
//!   `Retry-After`, a check that couldn't sync, an older row on its
//!   [`Lane`](crate::Lane)) is in the reply as it waits, and goes out from the engine's tick ([`Outbox::send_due`]);
//!   every row that settles comes as [`Event::Outbox`] too, and so does every row our own message coming back
//!   settles, by whichever read brought it (a catch-up, a delivery, a sync after a write, the check of another row),
//!   and every row a read finds may have arrived (an entry it couldn't read carrying its `clientmessageid`): one event
//!   per change of the row, the last one the row as it now stands ([`Outbox::take_arrived`]); a row just made (or
//!   adopted by a resend) is told first as [`Event::Queued`], before its first attempt. Once a write went
//!   through, its conversation is synced before its row is told, so the account's own change is in the store (a new
//!   chat with it) by the time its owner hears it went. A row can
//!   be read back by its id ([`EngineRequests::outbox_entry`]).
//! - **Events → store**: each delivery of the push channel ([`teams_trouter`]) goes through the catch-up's
//!   [`CatchUp::receive_message`], the path a sync takes, so a message that comes both ways is stored once, at its
//!   newest version; the account's own read horizon through [`CatchUp::receive_horizon`], its mark-all-read one through
//!   [`CatchUp::receive_app_horizon`]. A delivery naming a
//!   conversation the store doesn't know adds it first (a thread read), and the conversation is synced after. Typing
//!   is only an event, never stored; a delivery is never answered (the push channel can't).
//! - **Catch-up** ([`Gaps`]): after every connect of the push channel when one is due (on start, and after every
//!   disconnect), after a message-loss report about the chats (acknowledged once a catch-up ran with nothing failed
//!   but what the service refused, [`CatchUpReport::caught_up`]),
//!   and on [`EngineRequests::sync`]. One that failed is tried again from the tick, [`EngineOptions::catch_up_retry`]
//!   later, while the channel is up; so is a delivery that didn't reach the store (its conversation couldn't be
//!   added or synced, the store failed), and a sync after a write that failed: what they would have stored is missed
//!   until a catch-up brings it.
//! - **What the reads brought** ([`CatchUp::take_changes`]): after every read of the service (a catch-up, the sync
//!   after a write or after a delivery's new conversation, the outbox's check, older history) each message it stored
//!   new or newer is told as [`Event::Message`] marked with the read that brought it ([`Source::CatchUp`],
//!   [`Source::History`]), and each conversation it changed as [`Event::ConversationChanged`]; a message the store
//!   had already (a delivery the catch-up meets again) is not told twice.
//! - **Queries**: the conversation list, a page of a conversation's messages, one message, its reactions, a chat's
//!   members, the unread count and the account's read horizon of a conversation, whether its whole history is stored,
//!   an outbox row, a conversation's rows that aren't sent and the rows earlier runs left, all read from the store on
//!   the engine's thread, which owns it; older history from the service ([`EngineRequests::load_older`]). A row of an earlier run (or a failed or
//!   denied one) goes out again only by the explicit [`EngineRequests::resend`]. The requests are taken in the order
//!   they were made, but a query (and a person, the teams and the chat listing, from the store alone, their own
//!   background read started afterwards) is answered also while a read of the catch-up waits on the service, from the
//!   store as it stands (`inbox`): a catch-up that runs for minutes (hundreds of conversations, a page of 200 channel
//!   posts taking seconds) doesn't hold up a view's reads. Read receipts queue and advance the local horizon during
//!   those waits too; local queries and receipts also proceed while an outbox attempt awaits transport. Other requests
//!   wait their turn. The timestamp-ordered service horizon reconciles the local read without reviving stale badges.
//! - **People** ([`EngineRequests::person`]): someone's name and picture as the views show them, from the store, and
//!   read in the background when the store's are missing or old ([`crate::profiles`]: Graph, else the middle tier's
//!   profile lookup, a read sent as a `POST`); each change stored is told as [`Event::Profile`].
//! - **Others' presence** (M2 Task 17, [`EngineRequests::presence`]): the availability of the people the views asked
//!   about, pushed by UPS once the push channel's endpoint subscribed to it and read from UPS by polling besides
//!   ([`crate::presence`]: [`EngineOptions::presence_poll`], [`EngineOptions::presence_poll_subscribed`], never
//!   stored); each change told as [`Event::Presence`].
//! - **Search** ([`EngineRequests::search`]): the store's own index of the messages' plain text ([`crate::search`]),
//!   never the service's search. Each message is indexed as it is stored; the messages stored before are indexed by a
//!   backfill that runs from the engine's start, a batch at a time ([`EngineOptions::index_batch`]) between the
//!   loop's other work and the runtime's other tasks, told as [`Event::Indexed`] once it is over. A search in some
//!   conversations only ([`EngineRequests::search_in`]) leaves the others out before it counts the hits.
//! - **Teams and channels** (M2 Task 16, read-only): the account's teams with their channels
//!   ([`EngineRequests::teams`]), from the store, read from CSA in the background when the stored list is missing or
//!   old ([`crate::teams`]), each change of the list told as [`Event::Teams`]; a channel's posts and replies synced
//!   on demand like any conversation's ([`EngineRequests::sync_conversation`]: a channel other than a team's general
//!   one is in no conversation list, so no catch-up reaches it), and the post each stored channel message belongs to
//!   ([`EngineRequests::reply_roots`]). The channel view has no composer or message actions. A read whose
//!   favourites name the account's own notes ([`NOTES`]) while the store has none syncs them on the next tick: the
//!   catch-up that runs before the first read of the teams (the first run, the first after the store kept favourites)
//!   can't know them yet, and without a push channel's drop no other catch-up may come for a long time.
//! - **Transient writes** ([`EngineRequests::transient`]: typing) go out through the outbox too, but as no row
//!   ([`Transient`]); a throttled one goes out from the tick unless a newer one of the same took its place, it waits
//!   once and briefly ([`crate::MAX_TRANSIENT_WAIT`]), and a message post to its conversation drops it (told as
//!   [`Event::Transient`] with its drop).
//! - **The account's own availability** (M2 Task 19, [`EngineRequests::own_availability`]): its endpoint's `PUT`
//!   (available) and its manual status (set or cleared), transient writes naming the endpoint the push channel
//!   registered in this run ([`PushEvent::Registered`]); refused before a registration went through and after the push
//!   channel stopped, when one still waiting out a `Retry-After` is dropped too (told as [`Event::Transient`]). When
//!   to send what is the app's policy ([`crate::own_presence`]), not the engine's.
//! - **Its handles**: [`EngineRequests`], the request side (writes and queries, each a [`Reply`]), is cloneable, for a
//!   UI's views to share; [`EngineHandle`] owns the engine: its events, its thread and its shutdown.
//! - **Calls** (M3 Task 14, [`EngineRequests::call`], [`crate::calling`]): with [`Parts::calls`] the engine opens the
//!   calling stack's socket too and runs the account's calls: a call from a chat or the test call, a join, the answer
//!   or the decline of an invitation it offered ([`Event::IncomingCall`]), a hang-up, mute, a renegotiation; each
//!   write through the outbox's call kind and shared call floor, the call's media on threads of its own. The
//!   calling runs as a task of its own on the engine's thread (a `LocalSet`'s), so a call's write in flight, its
//!   deliveries and their responses go on while the engine awaits a chat's work. It tells [`Event::IncomingCall`],
//!   [`Event::RingEnded`] and [`Event::Call`]. One call at a time; nothing is answered or declined but by its action.
//!   Stopping, it hangs the call up first.
//! - **One engine per account** ([`teams_store::Store::engine_lock`]): [`start`] takes the account's engine lock
//!   without waiting, and is refused ([`EngineError::AlreadyRunning`]) while another engine holds it, in this process
//!   or another; the lock goes when the engine's thread ends.
//!
//! The engine sends nothing of its own: every write goes out through the outbox, every read through the catch-up's
//! client, and the push channel sends only its own typed frames. Without a push channel ([`Parts::push`] `None`:
//! an offline or polling client) nothing is caught up but on [`EngineRequests::sync`]. The store stays open
//! until the engine stops, after every attempt it began has finished.
//!
//! M1 Task 18 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`); M2 Task 5
//! (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`).

use std::fmt;
use std::future::{Future, pending};
use std::io;
use std::ops::Deref;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::task::{Context, Poll};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use teams_call::{CallId, Placement, RemoteEnd};
use teams_proto::Mri;
use teams_proto::chat::{self, MessageText};
use teams_proto::{
    AnalyticsPeriod, CalendarEvent, ChannelMeetup, Client, Credentials, EndpointRegistrar, EventAttendee, FileRange,
    FullEvent, HttpError, HttpTransport, InstalledApp, JoinableTeam, ManagedChannel, ManualAvailability,
    MeetingDetails, OwnTeamView, PendingInvite, RegisteredEndpoint, StoreApp, TagsRead, TeamAnalytics, TeamPerson,
    TeamPicture, TeamTemplateCard, TeamTemplateDetails, TeamWrite, Transport,
};
use teams_store::{
    Applied, Conversation, Member, Message, MessagePage, OutboxEntry, OutboxKind, OutboxState, PageCursor, Reaction,
    ReadHorizon, SearchHit, Store, StoreError, Team,
};
use teams_trouter::{
    Change, ChatEvent, Delivery, Down, MessageLoss, PushEvent, RegistrationReport, Stop, Trouter, TrouterCredentials,
    TrouterHandle,
};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, MissedTickBehavior};

use crate::calendar::{Calendar, EventWrite, EventWritten, Invitable, MeetingAvailability, WindowRead};
use crate::channels::{ChannelDraft, ChannelMade, ChannelProblem, Channels, Finished};
use crate::service_search::{SearchAnswer, SearchScope, ServiceSearch};
use crate::team_admin::{TeamAsk, TeamChange, TeamMember, TeamWritten};
use crate::team_apps::TeamAppsAsk;

/// How many people the attendee picker suggests at most.
const INVITABLE: usize = 8;
use crate::calling::{
    Act, CallAction, CallActionError, CallParts, CallStatus, CallView, Calling, CallingTask, IncomingCall, LobbyPerson,
};
use crate::emoticons::{Animations, Strip};
use crate::file_content::{FileContents, FileStream};
use crate::outbox::{Command, Outbox, OutboxError, Sent, Transient, TransientSent, create_chat, enqueue};
use crate::pictures::{Disk, PictureBytes, Pictures};
use crate::presence::{Availability, PRESENCE_POLL, PRESENCE_POLL_SUBSCRIBED, PresenceDone, Presences};
use crate::profiles::{PROFILE_RETRY, Person, Profiles};
use crate::search::{Backfill, INDEX_BATCH, Step};
use crate::sync::{CatchUp, CatchUpError, CatchUpReport, Gaps, NOTES, Synced};
use crate::teams::{ChatListing, READING_TEAMS, TEAMS_RETRY, Teams, TeamsRead, TeamsReadError};
use crate::thumbnails::Thumbnails;

mod described;
mod inbox;

use inbox::Inbox;

/// How the engine paces itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineOptions {
    /// How often the outbox's waiting rows are looked at, and a failed catch-up's time to try again: 1 s.
    pub tick: Duration,
    /// How long after a catch-up that failed (or left a conversation unsynced), or a delivery that didn't reach the
    /// store, the next catch-up is tried: 30 s.
    pub catch_up_retry: Duration,
    /// How long after a profile lookup that failed on the way the person is looked up again, from the tick:
    /// [`PROFILE_RETRY`].
    pub profile_retry: Duration,
    /// How many stored messages one step of the search backfill indexes at most ([`crate::search`]): [`INDEX_BATCH`].
    pub index_batch: usize,
    /// How long after a read of the teams that failed a view's asking reads them again, at the soonest
    /// ([`crate::teams`]): [`TEAMS_RETRY`].
    pub teams_retry: Duration,
    /// How often the watched people's presence is read while the subscription doesn't cover them all
    /// ([`crate::presence`]): [`PRESENCE_POLL`].
    pub presence_poll: Duration,
    /// How often it is read while the subscription covers everyone it may name: [`PRESENCE_POLL_SUBSCRIBED`].
    pub presence_poll_subscribed: Duration,
    /// Without a push channel, a catch-up at the start and then this often ([`Cause::Poll`]): the personal build's
    /// read-only mode, which opens no push channel, reads what is new so. `None` (the default): without a push channel
    /// nothing is caught up but on [`EngineRequests::sync`].
    pub poll: Option<Duration>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(1),
            catch_up_retry: Duration::from_secs(30),
            profile_retry: PROFILE_RETRY,
            index_batch: INDEX_BATCH,
            teams_retry: TEAMS_RETRY,
            presence_poll: PRESENCE_POLL,
            presence_poll_subscribed: PRESENCE_POLL_SUBSCRIBED,
            poll: None,
        }
    }
}

/// The account's push channel, built and not yet started: the engine starts it on its own runtime.
pub struct Push(Box<dyn FnOnce() -> TrouterHandle + Send>);

impl Push {
    #[must_use]
    pub fn new<T, K>(trouter: Trouter<T, K>) -> Self
    where
        T: EndpointRegistrar + Send + 'static,
        K: TrouterCredentials + 'static,
    {
        Self(Box::new(move || trouter.spawn()))
    }

    pub(crate) fn spawn(self) -> TrouterHandle {
        (self.0)()
    }
}

impl fmt::Debug for Push {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Push")
    }
}

/// What an engine is made of: the account's store, the client its reads go through, its outbox and, for a client
/// that listens, its push channel.
pub struct Parts<C, T> {
    pub store: Store,
    pub client: Arc<Client<HttpTransport<C>>>,
    pub outbox: Outbox<T>,
    pub push: Option<Push>,
    pub options: EngineOptions,
    /// Names the client knows already, by MRI: the last source for a person's name, after their profile and
    /// their messages ([`crate::profiles`]).
    pub known_names: Vec<(String, String)>,
    /// The account's calling ([`crate::calling`]): its calling socket, who it is in its calls and their media; `None`
    /// for an engine that makes no calls (every call action is refused, `CallActionError::NoCalling`).
    pub calls: Option<CallParts>,
}

/// What the user asks the engine to write. Every one goes out through the outbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// A message to an existing chat.
    Send { conversation: String, text: MessageText },
    /// One of the account's messages, its text now `text`.
    Edit {
        conversation: String,
        message_id: String,
        text: MessageText,
    },
    /// One of the account's messages, soft-deleted.
    Delete { conversation: String, message_id: String },
    /// The account's reaction `key` (`like`, …) on a message.
    React {
        conversation: String,
        message_id: String,
        key: String,
    },
    /// The account's reaction `key` on a message, taken back.
    Unreact {
        conversation: String,
        message_id: String,
        key: String,
    },
    /// The account's read horizon moved to a message (a read receipt others may see).
    Read { conversation: String, message_id: String },
    /// A chat of the account and `others`: their 1:1 chat for one, a new group chat for two or more.
    CreateChat { others: Vec<Mri> },
}

/// What a message event changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageChange {
    New,
    Edit,
    Delete,
    Reaction,
    Other,
}

impl From<Change> for MessageChange {
    fn from(change: Change) -> Self {
        match change {
            Change::Edit => Self::Edit,
            Change::Delete => Self::Delete,
            Change::Reaction => Self::Reaction,
            Change::Other => Self::Other,
        }
    }
}

/// What brought a message event: the push channel, or a read of the service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A delivery over the push channel.
    Delivery,
    /// A delivery the service had made before and made again (`mstore`).
    Replay,
    /// A read that brought the store up to date with the service: a catch-up, the sync after a write or after a
    /// delivery's new conversation, the outbox's check, a conversation's sync asked for
    /// ([`EngineRequests::sync_conversation`]: a channel opened).
    CatchUp,
    /// Older history, read on [`EngineRequests::load_older`].
    History,
}

/// Why a catch-up ran on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The push channel connected (the first time, or again after a drop) while one was due.
    Connected,
    /// The service reported pushes about the chats that the endpoint missed.
    MessageLoss,
    /// The last one failed (or left a conversation unsynced), or a delivery didn't reach the store, and its wait is
    /// over.
    Retry,
    /// There is no push channel, and the poll's time came ([`EngineOptions::poll`]).
    Poll,
}

/// What the engine tells its owner.
#[derive(Debug)]
pub enum Event {
    /// The push channel is up; a catch-up follows when one is due.
    Connected { endpoint: String },
    /// How a registration of the push channel's endpoint went: deliveries come only to a registered one.
    Registration(RegistrationReport),
    /// The push channel is down; it reconnects, and a catch-up is due. `heard`: when its socket last read a frame once
    /// connected, the last moment the channel was known to deliver (a socket that went down silently is noticed only
    /// later); `None` when it never connected (see [`PushEvent::Disconnected`]).
    Disconnected { down: Down, heard: Option<SystemTime> },
    /// The push channel stopped for good.
    PushStopped(Stop),
    /// A catch-up ran on its own (not one [`EngineRequests::sync`] asked for), and why.
    CaughtUp { cause: Cause, report: CatchUpReport },
    /// A catch-up that ran on its own failed; it is tried again.
    CatchUpFailed { cause: Cause, error: CatchUpError },
    /// A message came over the push channel and went through the store: `applied` says whether anything changed
    /// (a stored replay, or one the catch-up had already, changes nothing), `message` is the row as now stored. Or a
    /// read of the service stored it new or newer ([`Source::CatchUp`], [`Source::History`]): `change` is then
    /// [`MessageChange::New`] for a new one and [`MessageChange::Other`] for a newer one (a read doesn't say what
    /// changed), and `applied` is never [`Applied::Ignored`].
    Message {
        conversation: String,
        id: String,
        change: MessageChange,
        applied: Applied,
        message: Option<Box<Message>>,
        /// What brought it.
        source: Source,
    },
    /// Someone started (`typing`) or stopped typing; never stored.
    Typing {
        conversation: String,
        from: Option<String>,
        typing: bool,
    },
    /// The account's own read horizon in a conversation moved (stored).
    ReadHorizon { conversation: String },
    /// A conversation changed (its properties, its last message): a delivery said so, or a read of the service changed
    /// it in the store (added it, or its version, last activity, read horizon, roster or messages).
    ConversationChanged { conversation: String },
    /// A write's row was made, or an earlier one adopted by its explicit resend, and is about to go out: told before
    /// its first attempt, so a view shows it at once as sending ([`Event::Outbox`] then tells it as the attempt left
    /// it).
    Queued(OutboxEntry),
    /// An outbox row as it settled: after its first send, a later retry, or our own message coming back (by a
    /// catch-up, a delivery or any other sync).
    Outbox(OutboxEntry),
    /// A transient that waited out its `Retry-After`, as its send from the tick went; or one that waited and was
    /// dropped because a message post to its conversation went first.
    Transient(Transient, TransientSent),
    /// Someone's stored name or picture changed (a lookup in the background stored it): a view that shows them asks
    /// for them again ([`EngineRequests::person`]).
    Profile { mri: String },
    /// Someone's presence, as a read in the background now gave it ([`crate::presence`]): told only when it changed;
    /// [`Availability::Unknown`] too when they were dropped from the watch to make room for someone asked about later.
    Presence { mri: String, availability: Availability },
    /// The stored teams changed (a read in the background stored a list that differs): a view that shows them asks
    /// for them again ([`EngineRequests::teams`]).
    Teams,
    /// Something the engine did on its own failed: what, and why.
    Failed { during: &'static str, error: EngineError },
    /// The search backfill is over: every message the store held when the engine started (or that a run before left
    /// unindexed) is indexed; `messages` is how many this run indexed. Told once per run, unless it failed
    /// ([`Event::Failed`]; the next run goes on with it).
    Indexed { messages: usize },
    /// How a registration of the calling stack's endpoint went ([`crate::calling`]): invitations come only to a
    /// registered one, and a call is placed only once it is up.
    CallingRegistration(RegistrationReport),
    /// An invitation rings here (the call tracker offered it): answer it or decline it ([`CallAction::Answer`],
    /// [`CallAction::Decline`]); nothing does otherwise. One call at a time: none rings while a call is up.
    IncomingCall(IncomingCall),
    /// An offered invitation's ring ended before the account answered or declined it (as delivered: the caller
    /// cancelled, it was answered elsewhere, it timed out), or the engine let it go.
    RingEnded { call: CallId, end: RemoteEnd },
    /// A call of the account's moved on ([`CallStatus`]); every call ends with [`CallStatus::Ended`], once.
    Call { call: CallId, status: CallStatus },
    /// The account's call was taken by the callee's voicemail (a 1:1 call the callee declined or didn't answer, as
    /// captured: the call controller forwards it there); told once it is up, and it runs on until it is hung up.
    Voicemail { call: CallId },
    /// The camera of the account's call went off by itself (its device went away, or its line
    /// couldn't be offered again): nothing more is sent on it until the account turns it on again
    /// ([`CallAction::Camera`]).
    CameraOff { call: CallId },
    /// The microphone (`microphone`) or the speaker of the account's call went away for good (its stream ended, not
    /// moved to another device): the call goes on, nothing recorded or played on it.
    AudioClosed { call: CallId, microphone: bool },
    /// Who is in the account's call besides it, whose video each line shows, and who is speaking ([`CallView`]):
    /// told whenever that changed.
    CallParticipants { call: CallId, view: CallView },
    /// Ephemeral live captions for this viewer. Recognized speech is redacted in debug output.
    CallCaptions {
        call: CallId,
        captions: crate::CallCaptions,
    },
    /// Who waits in the lobby of the account's meeting now (empty once nobody does): each let in or turned away with
    /// [`CallAction::Lobby`]. Told whenever that changed.
    Lobby { call: CallId, waiting: Vec<LobbyPerson> },
}

/// What the engine's requests can fail with.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{why}")]
    Attachment { why: String },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Outbox(#[from] OutboxError),
    #[error(transparent)]
    CatchUp(#[from] CatchUpError),
    #[error("the teams can't be read: {0}")]
    Teams(#[from] TeamsReadError),
    #[error("no message {message} in conversation {conversation} in the store")]
    UnknownMessage { conversation: String, message: String },
    #[error("message {message} isn't the account's own, so it can't be {action}")]
    NotOwn { message: String, action: &'static str },
    #[error("message {message} is deleted")]
    Deleted { message: String },
    #[error("message {message}'s {missing} isn't known, so it can't be {action}")]
    Unknown {
        message: String,
        missing: &'static str,
        action: &'static str,
    },
    #[error("those members don't make a chat (none, the account itself, or one given twice)")]
    NotAChat,
    /// The account's own availability names its endpoint, and no registration of this run went through yet (or the
    /// push channel stopped): nothing can be sent.
    #[error("no endpoint of the account's is registered in this run, so its availability can't be set")]
    NoEndpoint,
    #[error("the engine's runtime can't be built: {0}")]
    Runtime(#[source] io::Error),
    #[error("the engine's thread can't be started: {0}")]
    Thread(#[source] io::Error),
    /// Another engine runs on the account, in this process or another: only one at a time ([`start`]).
    #[error("another engine runs on this account (its lock {} is held): only one runs at a time", lock.display())]
    AlreadyRunning { lock: PathBuf },
    #[error("the engine has stopped")]
    Stopped,
    /// A reaction's animation failed to be read on the way (not an answer that there is none): the next ask reads it
    /// again.
    #[error("the reaction's animation couldn't be read")]
    AnimationUnread,
    /// A message's picture failed to be read on the way (not an answer that there is none): the next ask reads it
    /// again.
    #[error("the message's picture couldn't be read")]
    PictureUnread,
    /// A shared file can't be read: its link names no SharePoint file, Graph gave no download link for it, or the
    /// account can't read it ([`crate::file_content`]).
    #[error("the shared file can't be read")]
    FileUnavailable,
    /// A part of a shared file failed to be read on the way (not an answer that it can't be had): the next ask reads
    /// it again.
    #[error("a part of the shared file couldn't be read")]
    FileUnread,
    /// The calendar couldn't be read, or its answer wasn't in the shape the web client reads ([`crate::calendar`]).
    #[error("the calendar couldn't be read")]
    CalendarUnread,
    /// A meeting's chat couldn't be read for the meeting it is ([`crate::calendar`]).
    #[error("the meeting couldn't be read")]
    MeetingUnread,
    /// An event's creation, change, cancellation or delete wasn't saved ([`crate::calendar`]): `why` says what
    /// happened (refused, the service's answer, no answer).
    #[error("the event's {kind} wasn't saved: {why}")]
    EventNotSaved { kind: &'static str, why: String },
    /// A team's channel wasn't made ([`crate::channels`]): `why` is Teams' reason for it.
    #[error("the channel wasn't made: {why:?}")]
    ChannelNotCreated { why: ChannelProblem },
    /// A team's write didn't go or was refused ([`crate::team_admin`]): `code` is what Teams called it, when it did.
    #[error("the team wasn't changed: {code:?}")]
    TeamNotChanged { code: Option<String> },
    /// The service's search couldn't be read, or its answer wasn't in a shape the app reads
    /// ([`crate::service_search`]).
    #[error("the search couldn't be read")]
    SearchFailed,
    /// A call action was refused: nothing was sent.
    #[error(transparent)]
    Call(CallActionError),
}

impl EngineError {
    /// The error's kind, as the log names a failed reply: no id, no text.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Attachment { .. } => "attachment",
            Self::Store(_) => "store",
            Self::Outbox(_) => "outbox",
            Self::CatchUp(_) => "catch-up",
            Self::Teams(_) => "teams",
            Self::UnknownMessage { .. } => "unknown message",
            Self::NotOwn { .. } => "not the account's own",
            Self::Deleted { .. } => "deleted",
            Self::Unknown { .. } => "unknown field",
            Self::NotAChat => "not a chat",
            Self::NoEndpoint => "no endpoint",
            Self::Runtime(_) => "runtime",
            Self::Thread(_) => "thread",
            Self::AlreadyRunning { .. } => "already running",
            Self::Stopped => "stopped",
            Self::AnimationUnread => "animation unread",
            Self::PictureUnread => "picture unread",
            Self::FileUnavailable => "file unavailable",
            Self::FileUnread => "file unread",
            Self::CalendarUnread => "calendar unread",
            Self::MeetingUnread => "meeting unread",
            Self::EventNotSaved { .. } => "event not saved",
            Self::ChannelNotCreated { .. } => "channel not created",
            Self::TeamNotChanged { .. } => "team not changed",
            Self::SearchFailed => "search failed",
            Self::Call(_) => "call",
        }
    }
}

/// The answer to one request, once the engine has made it: await it on any executor, or block on it outside one.
#[derive(Debug)]
#[must_use = "the request is made whether or not its reply is taken"]
pub struct Reply<T>(oneshot::Receiver<Result<T, EngineError>>);

impl<T> Reply<T> {
    /// A reply already answered with `answer`: for a stand-in of the engine (a UI test's fake core) to hand back.
    pub fn ready(answer: Result<T, EngineError>) -> Self {
        let (reply, taken) = oneshot::channel();
        // the receiver is held right here, so the answer is kept
        let _ = reply.send(answer);
        Self(taken)
    }

    /// A reply answered later, through the [`Answer`] that comes with it: for a stand-in of the engine (a UI test's
    /// fake core) to hold an answer back.
    pub fn later() -> (Answer<T>, Self) {
        let (reply, taken) = oneshot::channel();
        (Answer(reply), Self(taken))
    }

    /// Waits for the answer on a thread that runs no async executor (it panics inside one, as tokio's does).
    ///
    /// # Errors
    ///
    /// The request's own error, or [`EngineError::Stopped`] when the engine stopped before it answered.
    pub fn blocking_wait(self) -> Result<T, EngineError> {
        self.0.blocking_recv().unwrap_or(Err(EngineError::Stopped))
    }
}

impl<T> Future for Reply<T> {
    type Output = Result<T, EngineError>;

    /// A failed answer is told to the log by its kind ([`EngineError::kind`]): how a view's failed request shows there.
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().0).poll(cx).map(|answer| {
            let answer = answer.unwrap_or(Err(EngineError::Stopped));
            if let Err(error) = &answer {
                let level = if matches!(error, EngineError::Stopped) {
                    log::Level::Debug
                } else {
                    log::Level::Warn
                };
                log::log!(level, "a request's reply failed: {}", error.kind());
            }
            answer
        })
    }
}

pub(crate) type Responder<T> = oneshot::Sender<Result<T, EngineError>>;

/// Where a [`Reply::later`] is answered; dropped unanswered, the reply is [`EngineError::Stopped`].
#[derive(Debug)]
pub struct Answer<T>(Responder<T>);

impl<T> Answer<T> {
    /// Answers the reply (nobody hears it once the reply is dropped).
    pub fn send(self, answer: Result<T, EngineError>) {
        let _ = self.0.send(answer);
    }
}

/// A request to the engine's thread, with where its answer goes.
enum Request {
    UploadFile(PathBuf, Responder<teams_proto::UploadedFile>),
    Write(Action, Responder<OutboxEntry>),
    /// A read receipt already queued while the engine waited on the service.
    SendQueued(i64, Responder<OutboxEntry>),
    Resend(i64, Responder<OutboxEntry>),
    Transient(Transient, Responder<TransientSent>),
    OwnAvailability(OwnStatus, Responder<TransientSent>),
    Sync(Responder<CatchUpReport>),
    LoadOlder(String, Responder<Option<Synced>>),
    Call(CallAction, Responder<CallId>),
    SyncConversation(String, Responder<Synced>),
    Person(String, Responder<Person>),
    Presence(String, Responder<Availability>),
    ReactionAnimation(String, Responder<Option<Strip>>),
    MessagePicture(String, Responder<Option<PictureBytes>>),
    FileThumbnail(String, Responder<Option<PictureBytes>>),
    FileRange {
        share_url: String,
        offset: u64,
        len: u32,
        reply: Responder<FileRange>,
    },
    FileStream {
        share_url: String,
        offset: u64,
        len: u64,
        reply: Responder<FileStream>,
    },
    FileSize(String, Responder<u64>),
    Teams(Responder<Option<Vec<Team>>>),
    ChatListing(Responder<ChatListing>),
    Calendar {
        start_ms: i64,
        end_ms: i64,
        reply: Responder<Vec<CalendarEvent>>,
    },
    CalendarEvent {
        id: String,
        reply: Responder<FullEvent>,
    },
    ChannelEvent {
        mailbox: String,
        ical_uid: String,
        reply: Responder<FullEvent>,
    },
    TitleMeetNow {
        channel: String,
        title: String,
        reply: Responder<()>,
    },
    Meeting {
        chat: String,
        reply: Responder<Option<MeetingDetails>>,
    },
    ChannelMeetup {
        channel: String,
        reply: Responder<Option<ChannelMeetup>>,
    },
    WriteEvent {
        // boxed: an event's draft and the event as read are far larger than any other request
        write: Box<EventWrite>,
        reply: Responder<EventWritten>,
    },
    Invitable {
        query: String,
        reply: Responder<Vec<Invitable>>,
    },
    CreateChannel {
        // boxed: far larger than any other request
        draft: Box<ChannelDraft>,
        reply: Responder<ChannelMade>,
    },
    AddChannelMembers {
        team_group: String,
        channel_group: String,
        members: Vec<String>,
        reply: Responder<Vec<String>>,
    },
    ChannelThread {
        team: String,
        team_group: String,
        channel_group: String,
        name: String,
        reply: Responder<Option<String>>,
    },
    TeamPeople {
        team: String,
        keyword: String,
        reply: Responder<Vec<TeamPerson>>,
    },
    /// What the team pages ask ([`crate::team_admin`]).
    Team(Box<TeamAsk>),
    /// What the Apps tab, Change picture, a channel's analytics and "More create team options" ask ([`crate::team_apps`]).
    TeamApps(Box<TeamAppsAsk>),
    Workflow {
        request: Box<teams_proto::WorkflowRequest>,
        reply: Responder<teams_proto::WorkflowResponse>,
    },
    MeetingAvailability {
        attendees: Vec<EventAttendee>,
        start_ms: i64,
        meeting_minutes: u32,
        reply: Responder<MeetingAvailability>,
    },
    SearchService {
        query: String,
        scope: SearchScope,
        reply: Responder<SearchAnswer>,
    },
    Query(Query),
    Shutdown,
}

/// A request answered from the store alone.
enum Query {
    Conversations(usize, Responder<Vec<Conversation>>),
    Messages {
        conversation: String,
        before: Option<PageCursor>,
        limit: usize,
        reply: Responder<MessagePage>,
    },
    Message {
        conversation: String,
        id: String,
        reply: Responder<Option<Message>>,
    },
    Reactions {
        conversation: String,
        id: String,
        reply: Responder<Vec<Reaction>>,
    },
    Members(String, Responder<Vec<Member>>),
    RecentReactions(usize, Responder<Vec<String>>),
    FrequentReactions(usize, Responder<Vec<String>>),
    UnreadCount(String, Responder<u64>),
    DisplayName(String, Responder<Option<String>>),
    ReadHorizon(String, Responder<Option<ReadHorizon>>),
    HistoryComplete(String, Responder<Option<bool>>),
    OutboxEntry(i64, Responder<Option<OutboxEntry>>),
    OutboxUnsettled(String, Responder<Vec<OutboxEntry>>),
    LeftByEarlierRuns(Responder<Vec<OutboxEntry>>),
    ReplyRoots(String, Responder<Vec<(String, String)>>),
    /// The stored calendar events between two instants (ms): [`crate::calendar::stored`].
    CalendarEvents {
        start_ms: i64,
        end_ms: i64,
        reply: Responder<Vec<CalendarEvent>>,
    },
    Search {
        query: String,
        /// The conversations to look in; every one when `None`.
        within: Option<Vec<String>>,
        limit: usize,
        reply: Responder<Vec<SearchHit>>,
    },
}

/// What the account's own availability is set to ([`EngineRequests::own_availability`]), in the values a capture showed
/// (M2 Task 18).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OwnStatus {
    /// The account's endpoint is up and available (UPS's presence endpoint `PUT`): what makes it show online.
    Available,
    /// A manual status, as the account menu sets it, until it is cleared.
    Manual(ManualAvailability),
    /// The manual status cleared ("Reset status").
    ClearManual,
}

/// The engine's request side: writes and queries, each answered by a [`Reply`]. Cloneable and `Send`, so a UI's
/// views can share it; a request made after the engine stopped answers [`EngineError::Stopped`]. Holding one doesn't
/// keep the engine running: its [`EngineHandle`] owns it.
#[derive(Clone, Debug)]
pub struct EngineRequests {
    requests: mpsc::UnboundedSender<Request>,
    /// The account's MRI: who the engine writes as.
    me: Arc<str>,
    /// The engine sends nothing ([`EngineRequests::read_only`]).
    read_only: bool,
}

impl EngineRequests {
    /// Uploads a local file privately. Sharing waits for the message's Send action.
    pub fn upload_file(&self, path: PathBuf) -> Reply<teams_proto::UploadedFile> {
        self.ask(|reply| Request::UploadFile(path, reply))
    }
    /// The account's MRI: who the engine writes as, and whose read horizon and unread counts it answers with.
    #[must_use]
    pub fn me(&self) -> &str {
        &self.me
    }

    /// Whether the engine sends nothing (its outbox is read-only: the personal build's `ngteams --read-only`): every
    /// write it is asked for is refused below the views and logged to the diagnostic log as
    /// `refused_read_only` ([`Outbox::is_read_only`]). Fixed when the engine starts.
    #[must_use]
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// A call action ([`crate::calling`]); the reply is the call it acted on, once the action went (a start, a join or an
    /// answer once its media was set up), or why it was refused (nothing was sent).
    pub fn call(&self, action: CallAction) -> Reply<CallId> {
        self.ask(|reply| Request::Call(action, reply))
    }

    /// Writes through the outbox (see the module docs); the reply is the row as its first send left it.
    /// A read receipt advances the shared local horizon when queued, before the service answers; only the exact
    /// stored target is read, and the service reconciles it by the horizon timestamp as usual.
    pub fn write(&self, action: Action) -> Reply<OutboxEntry> {
        self.ask(|reply| Request::Write(action, reply))
    }

    /// Sends an outbox row again, by its id: the explicit resend ([`Store::resend`]) of a row an earlier run left
    /// pending or in flight, or of one that was denied or failed, of whichever run; only a pending or sending row of a
    /// run whose store is still open (this one included) is refused, and a sent one. The row keeps its
    /// `clientmessageid` and its budget starts over. A message post that had an attempt is first looked for (its
    /// earlier attempt may have arrived); any other kind (an edit, a delete, a reaction, a read receipt, a chat's
    /// creation) can't be looked for, so it goes out again unchecked, and the resend is the caller's decision to risk
    /// applying it twice. The reply is the row as its send left it, as [`EngineRequests::write`]'s.
    pub fn resend(&self, id: i64) -> Reply<OutboxEntry> {
        self.ask(|reply| Request::Resend(id, reply))
    }

    /// Sends a transient write (typing) through the outbox, as no row ([`Transient`]); the reply is how its send went.
    /// A typing in a conversation the store doesn't know is refused, and an availability naming an endpoint other than
    /// the one the push channel registered and keeps ([`EngineError::NoEndpoint`], as
    /// [`EngineRequests::own_availability`]).
    pub fn transient(&self, transient: Transient) -> Reply<TransientSent> {
        self.ask(|reply| Request::Transient(transient, reply))
    }

    /// Sets the account's own availability (M2 Task 19): a transient write through the outbox ([`Transient`]:
    /// [`Transient::EndpointAvailable`] or [`Transient::ManualStatus`]), naming the endpoint the push channel
    /// registered in this run; the reply is how its send went.
    /// [`EngineError::NoEndpoint`] before a registration of this run went through, or once the push channel stopped.
    pub fn own_availability(&self, status: OwnStatus) -> Reply<TransientSent> {
        self.ask(|reply| Request::OwnAvailability(status, reply))
    }

    /// Catches up now, whatever [`Gaps`] says.
    pub fn sync(&self) -> Reply<CatchUpReport> {
        self.ask(Request::Sync)
    }

    /// Reads the next older page of a conversation from the service ([`CatchUp::load_older`]).
    pub fn load_older(&self, conversation: impl Into<String>) -> Reply<Option<Synced>> {
        let conversation = conversation.into();
        self.ask(|reply| Request::LoadOlder(conversation, reply))
    }

    /// Syncs one conversation's messages from the service now ([`CatchUp::sync_messages`]: its newest page the first
    /// time, what changed since afterwards; one the store doesn't know is read as a thread first). What it stored comes
    /// as [`Event::Message`] ([`Source::CatchUp`]). For a channel other than a team's general one, which no
    /// conversation list (and so no catch-up) brings, this is the only way its posts are read.
    pub fn sync_conversation(&self, conversation: impl Into<String>) -> Reply<Synced> {
        let conversation = conversation.into();
        self.ask(|reply| Request::SyncConversation(conversation, reply))
    }

    /// The account's teams with their channels, from the store; `None` before a read of them settled. A read from
    /// CSA starts in the background when the stored list is missing or old, and a change it stores comes as
    /// [`Event::Teams`] ([`crate::teams`]).
    pub fn teams(&self) -> Reply<Option<Vec<Team>>> {
        self.ask(Request::Teams)
    }

    /// What the chat list shows besides the stored conversations ([`ChatListing`]): the favourites (chats and channels)
    /// as CSA's teams read last listed them, and the conversations the chat service calls empty. A read of the teams
    /// starts as [`Self::teams`] starts it, and a change of the favourites comes as [`Event::Teams`].
    pub fn chat_listing(&self) -> Reply<ChatListing> {
        self.ask(Request::ChatListing)
    }

    /// The post each stored message of a channel belongs to, as (message id, post id) in no set order: a post's is
    /// its own id; a message stored without one recorded (a chat's, or a channel's read before posts were kept) has
    /// none.
    pub fn reply_roots(&self, conversation: impl Into<String>) -> Reply<Vec<(String, String)>> {
        let conversation = conversation.into();
        self.query(|reply| Query::ReplyRoots(conversation, reply))
    }

    /// Up to `limit` stored conversations, the latest activity first.
    pub fn conversations(&self, limit: usize) -> Reply<Vec<Conversation>> {
        self.query(|reply| Query::Conversations(limit, reply))
    }

    /// Up to `limit` stored messages of a conversation older than `before` (from the newest when none), the newest
    /// first.
    pub fn messages(
        &self,
        conversation: impl Into<String>,
        before: Option<PageCursor>,
        limit: usize,
    ) -> Reply<MessagePage> {
        let conversation = conversation.into();
        self.query(|reply| Query::Messages {
            conversation,
            before,
            limit,
            reply,
        })
    }

    /// One stored message.
    pub fn message(&self, conversation: impl Into<String>, id: impl Into<String>) -> Reply<Option<Message>> {
        let (conversation, id) = (conversation.into(), id.into());
        self.query(|reply| Query::Message {
            conversation,
            id,
            reply,
        })
    }

    /// A stored message's reactions, one per user and key.
    pub fn reactions(&self, conversation: impl Into<String>, id: impl Into<String>) -> Reply<Vec<Reaction>> {
        let (conversation, id) = (conversation.into(), id.into());
        self.query(|reply| Query::Reactions {
            conversation,
            id,
            reply,
        })
    }

    /// The reaction keys the account set most recently on stored messages (and hasn't taken back), the newest first;
    /// at most `limit`.
    pub fn recent_reactions(&self, limit: usize) -> Reply<Vec<String>> {
        self.query(|reply| Query::RecentReactions(limit, reply))
    }

    /// The reaction keys the account uses most on stored messages (set and not taken back), the most used first; at
    /// most `limit`.
    pub fn frequent_reactions(&self, limit: usize) -> Reply<Vec<String>> {
        self.query(|reply| Query::FrequentReactions(limit, reply))
    }

    /// A chat's stored members.
    pub fn members(&self, conversation: impl Into<String>) -> Reply<Vec<Member>> {
        let conversation = conversation.into();
        self.query(|reply| Query::Members(conversation, reply))
    }

    /// How many stored messages of a conversation the account hasn't read ([`Store::unread_count`]).
    pub fn unread_count(&self, conversation: impl Into<String>) -> Reply<u64> {
        let conversation = conversation.into();
        self.query(|reply| Query::UnreadCount(conversation, reply))
    }

    /// The name `mri`'s newest stored message carried, in any conversation ([`Store::display_name`]).
    pub fn display_name(&self, mri: impl Into<String>) -> Reply<Option<String>> {
        let mri = mri.into();
        self.query(|reply| Query::DisplayName(mri, reply))
    }

    /// Someone as the views show them: their name (their profile's, else their newest message's, else one the binary
    /// knew) and their picture, from the store; when their stored profile is missing or old, a lookup starts in the
    /// background and its change comes as [`Event::Profile`] ([`crate::profiles`]).
    pub fn person(&self, mri: impl Into<String>) -> Reply<Person> {
        let mri = mri.into();
        self.ask(|reply| Request::Person(mri, reply))
    }

    /// Someone's presence as last read ([`Availability::Unknown`] before the first read); from now on they are watched,
    /// and each change of their presence comes as [`Event::Presence`] ([`crate::presence`]: pushed by UPS once
    /// subscribed, and read every [`EngineOptions::presence_poll`] or [`EngineOptions::presence_poll_subscribed`]). At
    /// most
    /// [`MAX_WATCHED`](crate::MAX_WATCHED) people are watched: past it the one asked about longest ago is dropped and
    /// told as [`Availability::Unknown`], so a view asks again about the people it shows whenever it shows them anew.
    pub fn presence(&self, mri: impl Into<String>) -> Reply<Availability> {
        let mri = mri.into();
        self.ask(|reply| Request::Presence(mri, reply))
    }

    /// The animation of the reaction `id` (its key without a skin tone), as Teams' CDN has it ([`crate::emoticons`]):
    /// its strip of frames, or `None` when there is none.
    pub fn reaction_animation(&self, id: impl Into<String>) -> Reply<Option<Strip>> {
        let id = id.into();
        self.ask(|reply| Request::ReactionAnimation(id, reply))
    }

    /// The picture a message's `<img>` names by its `src` (a view of an AMS object, as the account's AMS base has it, or
    /// a GIF on Giphy's media host; [`crate::pictures`]): its bytes and format, or `None` when there is none or `src`
    /// names neither.
    pub fn message_picture(&self, src: impl Into<String>) -> Reply<Option<PictureBytes>> {
        let src = src.into();
        self.ask(|reply| Request::MessagePicture(src, reply))
    }

    /// The account's calendar events between `start_ms` and `end_ms` (ms since the epoch), read from the middle tier
    /// now and kept in the store ([`crate::calendar`]): [`EngineError::CalendarUnread`] when the read failed.
    pub fn calendar(&self, start_ms: i64, end_ms: i64) -> Reply<Vec<CalendarEvent>> {
        self.ask(|reply| Request::Calendar {
            start_ms,
            end_ms,
            reply,
        })
    }

    /// The account's calendar events between `start_ms` and `end_ms` (ms since the epoch) as the store kept them from
    /// the last reads ([`crate::calendar`]), by start: what the calendar shows at once while [`Self::calendar`] reads
    /// the window again. Answered meanwhile too, while a catch-up runs.
    pub fn stored_calendar(&self, start_ms: i64, end_ms: i64) -> Reply<Vec<CalendarEvent>> {
        self.query(|reply| Query::CalendarEvents {
            start_ms,
            end_ms,
            reply,
        })
    }

    /// The event `id` of the account's calendar, read whole from the middle tier now: what the scheduling form opens
    /// on ([`crate::calendar`]); [`EngineError::CalendarUnread`] when the read failed.
    pub fn calendar_event(&self, id: impl Into<String>) -> Reply<FullEvent> {
        let id = id.into();
        self.ask(|reply| Request::CalendarEvent { id, reply })
    }

    /// The meeting whose chat is `chat`, as its chat's thread names it (its organizer, tenant, title and join link),
    /// read now ([`crate::calendar`]): what a meeting is joined by when its event's link names no organizer, or from its
    /// chat. `None` when the thread names no meeting; [`EngineError::MeetingUnread`] when the read failed.
    pub fn meeting(&self, chat: impl Into<String>) -> Reply<Option<MeetingDetails>> {
        let chat = chat.into();
        self.ask(|reply| Request::Meeting { chat, reply })
    }

    /// The meeting that runs in the channel `channel` now (its post, its conversation, its organizer and tenant), read
    /// now from CSA's teams ([`crate::calendar`]): what the channel's Join shows and joins. `None` when none runs;
    /// [`EngineError::MeetingUnread`] when the read failed.
    pub fn channel_meetup(&self, channel: impl Into<String>) -> Reply<Option<ChannelMeetup>> {
        let channel = channel.into();
        self.ask(|reply| Request::ChannelMeetup { channel, reply })
    }

    /// A channel's meeting `ical_uid`, read whole now from the group calendar of its team's `mailbox`: what its details
    /// open on to change it ([`crate::calendar`]); [`EngineError::CalendarUnread`] when the read failed.
    pub fn channel_event(&self, mailbox: impl Into<String>, ical_uid: impl Into<String>) -> Reply<FullEvent> {
        let (mailbox, ical_uid) = (mailbox.into(), ical_uid.into());
        self.ask(|reply| Request::ChannelEvent {
            mailbox,
            ical_uid,
            reply,
        })
    }

    /// Titles the meeting the account's Meet now in the channel `channel` starts, as its pre-join's subject says, once
    /// the service posted it ([`crate::calendar`]); [`EngineError::MeetingUnread`] when no post of it came.
    pub fn title_meet_now(&self, channel: impl Into<String>, title: impl Into<String>) -> Reply<()> {
        let (channel, title) = (channel.into(), title.into());
        self.ask(|reply| Request::TitleMeetNow { channel, title, reply })
    }

    /// Creates, changes, cancels or deletes an event of the account's calendar through the outbox
    /// ([`crate::calendar`]); [`EngineError::EventNotSaved`] when it wasn't saved.
    pub fn write_event(&self, write: EventWrite) -> Reply<EventWritten> {
        self.ask(|reply| Request::WriteEvent {
            write: Box::new(write),
            reply,
        })
    }

    /// Makes a team's channel through the outbox ([`crate::channels`]): stored once made (a private
    /// channel without its thread, once [`Self::channel_thread`] has it); [`EngineError::ChannelNotCreated`] when it
    /// wasn't made.
    pub fn create_channel(&self, draft: ChannelDraft) -> Reply<ChannelMade> {
        self.ask(|reply| Request::CreateChannel {
            draft: Box::new(draft),
            reply,
        })
    }

    /// Adds `members` (MRIs) to a private channel, by its group and its team's: answered with those who weren't added
    /// ([`crate::channels`]).
    pub fn add_channel_members(
        &self,
        team_group: impl Into<String>,
        channel_group: impl Into<String>,
        members: Vec<String>,
    ) -> Reply<Vec<String>> {
        let (team_group, channel_group) = (team_group.into(), channel_group.into());
        self.ask(|reply| Request::AddChannelMembers {
            team_group,
            channel_group,
            members,
            reply,
        })
    }

    /// Waits for a private channel's thread: answered with it, once it is stored; `None` when it didn't come in time
    /// ([`crate::channels`]).
    pub fn channel_thread(
        &self,
        team: impl Into<String>,
        team_group: impl Into<String>,
        channel_group: impl Into<String>,
        name: impl Into<String>,
    ) -> Reply<Option<String>> {
        let (team, team_group, channel_group, name) =
            (team.into(), team_group.into(), channel_group.into(), name.into());
        self.ask(|reply| Request::ChannelThread {
            team,
            team_group,
            channel_group,
            name,
            reply,
        })
    }

    /// Sends a write managing a team through the outbox, and puts `change` in the stored teams once it
    /// went ([`crate::team_admin`]); [`EngineError::TeamNotChanged`] when it didn't.
    /// Reads channel management data freshly from its own service.
    /// Moves a channel or creates a section in the account's own Teams hierarchy.
    pub fn channel_section(&self, action: teams_proto::ChannelSectionAction) -> Reply<()> {
        self.ask(|reply| Request::Team(Box::new(TeamAsk::ChannelSection { action, reply })))
    }

    pub fn channel_data(&self, query: teams_proto::ChannelRead) -> Reply<teams_proto::ChannelData> {
        self.ask(|reply| Request::Team(Box::new(TeamAsk::ChannelRead { query, reply })))
    }

    pub fn team_write(&self, write: TeamWrite, change: Option<TeamChange>) -> Reply<TeamWritten> {
        self.ask(|reply| Request::Team(Box::new(TeamAsk::Write { write, change, reply })))
    }

    /// Changes the account's own view of a team (its channels hidden or shown), which reaches nobody, and puts `change`
    /// in the stored teams once it did ([`crate::team_admin`]).
    pub fn own_team_view(&self, requests: Vec<OwnTeamView>, change: Option<TeamChange>) -> Reply<()> {
        self.ask(|reply| {
            Request::Team(Box::new(TeamAsk::OwnView {
                requests,
                change,
                reply,
            }))
        })
    }

    /// A team's people, owners first, read now.
    pub fn team_roster(&self, team: impl Into<String>) -> Reply<Vec<TeamMember>> {
        let team = team.into();
        self.ask(|reply| Request::Team(Box::new(TeamAsk::Roster { team, reply })))
    }

    /// A team's settings as the service keeps them, read now.
    pub fn team_settings(&self, team: impl Into<String>) -> Reply<Option<String>> {
        let team = team.into();
        self.ask(|reply| Request::Team(Box::new(TeamAsk::Settings { team, reply })))
    }

    /// A team's tags and who may change them, read now.
    pub fn team_tags(&self, team: impl Into<String>) -> Reply<TagsRead> {
        let team = team.into();
        self.ask(|reply| Request::Team(Box::new(TeamAsk::Tags { team, reply })))
    }

    /// A team's code, when it has one, read now.
    pub fn team_code(&self, team: impl Into<String>) -> Reply<Option<String>> {
        let team = team.into();
        self.ask(|reply| Request::Team(Box::new(TeamAsk::Code { team, reply })))
    }

    /// A team's channels as Manage team lists them, read now.
    pub fn team_channels(&self, group: impl Into<String>) -> Reply<Vec<ManagedChannel>> {
        let group = group.into();
        self.ask(|reply| Request::Team(Box::new(TeamAsk::Channels { group, reply })))
    }

    /// The invitations waiting for the account, read now.
    pub fn pending_invites(&self) -> Reply<Vec<PendingInvite>> {
        self.ask(|reply| Request::Team(Box::new(TeamAsk::Invites { reply })))
    }

    /// How a team did over `period`, read now.
    pub fn team_analytics(&self, group: impl Into<String>, period: AnalyticsPeriod) -> Reply<Option<TeamAnalytics>> {
        let group = group.into();
        self.ask(|reply| Request::Team(Box::new(TeamAsk::Analytics { group, period, reply })))
    }

    /// How the account's teams did over `period`, read now.
    pub fn all_teams_analytics(&self, period: AnalyticsPeriod) -> Reply<Vec<TeamAnalytics>> {
        self.ask(|reply| Request::Team(Box::new(TeamAsk::AllAnalytics { period, reply })))
    }

    /// The teams "Join a team" suggests, or finds for `keywords`, read now.
    pub fn joinable_teams(&self, keywords: Option<String>) -> Reply<Vec<JoinableTeam>> {
        self.ask(|reply| Request::Team(Box::new(TeamAsk::Joinable { keywords, reply })))
    }

    /// The people `keyword` finds in the organization, to add to a team, read now.
    pub fn addable_people(&self, keyword: impl Into<String>) -> Reply<Vec<TeamPerson>> {
        let keyword = keyword.into();
        self.ask(|reply| Request::Team(Box::new(TeamAsk::Addable { keyword, reply })))
    }

    /// The people asking to join a team, read now.
    pub fn team_join_requests(&self, group: impl Into<String>) -> Reply<Vec<TeamMember>> {
        let group = group.into();
        self.ask(|reply| Request::Team(Box::new(TeamAsk::JoinRequests { group, reply })))
    }

    /// A team's picture, read now: the service makes one for a team that set none.
    pub fn team_picture(&self, team: TeamPicture) -> Reply<Option<PictureBytes>> {
        self.ask(|reply| Request::Team(Box::new(TeamAsk::Picture { team, reply })))
    }

    /// The apps installed on a team, by name, read now ([`crate::team_apps`]).
    pub fn team_apps(&self, team: impl Into<String>) -> Reply<Vec<InstalledApp>> {
        let team = team.into();
        self.ask(|reply| Request::TeamApps(Box::new(TeamAppsAsk::Apps { team, reply })))
    }

    /// The apps the store offers to add to a team, with their definitions, read now.
    pub fn app_store(&self) -> Reply<Vec<StoreApp>> {
        self.ask(|reply| Request::TeamApps(Box::new(TeamAppsAsk::Store { reply })))
    }

    /// How a team's channel did over `period`, read now.
    pub fn channel_analytics(
        &self,
        group: impl Into<String>,
        channel: impl Into<String>,
        period: AnalyticsPeriod,
    ) -> Reply<Option<TeamAnalytics>> {
        let (group, channel) = (group.into(), channel.into());
        self.ask(|reply| {
            Request::TeamApps(Box::new(TeamAppsAsk::ChannelAnalytics {
                group,
                channel,
                period,
                reply,
            }))
        })
    }

    /// Native Power Automate management, bound to this engine's account. Mutations pass through the outbox.
    pub fn workflow_request(&self, request: teams_proto::WorkflowRequest) -> Reply<teams_proto::WorkflowResponse> {
        self.ask(|reply| Request::Workflow {
            request: Box::new(request),
            reply,
        })
    }

    /// Teams' team templates, read now.
    pub fn team_templates(&self) -> Reply<Vec<TeamTemplateCard>> {
        self.ask(|reply| Request::TeamApps(Box::new(TeamAppsAsk::Templates { reply })))
    }

    /// One template's channels and apps, read now.
    pub fn team_template(&self, id: impl Into<String>) -> Reply<Option<TeamTemplateDetails>> {
        let id = id.into();
        self.ask(|reply| Request::TeamApps(Box::new(TeamAppsAsk::Template { id, reply })))
    }

    /// How many Microsoft 365 groups the account owns that a team could be made of, read now.
    pub fn owned_groups(&self) -> Reply<Option<usize>> {
        self.ask(|reply| Request::TeamApps(Box::new(TeamAppsAsk::OwnedGroups { reply })))
    }

    /// The picture Teams makes for a team named `name`, read now: what Change picture's Remove saves.
    pub fn default_team_picture(&self, name: impl Into<String>) -> Reply<Option<Vec<u8>>> {
        let name = name.into();
        self.ask(|reply| Request::TeamApps(Box::new(TeamAppsAsk::DefaultPicture { name, reply })))
    }

    /// The people on the team `team` that `keyword` finds, read now: what a private channel's member step suggests
    /// ([`crate::channels`]).
    pub fn team_people(&self, team: impl Into<String>, keyword: impl Into<String>) -> Reply<Vec<TeamPerson>> {
        let (team, keyword) = (team.into(), keyword.into());
        self.ask(|reply| Request::TeamPeople { team, keyword, reply })
    }

    /// Up to eight people the account knows whose name holds `query`, each with the address their profile gives
    /// (read now): what the scheduling form's attendee picker suggests ([`crate::calendar`]).
    pub fn invitable(&self, query: impl Into<String>) -> Reply<Vec<Invitable>> {
        let query = query.into();
        self.ask(|reply| Request::Invitable { query, reply })
    }

    /// The busy times of `attendees` and the account, and the starts all of them are free for `meeting_minutes`, from
    /// `start_ms` (read now: [`crate::calendar`]): the scheduling form's Scheduler; [`EngineError::CalendarUnread`]
    /// when neither read went through.
    pub fn availability(
        &self,
        attendees: Vec<EventAttendee>,
        start_ms: i64,
        meeting_minutes: u32,
    ) -> Reply<MeetingAvailability> {
        self.ask(|reply| Request::MeetingAvailability {
            attendees,
            start_ms,
            meeting_minutes,
            reply,
        })
    }

    /// What the service's search finds for `query` in `scope` (read now: [`crate::service_search`]): the title bar's
    /// suggestions as the user types, and its results once the query is entered; [`EngineError::SearchFailed`] when the
    /// read failed.
    pub fn search_service(&self, query: impl Into<String>, scope: SearchScope) -> Reply<SearchAnswer> {
        let query = query.into();
        self.ask(|reply| Request::SearchService { query, scope, reply })
    }

    /// The thumbnail of the file a message carries, by its sharing link (`teams_proto::chat::SharedFile::share_url`),
    /// as Graph and SharePoint's media host have it ([`crate::thumbnails`]): its bytes and format, or `None` when there
    /// is none or the link names no SharePoint file.
    pub fn file_thumbnail(&self, share_url: impl Into<String>) -> Reply<Option<PictureBytes>> {
        let share_url = share_url.into();
        self.ask(|reply| Request::FileThumbnail(share_url, reply))
    }

    /// `len` bytes (at most [`teams_proto::MAX_RANGE`]) of the file a message carries, from `offset`, by its sharing
    /// link (`teams_proto::chat::SharedFile::share_url`), as Graph's download link has them
    /// ([`crate::file_content`]): what a view plays a video by. [`EngineError::FileUnavailable`] when the file can't be
    /// had, [`EngineError::FileUnread`] when the read failed on the way.
    pub fn file_range(&self, share_url: impl Into<String>, offset: u64, len: u32) -> Reply<FileRange> {
        let share_url = share_url.into();
        self.ask(|reply| Request::FileRange {
            share_url,
            offset,
            len,
            reply,
        })
    }

    /// A stream of `len` bytes (at most [`teams_proto::MAX_STREAM`]) of the file a message carries, from `offset`, by
    /// its sharing link, as Graph's download link has them ([`crate::file_content`]): answered once the read's
    /// headers came, its bytes then handed on as they arrive. Dropping the reply or the stream stops the read: what a
    /// view plays a video by. [`EngineError::FileUnavailable`] when the file can't be had, [`EngineError::FileUnread`]
    /// when the read failed on the way.
    pub fn file_stream(&self, share_url: impl Into<String>, offset: u64, len: u64) -> Reply<FileStream> {
        let share_url = share_url.into();
        self.ask(|reply| Request::FileStream {
            share_url,
            offset,
            len,
            reply,
        })
    }

    /// The size of the file a message carries, by its sharing link, as its item gives it ([`crate::file_content`]);
    /// its download link is kept for the reads that follow, so asking early spares the first read that wait.
    pub fn file_size(&self, share_url: impl Into<String>) -> Reply<u64> {
        let share_url = share_url.into();
        self.ask(|reply| Request::FileSize(share_url, reply))
    }

    /// The account's own stored read horizon in a conversation.
    pub fn read_horizon(&self, conversation: impl Into<String>) -> Reply<Option<ReadHorizon>> {
        let conversation = conversation.into();
        self.query(|reply| Query::ReadHorizon(conversation, reply))
    }

    /// Whether the store holds a conversation's whole history (its kept sync state says so); `None` when the
    /// conversation was never synced, so [`Self::load_older`]'s `None` doesn't yet mean nothing is older.
    pub fn history_complete(&self, conversation: impl Into<String>) -> Reply<Option<bool>> {
        let conversation = conversation.into();
        self.query(|reply| Query::HistoryComplete(conversation, reply))
    }

    /// An outbox row as it now stands, by its id (a write's reply has it).
    pub fn outbox_entry(&self, id: i64) -> Reply<Option<OutboxEntry>> {
        self.query(|reply| Query::OutboxEntry(id, reply))
    }

    /// A conversation's outbox rows that aren't sent, of any run, oldest first ([`Store::outbox_unsettled`]): what its
    /// view shows beside its messages.
    pub fn outbox_unsettled(&self, conversation: impl Into<String>) -> Reply<Vec<OutboxEntry>> {
        let conversation = conversation.into();
        self.query(|reply| Query::OutboxUnsettled(conversation, reply))
    }

    /// The rows runs that have ended left pending or in flight ([`Store::outbox_left_by_earlier_runs`]): they go out
    /// only by an explicit [`EngineRequests::resend`].
    pub fn left_by_earlier_runs(&self) -> Reply<Vec<OutboxEntry>> {
        self.query(Query::LeftByEarlierRuns)
    }

    /// Up to `limit` stored messages whose text holds every term of `query` (the user's input, each term a prefix),
    /// the newest first ([`Store::search`]): the account's own store, never the service's search. A message stored
    /// before the index existed is found once the backfill reached it ([`Event::Indexed`]).
    pub fn search(&self, query: impl Into<String>, limit: usize) -> Reply<Vec<SearchHit>> {
        let query = query.into();
        self.query(|reply| Query::Search {
            query,
            within: None,
            limit,
            reply,
        })
    }

    /// As [`EngineRequests::search`], in the conversations of `within` only ([`Store::search_in`]): the others'
    /// messages are left out before the hits are counted.
    pub fn search_in(&self, query: impl Into<String>, within: Vec<String>, limit: usize) -> Reply<Vec<SearchHit>> {
        let query = query.into();
        self.query(|reply| Query::Search {
            query,
            within: Some(within),
            limit,
            reply,
        })
    }

    fn query<T>(&self, query: impl FnOnce(Responder<T>) -> Query) -> Reply<T> {
        self.ask(|reply| Request::Query(query(reply)))
    }

    fn ask<T>(&self, request: impl FnOnce(Responder<T>) -> Request) -> Reply<T> {
        let (reply, answer) = oneshot::channel();
        // a stopped engine drops the request with its responder, and the reply is `Stopped`
        let _ = self.requests.send(request(reply));
        Reply(answer)
    }
}

/// The running engine, owned: its request side (also reached through `Deref`, and shared by
/// [`EngineHandle::requests`]), its events and its thread. Dropping it stops the engine (on its own thread, which
/// isn't waited for) whatever request handles are left; [`EngineHandle::shutdown`] waits.
#[derive(Debug)]
pub struct EngineHandle {
    requests: EngineRequests,
    events: mpsc::UnboundedReceiver<Event>,
    thread: Option<JoinHandle<()>>,
    /// Disconnected once the engine's thread has ended: nothing is ever sent on it.
    ended: std::sync::mpsc::Receiver<()>,
}

impl Deref for EngineHandle {
    type Target = EngineRequests;

    fn deref(&self) -> &EngineRequests {
        &self.requests
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        // a request handle left elsewhere keeps the channel open: the engine is told to stop
        let _ = self.requests.requests.send(Request::Shutdown);
    }
}

impl EngineHandle {
    /// A request handle of the engine's own, for another owner (a view) to keep.
    #[must_use]
    pub fn requests(&self) -> EngineRequests {
        self.requests.clone()
    }

    /// The next event; `None` once the engine has stopped and every event was taken.
    pub async fn next_event(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    /// The next event if one is waiting.
    pub fn try_next_event(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// The next event, waiting on a thread that runs no async executor (it panics inside one, as tokio's does).
    pub fn blocking_next_event(&mut self) -> Option<Event> {
        self.events.blocking_recv()
    }

    /// Stops the engine (its push channel sends its disconnect) and waits for its thread to end: call it outside an
    /// async executor, or where blocking the thread is fine.
    pub fn shutdown(mut self) {
        let _ = self.requests.requests.send(Request::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// As [`EngineHandle::shutdown`], but when the engine's thread hasn't ended within `grace`, calls `overdue` once
    /// (to end what holds the engine up: a secret store waiting on a prompt nobody answers) and then waits for it.
    pub fn shutdown_within(mut self, grace: Duration, overdue: impl FnOnce()) {
        let _ = self.requests.requests.send(Request::Shutdown);
        if let Some(thread) = self.thread.take() {
            if let Err(RecvTimeoutError::Timeout) = self.ended.recv_timeout(grace) {
                overdue();
            }
            let _ = thread.join();
        }
    }
}

/// Starts an engine on a thread of its own, with its own tokio runtime, once it holds the account's engine lock
/// ([`Store::engine_lock`]); it runs until its handle is shut down or dropped, and the lock goes when its thread ends.
///
/// # Errors
///
/// [`EngineError::AlreadyRunning`] while another engine holds the account's lock (nothing waits for it, and it is
/// never broken), [`EngineError::Store`] when the lock can't be taken, [`EngineError::Runtime`] when the runtime can't
/// be built, [`EngineError::Thread`] when the thread can't start.
pub fn start<C, T>(parts: Parts<C, T>) -> Result<EngineHandle, EngineError>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    let lock = parts.store.engine_lock().map_err(|error| match error {
        StoreError::EngineRunning { path } => EngineError::AlreadyRunning { lock: path },
        other => EngineError::Store(other),
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(EngineError::Runtime)?;
    let me: Arc<str> = parts.outbox.me().mri().as_str().into();
    let read_only = parts.outbox.is_read_only();
    let (requests, received) = mpsc::unbounded_channel();
    let (events, taken) = mpsc::unbounded_channel();
    let (ending, ended) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("teams-core".to_owned())
        .spawn(move || {
            // a `LocalSet`, so the calling runs as a task of its own on this thread (`crate::calling`)
            let local = tokio::task::LocalSet::new();
            local.block_on(&runtime, Core::new(parts, events, received).run());
            // its tasks (the push channel's) and the store are gone before another engine may start
            drop(local);
            drop(runtime);
            drop(lock);
            drop(ending);
        })
        .map_err(EngineError::Thread)?;
    Ok(EngineHandle {
        requests: EngineRequests {
            requests,
            me,
            read_only,
        },
        events: taken,
        thread: Some(thread),
        ended,
    })
}

/// The engine's state on its thread.
struct Core<C, T> {
    store: Store,
    /// The requests, which the catch-up's reads answer the queries of while they wait ([`Inbox`]).
    inbox: Arc<Inbox>,
    outbox: Arc<Outbox<T>>,
    catch_up: CatchUp<C, T>,
    profiles: Profiles<C>,
    teams: Teams<C>,
    presences: Presences<C>,
    animations: Animations<C>,
    pictures: Pictures<C>,
    thumbnails: Thumbnails<C>,
    files: FileContents<C>,
    uploads: crate::attachments::Uploads<C, T>,
    calendar: Calendar<C, T>,
    channels: Channels<C, T>,
    service_search: ServiceSearch<C>,
    events: mpsc::UnboundedSender<Event>,
    options: EngineOptions,
    push: Option<Push>,
    gaps: Gaps,
    /// The push channel is connected.
    up: bool,
    /// The endpoint the push channel's last registration that went through registered ([`PushEvent::Registered`]),
    /// while the channel runs: the one the account's own availability names.
    endpoint: Option<RegisteredEndpoint>,
    /// When a catch-up is tried again after one that failed, or after something a delivery brought was missed.
    retry_at: Option<Instant>,
    /// A read of the teams changed the favourites: the account's own notes may be due a sync
    /// ([`Core::sync_favourite_notes`]).
    notes_due: bool,
    /// The conversations the reads and deliveries brought since the last tick: a team's thread the stored teams don't
    /// name reads the teams again ([`Teams::note_conversation`](crate::teams)).
    noticed: std::cell::RefCell<Vec<String>>,
    /// When the next poll's catch-up is due, while there is no push channel and the engine polls
    /// ([`EngineOptions::poll`]).
    poll_at: Option<Instant>,
    /// Message-loss reports waiting for a catch-up that runs clean.
    losses: Vec<MessageLoss>,
    /// The search backfill of the messages stored before ([`crate::search`]), until it is over or failed.
    backfill: Option<Backfill>,
    /// The account's calling, when the engine makes calls, until the engine runs it as a task of its own.
    calling: Option<Calling<C, T>>,
    /// The account's calling, running.
    calls: Option<CallingTask>,
}

impl<C, T> Core<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    fn new(
        parts: Parts<C, T>,
        events: mpsc::UnboundedSender<Event>,
        requests: mpsc::UnboundedReceiver<Request>,
    ) -> Self {
        let outbox = Arc::new(parts.outbox);
        let me = outbox.me().mri().clone();
        let known = parts.known_names.iter().cloned().collect();
        let profiles = Profiles::new(
            Arc::clone(&parts.client),
            parts.known_names,
            parts.options.profile_retry,
        );
        let teams = Teams::new(Arc::clone(&parts.client), parts.options.teams_retry);
        let polls = (parts.options.presence_poll, parts.options.presence_poll_subscribed);
        let presences = Presences::new(Arc::clone(&parts.client), polls);
        let animations = Animations::new(Arc::clone(&parts.client));
        // the pictures cached across runs, beside the store, opened when first used: only this engine opens it, under
        // the account's lock
        let disk = Disk::beside(parts.store.path());
        let pictures = Pictures::new(Arc::clone(&parts.client), disk.clone());
        let thumbnails = Thumbnails::new(Arc::clone(&parts.client), disk);
        let files = FileContents::new(Arc::clone(&parts.client));
        let uploads = crate::attachments::Uploads::new(Arc::clone(&parts.client), Arc::clone(&outbox));
        let calendar = Calendar::new(Arc::clone(&parts.client), Arc::clone(&outbox));
        let channels = Channels::new(Arc::clone(&parts.client), Arc::clone(&outbox));
        let service_search = ServiceSearch::new(Arc::clone(&parts.client));
        let calling = parts
            .calls
            .map(|calls| Calling::new(calls, Arc::clone(&outbox), Arc::clone(&parts.client), events.clone()));
        let inbox = Arc::new(Inbox::new(
            me.as_str().to_owned(),
            known,
            requests,
            events.clone(),
            outbox.is_read_only(),
        ));
        let catch_up = CatchUp::new(parts.client, Arc::clone(&outbox), me).with_meanwhile(inbox.clone());
        Self {
            store: parts.store,
            inbox,
            outbox,
            catch_up,
            profiles,
            teams,
            presences,
            animations,
            pictures,
            thumbnails,
            files,
            uploads,
            calendar,
            channels,
            service_search,
            events,
            options: parts.options,
            push: parts.push,
            gaps: Gaps::new(),
            up: false,
            endpoint: None,
            retry_at: None,
            poll_at: None,
            notes_due: false,
            noticed: std::cell::RefCell::default(),
            losses: Vec::new(),
            backfill: Some(Backfill::new(parts.options.index_batch)),
            calling,
            calls: None,
        }
    }

    async fn run(mut self) {
        let mut push = self.push.take().map(Push::spawn);
        if push.is_none() && self.options.poll.is_some() {
            self.poll_at = Some(Instant::now());
        }
        self.calls = self.calling.take().map(Calling::spawn);
        let mut tick = tokio::time::interval(self.options.tick);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                request = self.inbox.next() => match request {
                    Some(Request::Shutdown) | None => break,
                    Some(request) => self.on_request(request).await,
                },
                event = next_push(push.as_mut()) => match event {
                    Some(event) => self.on_push(event).await,
                    None => push = None,
                },
                _ = tick.tick() => self.on_tick().await,
                done = background(
                    &mut self.profiles,
                    &mut self.teams,
                    &mut self.presences,
                    &mut self.channels,
                    &mut self.calendar,
                ),
                    if self.profiles.busy()
                        || self.teams.busy()
                        || self.presences.busy()
                        || self.channels.busy()
                        || self.calendar.busy() => {
                    self.on_background(done);
                }
                // ready once the runtime's other tasks had their turn (the push channel's, on this current-thread
                // runtime: an always-ready branch would never let them run until the backfill is over), and
                // `select!` picks among the ready branches at random, so a request or an event waits at most for one
                // batch
                () = tokio::task::yield_now(), if self.backfill.is_some() => self.index_step(),
            }
            // whatever a read settled on its way (a sync after a write, a new conversation's sync, …)
            self.arrived(&[]);
        }
        self.stop(push).await;
    }

    /// The engine stops: the call is hung up first ([`Calling::shut_down`]), then the push channel sends its
    /// disconnect.
    async fn stop(&mut self, push: Option<TrouterHandle>) {
        if let Some(calls) = self.calls.take() {
            calls.shut_down().await;
        }
        if let Some(push) = push {
            push.stop().await;
        }
    }

    /// One batch of the search backfill; once it is over, told as [`Event::Indexed`]. One that fails is told and
    /// given up for this run (the next run goes on where it stopped), so a store that keeps failing isn't tried in a
    /// loop.
    fn index_step(&mut self) {
        let Some(backfill) = self.backfill.as_mut() else {
            return;
        };
        match backfill.step(&mut self.store) {
            Ok(Step::Done(_)) => {
                let messages = backfill.indexed();
                self.backfill = None;
                self.emit(Event::Indexed { messages });
            }
            Ok(Step::Indexed(_)) => {}
            Err(error) => {
                self.backfill = None;
                self.failed("indexing stored messages for search", error);
            }
        }
    }

    fn emit(&self, event: Event) {
        emit_to(&self.events, event);
    }

    /// Tells the owner what the reads since the last look stored ([`Core::brought`], as [`Source::CatchUp`]), then
    /// every row a read changed ([`Outbox::take_arrived`]): our own message coming back, or an entry it couldn't read
    /// carrying a row's `clientmessageid` ([`Outbox::unreadable_seen`]); apart from one already told as it now stands
    /// (in `told`).
    fn arrived(&self, told: &[OutboxEntry]) {
        self.brought(Source::CatchUp);
        for row in self.outbox.take_arrived() {
            if !told.contains(&row) {
                self.emit(Event::Outbox(row));
            }
        }
    }

    /// Tells the owner what the reads since the last look changed in the store ([`CatchUp::take_changes`]): each
    /// message stored new or newer as [`Event::Message`] from `source`, as now stored, then each conversation changed
    /// as [`Event::ConversationChanged`].
    fn brought(&self, source: Source) {
        let changes = self.catch_up.take_changes();
        for stored in changes.messages {
            let message = match self.store.message(&stored.conversation, &stored.id) {
                Ok(message) => message.map(Box::new),
                Err(error) => {
                    self.failed("reading a synced message back", error);
                    None
                }
            };
            let change = if stored.applied == Applied::Inserted {
                MessageChange::New
            } else {
                MessageChange::Other
            };
            self.emit(Event::Message {
                conversation: stored.conversation,
                id: stored.id,
                change,
                applied: stored.applied,
                message,
                source,
            });
        }
        for conversation in changes.conversations {
            self.notice(&conversation);
            self.emit(Event::ConversationChanged { conversation });
        }
    }

    /// Keeps `conversation` for the next tick's look at the teams ([`Self::note_noticed`]).
    fn notice(&self, conversation: &str) {
        if teams_proto::is_team_thread_id(conversation) {
            self.noticed.borrow_mut().push(conversation.to_owned());
        }
    }

    /// The team threads noticed since the last tick, each looked at against the stored teams: one they don't name
    /// (the account was added to a team, or a channel was made in one) reads the teams again.
    fn note_noticed(&mut self) {
        let noticed = std::mem::take(&mut *self.noticed.borrow_mut());
        for conversation in noticed {
            if let Err(error) = self.teams.note_conversation(&self.store, &conversation) {
                self.failed("reading the stored teams", error);
            }
        }
    }

    fn failed(&self, during: &'static str, error: impl Into<EngineError>) {
        self.emit(Event::Failed {
            during,
            error: error.into(),
        });
    }

    /// Something that should be in the store isn't (a delivery that couldn't be stored, a sync that failed): it is
    /// told to the owner, and a catch-up is due ([`Core::catch_up_later`]).
    fn missed(&mut self, during: &'static str, error: impl Into<EngineError>) {
        self.failed(during, error);
        self.catch_up_later();
    }

    /// A catch-up is due, tried from the tick within [`EngineOptions::catch_up_retry`] (sooner when a retry was due
    /// already) while the channel is up.
    fn catch_up_later(&mut self) {
        self.gaps.missed();
        self.retry_at
            .get_or_insert_with(|| Instant::now() + self.options.catch_up_retry);
    }

    async fn on_request(&mut self, request: Request) {
        match request {
            Request::UploadFile(path, reply) => self.uploads.upload(path, reply),
            Request::Write(action, reply) => {
                let _ = reply.send(self.logged_write(action).await);
            }
            Request::SendQueued(id, reply) => {
                let _ = reply.send(self.send_row(id).await);
            }
            Request::Sync(reply) => {
                log::info!("action: a catch-up asked for");
                self.gaps.missed();
                let report = self
                    .catch_up_due()
                    .await
                    .unwrap_or_else(|| Ok(CatchUpReport::default()));
                let _ = reply.send(report.map_err(EngineError::from));
            }
            Request::Resend(id, reply) => {
                log::info!("action: resend of row {id}");
                let _ = reply.send(logged(self.resend(id).await, || format!("resend of row {id}")));
            }
            Request::Transient(transient, reply) => {
                let _ = reply.send(self.transient(transient).await);
            }
            Request::Call(action, reply) => self.call(action, reply),
            Request::OwnAvailability(status, reply) => {
                log::info!("action: own availability {status:?}");
                let _ = reply.send(self.own_availability(status).await);
            }
            Request::LoadOlder(conversation, reply) => {
                log::info!("action: older history of {conversation}");
                let older = self.catch_up.load_older(&mut self.store, &conversation).await;
                self.brought(Source::History);
                self.arrived(&[]);
                let _ = reply.send(older.map_err(EngineError::from));
            }
            Request::SyncConversation(conversation, reply) => {
                log::info!("action: a sync of {conversation}");
                let synced = self.catch_up.sync_messages(&mut self.store, &conversation).await;
                let synced = synced.map_err(EngineError::from);
                let _ = reply.send(logged(synced, || format!("sync of {conversation}")));
            }
            Request::Query(query) => self.answer(query),
            Request::Shutdown => {}
            lookup => self.on_lookup(lookup),
        }
    }

    /// Someone's presence asked: answered with what the watch holds; someone the watch dropped to make room is told
    /// unknown, as no read refreshes what a view was told of them.
    fn presence_asked(&mut self, mri: &str, reply: Responder<Availability>) {
        let (availability, dropped) = self.presences.presence(mri);
        let _ = reply.send(Ok(availability));
        if let Some(mri) = dropped {
            self.emit(Event::Presence {
                mri,
                availability: Availability::Unknown,
            });
        }
    }

    /// A request a part of the engine answers on its own (a picture, a file's bytes, the calendar, the search, a
    /// person): handed to that part (`on_request` takes the rest).
    fn on_lookup(&mut self, request: Request) {
        match request {
            Request::ReactionAnimation(id, reply) => self.animations.animation(id, reply),
            Request::MessagePicture(src, reply) => self.pictures.picture(&src, reply),
            Request::FileThumbnail(share_url, reply) => self.thumbnails.thumbnail(&share_url, reply),
            Request::Calendar {
                start_ms,
                end_ms,
                reply,
            } => self.calendar.view(start_ms, end_ms, reply),
            Request::CalendarEvent { id, reply } => self.calendar.event(&id, reply),
            Request::ChannelEvent {
                mailbox,
                ical_uid,
                reply,
            } => self.calendar.channel_event(&mailbox, &ical_uid, reply),
            Request::TitleMeetNow { channel, title, reply } => self.calendar.title_meet_now(channel, title, reply),
            Request::Meeting { chat, reply } => self.calendar.meeting(chat, reply),
            Request::ChannelMeetup { channel, reply } => self.calendar.channel_meetup(channel, reply),
            Request::WriteEvent { write, reply } => self.calendar.write(*write, reply),
            Request::CreateChannel { draft, reply } => self.channels.create(*draft, reply),
            Request::AddChannelMembers {
                team_group,
                channel_group,
                members,
                reply,
            } => self.channels.add_members(team_group, channel_group, members, reply),
            Request::ChannelThread {
                team,
                team_group,
                channel_group,
                name,
                reply,
            } => self.channels.thread(team, team_group, channel_group, name, reply),
            Request::TeamPeople { team, keyword, reply } => self.channels.people(team, keyword, reply),
            Request::Team(ask) => self.channels.team_ask(*ask),
            Request::TeamApps(ask) => self.channels.team_apps_ask(*ask),
            Request::Workflow { request, reply } => self.channels.workflow(*request, reply),
            Request::SearchService { query, scope, reply } => self.service_search.search(query, scope, reply),
            Request::MeetingAvailability {
                attendees,
                start_ms,
                meeting_minutes,
                reply,
            } => self.calendar.availability(attendees, start_ms, meeting_minutes, reply),
            Request::Invitable { query, reply } => match self.store.profiles_named(&query, INVITABLE) {
                Ok(named) => self.calendar.invitable(named, reply),
                Err(error) => {
                    let _ = reply.send(Err(EngineError::Store(error)));
                }
            },
            Request::FileRange {
                share_url,
                offset,
                len,
                reply,
            } => self.files.range(&share_url, offset, len, reply),
            Request::FileStream {
                share_url,
                offset,
                len,
                reply,
            } => self.files.stream(&share_url, offset, len, reply),
            Request::FileSize(share_url, reply) => self.files.size(&share_url, reply),
            Request::Person(mri, reply) => {
                let person = self.profiles.person(&self.store, &mri);
                let _ = reply.send(person.map_err(EngineError::from));
            }
            Request::Presence(mri, reply) => self.presence_asked(&mri, reply),
            Request::Teams(reply) => {
                let teams = self.teams.teams(&self.store);
                let _ = reply.send(teams.map_err(EngineError::from));
            }
            Request::ChatListing(reply) => {
                let listing = self.teams.chat_listing(&self.store);
                let _ = reply.send(listing.map_err(EngineError::from));
            }
            // the engine's own: `on_request` takes them
            _ => {}
        }
    }

    /// A lookup finished (`None`: none was under way): what it found stored, and told when it changed the store.
    fn on_looked(&mut self, looked: Option<crate::profiles::Looked>) {
        let Some(looked) = looked else { return };
        match self.profiles.store(&mut self.store, looked) {
            Ok(Some(profile)) => self.emit(Event::Profile { mri: profile.mri }),
            Ok(None) => {}
            Err(error) => self.failed("storing a profile", error),
        }
    }

    /// A read in the background is over: a person's lookup, the teams' or the presence read.
    fn on_background(&mut self, done: Background) {
        match done {
            Background::Looked(looked) => self.on_looked(looked),
            Background::Teams(read) => self.on_teams(read),
            Background::Presence(polled) => self.on_presence(polled),
            Background::Channel(finished) => self.on_channel(finished),
            Background::Calendar(read) => self.on_calendar(read),
        }
    }

    /// A calendar window's read is over (`None`: it failed): kept in the store, its asker answered already.
    fn on_calendar(&mut self, read: Option<WindowRead>) {
        let Some(read) = read else { return };
        if let Err(error) = self
            .store
            .replace_calendar_window(read.start_ms, read.end_ms, &read.events)
        {
            self.failed("storing the calendar", error);
        }
    }

    /// A channel's write or wait is over (`None`: its task was lost, the asker told by its reply's end): a channel made
    /// is stored and told before the asker hears of it, so it can open the channel at once.
    fn on_channel(&mut self, finished: Option<Finished>) {
        let Some(Finished {
            created,
            team,
            reread,
            reply,
        }) = finished
        else {
            return;
        };
        if reread {
            match self.teams.note_change(&mut self.store, team.map(|change| *change)) {
                Ok(true) => self.emit(Event::Teams),
                Ok(false) => {}
                Err(error) => self.failed("storing the teams", error),
            }
        }
        if let Some(created) = created {
            match self.teams.note_created(&mut self.store, created) {
                Ok(true) => self.emit(Event::Teams),
                Ok(false) => {}
                Err(error) => self.failed("storing the teams", error),
            }
        }
        reply();
    }

    /// A presence read or subscription is over (`None`: none ran): each change it brought told.
    fn on_presence(&mut self, done: Option<PresenceDone>) {
        let Some(done) = done else { return };
        for (mri, availability) in self.presences.settle(done) {
            self.emit(Event::Presence { mri, availability });
        }
    }

    /// A read of the teams is over (`None`: none ran): stored, and told when the list changed; a failure is told.
    /// The account's own notes, once a favourite and not stored yet, are synced from the next tick (see the module
    /// docs).
    fn on_teams(&mut self, read: Option<Result<TeamsRead, TeamsReadError>>) {
        let Some(read) = read else { return };
        match self.teams.store(&mut self.store, read) {
            Ok(Ok(true)) => {
                self.emit(Event::Teams);
                self.notes_due = true;
            }
            Ok(Ok(false)) => {}
            Ok(Err(error)) => self.failed(READING_TEAMS, error),
            Err(error) => self.failed("storing the teams", error),
        }
    }

    /// Syncs the account's own notes ([`NOTES`]) when the stored favourites name them and the store has none yet
    /// (the catch-up's own pass over them, [`CatchUp::catch_up`], ran before it knew they were a favourite).
    async fn sync_favourite_notes(&mut self) {
        if !std::mem::take(&mut self.notes_due) {
            return;
        }
        let due = self
            .store
            .favorites()
            .map(|favorites| favorites.is_some_and(|favorites| favorites.iter().any(|id| id == NOTES)))
            .and_then(|favourite| Ok(favourite && self.store.conversation(NOTES)?.is_none()));
        match due {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => return self.failed("reading the favourites", error),
        }
        log::info!("the account's notes became a favourite: synced");
        if let Err(error) = self.catch_up.sync_messages(&mut self.store, NOTES).await {
            self.missed("syncing the account's notes", error);
        }
    }

    /// Answers a query from the store.
    fn answer(&self, query: Query) {
        self.inbox.answer(&self.store, query);
    }

    /// The account's MRI.
    fn me(&self) -> &str {
        self.outbox.me().mri().as_str()
    }

    // --- writes ---

    /// [`EngineRequests::write`]'s action, told to the log by its kind and ids and the row it made.
    async fn logged_write(&mut self, action: Action) -> Result<OutboxEntry, EngineError> {
        let described = described::action(&action);
        log::info!("action: {described}");
        let written = self.write(action).await;
        if let Ok(row) = &written {
            log::info!("action {described}: row {} {:?}", row.id, row.state);
        }
        logged(written, || described)
    }

    async fn write(&mut self, action: Action) -> Result<OutboxEntry, EngineError> {
        if let Action::Read {
            conversation,
            message_id,
        } = action
        {
            let row = self.inbox.queue_read(&mut self.store, conversation, message_id)?;
            return self.send_row(row.id).await;
        }
        let command = self.command(action)?;
        if let Command::Send { conversation, text } = &command {
            self.uploads.share(conversation, text.files()).await?;
        }
        if let Command::Send { conversation, .. } = &command {
            // the message a waiting typing announced goes now: the typing must not follow it
            for (transient, dropped) in self.outbox.drop_waiting(conversation) {
                self.emit(Event::Transient(transient, dropped));
            }
        }
        let row = enqueue(&mut self.store, &command)?;
        self.emit(Event::Queued(row.clone()));
        self.send_row(row.id).await
    }

    /// The explicit resend of a row ([`EngineRequests::resend`]): adopted into this run by the store, then sent as a
    /// new write is.
    async fn resend(&mut self, id: i64) -> Result<OutboxEntry, EngineError> {
        let row = self.store.resend(id)?;
        self.emit(Event::Queued(row.clone()));
        self.send_row(row.id).await
    }

    /// A transient write ([`EngineRequests::transient`]), to a conversation the store knows; an availability only
    /// from the endpoint the push channel registered and still keeps ([`EngineRequests::own_availability`]'s rule).
    async fn transient(&mut self, transient: Transient) -> Result<TransientSent, EngineError> {
        if let Some(endpoint) = transient.endpoint()
            && self.endpoint.as_ref() != Some(endpoint)
        {
            return Err(EngineError::NoEndpoint);
        }
        if let Some(conversation) = transient.conversation()
            && self.store.conversation(conversation)?.is_none()
        {
            return Err(StoreError::UnknownConversation(conversation.to_owned()).into());
        }
        Ok(self.outbox.send_transient(transient).await)
    }

    /// The account's own availability ([`EngineRequests::own_availability`]), from the endpoint registered in this run.
    async fn own_availability(&mut self, status: OwnStatus) -> Result<TransientSent, EngineError> {
        let endpoint = self.endpoint.clone().ok_or(EngineError::NoEndpoint)?;
        let transient = match status {
            OwnStatus::Available => Transient::EndpointAvailable { endpoint },
            OwnStatus::Manual(status) => Transient::ManualStatus {
                endpoint,
                status: Some(status),
            },
            OwnStatus::ClearManual => Transient::ManualStatus { endpoint, status: None },
        };
        Ok(self.outbox.send_transient(transient).await)
    }

    /// Sends one of this run's rows at once; the row as its send left it (see [`EngineRequests::write`]).
    async fn send_row(&mut self, id: i64) -> Result<OutboxEntry, EngineError> {
        let row = match self.outbox.send(&mut self.store, &self.catch_up, id).await {
            Ok(row) => row,
            Err(error) => self.row_after(id, error)?,
        };
        self.settled(&row).await;
        // its own check's sync may have settled it, or another row, on the way
        self.arrived(std::slice::from_ref(&row));
        Ok(row)
    }

    /// A row whose send ended in an error, as it now stands: it exists, and one left pending goes out from the tick,
    /// so the reply is the row (an owner told the write failed would ask for it again, and it would go out twice). A
    /// check that couldn't sync is the row's own `last_error`; any other error is told as [`Event::Failed`]. The
    /// error itself when the row can't be read back.
    fn row_after(&self, id: i64, error: OutboxError) -> Result<OutboxEntry, EngineError> {
        let Ok(Some(row)) = self.store.outbox_entry(id) else {
            return Err(error.into());
        };
        if !matches!(error, OutboxError::Sync { .. }) {
            self.failed("sending a write", error);
        }
        Ok(row)
    }

    /// The outbox command for `action`, checked against the store.
    fn command(&self, action: Action) -> Result<Command, EngineError> {
        Ok(match action {
            Action::Send { conversation, text } => Command::Send { conversation, text },
            Action::Edit {
                conversation,
                message_id,
                text,
            } => {
                let message = self.own_standing(&conversation, &message_id, "edited")?;
                let original = original(&message)?;
                Command::Edit {
                    conversation,
                    message_id,
                    text,
                    original,
                }
            }
            Action::Delete {
                conversation,
                message_id,
            } => {
                self.own_standing(&conversation, &message_id, "deleted")?;
                Command::Delete {
                    conversation,
                    message_id,
                }
            }
            Action::React {
                conversation,
                message_id,
                key,
            } => self.reaction(conversation, message_id, key, true)?,
            Action::Unreact {
                conversation,
                message_id,
                key,
            } => self.reaction(conversation, message_id, key, false)?,
            Action::Read {
                conversation,
                message_id,
            } => inbox::read_command(&self.store, conversation, message_id)?,
            Action::CreateChat { others } => {
                let others: Vec<String> = others.iter().map(|other| other.as_str().to_owned()).collect();
                create_chat(self.outbox.me().mri(), &others).ok_or(EngineError::NotAChat)?;
                Command::CreateChat { others }
            }
        })
    }

    fn reaction(
        &self,
        conversation: String,
        message_id: String,
        key: String,
        on: bool,
    ) -> Result<Command, EngineError> {
        self.standing(&conversation, &message_id)?;
        Ok(Command::React {
            conversation,
            message_id,
            key,
            on,
        })
    }

    fn stored(&self, conversation: &str, id: &str) -> Result<Message, EngineError> {
        self.store
            .message(conversation, id)?
            .ok_or_else(|| EngineError::UnknownMessage {
                conversation: conversation.to_owned(),
                message: id.to_owned(),
            })
    }

    /// A stored message that isn't deleted.
    fn standing(&self, conversation: &str, id: &str) -> Result<Message, EngineError> {
        let message = self.stored(conversation, id)?;
        if message.delete_time.is_some() {
            return Err(EngineError::Deleted { message: id.to_owned() });
        }
        Ok(message)
    }

    /// One of the account's own messages that isn't deleted.
    fn own_standing(&self, conversation: &str, id: &str, action: &'static str) -> Result<Message, EngineError> {
        let message = self.standing(conversation, id)?;
        if message.sender.as_deref() != Some(self.outbox.me().mri().as_str()) {
            return Err(EngineError::NotOwn {
                message: id.to_owned(),
                action,
            });
        }
        Ok(message)
    }

    /// A row as it settled: once it went through, its conversation synced (a new chat's thread), so the account's own
    /// change is in the store; then told to the owner. Told after the sync, not before: the store answers queries
    /// while the sync waits on the service, so an owner told first could look for the change and not find it yet.
    async fn settled(&mut self, row: &OutboxEntry) {
        let conversation = match row.kind {
            _ if row.state != OutboxState::Sent => None,
            OutboxKind::ReadReceipt => None,
            OutboxKind::CreateChat => row.message_id.clone(),
            _ => row.conversation_id.clone(),
        };
        if let Some(conversation) = conversation
            && let Err(error) = self.catch_up.sync_messages(&mut self.store, &conversation).await
        {
            self.missed("the sync after a write", error);
        }
        self.emit(Event::Outbox(row.clone()));
    }

    // --- calls ---

    /// A call action ([`EngineRequests::call`]): a chat's call resolved into whom it calls, then handed to the calling.
    fn call(&mut self, action: CallAction, reply: Responder<CallId>) {
        log::info!("action: {}", described::call(&action));
        let act = match action {
            CallAction::Captions { call, enabled } => Act::Captions(call, enabled),
            CallAction::CaptionLanguage { call, language } => Act::CaptionLanguage(call, language),
            CallAction::Call { chat } => match self.placement(&chat) {
                Ok(placement) => Act::Place(placement),
                Err(error) => {
                    let _ = reply.send(Err(error.into()));
                    return;
                }
            },
            CallAction::TestCall { echo } => Act::Place(Placement::TestCall { echo }),
            CallAction::Join { chat } => Act::Join(chat),
            // a channel's meeting is joined at the conversation its channel names it runs at, read as it goes
            CallAction::JoinMeeting { chat, .. } if teams_proto::is_team_thread_id(&chat) => {
                Act::JoinChannel(chat, Arc::default())
            }
            CallAction::JoinMeeting { chat, meeting } => Act::Place(Placement::Meeting { chat, meeting }),
            CallAction::MeetNow { channel } => Act::Place(Placement::Channel { channel }),
            CallAction::JoinChannel { channel } => Act::JoinChannel(channel, Arc::default()),
            CallAction::Answer { call } => Act::Answer(call),
            CallAction::Decline { call } => Act::Decline(call),
            CallAction::HangUp { call } => Act::HangUp(call),
            CallAction::Mute { call, muted } => Act::Mute(call, muted),
            CallAction::Renegotiate { call } => Act::Renegotiate(call),
            CallAction::Camera { call, on } => Act::Camera(call, on),
            CallAction::Lobby { call, waiting, admit } => Act::Lobby(call, waiting, admit),
        };
        match self.calls.as_ref() {
            Some(calls) => calls.act(act, reply),
            None => {
                let _ = reply.send(Err(CallActionError::NoCalling.into()));
            }
        }
    }

    /// Whom a call from `chat` calls: a 1:1 chat's other member (from its id), or a group chat's other members as
    /// the store holds them.
    fn placement(&self, chat: &str) -> Result<Placement, CallActionError> {
        let me = self.me();
        if let Some(other) = one_to_one_other(chat, me) {
            return Ok(Placement::Chat {
                callees: vec![Mri::new(other)],
                group_chat: None,
            });
        }
        let members = self.store.members(chat).map_err(|_| CallActionError::NotAChat)?;
        let callees: Vec<Mri> = members
            .into_iter()
            .map(|member| member.mri)
            .filter(|mri| mri != me)
            .map(Mri::new)
            .collect();
        if callees.is_empty() {
            return Err(CallActionError::NotAChat);
        }
        Ok(Placement::Chat {
            callees,
            group_chat: Some(chat.to_owned()),
        })
    }

    // --- the tick ---

    async fn on_tick(&mut self) {
        let (done, error) = match self.outbox.send_due(&mut self.store, &self.catch_up).await {
            Ok(done) => (done, None),
            Err(failed) => (failed.done, Some(failed.error)),
        };
        for row in &done.rows {
            self.settled(row).await;
        }
        // a row's check may have settled it, or another row, on the way: told after the rows as they were sent, so
        // one a later row's check settled is told last as it now stands
        self.arrived(&done.rows);
        if let Some(error) = error {
            self.failed("sending the outbox's due rows", error);
        }
        for (transient, sent) in self.outbox.send_due_transients().await {
            self.emit(Event::Transient(transient, sent));
        }
        if self.up && self.retry_at.is_some_and(|at| Instant::now() >= at) {
            self.auto_catch_up(Cause::Retry).await;
        }
        self.poll_due().await;
        // the account's own notes, when a read of the teams just made them a favourite
        self.sync_favourite_notes().await;
        // a team the stored list doesn't name yet, as a read or delivery brought its thread
        self.note_noticed();
        // profile lookups that failed on the way, their wait over
        self.profiles.retry_due();
        // the watched people's presence, when a read is due
        self.presences.poll_due();
    }

    // --- catch-up ---

    /// Without a push channel, the poll's catch-up when it is due ([`EngineOptions::poll`]), the next one set.
    async fn poll_due(&mut self) {
        let (Some(at), Some(every)) = (self.poll_at, self.options.poll) else {
            return;
        };
        if Instant::now() < at {
            return;
        }
        self.poll_at = Some(Instant::now() + every);
        self.gaps.missed();
        self.auto_catch_up(Cause::Poll).await;
    }

    /// A catch-up if one is due; `None` when none is.
    async fn catch_up_due(&mut self) -> Option<Result<CatchUpReport, CatchUpError>> {
        let result = self
            .catch_up
            .catch_up_if_due(&mut self.store, &mut self.gaps)
            .await
            .transpose();
        // our own messages it brought back settled their rows, whether or not it ran to the end
        self.arrived(&[]);
        let result = result?;
        match &result {
            Ok(report) if report.caught_up() => {
                self.retry_at = None;
                for loss in self.losses.drain(..) {
                    loss.caught_up();
                }
            }
            _ => self.retry_at = Some(Instant::now() + self.options.catch_up_retry),
        }
        Some(result)
    }

    async fn auto_catch_up(&mut self, cause: Cause) {
        match self.catch_up_due().await {
            Some(Ok(report)) => self.emit(Event::CaughtUp { cause, report }),
            Some(Err(error)) => self.emit(Event::CatchUpFailed { cause, error }),
            None => {}
        }
    }

    // --- the push channel ---

    async fn on_push(&mut self, event: PushEvent) {
        match event {
            PushEvent::Connected { endpoint, push_url, .. } => {
                self.presences.connected(push_url);
                self.up = true;
                self.gaps.connected();
                self.emit(Event::Connected { endpoint });
                self.auto_catch_up(Cause::Connected).await;
            }
            PushEvent::Registered(endpoint) => {
                self.presences.registered(endpoint.clone());
                self.endpoint = Some(endpoint);
            }
            PushEvent::Registration(report) => self.emit(Event::Registration(report)),
            PushEvent::MessageLoss(loss) => {
                // a presence change may be among what was missed
                self.presences.missed();
                if loss.messaging() {
                    self.losses.push(loss);
                    self.gaps.missed();
                    self.auto_catch_up(Cause::MessageLoss).await;
                } else {
                    // nothing of the chats was missed
                    loss.caught_up();
                }
            }
            PushEvent::Delivery(delivery) => self.on_delivery(delivery).await,
            PushEvent::Presence(delivery) => {
                for (mri, availability) in self.presences.delivered(&delivery) {
                    self.emit(Event::Presence { mri, availability });
                }
            }
            // an incoming call, or another endpoint's push: never acted on, never answered; a call delivery comes only on
            // the calling stack's socket, which `crate::calling` runs when the engine has `Parts::calls`
            PushEvent::Ignored(_) | PushEvent::Call(_) => {}
            PushEvent::Disconnected { down, heard } => {
                self.presences.down();
                self.up = false;
                self.gaps.disconnected();
                self.emit(Event::Disconnected { down, heard });
            }
            PushEvent::Stopped(stop) => {
                self.presences.down();
                self.up = false;
                // no registration is renewed any more: the endpoint it kept is no longer the account's live one, and
                // an availability waiting out a `Retry-After` doesn't go out from the tick after it
                self.endpoint = None;
                for (transient, dropped) in self.outbox.drop_own_availability() {
                    self.emit(Event::Transient(transient, dropped));
                }
                self.gaps.disconnected();
                self.emit(Event::PushStopped(stop));
            }
        }
    }

    async fn on_delivery(&mut self, delivery: Delivery) {
        let source = if delivery.stored {
            Source::Replay
        } else {
            Source::Delivery
        };
        match delivery.event {
            ChatEvent::Message { conversation, message } => {
                self.on_message(conversation, MessageChange::New, &message, source)
                    .await;
            }
            ChatEvent::MessageUpdate {
                conversation,
                change,
                message,
            } => self.on_message(conversation, change.into(), &message, source).await,
            ChatEvent::Typing { conversation, from } => self.emit(Event::Typing {
                conversation,
                from,
                typing: true,
            }),
            ChatEvent::TypingStopped { conversation, from } => self.emit(Event::Typing {
                conversation,
                from,
                typing: false,
            }),
            ChatEvent::ReadHorizon { conversation, horizon } => self.on_horizon(conversation, &horizon),
            ChatEvent::AppReadHorizon(horizon) => match self.catch_up.receive_app_horizon(&mut self.store, horizon) {
                Ok(()) => self.brought(source),
                Err(error) => self.missed("storing the account's mark-all-read horizon", error),
            },
            ChatEvent::ConversationUpdate { conversation } => self.on_conversation(conversation.id.clone()).await,
            // logged by its kind where it was read
            ChatEvent::Unknown { .. } => {}
        }
    }

    /// A message delivered (new, or changed): stored as a sync would store it, its conversation added first when the
    /// store doesn't know it, and synced after. One that doesn't reach the store is missed ([`Core::missed`]).
    async fn on_message(
        &mut self,
        conversation: String,
        change: MessageChange,
        message: &chat::Message,
        source: Source,
    ) {
        self.notice(&conversation);
        // a team's own activity (people added or gone, a channel made, renamed or deleted) changes what its list shows
        if teams_proto::is_team_thread_id(&conversation) && message.message_type.as_str().starts_with("ThreadActivity/")
        {
            self.teams.note_activity();
        }
        let added = match self.add_if_unknown(&conversation).await {
            Ok(added) => added,
            Err(error) => return self.missed("adding a delivery's conversation", error),
        };
        match self.catch_up.receive_message(&mut self.store, &conversation, message) {
            Ok(Some(received)) => {
                // its row, when this delivery settled it (one a sync settled before was told then)
                self.arrived(&[]);
                let stored = match self.store.message(&conversation, &message.id) {
                    Ok(stored) => stored.map(Box::new),
                    Err(error) => {
                        self.failed("reading a delivered message back", error);
                        None
                    }
                };
                self.emit(Event::Message {
                    conversation: conversation.clone(),
                    id: message.id.clone(),
                    change,
                    applied: received.applied,
                    message: stored,
                    source,
                });
            }
            // a control signal, or a message this build can't read: nothing stored (its row may be settled)
            Ok(None) => self.arrived(&[]),
            Err(error) => self.missed("storing a delivered message", error),
        }
        if added && let Err(error) = self.catch_up.sync_messages(&mut self.store, &conversation).await {
            self.missed("syncing a delivery's new conversation", error);
        }
    }

    /// Adds a conversation the store doesn't know (a thread read); whether it did.
    async fn add_if_unknown(&mut self, conversation: &str) -> Result<bool, EngineError> {
        if self.store.conversation(conversation)?.is_some() {
            return Ok(false);
        }
        self.catch_up.add_conversation(&mut self.store, conversation).await?;
        Ok(true)
    }

    fn on_horizon(&mut self, conversation: String, horizon: &chat::ConsumptionHorizon) {
        match self.store.conversation(&conversation) {
            // a conversation the store doesn't know yet brings its horizon with its catch-up, made due here
            Ok(None) => return self.catch_up_later(),
            Ok(Some(_)) => {}
            Err(error) => return self.missed("storing a read horizon", error),
        }
        match self.catch_up.receive_horizon(&mut self.store, &conversation, horizon) {
            Ok(Some(_)) => self.emit(Event::ReadHorizon { conversation }),
            Ok(None) => {}
            Err(error) => self.missed("storing a read horizon", error),
        }
    }

    /// A conversation changed: one the store doesn't know is added and synced (missed when it can't be, [`Core::missed`]),
    /// and told as the reads' change ([`Core::brought`]); the rest comes with the next catch-up.
    async fn on_conversation(&mut self, conversation: String) {
        self.notice(&conversation);
        match self.add_if_unknown(&conversation).await {
            Ok(true) => {
                if let Err(error) = self.catch_up.sync_messages(&mut self.store, &conversation).await {
                    self.missed("syncing a new conversation", error);
                }
            }
            Ok(false) => self.emit(Event::ConversationChanged { conversation }),
            Err(error) => self.missed("adding a changed conversation", error),
        }
    }
}

/// Tells the engine's owner `event`, logged first at its own level (a failure's warning at `warn` too).
pub(crate) fn emit_to(events: &mpsc::UnboundedSender<Event>, event: Event) {
    if let Some((level, line)) = described::logged(&event, log::max_level()) {
        log::log!(level, "{line}");
    }
    // an owner that dropped its handle takes no more events
    let _ = events.send(event);
}

/// What an edit sends again of the message it changes: its `clientmessageid` and its compose time must be known.
fn original(message: &Message) -> Result<Sent, EngineError> {
    let unknown = |missing| EngineError::Unknown {
        message: message.id.clone(),
        missing,
        action: "edited",
    };
    Ok(Sent {
        client_message_id: message
            .client_message_id
            .clone()
            .ok_or_else(|| unknown("clientmessageid"))?,
        compose_time_ms: message.compose_time.ok_or_else(|| unknown("compose time"))?,
        arrival_time_ms: message.arrival_time,
        attachment_properties: Some(
            crate::outbox::attachment_properties(message).map_err(|_| unknown("attachment properties"))?,
        ),
    })
}

/// `result` as it is, its failure told to the log with what failed (`what`, by kind and ids).
fn logged<T>(result: Result<T, EngineError>, what: impl FnOnce() -> String) -> Result<T, EngineError> {
    if let Err(error) = &result {
        log::warn!("action {} failed: {error}", what());
    }
    result
}

/// The other member of the 1:1 chat `chat` (`19:<object id>_<object id>@unq.gbl.spaces`) of `me`, as an MRI of the
/// form `me` has; `None` for any other chat, or one `me` isn't in.
fn one_to_one_other(chat: &str, me: &str) -> Option<String> {
    let pair = chat.strip_prefix("19:")?.strip_suffix("@unq.gbl.spaces")?;
    let (one, other) = pair.split_once('_')?;
    let (prefix, own) = me.rsplit_once(':')?;
    let them = if one == own {
        other
    } else if other == own {
        one
    } else {
        return None;
    };
    (!them.is_empty() && them != own).then(|| format!("{prefix}:{them}"))
}

/// A read in the background that is over.
enum Background {
    Looked(Option<crate::profiles::Looked>),
    Teams(Option<Result<TeamsRead, TeamsReadError>>),
    Presence(Option<PresenceDone>),
    Channel(Option<Finished>),
    Calendar(Option<WindowRead>),
}

/// The next read in the background to be over, a person's lookup, the teams', the presence read or a calendar window's;
/// never while none runs.
async fn background<C: Credentials + 'static, T: Transport<Error = HttpError> + Send + 'static>(
    profiles: &mut Profiles<C>,
    teams: &mut Teams<C>,
    presences: &mut Presences<C>,
    channels: &mut Channels<C, T>,
    calendar: &mut Calendar<C, T>,
) -> Background {
    tokio::select! {
        looked = profiles.next(), if profiles.busy() => Background::Looked(looked),
        read = teams.next(), if teams.busy() => Background::Teams(read),
        polled = presences.next(), if presences.busy() => Background::Presence(polled),
        finished = channels.next(), if channels.busy() => Background::Channel(finished),
        read = calendar.next(), if calendar.busy() => Background::Calendar(read),
        else => pending().await,
    }
}

/// The push channel's next event; never, without one.
async fn next_push(push: Option<&mut TrouterHandle>) -> Option<PushEvent> {
    match push {
        Some(push) => push.next().await,
        None => pending().await,
    }
}
