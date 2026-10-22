//! The bridge between GPUI and the engine.
//!
//! The engine runs on a thread and a runtime of its own. Its owner, the [`EngineHandle`], has the one event receiver
//! and the shutdown, so it can't be shared: the [`Bridge`] owns it (as an [`EventSource`]), and one task on GPUI's
//! foreground drains its events into the [`Models`]. Views get only the request side, behind [`Core`] (in the app the
//! engine's [`EngineRequests`]; in a view test a fake), and await each query's [`Reply`] on GPUI's executor.
//!
//! [`Bridge::shutdown`] stops the drain, takes the owner back from it and shuts the engine down on GPUI's background
//! executor, never the foreground ([`EventSource::shutdown`] blocks until the engine's thread is joined). Every call,
//! the first and any later one, answers with a task that ends only once that join has: a caller that quits after it
//! never quits before the engine is joined. The bridge keeps the shutdown going whether or not a caller keeps its
//! task. From then on every request answers `Stopped`: nothing more goes out. [`Bridge::sign_out`] does the same once
//! the account's session can't renew its tokens, and shows the account signed out.

use std::pin::pin;
use std::sync::Arc;

use futures_util::FutureExt as _;
use futures_util::future::{Either, Shared, select};
use gpui::{AppContext as _, AsyncApp, Context, SharedString, Task};
use teams_core::sync::{CatchUpReport, Synced};
use teams_core::{Action, Availability, EngineHandle, EngineRequests, Event, Person, Reply, Transient, TransientSent};
use teams_store::{
    Conversation, Member, Message, MessagePage, OutboxEntry, PageCursor, Reaction, ReadHorizon, SearchHit, Team,
};
use tokio::sync::oneshot;

use crate::models::Models;

/// The engine's request side, as the views see it: writes and queries, each answered by a [`Reply`]. No view builds a
/// request of its own; every write goes out through the engine's outbox, under the fence.
pub trait Core: Send + Sync + 'static {
    /// The account's MRI: who the engine writes as.
    fn me(&self) -> &str;
    /// A write through the outbox; the reply is the row as its first send left it.
    fn write(&self, action: Action) -> Reply<OutboxEntry>;
    /// The explicit resend of an outbox row, by its id.
    fn resend(&self, id: i64) -> Reply<OutboxEntry>;
    /// A transient write (typing), as no row.
    fn transient(&self, transient: Transient) -> Reply<TransientSent>;
    /// A catch-up now.
    fn sync(&self) -> Reply<CatchUpReport>;
    /// The next older page of a conversation, from the service.
    fn load_older(&self, conversation: String) -> Reply<Option<Synced>>;
    /// Up to `limit` stored conversations, the latest activity first.
    fn conversations(&self, limit: usize) -> Reply<Vec<Conversation>>;
    /// Up to `limit` stored messages of a conversation older than `before`, the newest first.
    fn messages(&self, conversation: String, before: Option<PageCursor>, limit: usize) -> Reply<MessagePage>;
    /// One stored message.
    fn message(&self, conversation: String, id: String) -> Reply<Option<Message>>;
    /// A stored message's reactions.
    fn reactions(&self, conversation: String, id: String) -> Reply<Vec<Reaction>>;
    /// A chat's stored members.
    fn members(&self, conversation: String) -> Reply<Vec<Member>>;
    /// How many stored messages of a conversation the account hasn't read.
    fn unread_count(&self, conversation: String) -> Reply<u64>;
    /// Someone's name and picture as the engine has them; a lookup starts in the background when its stored ones
    /// are missing or old, and its change comes as `Event::Profile`.
    fn person(&self, mri: String) -> Reply<Person>;
    /// Someone's presence as last read; from then on the engine reads it (for those this build may read), and each
    /// change comes as `Event::Presence`.
    fn presence(&self, mri: String) -> Reply<Availability>;
    /// The account's own read horizon in a conversation.
    fn read_horizon(&self, conversation: String) -> Reply<Option<ReadHorizon>>;
    /// Whether the store holds a conversation's whole history; `None` when it was never synced.
    fn history_complete(&self, conversation: String) -> Reply<Option<bool>>;
    /// An outbox row as it now stands.
    fn outbox_entry(&self, id: i64) -> Reply<Option<OutboxEntry>>;
    /// A conversation's outbox rows that aren't sent.
    fn outbox_unsettled(&self, conversation: String) -> Reply<Vec<OutboxEntry>>;
    /// The rows runs that have ended left pending or in flight.
    fn left_by_earlier_runs(&self) -> Reply<Vec<OutboxEntry>>;
    /// Up to `limit` stored messages of the conversations of `within` whose text holds every term of `query`, the
    /// newest first: the store's own index, never the service's search. The other conversations' messages are left
    /// out before the hits are counted.
    fn search(&self, query: String, within: Vec<String>, limit: usize) -> Reply<Vec<SearchHit>>;
    /// The account's teams with their channels, from the store (`None` before a read settled them); a read starts in
    /// the background when they are missing or old, and a change comes as `Event::Teams` (M2 Task 16).
    fn teams(&self) -> Reply<Option<Vec<Team>>>;
    /// Syncs one conversation's messages from the service now: how a channel's posts are read.
    fn sync_conversation(&self, conversation: String) -> Reply<Synced>;
    /// The post each stored message of a channel belongs to, as (message id, post id).
    fn reply_roots(&self, conversation: String) -> Reply<Vec<(String, String)>>;
}

