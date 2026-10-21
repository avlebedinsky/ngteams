//! The catch-up sync on a real store (a temporary file) against the mock chat service (`teams-mock`, reached through
//! the `test-support` mock origin): the first catch-up fills the store; one after a gap brings what was missed, once;
//! Trouter deliveries (here the mock's queued events, the resources a delivery carries) and the sync agree; a
//! reconnect triggers a catch-up; the outbox's check after a lost answer finds the message by its `clientmessageid`.
//! A few shapes the mock doesn't serve (versions that can't be read, a sync answer with a `backwardLink`) are played
//! on plain wiremock.
//!
//! The other lab accounts write straight to the mock with plain HTTP, as their own clients would. Every id and token
//! here is the mock's made-up one, or a placeholder, never a real one.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::collections::VecDeque;
use std::future::{self, Future};
use std::io;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::{CatchUp, CatchUpError, Command, Gaps, MAX_PAGES, Outbox, enqueue, outbox::ConversationSync};
use teams_fence::{Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, Roster, Scope};
use teams_mock::{
    AB_CHAT, ABC_CHAT, AC_CHAT, AX_CHAT, CHANNEL, ChatEvent, ChatServiceMock, EventKind, LAB_A, LAB_B, LAB_C,
    NOTIFICATIONS,
};
use teams_proto::chat;
use teams_proto::{
    Authorized, Client, Credentials, Failure, HttpError, HttpTransport, Mutating, OwnState, Read, Response, Transport,
};
use teams_store::{Applied, Conversation, ConversationKind, Member, Message, OutboxState, Store};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CHAT_BASE: &str = teams_mock::CHAT_BASE;
const START: i64 = 1_760_100_000_000;

// --- fakes ---

/// The mock's token for one account, the same after a rejection.
struct MockToken(String);

impl Credentials for MockToken {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }
}

/// Every chat's members as the fence reads them: the lab accounts a and b (the outbox writes only to the a–b chat).
struct LabMembers;

impl MembershipSource for LabMembers {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(future::ready(Membership::Complete(vec![
            Mri::new(LAB_A),
            Mri::new(LAB_B),
        ])))
    }
}

/// What the next write's delivery does instead of an ordinary one.
#[derive(Clone, Copy, Debug)]
enum Fault {
    /// The post reaches the service, but its answer is lost: a timeout.
    LoseAnswer,
    /// The post times out before it reaches the service.
    DropRequest,
}

/// The HTTP transport with scripted faults on its writes.
struct Flaky {
    inner: HttpTransport<MockToken>,
    faults: Arc<Mutex<VecDeque<Fault>>>,
}

impl Transport for Flaky {
    type Error = HttpError;

    fn read<R: Read>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        self.inner.read(request)
    }

    fn own_state<R: OwnState>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        self.inner.own_state(request)
    }

    async fn deliver<R: Mutating + Send>(&self, write: Authorized<R>) -> Result<Response, HttpError> {
        let fault = self.faults.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
        match fault {
            None => self.inner.deliver(write).await,
            Some(Fault::LoseAnswer) => {
                self.inner.deliver(write).await?;
                Err(HttpError::Failed(Failure::Timeout))
            }
            Some(Fault::DropRequest) => Err(HttpError::Failed(Failure::Timeout)),
        }
    }
}

// --- setup ---

fn lab_fence() -> Arc<Fence> {
    Arc::new(Fence::new(
        Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
        FenceLog::from_writer(io::sink()),
    ))
}

fn transport(origin: &str, token: &str) -> HttpTransport<MockToken> {
    HttpTransport::with_mock_origin(Arc::new(MockToken(token.to_owned())), lab_fence(), CHAT_BASE, origin).unwrap()
}

/// The outbox of the account `me` (the one place that builds one).
fn outbox(transport: Flaky, me: &str) -> Outbox<Flaky> {
    Outbox::new(Arc::new(transport), lab_fence(), Arc::new(LabMembers), Mri::new(me))
}

