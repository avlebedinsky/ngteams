//! Stand-ins for the engine the view tests run on: a fake event source the test feeds, and a fake core that records
//! every request and answers from its own state. Every id here is made up.

use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use gpui::{AppContext as _, Context, Entity, TestAppContext};
use teams_core::sync::{CatchUpReport, Synced};
use teams_core::{Action, EngineError, Event, Reply, Transient, TransientSent};
use teams_store::{
    Conversation, ConversationKind, Member, Message, MessagePage, OutboxEntry, OutboxKind, OutboxState, PageCursor,
    Reaction, ReadHorizon,
};
use teams_ui::{Bridge, Core, EventSource, Models};
use tokio::sync::mpsc;

pub const CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
pub const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";

/// The engine's owning side, fed by the test; counts its shutdowns.
pub struct FakeEvents {
    events: mpsc::UnboundedReceiver<Event>,
    shutdowns: Arc<AtomicUsize>,
}

impl EventSource for FakeEvents {
    fn next_event(&mut self) -> impl Future<Output = Option<Event>> {
        self.events.recv()
    }

    fn shutdown(self) {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
    }
}

/// The engine's request side: records each request by name, answers the conversation list from `conversations`
/// and everything else `Stopped`.
#[derive(Default)]
pub struct FakeCore {
    pub asked: Mutex<Vec<&'static str>>,
    pub conversations: Mutex<Vec<Conversation>>,
}

impl FakeCore {
    fn ask<T>(&self, name: &'static str) -> Reply<T> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner).push(name);
        Reply::ready(Err(EngineError::Stopped))
    }

    pub fn asked(&self) -> Vec<&'static str> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Core for FakeCore {
    fn write(&self, _action: Action) -> Reply<OutboxEntry> {
        self.ask("write")
    }

    fn resend(&self, _id: i64) -> Reply<OutboxEntry> {
        self.ask("resend")
    }

    fn transient(&self, _transient: Transient) -> Reply<TransientSent> {
        self.ask("transient")
    }

    fn sync(&self) -> Reply<CatchUpReport> {
        self.ask("sync")
    }

    fn load_older(&self, _conversation: String) -> Reply<Option<Synced>> {
        self.ask("load_older")
    }

    fn conversations(&self, limit: usize) -> Reply<Vec<Conversation>> {
        self.asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("conversations");
        let stored = self.conversations.lock().unwrap_or_else(PoisonError::into_inner);
        Reply::ready(Ok(stored.iter().take(limit).cloned().collect()))
    }

    fn messages(&self, _conversation: String, _before: Option<PageCursor>, _limit: usize) -> Reply<MessagePage> {
        self.ask("messages")
    }

    fn message(&self, _conversation: String, _id: String) -> Reply<Option<Message>> {
        self.ask("message")
    }

    fn reactions(&self, _conversation: String, _id: String) -> Reply<Vec<Reaction>> {
        self.ask("reactions")
    }

    fn members(&self, _conversation: String) -> Reply<Vec<Member>> {
        self.ask("members")
    }

    fn unread_count(&self, _conversation: String) -> Reply<u64> {
        self.ask("unread_count")
    }

    fn read_horizon(&self, _conversation: String) -> Reply<Option<ReadHorizon>> {
        self.ask("read_horizon")
    }

    fn outbox_entry(&self, _id: i64) -> Reply<Option<OutboxEntry>> {
        self.ask("outbox_entry")
    }

    fn outbox_unsettled(&self, _conversation: String) -> Reply<Vec<OutboxEntry>> {
        self.ask("outbox_unsettled")
    }

    fn left_by_earlier_runs(&self) -> Reply<Vec<OutboxEntry>> {
        self.ask("left_by_earlier_runs")
    }
}

/// A bridge over fakes, as a test holds it.
pub struct Rig {
    pub cx: TestAppContext,
    pub events: mpsc::UnboundedSender<Event>,
    pub shutdowns: Arc<AtomicUsize>,
    pub core: Arc<FakeCore>,
    pub models: Models,
    pub bridge: Entity<Bridge>,
}

impl Rig {
    pub fn start() -> Self {
        let cx = TestAppContext::single();
        let (events, received) = mpsc::unbounded_channel();
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let source = FakeEvents {
            events: received,
            shutdowns: Arc::clone(&shutdowns),
        };
        let core = Arc::new(FakeCore::default());
        let models = cx.update(Models::new);
        let shared: Arc<dyn Core> = core.clone();
        let drained = models.clone();
        let bridge = cx.update(|cx| cx.new(|cx| Bridge::start(source, shared, drained, cx)));
        Self {
            cx,
            events,
            shutdowns,
            core,
            models,
            bridge,
        }
    }

    /// Hands the bridge an event and lets the drain apply it.
    pub fn send(&self, event: Event) {
        self.events.send(event).expect("the drain is listening");
        self.cx.run_until_parked();
    }

    pub fn shutdowns(&self) -> usize {
        self.shutdowns.load(Ordering::SeqCst)
    }
}

/// A view that counts how often a model told it to look again.
pub struct Watcher {
    pub seen: usize,
    _observing: gpui::Subscription,
}

impl Watcher {
    pub fn on<T: 'static>(model: &Entity<T>, cx: &mut Context<Self>) -> Self {
        Self {
            seen: 0,
            _observing: cx.observe(model, |this, _, _| this.seen += 1),
        }
    }
}

/// What a trace wrote, shared with the test.
#[derive(Clone, Default)]
pub struct Written(pub Arc<Mutex<Vec<u8>>>);

impl Written {
    pub fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()).expect("UTF-8")
    }

    pub fn lines(&self) -> Vec<serde_json::Value> {
        self.text()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    }
}

impl Write for Written {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn conversation(id: &str) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind: ConversationKind::OneToOne,
        topic: None,
        version: 1,
        last_activity: Some(1_767_225_600_000),
    }
}

/// A message of `CHAT` from b, with `text` and b's name `name`.
pub fn message(id: &str, text: &str, name: &str) -> Message {
    Message {
        conversation_id: CHAT.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: Some("1234567890123456789".to_owned()),
        sender: Some(LAB_B.to_owned()),
        sender_name: Some(name.to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: format!("<p>{text}</p>"),
        properties: None,
        arrival_time: 1_767_225_600_000,
        compose_time: Some(1_767_225_600_000),
        edit_time: None,
        delete_time: None,
    }
}

pub fn outbox_row(id: i64, state: OutboxState) -> OutboxEntry {
    OutboxEntry {
        id,
        run_id: 1,
        client_message_id: "1234567890123456789".to_owned(),
        conversation_id: Some(CHAT.to_owned()),
        kind: OutboxKind::Send,
        payload: "{}".to_owned(),
        state,
        attempts: 1,
        attempts_at_resend: 0,
        failed_checks: 0,
        not_before: None,
        last_error: None,
        message_id: None,
        created_at: 1_767_225_600_000,
        updated_at: 1_767_225_600_000,
        unreadable_seen_at: None,
    }
}
