//! The engine: one account's client core on a tokio runtime and a thread of its own, reached only through channels,
//! the shape M2's GPUI bridge needs. A UI thread hands it [`Action`]s and queries and awaits each [`Reply`] on its own
//! executor (a CLI blocks on it, [`Reply::blocking_wait`]), and takes its [`Event`]s as they come.
//!
//! - **Commands** ([`EngineRequests::write`]): an [`Action`] (send, edit, delete, react, take a reaction back, read,
//!   create a chat) is checked against the store first: an edit or a delete only of the account's own message that
//!   isn't deleted, a reaction only on a message that isn't, a read receipt only up to a stored message. It becomes an
//!   outbox row ([`enqueue`]) and is sent at once ([`Outbox::send`]): every attempt under a fresh `Fence::check`,
//!   then the transport. The reply is the row as that send left it, an error only when no row was made (or it can't
//!   be read back): a row that waits (a `Retry-After`, a check that couldn't sync, an older row on its
//!   [`Lane`](crate::Lane)) is in the reply as it waits, and goes out from the engine's tick ([`Outbox::send_due`]);
//!   every row that settles comes as [`Event::Outbox`] too, and so does every row our own message coming back
//!   settles, by whichever read brought it (a catch-up, a delivery, a sync after a write, the check of another row),
//!   and every row a read finds may have arrived (an entry it couldn't read carrying its `clientmessageid`): one event
//!   per change of the row, the last one the row as it now stands ([`Outbox::take_arrived`]). Once a write went
//!   through, its conversation is synced, so the account's own change is in the store (a new chat with it). A row can
//!   be read back by its id ([`EngineRequests::outbox_entry`]).
//! - **Events → store**: each delivery of the push channel ([`teams_trouter`]) goes through the catch-up's
//!   [`CatchUp::receive_message`], the path a sync takes, so a message that comes both ways is stored once, at its
//!   newest version; the account's own read horizon through [`CatchUp::receive_horizon`]. A delivery naming a
//!   conversation the store doesn't know adds it first (a thread read), and the conversation is synced after. Typing
//!   is only an event, never stored; a delivery is never answered (the push channel can't).
//! - **Catch-up** ([`Gaps`]): after every connect of the push channel when one is due (on start, and after every
//!   disconnect), after a message-loss report about the chats (acknowledged once a catch-up ran with nothing failed),
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
//!   members, the unread count and the account's read horizon of a conversation, an outbox row, a conversation's
//!   rows that aren't sent and the rows earlier runs left, all read from the store on the engine's thread, which owns
//!   it; older history from the service ([`EngineRequests::load_older`]). A row of an earlier run (or a failed or
//!   denied one) goes out again only by the explicit [`EngineRequests::resend`].
//! - **Transient writes** ([`EngineRequests::transient`]: typing) go out through the outbox too, but as no row
//!   ([`Transient`]); a throttled one goes out from the tick unless a newer one of the same took its place, it waits
//!   once and briefly ([`crate::MAX_TRANSIENT_WAIT`]), and a message post to its conversation drops it (told as
//!   [`Event::Transient`] with its drop).
//! - **Its handles**: [`EngineRequests`], the request side (writes and queries, each a [`Reply`]), is cloneable, for a
//!   UI's views to share; [`EngineHandle`] owns the engine: its events, its thread and its shutdown.
//! - **One engine per account** ([`teams_store::Store::engine_lock`]): [`start`] takes the account's engine lock
//!   without waiting, and is refused ([`EngineError::AlreadyRunning`]) while another engine holds it, in this process
//!   or another; the lock goes when the engine's thread ends.
//!
//! The engine sends nothing of its own: every write goes out through the outbox, every read through the catch-up's
//! client, and the push channel sends only its reviewed own frames. Without a push channel ([`Parts::push`] `None`:
//! a one-shot command of the lab binary) nothing is caught up but on [`EngineRequests::sync`]. The store stays open
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
use std::time::Duration;

use teams_fence::Mri;
use teams_proto::chat::{self, MessageText};
use teams_proto::{Client, Credentials, HttpError, HttpTransport, Transport};
use teams_store::{
    Applied, Conversation, Member, Message, MessagePage, OutboxEntry, OutboxKind, OutboxState, PageCursor, Reaction,
    ReadHorizon, Store, StoreError,
};
use teams_trouter::{
    Change, ChatEvent, Delivery, Down, MessageLoss, PushEvent, RegistrationReport, Stop, Trouter, TrouterCredentials,
    TrouterHandle,
};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, MissedTickBehavior};