/// A message post as a command (the one place that builds one).
fn send(conversation: &str, text: &str) -> Command {
    Command::Send {
        conversation: conversation.to_owned(),
        content: format!("<p>{text}</p>"),
    }
}

/// One native client's account: its store, its outbox and its catch-up, against one origin.
struct Account {
    store: Store,
    outbox: Arc<Outbox<Flaky>>,
    catch_up: CatchUp<MockToken, Flaky>,
    faults: Arc<Mutex<VecDeque<Fault>>>,
    _dir: tempfile::TempDir,
}

impl Account {
    fn on(origin: &str, me: &str, token: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("store").join("lab.sqlite")).unwrap();
        let faults = Arc::new(Mutex::new(VecDeque::new()));
        let flaky = Flaky {
            inner: transport(origin, token),
            faults: Arc::clone(&faults),
        };
        let outbox = Arc::new(outbox(flaky, me));
        let client = Arc::new(Client::new(transport(origin, token)));
        let clock = Arc::new(AtomicI64::new(START));
        let catch_up = CatchUp::new(client, Arc::clone(&outbox), Mri::new(me))
            .with_clock(Arc::new(move || clock.fetch_add(1, Ordering::SeqCst)));
        Self {
            store,
            outbox,
            catch_up,
            faults,
            _dir: dir,
        }
    }

    fn of(mock: &ChatServiceMock, me: &str) -> Self {
        Self::on(&mock.origin(), me, &mock.token(me).unwrap())
    }

    fn fault(&self, fault: Fault) {
        self.faults
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(fault);
    }

    /// Every stored message of a conversation, the newest first.
    fn messages(&self, conversation: &str) -> Vec<Message> {
        self.store.messages_before(conversation, None, 1000).unwrap().messages
    }

    fn message(&self, conversation: &str, id: &str) -> Message {
        self.store.message(conversation, id).unwrap().unwrap()
    }

    /// Stores the resource of each event, as a Trouter delivery of it would; returns what the store did with each.
    fn deliver(&mut self, events: &[ChatEvent]) -> Vec<Applied> {
        events
            .iter()
            .map(|event| {
                let message: chat::Message = serde_json::from_value(event.resource.clone()).unwrap();
                let received = self
                    .catch_up
                    .receive_message(&mut self.store, &event.conversation, &message)
                    .unwrap();
                received.unwrap().applied
            })
            .collect()
    }
}

/// Another lab account's own client, writing straight to the mock with the web client's headers.
struct Writer {
    http: reqwest::Client,
    origin: String,
    token: String,
}

impl Writer {
    fn of(mock: &ChatServiceMock, who: &str) -> Self {
        Self {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            origin: mock.origin(),
            token: mock.token(who).unwrap(),
        }
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: &Value) -> reqwest::Response {
        let answer = self
            .http
            .request(method, format!("{}/api/chatsvc/amer/v1/{path}", self.origin))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("X-MS-Migration", "True")
            .header("BehaviorOverride", "redirectAs404")
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        assert!(answer.status().is_success(), "{path}: {}", answer.status());
        answer
    }

    /// Posts a message to `chat`; returns its id.
    async fn post(&self, chat: &str, text: &str) -> String {
        let body = json!({
            "content": format!("<p>{text}</p>"),
            "messagetype": "RichText/Html",
            "contenttype": "Text",
            "clientmessageid": format!("9{}", text.len()),
            "imdisplayname": "Lab",
            "properties": {"mentions": "[]", "files": "[]"},
        });
        let answer = self
            .send(
                reqwest::Method::POST,
                &format!("{}/messages", conversation(chat)),
                &body,
            )
            .await;
        let posted: Value = serde_json::from_str(&answer.text().await.unwrap()).unwrap();
        posted["OriginalArrivalTime"].to_string()
    }

    async fn edit(&self, chat: &str, id: &str, text: &str) {
        let body = json!({"content": format!("<p>{text}</p>"), "messagetype": "RichText/Html"});
        self.send(
            reqwest::Method::PUT,
            &format!("{}/messages/{id}", conversation(chat)),
            &body,
        )
        .await;
    }

