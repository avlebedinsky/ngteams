//! The bridge between GPUI and the engine.
//!
//! The engine runs on a thread and a runtime of its own. Its owner, the [`EngineHandle`], has the one event receiver
//! and the shutdown, so it can't be shared: the [`Bridge`] owns it (as an [`EventSource`]), and one task on GPUI's
//! foreground drains its events into the [`Models`]. Views get only the request side, behind [`Core`] (in the app the
//! engine's [`EngineRequests`]; in a view test a fake), and await each query's [`Reply`] on GPUI's executor.
//!
//! [`Bridge::shutdown`] stops the drain, takes the owner back from it and shuts the engine down on GPUI's background
//! executor, never the foreground ([`EventSource::shutdown`] blocks until the engine's thread is joined). From then on
//! every request answers `Stopped`: nothing more goes out. [`Bridge::sign_out`] does the same once the account's
//! session can't renew its tokens, and shows the account signed out.

use std::pin::pin;
use std::sync::Arc;

use futures_util::future::{Either, select};
use gpui::{AppContext as _, AsyncApp, Context, SharedString, Task};
use teams_core::sync::{CatchUpReport, Synced};
use teams_core::{Action, EngineHandle, EngineRequests, Event, Reply, Transient, TransientSent};
use teams_store::{Conversation, Member, Message, MessagePage, OutboxEntry, PageCursor, Reaction, ReadHorizon};
use tokio::sync::oneshot;

use crate::models::Models;

/// The engine's request side, as the views see it: writes and queries, each answered by a [`Reply`]. No view builds a
/// request of its own; every write goes out through the engine's outbox, under the fence.
pub trait Core: Send + Sync + 'static {
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
    /// The account's own read horizon in a conversation.
    fn read_horizon(&self, conversation: String) -> Reply<Option<ReadHorizon>>;
    /// An outbox row as it now stands.
    fn outbox_entry(&self, id: i64) -> Reply<Option<OutboxEntry>>;
    /// A conversation's outbox rows that aren't sent.
    fn outbox_unsettled(&self, conversation: String) -> Reply<Vec<OutboxEntry>>;
    /// The rows runs that have ended left pending or in flight.
    fn left_by_earlier_runs(&self) -> Reply<Vec<OutboxEntry>>;
}

impl Core for EngineRequests {
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

    fn read_horizon(&self, conversation: String) -> Reply<Option<ReadHorizon>> {
        EngineRequests::read_horizon(self, conversation)
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

/// The engine as the UI holds it (see the module docs). A GPUI entity: [`Bridge::start`] builds it in a
/// `cx.new(...)`.
pub struct Bridge {
    core: Arc<dyn Core>,
    models: Models,
    draining: Option<Draining>,
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
            draining: Some(Draining { stop, task }),
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

    /// Whether the engine is still held (not shut down yet).
    #[must_use]
    pub fn running(&self) -> bool {
        self.draining.is_some()
    }

    /// Stops the drain and shuts the engine down off the foreground; the task ends once the engine's thread is
    /// joined. A second call has nothing left to do.
    pub fn shutdown(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let Some(Draining { stop, task }) = self.draining.take() else {
            return Task::ready(());
        };
        // a drain that already ended (the engine stopped on its own) has dropped its receiver
        let _ = stop.send(());
        let models = self.models.clone();
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            let shutdown = task.await;
            cx.background_spawn(async move { shutdown() }).await;
            cx.update(|cx| models.engine_stopped(cx));
        })
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