use crate::outbox::{Command, Outbox, OutboxError, Sent, Transient, TransientSent, create_chat, enqueue};
use crate::sync::{CatchUp, CatchUpError, CatchUpReport, Gaps, Synced};

/// How the engine paces itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineOptions {
    /// How often the outbox's waiting rows are looked at, and a failed catch-up's time to try again: 1 s.
    pub tick: Duration,
    /// How long after a catch-up that failed (or left a conversation unsynced), or a delivery that didn't reach the
    /// store, the next catch-up is tried: 30 s.
    pub catch_up_retry: Duration,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(1),
            catch_up_retry: Duration::from_secs(30),
        }
    }
}

/// The account's push channel, built and not yet started: the engine starts it on its own runtime.
pub struct Push(Box<dyn FnOnce() -> TrouterHandle + Send>);

impl Push {
    #[must_use]
    pub fn new<T, K>(trouter: Trouter<T, K>) -> Self
    where
        T: Transport + Send + 'static,
        K: TrouterCredentials + 'static,
    {
        Self(Box::new(move || trouter.spawn()))
    }

    fn spawn(self) -> TrouterHandle {
        (self.0)()
    }
}

impl fmt::Debug for Push {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Push")
    }
}

/// What an engine is made of: the account's store, the client its reads go through, its outbox (with the fence, the
/// membership source and the account it writes as) and, for a client that listens, its push channel.
pub struct Parts<C, T> {
    pub store: Store,
    pub client: Arc<Client<HttpTransport<C>>>,
    pub outbox: Outbox<T>,
    pub push: Option<Push>,
    pub options: EngineOptions,
}

/// What the user asks the engine to write. Every one goes out through the outbox, under the fence.
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
    /// delivery's new conversation, the outbox's check.
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
}