    async fn react(&self, chat: &str, id: &str, key: &str) {
        let path = format!("{}/messages/{id}/properties?name=emotions", conversation(chat));
        let body = json!({"emotions": {"key": key, "value": 1_760_000_055_000_u64}});
        self.send(reqwest::Method::PUT, &path, &body).await;
    }
}

/// A conversation's path, its id one segment.
fn conversation(chat: &str) -> String {
    format!(
        "users/ME/conversations/{}",
        chat.replace(':', "%3A").replace('@', "%40")
    )
}

/// The path (after the chat base's `v1`) and the `syncState` of every request the mock got since `from`.
async fn reads_since(mock: &ChatServiceMock, from: usize) -> Vec<String> {
    mock.received_requests()
        .await
        .iter()
        .skip(from)
        .map(|request| {
            let path = request
                .url
                .path()
                .trim_start_matches("/api/chatsvc/amer/v1/")
                .to_owned();
            let sync = request
                .url
                .query_pairs()
                .find(|(name, _)| name == "syncState")
                .map(|(_, value)| format!(" since {value}"))
                .unwrap_or_default();
            format!("{} {path}{sync}", request.method)
        })
        .collect()
}

fn encoded(chat: &str) -> String {
    chat.replace(':', "%3A").replace('@', "%40")
}

// --- the first catch-up ---

#[tokio::test]
async fn the_first_catch_up_fills_the_store() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!((report.conversations, report.skipped), (6, 0));

    let kind = |id: &str| a.store.conversation(id).unwrap().unwrap().kind;
    assert_eq!(kind(AB_CHAT), ConversationKind::OneToOne);
    assert_eq!(kind(AC_CHAT), ConversationKind::OneToOne);
    assert_eq!(kind(AX_CHAT), ConversationKind::OneToOne);
    assert_eq!(kind(ABC_CHAT), ConversationKind::Group);
    assert_eq!(kind(CHANNEL), ConversationKind::Channel);
    assert_eq!(kind(NOTIFICATIONS), ConversationKind::Other("notifications".to_owned()));

    // the a–b chat's messages, each as the service has it now
    let messages = a.messages(AB_CHAT);
    assert_eq!(messages.len(), 5);
    assert_eq!(
        messages.len() + a.messages(NOTIFICATIONS).len(),
        report.messages.inserted
    );
    let deleted = &messages[0];
    assert!(deleted.delete_time.is_some() && deleted.content.is_empty());
    assert_eq!(messages[2].edit_time, Some(1_760_000_020_000));
    assert_eq!(messages[4].message_type, "ThreadActivity/AddMember");
    assert!(messages[1].client_message_id.is_some() && messages[1].sender.is_some());
    let page = a.store.conversations_by_activity(10).unwrap();
    assert_eq!(page[0].id, AB_CHAT, "the most recent activity first");

    // members of the chats, none of the read-only conversations
    let mris = |id: &str| -> Vec<String> { a.store.members(id).unwrap().into_iter().map(|m| m.mri).collect() };
    assert_eq!(mris(AB_CHAT), [LAB_A, LAB_B]);
    assert_eq!(mris(ABC_CHAT), [LAB_A, LAB_B, LAB_C]);
    assert!(mris(CHANNEL).is_empty() && mris(NOTIFICATIONS).is_empty());

    // a's own read horizon, and where every sync stands
    let horizon = a.store.read_horizon(AB_CHAT, LAB_A).unwrap().unwrap();
    assert!(horizon.raw.is_some() && horizon.last_read_arrival > 0);
    for id in [AB_CHAT, AC_CHAT, AX_CHAT, ABC_CHAT, CHANNEL, NOTIFICATIONS] {
        let state = a.store.sync_state(id).unwrap().unwrap();
        assert!(state.sync_link.is_some() && state.history_complete, "{id}: {state:?}");
    }
    assert!(a.store.conversation_list_sync_link().unwrap().is_some());

    // nothing changed: the next catch-up reads the list's sync link and nothing else
    let before = mock.received_requests().await.len();
    let again = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert_eq!((again.conversations, again.messages.changed()), (0, 0));
    let reads = reads_since(&mock, before).await;
    assert_eq!(reads.len(), 1, "{reads:?}");
    assert!(reads[0].starts_with(&format!("GET users/{LAB_A}/conversations since mock-since-")));
}