impl Core for EngineRequests {
    fn me(&self) -> &str {
        EngineRequests::me(self)
    }

    fn write(&self, action: Action) -> Reply<OutboxEntry> {
        EngineRequests::write(self, action)
    }

    fn resend(&self, id: i64) -> Reply<OutboxEntry> {
        EngineRequests::resend(self, id)
    }

    fn transient(&self, transient: Transient) -> Reply<TransientSent> {
        EngineRequests::transient(self, transient)
    }

    fn sync(&self) -> Reply<CatchUpReport> {
        EngineRequests::sync(self)
    }

    fn load_older(&self, conversation: String) -> Reply<Option<Synced>> {
        EngineRequests::load_older(self, conversation)
    }

    fn conversations(&self, limit: usize) -> Reply<Vec<Conversation>> {
        EngineRequests::conversations(self, limit)
    }

    fn messages(&self, conversation: String, before: Option<PageCursor>, limit: usize) -> Reply<MessagePage> {
        EngineRequests::messages(self, conversation, before, limit)
    }

    fn message(&self, conversation: String, id: String) -> Reply<Option<Message>> {
        EngineRequests::message(self, conversation, id)
    }

    fn reactions(&self, conversation: String, id: String) -> Reply<Vec<Reaction>> {
        EngineRequests::reactions(self, conversation, id)
    }

    fn members(&self, conversation: String) -> Reply<Vec<Member>> {
        EngineRequests::members(self, conversation)
    }

    fn unread_count(&self, conversation: String) -> Reply<u64> {
        EngineRequests::unread_count(self, conversation)
    }

    fn person(&self, mri: String) -> Reply<Person> {
        EngineRequests::person(self, mri)
    }

    fn presence(&self, mri: String) -> Reply<Availability> {
        EngineRequests::presence(self, mri)
    }

    fn read_horizon(&self, conversation: String) -> Reply<Option<ReadHorizon>> {
        EngineRequests::read_horizon(self, conversation)
    }

    fn history_complete(&self, conversation: String) -> Reply<Option<bool>> {
        EngineRequests::history_complete(self, conversation)
    }

    fn outbox_entry(&self, id: i64) -> Reply<Option<OutboxEntry>> {
        EngineRequests::outbox_entry(self, id)
    }

    fn outbox_unsettled(&self, conversation: String) -> Reply<Vec<OutboxEntry>> {
        EngineRequests::outbox_unsettled(self, conversation)
    }

    fn left_by_earlier_runs(&self) -> Reply<Vec<OutboxEntry>> {
        EngineRequests::left_by_earlier_runs(self)
    }

    fn search(&self, query: String, within: Vec<String>, limit: usize) -> Reply<Vec<SearchHit>> {
        EngineRequests::search_in(self, query, within, limit)
    }

    fn teams(&self) -> Reply<Option<Vec<Team>>> {
        EngineRequests::teams(self)
    }

    fn sync_conversation(&self, conversation: String) -> Reply<Synced> {
        EngineRequests::sync_conversation(self, conversation)
    }

