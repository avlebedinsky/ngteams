//! Stand-ins for the engine the view tests run on: a fake event source the test feeds, and a fake core that records
//! every request and answers from its own state. Every id here is made up.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use gpui::{AppContext as _, Entity, TestAppContext};
use teams_core::sync::{CatchUpReport, Synced};
use teams_core::{Action, Answer, EngineError, Event, NameFrom, Person, Reply, Transient, TransientSent};
use teams_store::{
    Conversation, Member, Message, MessagePage, OutboxEntry, OutboxKind, OutboxState, PageCursor, Reaction,
    ReadHorizon, SearchHit, Team,
};
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
    /// Display names by MRI, as their messages carried them.
    pub names: Mutex<HashMap<String, String>>,
    /// People as their profiles have them, by MRI: answered before `names`.
    pub people: Mutex<HashMap<String, Person>>,
    /// People whose answer is held back: made when asked, kept in `held` for the test to send.
    pub hold_people: Mutex<HashSet<String>>,
    /// The answers held back, as they were when asked.
    pub held: Mutex<Vec<(Answer<Person>, Person)>>,
    /// Reactions by message id.
    pub reactions: Mutex<HashMap<String, Vec<Reaction>>>,
    /// The account's read horizons by conversation.
    pub horizons: Mutex<HashMap<String, ReadHorizon>>,
    /// Per conversation, the service's older pages `load_older` brings into `messages`, the next one first (each the
    /// newest first); once none is left it answers `None`. A conversation not named answers `None` at once.
    pub older: Mutex<HashMap<String, Vec<Vec<Message>>>>,
    /// Conversations never synced: `history_complete` answers `None` for them; for any other, whether `older` has
    /// no page left.
    pub unsynced: Mutex<HashSet<String>>,
    /// Per conversation, a catch-up that lands just before `history_complete` answers (between it and a `load_older`
    /// that found nothing): its messages go into `messages` and the conversation is synced, once.
    pub catch_up_before_history_complete: Mutex<HashMap<String, Vec<Message>>>,
    /// The `limit` of each `messages` read, in order.
    pub limits: Mutex<Vec<usize>>,
    /// Every write asked for, in order; each is answered with a pending row of its own.
    pub actions: Mutex<Vec<Action>>,
    /// Every transient asked for, in order; each is answered sent.
    pub transients: Mutex<Vec<Transient>>,
    /// Every row resent, by id, in order.
    pub resent: Mutex<Vec<i64>>,
    /// The outbox rows that aren't sent, of any conversation, the oldest first.
    pub unsettled: Mutex<Vec<OutboxEntry>>,
    /// While set, `outbox_unsettled` isn't answered (as the engine's loop doesn't while it sends): its answers are kept
    /// in `held_unsettled`, never sent.
    pub hold_unsettled: Mutex<bool>,
    pub held_unsettled: Mutex<Vec<Answer<Vec<OutboxEntry>>>>,
    /// While `hold_unsettled` is set, how many `outbox_unsettled` are still answered before the hold applies.
    pub unsettled_before_hold: Mutex<usize>,
    /// The rows earlier runs left.
    pub earlier: Mutex<Vec<OutboxEntry>>,
    /// Every search asked for, its query, in order.
    pub searches: Mutex<Vec<String>>,
    /// The account's teams as the store has them; `None` before a read settled them.
    pub teams: Mutex<Option<Vec<Team>>>,
    /// Per channel, the post each stored message belongs to.
    pub roots: Mutex<HashMap<String, Vec<(String, String)>>>,
    /// Every conversation a sync was asked for, in order.
    pub synced: Mutex<Vec<String>>,
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
            people: Mutex::default(),
            hold_people: Mutex::default(),
            held: Mutex::default(),
            reactions: Mutex::default(),
            horizons: Mutex::default(),
            older: Mutex::default(),
            unsynced: Mutex::default(),
            catch_up_before_history_complete: Mutex::default(),
            limits: Mutex::default(),
            actions: Mutex::default(),
            transients: Mutex::default(),
            resent: Mutex::default(),
            unsettled: Mutex::default(),
            hold_unsettled: Mutex::new(false),
            held_unsettled: Mutex::default(),
            unsettled_before_hold: Mutex::new(0),
            earlier: Mutex::default(),
            searches: Mutex::default(),
            teams: Mutex::default(),
            roots: Mutex::default(),
            synced: Mutex::default(),
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