#[tokio::test]
async fn older_history_is_read_a_page_at_a_time_from_the_kept_backward_link() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut b = Account::of(&mock, LAB_B);
    b.catch_up = b.catch_up.with_page_size(2);
    let first = b.catch_up.sync_messages(&mut b.store, AB_CHAT).await.unwrap();
    assert_eq!((first.inserted, first.pages), (2, 1));
    let state = b.store.sync_state(AB_CHAT).unwrap().unwrap();
    assert!(!state.history_complete && state.backward_link.is_some());

    let mut loaded = 0;
    while let Some(older) = b.catch_up.load_older(&mut b.store, AB_CHAT).await.unwrap() {
        loaded += older.inserted;
    }
    assert_eq!(loaded, 3);
    assert_eq!(b.messages(AB_CHAT).len(), 5);
    let done = b.store.sync_state(AB_CHAT).unwrap().unwrap();
    assert!(done.history_complete && done.backward_link.is_none());
    assert_eq!(done.sync_link, state.sync_link, "paging back keeps the sync link");
    assert!(b.catch_up.load_older(&mut b.store, ABC_CHAT).await.unwrap().is_none());
}

#[tokio::test]
async fn a_conversation_the_store_does_not_know_is_read_as_a_thread_first() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut c = Account::of(&mock, LAB_C);
    let synced = c.catch_up.sync_messages(&mut c.store, ABC_CHAT).await.unwrap();
    assert_eq!(synced.changed(), 0);
    let stored = c.store.conversation(ABC_CHAT).unwrap().unwrap();
    assert_eq!((stored.kind, stored.version), (ConversationKind::Group, 0));
    assert_eq!(
        c.store.members(ABC_CHAT).unwrap(),
        [LAB_A, LAB_B, LAB_C].map(|mri| Member::new(mri, Some("Admin")))
    );
    // the list's version replaces the thread's placeholder
    c.catch_up.catch_up(&mut c.store).await.unwrap();
    assert!(c.store.conversation(ABC_CHAT).unwrap().unwrap().version > 0);
}

// --- a gap ---

#[tokio::test]
async fn a_catch_up_after_a_gap_brings_what_was_missed_exactly_once() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    a.catch_up.catch_up(&mut a.store).await.unwrap();
    let edited_before = a.message(AB_CHAT, "1760000010000");

    // a's push channel is down: b and c write meanwhile
    let b = Writer::of(&mock, LAB_B);
    let c = Writer::of(&mock, LAB_C);
    let first = b.post(AB_CHAT, "one").await;
    b.post(AB_CHAT, "two two").await;
    c.post(ABC_CHAT, "to the group").await;
    b.edit(AB_CHAT, &first, "one, edited").await;
    b.react(AB_CHAT, &edited_before.id, "heart").await;

    let before = mock.received_requests().await.len();
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.conversations, 2, "the two chats that changed");
    assert_eq!((report.messages.inserted, report.messages.updated), (3, 1));
    let reads = reads_since(&mock, before).await;
    assert_eq!(reads.len(), 3, "the list and the two chats' sync links: {reads:?}");
    assert!(
        reads[1..]
            .iter()
            .all(|read| read.contains("/messages since mock-since-")),
        "{reads:?}"
    );

    let messages = a.messages(AB_CHAT);
    assert_eq!(messages.len(), 7, "each message once");
    assert_eq!(a.message(AB_CHAT, &first).content, "<p>one, edited</p>");
    assert_eq!(
        a.store
            .reactions(AB_CHAT, &edited_before.id)
            .unwrap()
            .iter()
            .map(|r| (r.key.as_str(), r.user.as_str()))
            .collect::<Vec<_>>(),
        [("heart", LAB_B)]
    );
    assert_eq!(a.messages(ABC_CHAT).len(), 1);
    assert_eq!(a.store.unread_count(ABC_CHAT, LAB_A).unwrap(), 1);

    // caught up: the next one brings nothing
    let again = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert_eq!((again.conversations, again.messages.changed()), (0, 0));
    assert_eq!(a.messages(AB_CHAT).len(), 7);
}