/// What the engine tells its owner.
#[derive(Debug)]
pub enum Event {
    /// The push channel is up; a catch-up follows when one is due.
    Connected { endpoint: String },
    /// How a registration of the push channel's endpoint went: deliveries come only to a registered one.
    Registration(RegistrationReport),
    /// The push channel is down; it reconnects, and a catch-up is due.
    Disconnected(Down),
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
    /// An outbox row as it settled: after its first send, a later retry, or our own message coming back (by a
    /// catch-up, a delivery or any other sync).
    Outbox(OutboxEntry),
    /// A transient that waited out its `Retry-After`, as its send from the tick went; or one that waited and was
    /// dropped because a message post to its conversation went first.
    Transient(Transient, TransientSent),
    /// Something the engine did on its own failed: what, and why.
    Failed { during: &'static str, error: EngineError },
}

/// What the engine's requests can fail with.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Outbox(#[from] OutboxError),
    #[error(transparent)]
    CatchUp(#[from] CatchUpError),
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
    #[error("the engine's runtime can't be built: {0}")]
    Runtime(#[source] io::Error),
    #[error("the engine's thread can't be started: {0}")]
    Thread(#[source] io::Error),
    /// Another engine runs on the account, in this process or another: only one at a time ([`start`]).
    #[error("another engine runs on this account (its lock {} is held): only one runs at a time", lock.display())]
    AlreadyRunning { lock: PathBuf },
    #[error("the engine has stopped")]
    Stopped,
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

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().0)
            .poll(cx)
            .map(|answer| answer.unwrap_or(Err(EngineError::Stopped)))
    }
}

type Responder<T> = oneshot::Sender<Result<T, EngineError>>;

/// A request to the engine's thread, with where its answer goes.
enum Request {
    Write(Action, Responder<OutboxEntry>),
    Resend(i64, Responder<OutboxEntry>),
    Transient(Transient, Responder<TransientSent>),
    Sync(Responder<CatchUpReport>),
    LoadOlder(String, Responder<Option<Synced>>),
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
    UnreadCount(String, Responder<u64>),
    ReadHorizon(String, Responder<Option<ReadHorizon>>),
    OutboxEntry(i64, Responder<Option<OutboxEntry>>),
    OutboxUnsettled(String, Responder<Vec<OutboxEntry>>),
    LeftByEarlierRuns(Responder<Vec<OutboxEntry>>),
}

/// The engine's request side: writes and queries, each answered by a [`Reply`]. Cloneable and `Send`, so a UI's
/// views can share it; a request made after the engine stopped answers [`EngineError::Stopped`]. Holding one doesn't
/// keep the engine running: its [`EngineHandle`] owns it.
#[derive(Clone, Debug)]
pub struct EngineRequests {
    requests: mpsc::UnboundedSender<Request>,
}

impl EngineRequests {
    /// Writes through the outbox (see the module docs); the reply is the row as its first send left it.
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
    /// A typing in a conversation the store doesn't know is refused.
    pub fn transient(&self, transient: Transient) -> Reply<TransientSent> {
        self.ask(|reply| Request::Transient(transient, reply))
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

    /// The account's own stored read horizon in a conversation.
    pub fn read_horizon(&self, conversation: impl Into<String>) -> Reply<Option<ReadHorizon>> {
        let conversation = conversation.into();
        self.query(|reply| Query::ReadHorizon(conversation, reply))
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
    let (requests, received) = mpsc::unbounded_channel();
    let (events, taken) = mpsc::unbounded_channel();
    let (ending, ended) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("teams-core".to_owned())
        .spawn(move || {
            runtime.block_on(Core::new(parts, events).run(received));
            // its tasks (the push channel's) and the store are gone before another engine may start
            drop(runtime);
            drop(lock);
            drop(ending);
        })
        .map_err(EngineError::Thread)?;
    Ok(EngineHandle {
        requests: EngineRequests { requests },
        events: taken,
        thread: Some(thread),
        ended,
    })
}

/// The engine's state on its thread.
struct Core<C, T> {
    store: Store,
    outbox: Arc<Outbox<T>>,
    catch_up: CatchUp<C, T>,
    events: mpsc::UnboundedSender<Event>,
    options: EngineOptions,
    push: Option<Push>,
    gaps: Gaps,
    /// The push channel is connected.
    up: bool,
    /// When a catch-up is tried again after one that failed, or after something a delivery brought was missed.
    retry_at: Option<Instant>,
    /// Message-loss reports waiting for a catch-up that runs clean.
    losses: Vec<MessageLoss>,
}

impl<C, T> Core<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    fn new(parts: Parts<C, T>, events: mpsc::UnboundedSender<Event>) -> Self {
        let outbox = Arc::new(parts.outbox);
        let me = outbox.me().mri().clone();
        let catch_up = CatchUp::new(parts.client, Arc::clone(&outbox), me);
        Self {
            store: parts.store,
            outbox,
            catch_up,
            events,
            options: parts.options,
            push: parts.push,
            gaps: Gaps::new(),
            up: false,
            retry_at: None,
            losses: Vec::new(),
        }
    }

    async fn run(mut self, mut requests: mpsc::UnboundedReceiver<Request>) {
        let mut push = self.push.take().map(Push::spawn);
        let mut tick = tokio::time::interval(self.options.tick);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                request = requests.recv() => match request {
                    Some(Request::Shutdown) | None => break,
                    Some(request) => self.on_request(request).await,
                },
                event = next_push(push.as_mut()) => match event {
                    Some(event) => self.on_push(event).await,
                    None => push = None,
                },
                _ = tick.tick() => self.on_tick().await,
            }
            // whatever a read settled on its way (a sync after a write, a new conversation's sync, …)
            self.arrived(&[]);
        }
        if let Some(push) = push {
            push.stop().await;
        }
    }