    fn reply_roots(&self, conversation: String) -> Reply<Vec<(String, String)>> {
        EngineRequests::reply_roots(self, conversation)
    }
}

/// A core whose engine has stopped: every request answers [`EngineError::Stopped`](teams_core::EngineError::Stopped)
/// and nothing goes anywhere. For views shown without an engine (a window's own tests).
#[derive(Clone, Debug)]
pub struct StoppedCore {
    me: String,
}

impl StoppedCore {
    /// A stopped core of the account `me`.
    #[must_use]
    pub fn new(me: impl Into<String>) -> Self {
        Self { me: me.into() }
    }
}

fn stopped<T>() -> Reply<T> {
    Reply::ready(Err(teams_core::EngineError::Stopped))
}

impl Core for StoppedCore {
    fn me(&self) -> &str {
        &self.me
    }

    fn write(&self, _: Action) -> Reply<OutboxEntry> {
        stopped()
    }

    fn resend(&self, _: i64) -> Reply<OutboxEntry> {
        stopped()
    }

    fn transient(&self, _: Transient) -> Reply<TransientSent> {
        stopped()
    }

    fn sync(&self) -> Reply<CatchUpReport> {
        stopped()
    }

    fn load_older(&self, _: String) -> Reply<Option<Synced>> {
        stopped()
    }

    fn conversations(&self, _: usize) -> Reply<Vec<Conversation>> {
        stopped()
    }

    fn messages(&self, _: String, _: Option<PageCursor>, _: usize) -> Reply<MessagePage> {
        stopped()
    }

    fn message(&self, _: String, _: String) -> Reply<Option<Message>> {
        stopped()
    }

    fn reactions(&self, _: String, _: String) -> Reply<Vec<Reaction>> {
        stopped()
    }

    fn members(&self, _: String) -> Reply<Vec<Member>> {
        stopped()
    }

    fn unread_count(&self, _: String) -> Reply<u64> {
        stopped()
    }

    fn person(&self, _: String) -> Reply<Person> {
        stopped()
    }

    fn presence(&self, _: String) -> Reply<Availability> {
        stopped()
    }

    fn read_horizon(&self, _: String) -> Reply<Option<ReadHorizon>> {
        stopped()
    }

    fn history_complete(&self, _: String) -> Reply<Option<bool>> {
        stopped()
    }

    fn outbox_entry(&self, _: i64) -> Reply<Option<OutboxEntry>> {
        stopped()
    }

    fn outbox_unsettled(&self, _: String) -> Reply<Vec<OutboxEntry>> {
        stopped()
    }

    fn left_by_earlier_runs(&self) -> Reply<Vec<OutboxEntry>> {
        stopped()
    }

    fn search(&self, _: String, _: Vec<String>, _: usize) -> Reply<Vec<SearchHit>> {
        stopped()
    }

    fn teams(&self) -> Reply<Option<Vec<Team>>> {
        stopped()
    }

    fn sync_conversation(&self, _: String) -> Reply<Synced> {
        stopped()
    }

    fn reply_roots(&self, _: String) -> Reply<Vec<(String, String)>> {
        stopped()
    }
}

/// The engine's owning side: its events, one at a time, and its shutdown. The app's is the [`EngineHandle`]; a view
/// test hands in a fake.
pub trait EventSource: Send + 'static {
    /// The next event; `None` once the engine has stopped and every event was taken. Dropping the future before it
    /// is ready loses no event.
    fn next_event(&mut self) -> impl Future<Output = Option<Event>>;

    /// Stops the engine and waits until it has: blocking, so the bridge calls it off GPUI's foreground.
    fn shutdown(self);
}

impl EventSource for EngineHandle {
    fn next_event(&mut self) -> impl Future<Output = Option<Event>> {
        EngineHandle::next_event(self)
    }

    fn shutdown(self) {
        EngineHandle::shutdown(self);
    }
}

/// How the engine is shut down, once the drain hands its owner back.
type Shutdown = Box<dyn FnOnce() + Send>;

/// The drain while it runs: its stop signal, and the task that hands the owner back.
struct Draining {
    stop: oneshot::Sender<()>,
    task: Task<Shutdown>,
}