#[tokio::test]
async fn a_conversation_that_fails_to_sync_is_listed_again_by_the_next_catch_up() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    a.catch_up.catch_up(&mut a.store).await.unwrap();
    let list_link = a.store.conversation_list_sync_link().unwrap();
    Writer::of(&mock, LAB_B).post(AB_CHAT, "missed").await;

    // the chat's sync link is one the service doesn't take: its sync fails, the list's link stays
    let mut state = a.store.sync_state(AB_CHAT).unwrap().unwrap();
    let good = state.sync_link.replace(format!(
        "{CHAT_BASE}/v1/users/ME/conversations/{}/messages?syncState=not-the-mocks",
        encoded(AB_CHAT)
    ));
    a.store.set_sync_state(&state).unwrap();
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].conversation, AB_CHAT);
    assert!(
        matches!(report.failed[0].error, CatchUpError::Read(_)),
        "{:?}",
        report.failed[0].error
    );
    assert_eq!(a.store.conversation_list_sync_link().unwrap(), list_link);
    assert_eq!(a.messages(AB_CHAT).len(), 5);

    // with the link back, the next catch-up lists the chat again and brings the message
    state.sync_link = good;
    a.store.set_sync_state(&state).unwrap();
    let report = a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert!(report.failed.is_empty());
    assert_eq!(report.messages.inserted, 1);
    assert_eq!(a.messages(AB_CHAT).len(), 6);
}

// --- deliveries and the sync ---

#[tokio::test]
async fn trouter_deliveries_and_the_sync_agree() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut b = Account::of(&mock, LAB_B);
    b.catch_up.catch_up(&mut b.store).await.unwrap();

    let a = Writer::of(&mock, LAB_A);
    let id = a.post(AB_CHAT, "hi b").await;
    a.edit(AB_CHAT, &id, "hi b, edited").await;
    a.react(AB_CHAT, &id, "like").await;
    let events = mock.take_events(LAB_B);
    let kinds: Vec<EventKind> = events.iter().map(|event| event.kind).collect();
    assert_eq!(kinds, [EventKind::Message, EventKind::Edit, EventKind::Reaction]);

    // delivered first, synced after: each version once, the sync changes nothing
    assert_eq!(
        b.deliver(&events),
        [Applied::Inserted, Applied::Updated, Applied::Updated]
    );
    let report = b.catch_up.catch_up(&mut b.store).await.unwrap();
    assert_eq!((report.messages.changed(), report.messages.unchanged), (0, 1));
    assert_eq!(b.messages(AB_CHAT).len(), 6);
    let stored = b.message(AB_CHAT, &id);
    assert_eq!(stored.content, "<p>hi b, edited</p>");
    assert_eq!(b.store.reactions(AB_CHAT, &id).unwrap().len(), 1);

    // a late delivery of an older version changes nothing
    assert_eq!(b.deliver(&events[..2]), [Applied::Ignored, Applied::Ignored]);
    assert_eq!(b.message(AB_CHAT, &id), stored);

    // synced first, delivered after: the same rows
    let second = a.post(AB_CHAT, "again").await;
    let events = mock.take_events(LAB_B);
    let report = b.catch_up.catch_up(&mut b.store).await.unwrap();
    assert_eq!(report.messages.inserted, 1);
    assert_eq!(b.deliver(&events), [Applied::Ignored]);
    assert_eq!(b.messages(AB_CHAT).len(), 7);
    assert_eq!(b.message(AB_CHAT, &second).content, "<p>again</p>");
}

// --- when ---