/// A made-up outbox row `id` of `kind` in `state`, with no conversation and an empty payload.
pub fn outbox_row(id: i64, kind: OutboxKind, state: OutboxState) -> OutboxEntry {
    OutboxEntry {
        id,
        run_id: 1,
        client_message_id: format!("{:019}", 1_000_000_000_000_000_000_i64 + id),
        conversation_id: None,
        kind,
        payload: "{}".to_owned(),
        state,
        attempts: 0,
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

/// The conversation an action writes to.
fn conversation_of(action: &Action) -> Option<String> {
    match action {
        Action::Send { conversation, .. }
        | Action::Edit { conversation, .. }
        | Action::Delete { conversation, .. }
        | Action::React { conversation, .. }
        | Action::Unreact { conversation, .. }
        | Action::Read { conversation, .. } => Some(conversation.clone()),
        Action::CreateChat { .. } => None,
    }
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

    fn person_now(&self, mri: String) -> Person {
        if let Some(person) = locked(&self.people).get(&mri) {
            return person.clone();
        }
        let name = locked(&self.names).get(&mri).cloned();
        Person {
            from: name.as_ref().map(|_| NameFrom::Message),
            name,
            picture: None,
            mri,
        }
    }
}

impl Core for FakeCore {
    fn me(&self) -> &str {
        &self.me
    }

    fn write(&self, action: Action) -> Reply<OutboxEntry> {
        let mut actions = locked(&self.actions);
        actions.push(action.clone());
        let id = i64::try_from(actions.len()).unwrap();
        drop(actions);
        self.answer("write", || {
            let mut row = outbox_row(id, OutboxKind::Send, OutboxState::Pending);
            row.conversation_id = conversation_of(&action);
            row
        })
    }

    fn resend(&self, id: i64) -> Reply<OutboxEntry> {
        locked(&self.resent).push(id);
        self.answer("resend", || outbox_row(id, OutboxKind::Send, OutboxState::Pending))
    }

    fn transient(&self, transient: Transient) -> Reply<TransientSent> {
        locked(&self.transients).push(transient);
        self.answer("transient", || TransientSent::Sent)
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

    fn message(&self, conversation: String, id: String) -> Reply<Option<Message>> {
        self.answer("message", || {
            locked(&self.messages)
                .get(&conversation)
                .and_then(|messages| messages.iter().find(|message| message.id == id).cloned())
        })
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

    fn person(&self, mri: String) -> Reply<Person> {
        if !*locked(&self.stopped) && locked(&self.hold_people).contains(&mri) {
            locked(&self.asked).push("person");
            let (answer, reply) = Reply::later();
            let person = self.person_now(mri);
            locked(&self.held).push((answer, person));
            return reply;
        }
        self.answer("person", || self.person_now(mri))
    }

    fn read_horizon(&self, conversation: String) -> Reply<Option<ReadHorizon>> {
        self.answer("read_horizon", || locked(&self.horizons).get(&conversation).cloned())
    }

    fn history_complete(&self, conversation: String) -> Reply<Option<bool>> {
        self.answer("history_complete", || {
            if let Some(caught_up) = locked(&self.catch_up_before_history_complete).remove(&conversation) {
                locked(&self.messages)
                    .entry(conversation.clone())
                    .or_default()
                    .extend(caught_up);
                locked(&self.unsynced).remove(&conversation);
            }
            if locked(&self.unsynced).contains(&conversation) {
                return None;
            }
            Some(locked(&self.older).get(&conversation).is_none_or(Vec::is_empty))
        })
    }

    fn outbox_entry(&self, _id: i64) -> Reply<Option<OutboxEntry>> {
        self.ask("outbox_entry")
    }

    fn outbox_unsettled(&self, conversation: String) -> Reply<Vec<OutboxEntry>> {
        if !*locked(&self.stopped) && *locked(&self.hold_unsettled) && !counted_down(&self.unsettled_before_hold) {
            locked(&self.asked).push("outbox_unsettled");
            let (answer, reply) = Reply::later();
            locked(&self.held_unsettled).push(answer);
            return reply;
        }
        self.answer("outbox_unsettled", || {
            locked(&self.unsettled)
                .iter()
                .filter(|row| row.conversation_id.as_deref() == Some(conversation.as_str()))
                .cloned()
                .collect()
        })
    }

    fn left_by_earlier_runs(&self) -> Reply<Vec<OutboxEntry>> {
        self.answer("left_by_earlier_runs", || locked(&self.earlier).clone())
    }

    /// The store's search, played on the messages held: a message of a conversation of `within`, not deleted, whose
    /// content holds every term (any case), the newest first, the first term marked in its snippet (the content as it
    /// is).
    fn search(&self, query: String, within: Vec<String>, limit: usize) -> Reply<Vec<SearchHit>> {
        locked(&self.searches).push(query.clone());
        self.answer("search", || {
            let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
            let mut hits: Vec<SearchHit> = locked(&self.messages)
                .values()
                .flatten()
                .filter(|message| within.contains(&message.conversation_id))
                .filter(|message| message.delete_time.is_none())
                .filter(|message| {
                    let content = message.content.to_lowercase();
                    !terms.is_empty() && terms.iter().all(|term| content.contains(term.as_str()))
                })
                .map(|message| SearchHit {
                    conversation_id: message.conversation_id.clone(),
                    message_id: message.id.clone(),
                    arrival_time: message.arrival_time,
                    sender: message.sender.clone(),
                    sender_name: message.sender_name.clone(),
                    snippet: marked(&message.content, &terms[0]),
                })
                .collect();
            hits.sort_by(|one, other| {
                (other.arrival_time, &other.message_id).cmp(&(one.arrival_time, &one.message_id))
            });
            hits.truncate(limit);
            hits
        })
    }

    fn teams(&self) -> Reply<Option<Vec<Team>>> {
        self.answer("teams", || locked(&self.teams).clone())
    }

    fn sync_conversation(&self, conversation: String) -> Reply<Synced> {
        locked(&self.synced).push(conversation);
        self.answer("sync_conversation", Synced::default)
    }

    fn reply_roots(&self, conversation: String) -> Reply<Vec<(String, String)>> {
        self.answer("reply_roots", || {
            locked(&self.roots).get(&conversation).cloned().unwrap_or_default()
        })
    }
}

/// Whether `count` was above zero, taking one off it.
fn counted_down(count: &Mutex<usize>) -> bool {
    let mut count = locked(count);
    let was = *count > 0;
    *count = count.saturating_sub(1);
    was
}

/// `content` with its first match of `term` (lower case; ASCII content) between the store's match marks.
fn marked(content: &str, term: &str) -> String {
    let Some(at) = content.to_lowercase().find(term) else {
        return content.to_owned();
    };
    let end = at + term.len();
    format!(
        "{}{}{}{}{}",
        &content[..at],
        teams_store::search::MATCH_START,
        &content[at..end],
        teams_store::search::MATCH_END,
        &content[end..]
    )
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
