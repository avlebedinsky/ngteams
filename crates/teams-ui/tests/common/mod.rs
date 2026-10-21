//! Stand-ins for the engine the view tests run on: a fake event source the test feeds, and a fake core that records
//! every request and answers from its own state. Every id here is made up.

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use gpui::{AppContext as _, Entity, TestAppContext};
use teams_core::sync::{CatchUpReport, Synced};
use teams_core::{Action, EngineError, Event, Reply, Transient, TransientSent};
use teams_store::{Conversation, Member, Message, MessagePage, OutboxEntry, PageCursor, Reaction, ReadHorizon};
use teams_ui::{Bridge, Core, EventSource, Models};
use tokio::sync::mpsc;

pub const LAB_A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
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

/// The engine's request side: records each request by name, answers the store's reads from its own state (as account
/// a) and everything else `Stopped`; with `stopped` set, every request answers `Stopped`.
pub struct FakeCore {
    pub me: String,
    pub stopped: Mutex<bool>,
    pub asked: Mutex<Vec<&'static str>>,
    pub conversations: Mutex<Vec<Conversation>>,
    /// Per conversation, the newest first.
    pub messages: Mutex<HashMap<String, Vec<Message>>>,
    pub members: Mutex<HashMap<String, Vec<Member>>>,
    pub unread: Mutex<HashMap<String, u64>>,
    /// Display names by MRI.
    pub names: Mutex<HashMap<String, String>>,
    /// Reactions by message id.
    pub reactions: Mutex<HashMap<String, Vec<Reaction>>>,
    /// The account's read horizons by conversation.
    pub horizons: Mutex<HashMap<String, ReadHorizon>>,
    /// Per conversation, the service's older pages `load_older` brings into `messages`, the next one first (each the
    /// newest first); once none is left it answers `None`. A conversation not named answers `None` at once.
    pub older: Mutex<HashMap<String, Vec<Vec<Message>>>>,
    /// The `limit` of each `messages` read, in order.
    pub limits: Mutex<Vec<usize>>,
}

impl Default for FakeCore {
    fn default() -> Self {
        Self {
            me: LAB_A.to_owned(),
            stopped: Mutex::new(false),
            asked: Mutex::default(),
            conversations: Mutex::default(),
            messages: Mutex::default(),
            members: Mutex::default(),
            unread: Mutex::default(),
            names: Mutex::default(),
            reactions: Mutex::default(),
            horizons: Mutex::default(),
            older: Mutex::default(),
            limits: Mutex::default(),
        }
    }
}

/// The store's page of `messages` (the newest first) older than `before`: at most `limit`, with the cursor for the
/// next when more are older.
pub fn page_of(messages: &[Message], before: Option<&PageCursor>, limit: usize) -> MessagePage {
    let mut sorted: Vec<&Message> = messages.iter().collect();
    sorted.sort_by(|one, other| (other.arrival_time, &other.id).cmp(&(one.arrival_time, &one.id)));
    let older: Vec<&Message> = sorted
        .into_iter()
        .filter(|message| {
            before.is_none_or(|cursor| (message.arrival_time, &message.id) < (cursor.arrival_time, &cursor.id))
        })
        .collect();
    let page: Vec<Message> = older.iter().take(limit).map(|message| (*message).clone()).collect();
    let next = (older.len() > limit)
        .then(|| page.last())
        .flatten()
        .map(|last| PageCursor {
            arrival_time: last.arrival_time,
            id: last.id.clone(),
        });
    MessagePage { messages: page, next }
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakeCore {
    fn ask<T>(&self, name: &'static str) -> Reply<T> {
        locked(&self.asked).push(name);
        Reply::ready(Err(EngineError::Stopped))
    }

    /// Records `name` and answers `answer`, unless the core is stopped.
    fn answer<T>(&self, name: &'static str, answer: impl FnOnce() -> T) -> Reply<T> {
        locked(&self.asked).push(name);
        if *locked(&self.stopped) {
            return Reply::ready(Err(EngineError::Stopped));
        }
        Reply::ready(Ok(answer()))
    }

    pub fn asked(&self) -> Vec<&'static str> {
        locked(&self.asked).clone()
    }
}

impl Core for FakeCore {
    fn me(&self) -> &str {
        &self.me
    }

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

    fn load_older(&self, conversation: String) -> Reply<Option<Synced>> {
        self.answer("load_older", || {
            let mut older = locked(&self.older);
            let page = older
                .get_mut(&conversation)
                .filter(|pages| !pages.is_empty())?
                .remove(0);
            let synced = Synced {
                inserted: page.len(),
                pages: 1,
                ..Synced::default()
            };
            locked(&self.messages).entry(conversation).or_default().extend(page);
            Some(synced)
        })
    }

    fn conversations(&self, limit: usize) -> Reply<Vec<Conversation>> {
        // the store's order: the latest activity first
        self.answer("conversations", || {
            let mut stored = locked(&self.conversations).clone();
            stored.sort_by_key(|conversation| std::cmp::Reverse(conversation.last_activity));
            stored.truncate(limit);
            stored
        })
    }

    fn messages(&self, conversation: String, before: Option<PageCursor>, limit: usize) -> Reply<MessagePage> {
        locked(&self.limits).push(limit);
        self.answer("messages", || {
            let messages = locked(&self.messages);
            page_of(
                messages.get(&conversation).map_or(&[][..], Vec::as_slice),
                before.as_ref(),
                limit,
            )
        })
    }

    fn message(&self, _conversation: String, _id: String) -> Reply<Option<Message>> {
        self.ask("message")
    }

    fn reactions(&self, _conversation: String, id: String) -> Reply<Vec<Reaction>> {
        self.answer("reactions", || {
            locked(&self.reactions).get(&id).cloned().unwrap_or_default()
        })
    }

    fn members(&self, conversation: String) -> Reply<Vec<Member>> {
        self.answer("members", || {
            locked(&self.members).get(&conversation).cloned().unwrap_or_default()
        })
    }

    fn unread_count(&self, conversation: String) -> Reply<u64> {
        self.answer("unread_count", || {
            locked(&self.unread).get(&conversation).copied().unwrap_or(0)
        })
    }

    fn display_name(&self, mri: String) -> Reply<Option<String>> {
        self.answer("display_name", || locked(&self.names).get(&mri).cloned())
    }

    fn read_horizon(&self, conversation: String) -> Reply<Option<ReadHorizon>> {
        self.answer("read_horizon", || locked(&self.horizons).get(&conversation).cloned())
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