#[tokio::test]
async fn a_reconnect_triggers_a_catch_up() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    let mut gaps = Gaps::new();

    // on start, before the channel is up: a catch-up, and another stays due for when it connects
    let started = a.catch_up.catch_up_if_due(&mut a.store, &mut gaps).await.unwrap();
    assert!(started.is_some() && gaps.is_due());
    gaps.connected();
    assert!(
        a.catch_up
            .catch_up_if_due(&mut a.store, &mut gaps)
            .await
            .unwrap()
            .is_some()
    );
    assert!(!gaps.is_due());

    // up, and caught up: none is due, nothing is read
    let before = mock.received_requests().await.len();
    assert!(
        a.catch_up
            .catch_up_if_due(&mut a.store, &mut gaps)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(mock.received_requests().await.len(), before);

    // the channel drops, b writes meanwhile, the channel is back: the reconnect brings b's message
    gaps.disconnected();
    let id = Writer::of(&mock, LAB_B).post(AB_CHAT, "while you were away").await;
    gaps.connected();
    let report = a
        .catch_up
        .catch_up_if_due(&mut a.store, &mut gaps)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.messages.inserted, 1);
    assert_eq!(a.message(AB_CHAT, &id).content, "<p>while you were away</p>");
    assert!(
        a.catch_up
            .catch_up_if_due(&mut a.store, &mut gaps)
            .await
            .unwrap()
            .is_none()
    );

    // a catch-up that fails keeps one due
    gaps.disconnected();
    gaps.connected();
    assert!(mock.set_token(LAB_A, "mock-ic3-token-replaced"));
    assert!(a.catch_up.catch_up_if_due(&mut a.store, &mut gaps).await.is_err());
    assert!(gaps.is_due());
}

// --- the outbox's check ---

#[tokio::test]
async fn the_outbox_check_finds_a_message_whose_answer_was_lost() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    a.catch_up.catch_up(&mut a.store).await.unwrap();

    a.fault(Fault::LoseAnswer);
    let row = enqueue(&mut a.store, &send(AB_CHAT, "did it arrive")).unwrap();
    let sent = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap();
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(
        posts(&mock).await,
        std::slice::from_ref(&row.client_message_id),
        "posted once"
    );
    // found by the sync, stored once, matched to the row
    let found = a
        .store
        .message_by_client_id(AB_CHAT, &row.client_message_id)
        .unwrap()
        .unwrap();
    assert_eq!(sent.message_id.as_deref(), Some(found.id.as_str()));
    assert_eq!(found.sender.as_deref(), Some(LAB_A));
    assert_eq!(a.messages(AB_CHAT).len(), 6);
}

#[tokio::test]
async fn the_outbox_check_sends_again_with_the_same_id_when_the_message_did_not_arrive() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    a.catch_up.catch_up(&mut a.store).await.unwrap();

    a.fault(Fault::DropRequest);
    let row = enqueue(&mut a.store, &send(AB_CHAT, "second try")).unwrap();
    let sent = a.outbox.send(&mut a.store, &a.catch_up, row.id).await.unwrap();
    assert_eq!((sent.state, sent.attempts), (OutboxState::Sent, 2));
    assert_eq!(
        posts(&mock).await,
        std::slice::from_ref(&row.client_message_id),
        "the second attempt only"
    );

    // the post's answer and the sync agree: one message
    a.catch_up.catch_up(&mut a.store).await.unwrap();
    assert_eq!(a.messages(AB_CHAT).len(), 6);
    let stored = a
        .store
        .message_by_client_id(AB_CHAT, &row.client_message_id)
        .unwrap()
        .unwrap();
    assert_eq!(sent.message_id.as_deref(), Some(stored.id.as_str()));
}

#[tokio::test]
async fn the_check_reads_the_service_and_a_failed_read_is_an_error() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mut a = Account::of(&mock, LAB_A);
    a.catch_up.catch_up(&mut a.store).await.unwrap();
    let before = mock.received_requests().await.len();
    a.catch_up.sync_conversation(&mut a.store, AB_CHAT).await.unwrap();
    assert_eq!(
        reads_since(&mock, before).await.len(),
        1,
        "read from the service, not the store"
    );
    assert!(mock.set_token(LAB_A, "mock-ic3-token-replaced"));
    assert!(a.catch_up.sync_conversation(&mut a.store, AB_CHAT).await.is_err());
}