    fn emit(&self, event: Event) {
        // an owner that dropped its handle takes no more events
        let _ = self.events.send(event);
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
            self.emit(Event::ConversationChanged { conversation });
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
            Request::Write(action, reply) => {
                let _ = reply.send(self.write(action).await);
            }
            Request::Sync(reply) => {
                self.gaps.missed();
                let report = self
                    .catch_up_due()
                    .await
                    .unwrap_or_else(|| Ok(CatchUpReport::default()));
                let _ = reply.send(report.map_err(EngineError::from));
            }
            Request::Resend(id, reply) => {
                let _ = reply.send(self.resend(id).await);
            }
            Request::Transient(transient, reply) => {
                let _ = reply.send(self.transient(transient).await);
            }
            Request::LoadOlder(conversation, reply) => {
                let older = self.catch_up.load_older(&mut self.store, &conversation).await;
                self.brought(Source::History);
                self.arrived(&[]);
                let _ = reply.send(older.map_err(EngineError::from));
            }
            Request::Query(query) => self.answer(query),
            Request::Shutdown => {}
        }
    }

    /// Answers a query from the store.
    fn answer(&self, query: Query) {
        fn give<T>(reply: Responder<T>, answer: Result<T, StoreError>) {
            let _ = reply.send(answer.map_err(EngineError::from));
        }
        let store = &self.store;
        match query {
            Query::Conversations(limit, reply) => give(reply, store.conversations_by_activity(limit)),
            Query::Messages {
                conversation,
                before,
                limit,
                reply,
            } => give(reply, store.messages_before(&conversation, before.as_ref(), limit)),
            Query::Message {
                conversation,
                id,
                reply,
            } => give(reply, store.message(&conversation, &id)),
            Query::Reactions {
                conversation,
                id,
                reply,
            } => give(reply, store.reactions(&conversation, &id)),
            Query::Members(conversation, reply) => give(reply, store.members(&conversation)),
            Query::UnreadCount(conversation, reply) => give(reply, store.unread_count(&conversation, self.me())),
            Query::ReadHorizon(conversation, reply) => give(reply, store.read_horizon(&conversation, self.me())),
            Query::OutboxEntry(id, reply) => give(reply, store.outbox_entry(id)),
            Query::OutboxUnsettled(conversation, reply) => give(reply, store.outbox_unsettled(&conversation)),
            Query::LeftByEarlierRuns(reply) => give(reply, store.outbox_left_by_earlier_runs()),
        }
    }

    /// The account's MRI.
    fn me(&self) -> &str {
        self.outbox.me().mri().as_str()
    }

    // --- writes ---

    async fn write(&mut self, action: Action) -> Result<OutboxEntry, EngineError> {
        let command = self.command(action)?;
        if let Command::Send { conversation, .. } = &command {
            // the message a waiting typing announced goes now: the typing must not follow it
            for (transient, dropped) in self.outbox.drop_waiting(conversation) {
                self.emit(Event::Transient(transient, dropped));
            }
        }
        let row = enqueue(&mut self.store, &command)?;
        self.send_row(row.id).await
    }

    /// The explicit resend of a row ([`EngineRequests::resend`]): adopted into this run by the store, then sent as a
    /// new write is.
    async fn resend(&mut self, id: i64) -> Result<OutboxEntry, EngineError> {
        let row = self.store.resend(id)?;
        self.send_row(row.id).await
    }

    /// A transient write ([`EngineRequests::transient`]), to a conversation the store knows.
    async fn transient(&mut self, transient: Transient) -> Result<TransientSent, EngineError> {
        if let Some(conversation) = transient.conversation()
            && self.store.conversation(conversation)?.is_none()
        {
            return Err(StoreError::UnknownConversation(conversation.to_owned()).into());
        }
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
            } => {
                let message = self.stored(&conversation, &message_id)?;
                let client_message_id = message.client_message_id.ok_or_else(|| EngineError::Unknown {
                    message: message_id.clone(),
                    missing: "clientmessageid",
                    action: "read up to",
                })?;
                Command::ReadReceipt {
                    conversation,
                    message_id,
                    client_message_id,
                }
            }
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

    /// A row as it settled: told to the owner, and once it went through, its conversation synced (a new chat's
    /// thread), so the account's own change is in the store.
    async fn settled(&mut self, row: &OutboxEntry) {
        self.emit(Event::Outbox(row.clone()));
        if row.state != OutboxState::Sent {
            return;
        }
        let conversation = match row.kind {
            OutboxKind::ReadReceipt => None,
            OutboxKind::CreateChat => row.message_id.clone(),
            _ => row.conversation_id.clone(),
        };
        if let Some(conversation) = conversation
            && let Err(error) = self.catch_up.sync_messages(&mut self.store, &conversation).await
        {
            self.missed("the sync after a write", error);
        }
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
    }

    // --- catch-up ---

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
            Ok(report) if report.failed.is_empty() => {
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
            PushEvent::Connected { endpoint, .. } => {
                self.up = true;
                self.gaps.connected();
                self.emit(Event::Connected { endpoint });
                self.auto_catch_up(Cause::Connected).await;
            }
            PushEvent::Registration(report) => self.emit(Event::Registration(report)),
            PushEvent::MessageLoss(loss) => {
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
            // an incoming call, or another endpoint's push: never acted on, never answered
            PushEvent::Ignored(_) => {}
            PushEvent::Disconnected(down) => {
                self.up = false;
                self.gaps.disconnected();
                self.emit(Event::Disconnected(down));
            }
            PushEvent::Stopped(stop) => {
                self.up = false;
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
    })
}

/// The push channel's next event; never, without one.
async fn next_push(push: Option<&mut TrouterHandle>) -> Option<PushEvent> {
    match push {
        Some(push) => push.next().await,
        None => pending().await,
    }
}