/// Where the engine's owner is.
enum Held {
    /// With the drain.
    Draining(Draining),
    /// Being shut down: the task ends once the engine's thread is joined. Every [`Bridge::shutdown`] awaits this one.
    Stopping(Shared<Task<()>>),
    /// Shut down, its thread joined.
    Stopped,
}

/// The engine as the UI holds it (see the module docs). A GPUI entity: [`Bridge::start`] builds it in a
/// `cx.new(...)`.
pub struct Bridge {
    core: Arc<dyn Core>,
    models: Models,
    held: Held,
}

impl Bridge {
    /// Takes over the engine's owner (`events`) and its request side (`core`), and starts draining its events into
    /// `models`.
    pub fn start<E: EventSource>(events: E, core: Arc<dyn Core>, models: Models, cx: &mut Context<Self>) -> Self {
        let (stop, stopped) = oneshot::channel();
        let drained = models.clone();
        let task = cx.spawn(async move |_, cx: &mut AsyncApp| {
            let events = drain(events, stopped, &drained, cx).await;
            Box::new(move || events.shutdown()) as Shutdown
        });
        Self {
            core,
            models,
            held: Held::Draining(Draining { stop, task }),
        }
    }

    /// The request side, for a view to keep.
    #[must_use]
    pub fn core(&self) -> Arc<dyn Core> {
        Arc::clone(&self.core)
    }

    #[must_use]
    pub fn models(&self) -> &Models {
        &self.models
    }

    /// Whether the engine is still held: not shut down, or its thread not joined yet.
    #[must_use]
    pub fn running(&self) -> bool {
        !matches!(self.held, Held::Stopped)
    }

    /// Stops the drain and shuts the engine down off the foreground; the task ends once the engine's thread is
    /// joined. A later call starts nothing new: its task ends with the first one's join, at once when that is done.
    /// Dropping the task doesn't cancel the shutdown.
    pub fn shutdown(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let stopping = match std::mem::replace(&mut self.held, Held::Stopped) {
            Held::Draining(draining) => self.stop(draining, cx),
            Held::Stopping(stopping) => stopping,
            Held::Stopped => return Task::ready(()),
        };
        self.held = Held::Stopping(stopping.clone());
        cx.spawn(async move |_, _: &mut AsyncApp| stopping.await)
    }

    /// The shutdown itself: the drain stopped and its owner taken back, the engine shut down on the background
    /// executor, then the models and the bridge told.
    fn stop(&self, Draining { stop, task }: Draining, cx: &mut Context<Self>) -> Shared<Task<()>> {
        // a drain that already ended (the engine stopped on its own) has dropped its receiver
        let _ = stop.send(());
        let models = self.models.clone();
        cx.spawn(async move |bridge, cx: &mut AsyncApp| {
            let shutdown = task.await;
            cx.background_spawn(async move { shutdown() }).await;
            cx.update(|cx| models.engine_stopped(cx));
            // a bridge that is gone has nobody left to ask
            let _ = bridge.update(cx, |bridge, _| bridge.held = Held::Stopped);
        })
        .shared()
    }

    /// The account's session can't renew its tokens: shows it signed out with `hint` and shuts the engine down
    /// ([`Bridge::shutdown`]), so nothing more is sent.
    pub fn sign_out(&mut self, hint: SharedString, cx: &mut Context<Self>) -> Task<()> {
        self.models.signed_out(hint, cx);
        self.shutdown(cx)
    }
}

/// Applies `events` to `models` until the engine's events end or `stop` comes, and hands the owner back.
async fn drain<E: EventSource>(
    mut events: E,
    mut stop: oneshot::Receiver<()>,
    models: &Models,
    cx: &mut AsyncApp,
) -> E {
    loop {
        let next = {
            let event = pin!(events.next_event());
            match select(event, &mut stop).await {
                Either::Left((event, _)) => Some(event),
                // stopped, or the bridge is gone without a shutdown
                Either::Right(_) => None,
            }
        };
        match next {
            Some(Some(event)) => cx.update(|cx| models.apply(event, cx)),
            Some(None) => {
                cx.update(|cx| models.engine_stopped(cx));
                break;
            }
            None => break,
        }
    }
    events
}