/// The `clientmessageid` of every message post the mock got, in order.
async fn posts(mock: &ChatServiceMock) -> Vec<String> {
    mock.received_requests()
        .await
        .iter()
        .filter(|request| request.method.as_str() == "POST" && request.url.path().ends_with("/messages"))
        .map(|request| {
            let body: Value = request.body_json().unwrap();
            body["clientmessageid"].as_str().unwrap_or_default().to_owned()
        })
        .collect()
}

// --- shapes the mock doesn't serve, on plain wiremock ---

const CHAT: &str = "19:00000000000000000000000000000def@thread.v2";

/// A plain service whose message reads answer by their `syncState`: none for the first read, else the token.
async fn scripted(pages: Vec<(&str, Value)>) -> MockServer {
    let server = MockServer::start().await;
    let pages: Vec<(String, Value)> = pages
        .into_iter()
        .map(|(token, page)| (token.to_owned(), page))
        .collect();
    Mock::given(wiremock::matchers::method("GET"))
        .respond_with(move |request: &Request| {
            let token = request
                .url
                .query_pairs()
                .find(|(name, _)| name == "syncState")
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default();
            pages.iter().find(|(expected, _)| *expected == token).map_or_else(
                || ResponseTemplate::new(404),
                |(_, page)| ResponseTemplate::new(200).set_body_json(page),
            )
        })
        .mount(&server)
        .await;
    server
}

fn link(token: &str) -> String {
    format!(
        "{CHAT_BASE}/v1/users/ME/conversations/{}/messages?syncState={token}&view=msnp24Equivalent",
        encoded(CHAT)
    )
}

fn entry(id: u64, version: Value) -> Value {
    let mut entry = json!({
        "id": id.to_string(),
        "sequenceId": id - 1000,
        "version": Value::Null,
        "conversationid": CHAT,
        "messagetype": "RichText/Html",
        "content": format!("<p>{id}</p>"),
        "from": format!("{CHAT_BASE}/v1/users/ME/contacts/{LAB_B}"),
        "originalarrivaltime": "2025-10-09T08:43:20.0000000Z",
    });
    entry["version"] = version;
    entry
}

fn page(messages: Vec<Value>, sync: Option<&str>, backward: Option<&str>) -> Value {
    let mut metadata = serde_json::Map::new();
    if let Some(token) = sync {
        metadata.insert("syncState".to_owned(), json!(link(token)));
    }
    if let Some(token) = backward {
        metadata.insert("backwardLink".to_owned(), json!(link(token)));
    }
    json!({"messages": Value::Array(messages), "_metadata": metadata})
}

fn plain_account(server: &MockServer) -> Account {
    let mut account = Account::on(&server.uri(), LAB_A, "placeholder-token");
    let chat = Conversation {
        id: CHAT.to_owned(),
        kind: ConversationKind::Group,
        topic: None,
        version: 1,
        last_activity: None,
    };
    account.store.upsert_conversation(&chat).unwrap();
    account
}

#[tokio::test]
async fn a_message_whose_version_cannot_be_read_is_not_stored() {
    let mut other = entry(1004, json!("1004"));
    other["conversationid"] = json!("19:another@thread.v2");
    let mut timeless = entry(1005, json!("1005"));
    timeless["id"] = json!("not-a-time");
    timeless.as_object_mut().unwrap().remove("originalarrivaltime");
    let mut odd_reactions = entry(1006, json!("1006"));
    odd_reactions["properties"] = json!({"emotions": [{"key": "like", "users": [{"mri": LAB_C, "time": "soon"}]}]});
    let server = scripted(vec![(
        "",
        page(
            vec![
                entry(1001, json!("1001")),
                entry(1002, json!("v2")),
                entry(1003, Value::Null),
                {
                    let mut unversioned = entry(1007, Value::Null);
                    unversioned.as_object_mut().unwrap().remove("version");
                    unversioned
                },
                other,
                timeless,
                odd_reactions,
                json!({"no": "id"}),
            ],
            Some("s1"),
            None,
        ),
    )])
    .await;
    let mut a = plain_account(&server);
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!((synced.inserted, synced.skipped), (2, 7), "{synced:?}");
    let stored: Vec<String> = a.messages(CHAT).into_iter().map(|message| message.id).collect();
    assert_eq!(stored, ["1006", "1001"]);
    assert!(
        a.store.reactions(CHAT, "1006").unwrap().is_empty(),
        "a set it can't read is left out"
    );
}

#[tokio::test]
async fn an_unreadable_version_never_replaces_a_stored_one() {
    let server = scripted(vec![
        ("", page(vec![entry(1001, json!("5"))], Some("s1"), None)),
        ("s1", page(vec![entry(1001, json!("later, maybe"))], Some("s2"), None)),
    ])
    .await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    let stored = a.message(CHAT, "1001");
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!((synced.changed(), synced.skipped), (0, 1));
    assert_eq!(a.message(CHAT, "1001"), stored);
}

#[tokio::test]
async fn a_sync_answer_with_a_backward_link_is_read_back_to_what_the_store_had() {
    let known = entry(1001, json!("1001"));
    let server = scripted(vec![
        ("", page(vec![known.clone()], Some("s1"), None)),
        // what changed: more than a page, the older part behind a backward link
        (
            "s1",
            page(
                vec![entry(1005, json!("1005")), entry(1004, json!("1004"))],
                Some("s2"),
                Some("b1"),
            ),
        ),
        (
            "b1",
            page(vec![entry(1003, json!("1003")), known.clone()], Some("s2"), Some("b2")),
        ),
        ("b2", page(vec![known.clone()], Some("s2"), Some("b3"))),
        ("b3", page(vec![entry(1002, json!("1002"))], Some("s2"), None)),
    ])
    .await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!((synced.inserted, synced.unchanged, synced.pages), (3, 2, 3));
    let tokens: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter_map(|request| {
            request
                .url
                .query_pairs()
                .find(|(name, _)| name == "syncState")
                .map(|(_, value)| value.into_owned())
        })
        .collect();
    assert_eq!(tokens, ["s1", "b1", "b2"], "b2 brought nothing new, so b3 isn't read");
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.sync_link, Some(link("s2")));
    assert!(state.history_complete && state.backward_link.is_none());
}

#[tokio::test]
async fn a_backward_walk_past_the_page_cap_leaves_the_rest_as_older_history() {
    let tokens: Vec<String> = (0..=MAX_PAGES).map(|n| format!("w{n}")).collect();
    let mut pages = vec![
        ("", page(vec![], Some("s1"), None)),
        (
            "s1",
            page(vec![entry(5000, json!("5000"))], Some("s2"), Some(&tokens[0])),
        ),
    ];
    for (n, token) in tokens.iter().enumerate() {
        let id = 4000 + u64::try_from(n).unwrap();
        let older = tokens.get(n + 1).map_or("end", String::as_str);
        pages.push((
            token,
            page(vec![entry(id, json!(id.to_string()))], Some("s2"), Some(older)),
        ));
    }
    let server = scripted(pages).await;
    let mut a = plain_account(&server);
    a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    let synced = a.catch_up.sync_messages(&mut a.store, CHAT).await.unwrap();
    assert_eq!(synced.pages, MAX_PAGES);
    let state = a.store.sync_state(CHAT).unwrap().unwrap();
    assert_eq!(state.sync_link, Some(link("s2")));
    assert_eq!(state.backward_link, Some(link(&tokens[MAX_PAGES - 1])));
    assert!(!state.history_complete);
    // the rest comes with the older history
    let older = a.catch_up.load_older(&mut a.store, CHAT).await.unwrap().unwrap();
    assert_eq!(older.inserted, 1);
}
