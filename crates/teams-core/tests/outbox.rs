//! The outbox on a real store (a temporary file) and the native HTTP transport on wiremock, reached through the
//! `test-support` mock origin: a command becomes a row, each attempt asks the fence again, the outbox's part of the
//! retry table, our own message matched by its `clientmessageid`, and no resend across runs.
//!
//! The chat service's sync is played by `ServiceSync`, which stores as messages what the mock service received (or was
//! seeded with). Tokens, MRIs and conversation ids here are placeholders, never real ones.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::collections::VecDeque;
use std::future::{self, Future};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::{
    Clock, Command, ConversationSync, MAX_ATTEMPTS, Outbox, OutboxError, SyncError, enqueue, new_client_message_id,
};
use teams_fence::{Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, Roster, Scope};
use teams_proto::{
    Authorized, Credentials, DEFAULT_RETRY_WAIT, Failure, HttpError, HttpTransport, Mutating, OwnState, Read, Response,
    Transport,
};
use teams_store::{
    Applied, AttemptOutcome, Conversation, ConversationKind, Message, OutboxEntry, OutboxKind, OutboxState, Store,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ME: &str = "8:orgid:lab-a";
const PEER: &str = "8:orgid:lab-b";
const OUTSIDER: &str = "8:orgid:outsider";
const CHAT: &str = "19:lab-chat@thread.v2";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const MESSAGES: &str = "/api/chatsvc/amer/v1/users/ME/conversations/19:lab-chat@thread.v2/messages";
const LOCATION: &str =
    "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19:lab-chat@thread.v2/messages/1001";
const START: i64 = 1_760_000_000_000;

// --- fakes ---

/// Hands out placeholder tokens; each rejection moves on to the next.
#[derive(Default)]
struct FakeTokens {
    generation: AtomicUsize,
    rejected: AtomicUsize,
}

impl FakeTokens {
    fn current(&self) -> Secret {
        Secret::new(format!(
            "placeholder-token-generation-{}",
            self.generation.load(Ordering::SeqCst)
        ))
    }
}

impl Credentials for FakeTokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(self.current()))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.rejected.fetch_add(1, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        future::ready(Ok(self.current()))
    }
}

/// A `fence.log` the test can read back.
#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl SharedLog {
    fn verdicts(&self) -> Vec<String> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter_map(|line| line["verdict"].as_str().map(str::to_owned))
            .collect()
    }
}

impl Write for SharedLog {
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

/// The chat's members as the fence reads them, counting each read.
struct Members {
    members: Mutex<Vec<Mri>>,
    reads: AtomicUsize,
}

impl Members {
    fn lab() -> Arc<Self> {
        Arc::new(Self {
            members: Mutex::new(vec![Mri::new(ME), Mri::new(PEER)]),
            reads: AtomicUsize::new(0),
        })
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    fn set(&self, members: &[&str]) {
        *self.members.lock().unwrap_or_else(PoisonError::into_inner) =
            members.iter().map(|mri| Mri::new(*mri)).collect();
    }
}

impl MembershipSource for Members {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let members = self.members.lock().unwrap_or_else(PoisonError::into_inner).clone();
        Box::pin(future::ready(Membership::Complete(members)))
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
    inner: HttpTransport<FakeTokens>,
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

/// The chat service's sync as the outbox's check sees it: each call stores, as our messages, the `clientmessageid`s
/// the service was seeded with and, when `finds` (the service stored what it was sent, its answer lost or not), those
/// of every post the mock service received.
struct ServiceSync {
    server: Arc<MockServer>,
    finds: AtomicBool,
    fails: AtomicBool,
    seeded: Mutex<Vec<String>>,
    /// How many posts the service had received at each call.
    calls: Mutex<Vec<usize>>,
}

impl ServiceSync {
    fn calls(&self) -> Vec<usize> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn seed(&self, client_message_id: &str) {
        self.seeded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(client_message_id.to_owned());
    }
}

impl ConversationSync for ServiceSync {
    async fn sync_conversation(&self, store: &mut Store, conversation: &str) -> Result<(), SyncError> {
        let posted = posted_ids(&self.server).await;
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(posted.len());
        if self.fails.load(Ordering::SeqCst) {
            return Err("the sync failed".into());
        }
        let mut found = self.seeded.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if self.finds.load(Ordering::SeqCst) {
            found.extend(posted);
        }
        for (n, client_message_id) in found.iter().enumerate() {
            let id = format!("{}", 2000 + n);
            store.upsert_message(&message(conversation, &id, 1, client_message_id, ME), None)?;
        }
        Ok(())
    }
}

/// The `clientmessageid` of every message post the mock service received, in order.
async fn posted_ids(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.method.as_str() == "POST" && request.url.path() == MESSAGES)
        .map(|request| {
            let body: Value = request.body_json().unwrap();
            body["clientmessageid"].as_str().unwrap_or_default().to_owned()
        })
        .collect()
}

fn message(conversation: &str, id: &str, version: i64, client_message_id: &str, sender: &str) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version,
        client_message_id: Some(client_message_id.to_owned()),
        sender: Some(sender.to_owned()),
        sender_name: Some("Synthetic".to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: "<p>hello</p>".to_owned(),
        properties: None,
        arrival_time: START,
        compose_time: Some(START),
        edit_time: None,
        delete_time: None,
    }
}

// --- setup ---

struct Setup {
    server: Arc<MockServer>,
    _dir: tempfile::TempDir,
    store_path: PathBuf,
    outbox: Outbox<Flaky>,
    sync: ServiceSync,
    tokens: Arc<FakeTokens>,
    log: SharedLog,
    members: Arc<Members>,
    faults: Arc<Mutex<VecDeque<Fault>>>,
    clock: Arc<AtomicI64>,
}

impl Setup {
    fn open_store(&self) -> Store {
        let mut store = Store::open(&self.store_path).unwrap();
        let chat = Conversation {
            id: CHAT.to_owned(),
            kind: ConversationKind::OneToOne,
            topic: None,
            version: 1,
            last_activity: None,
        };
        store.upsert_conversation(&chat).unwrap();
        store
    }

    fn fault(&self, fault: Fault) {
        self.faults
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(fault);
    }

    fn now(&self) -> i64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn advance(&self, by: Duration) {
        self.clock
            .fetch_add(i64::try_from(by.as_millis()).unwrap(), Ordering::SeqCst);
    }

    async fn posted(&self) -> Vec<String> {
        posted_ids(&self.server).await
    }

    async fn answer(&self, status: u16) {
        Mock::given(method("POST"))
            .and(path(MESSAGES))
            .respond_with(ResponseTemplate::new(status))
            .mount(&self.server)
            .await;
    }

    /// Answers the next post with `status` (with `Retry-After` when given), the ones after it as mounted later.
    async fn answer_once(&self, status: u16, retry_after: Option<&str>) {
        let mut answer = ResponseTemplate::new(status);
        if let Some(wait) = retry_after {
            answer = answer.insert_header("Retry-After", wait);
        }
        Mock::given(method("POST"))
            .and(path(MESSAGES))
            .respond_with(answer)
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    async fn created(&self) {
        Mock::given(method("POST"))
            .and(path(MESSAGES))
            .respond_with(
                ResponseTemplate::new(201)
                    .insert_header("Location", LOCATION)
                    .set_body_json(json!({"OriginalArrivalTime": START})),
            )
            .mount(&self.server)
            .await;
    }
}

async fn setup() -> Setup {
    let server = Arc::new(MockServer::start().await);
    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("store").join("a.sqlite");
    let tokens = Arc::new(FakeTokens::default());
    let log = SharedLog::default();
    let fence = Arc::new(Fence::new(
        Roster::new([Mri::new(ME), Mri::new(PEER)], []),
        FenceLog::from_writer(log.clone()),
    ));
    let faults = Arc::new(Mutex::new(VecDeque::new()));
    let transport = Flaky {
        inner: HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &server.uri())
            .unwrap(),
        faults: Arc::clone(&faults),
    };
    let members = Members::lab();
    let clock = Arc::new(AtomicI64::new(START));
    let read_clock = Arc::clone(&clock);
    let clock_fn: Clock = Arc::new(move || read_clock.load(Ordering::SeqCst));
    let membership: Arc<dyn MembershipSource + Send> = members.clone();
    let outbox = Outbox::new(Arc::new(transport), fence, membership, Mri::new(ME)).with_clock(clock_fn);
    let sync = ServiceSync {
        server: Arc::clone(&server),
        finds: AtomicBool::new(false),
        fails: AtomicBool::new(false),
        seeded: Mutex::new(Vec::new()),
        calls: Mutex::new(Vec::new()),
    };
    Setup {
        server,
        _dir: dir,
        store_path,
        outbox,
        sync,
        tokens,
        log,
        members,
        faults,
        clock,
    }
}

fn hello() -> Command {
    Command::Send {
        conversation: CHAT.to_owned(),
        content: "<p>hello</p>".to_owned(),
    }
}

async fn send(setup: &Setup, store: &mut Store, row: &OutboxEntry) -> OutboxEntry {
    setup.outbox.send(store, &setup.sync, row.id).await.unwrap()
}

// --- a command becomes a row ---

#[test]
fn a_command_becomes_a_pending_row_with_a_fresh_client_message_id() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let setup = runtime.block_on(setup());
    let mut store = setup.open_store();

    let first = enqueue(&mut store, &hello()).unwrap();
    let second = enqueue(&mut store, &hello()).unwrap();

    for row in [&first, &second] {
        assert_eq!(row.state, OutboxState::Pending);
        assert_eq!(row.kind, OutboxKind::Send);
        assert_eq!(row.conversation_id.as_deref(), Some(CHAT));
        assert_eq!(row.run_id, store.run_id());
        assert_eq!(row.attempts, 0);
        assert_eq!(row.client_message_id.len(), 19, "{}", row.client_message_id);
        assert!(row.client_message_id.bytes().all(|byte| byte.is_ascii_digit()));
        assert_eq!(serde_json::from_str::<Command>(&row.payload).unwrap(), hello());
    }
    assert_ne!(
        first.client_message_id, second.client_message_id,
        "fresh for each command"
    );
}

#[test]
fn client_message_ids_are_random_19_digit_numbers() {
    let ids: Vec<String> = (0..64).map(|_| new_client_message_id().unwrap()).collect();
    for id in &ids {
        assert_eq!(id.len(), 19, "{id}");
        assert!(!id.starts_with('0'));
    }
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ids.len());
}

// --- each attempt asks the fence ---

#[tokio::test]
async fn a_permitted_send_posts_the_rows_client_message_id_and_takes_the_message_id_from_the_answer() {
    let setup = setup().await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.message_id.as_deref(), Some("1001"), "from Location");
    assert_eq!(sent.attempts, 1);
    assert_eq!(setup.posted().await, std::slice::from_ref(&row.client_message_id));
    assert_eq!(setup.members.reads(), 1, "the members were read for the attempt");
    assert_eq!(setup.log.verdicts(), ["allow"]);
    assert!(setup.sync.calls().is_empty(), "a first attempt needs no check");
}

#[tokio::test]
async fn without_a_location_the_message_id_is_the_arrival_time() {
    let setup = setup().await;
    Mock::given(method("POST"))
        .and(path(MESSAGES))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"OriginalArrivalTime": START})))
        .mount(&setup.server)
        .await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.message_id, Some(START.to_string()));
}

#[tokio::test]
async fn each_attempt_asks_the_fence_again_with_a_fresh_membership_read() {
    let setup = setup().await;
    setup.answer_once(401, None).await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.attempts, 2);
    assert_eq!(setup.members.reads(), 2, "one fresh read per attempt");
    assert_eq!(setup.log.verdicts(), ["allow", "allow"], "one decision per attempt");
}

#[tokio::test]
async fn a_denied_attempt_marks_the_row_denied_and_sends_nothing() {
    let setup = setup().await;
    setup.created().await;
    setup.members.set(&[ME, OUTSIDER]);
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let denied = send(&setup, &mut store, &row).await;

    assert_eq!(denied.state, OutboxState::Denied);
    assert!(
        denied.last_error.as_deref().unwrap().contains("outside the lab"),
        "{denied:?}"
    );
    assert!(setup.posted().await.is_empty(), "nothing was sent");
    assert_eq!(setup.log.verdicts(), ["deny"]);
    assert!(
        store.outbox_due(i64::MAX).unwrap().is_empty(),
        "a denied row isn't retried"
    );
}

#[tokio::test]
async fn a_later_attempt_is_denied_when_the_membership_changed_since_the_first() {
    let setup = setup().await;
    setup.answer_once(429, Some("1")).await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();
    assert_eq!(send(&setup, &mut store, &row).await.state, OutboxState::Pending);

    setup.members.set(&[ME, PEER, OUTSIDER]);
    setup.advance(Duration::from_secs(1));
    let denied = send(&setup, &mut store, &row).await;

    assert_eq!(denied.state, OutboxState::Denied);
    assert_eq!(
        setup.posted().await.len(),
        1,
        "only the throttled first attempt went out"
    );
    assert_eq!(setup.log.verdicts(), ["allow", "deny"]);
}

// --- the retry table ---

#[tokio::test]
async fn a_401_is_tried_once_more_with_the_refreshed_token_and_the_same_client_message_id() {
    let setup = setup().await;
    setup.answer_once(401, None).await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(
        setup.tokens.rejected.load(Ordering::SeqCst),
        1,
        "the session refreshed the refused token"
    );
    let received = setup.server.received_requests().await.unwrap();
    let tokens: Vec<_> = received
        .iter()
        .map(|request| {
            request
                .headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        tokens,
        [
            "Bearer placeholder-token-generation-0",
            "Bearer placeholder-token-generation-1"
        ]
    );
    let cmid = row.client_message_id.clone();
    assert_eq!(setup.posted().await, [cmid.clone(), cmid]);
}

#[tokio::test]
async fn a_second_401_fails_the_row() {
    let setup = setup().await;
    setup.answer(401).await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let failed = send(&setup, &mut store, &row).await;

    assert_eq!(failed.state, OutboxState::Failed);
    assert_eq!(failed.last_error.as_deref(), Some("HTTP 401"));
    assert_eq!(setup.posted().await.len(), 2, "one new attempt, no more");
}

#[tokio::test]
async fn a_429_waits_its_retry_after_before_a_new_attempt_with_the_same_client_message_id() {
    let setup = setup().await;
    setup.answer_once(429, Some("7")).await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let waiting = send(&setup, &mut store, &row).await;
    assert_eq!(waiting.state, OutboxState::Pending);
    assert_eq!(waiting.not_before, Some(setup.now() + 7_000));
    assert_eq!(waiting.last_error.as_deref(), Some("HTTP 429"));

    // not due yet: nothing goes out
    assert_eq!(send(&setup, &mut store, &row).await, waiting);
    setup.advance(Duration::from_millis(6_999));
    assert!(setup.outbox.send_due(&mut store, &setup.sync).await.unwrap().is_empty());
    assert_eq!(setup.posted().await.len(), 1);

    setup.advance(Duration::from_millis(1));
    let sent = setup.outbox.send_due(&mut store, &setup.sync).await.unwrap();

    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].state, OutboxState::Sent);
    let cmid = row.client_message_id.clone();
    assert_eq!(setup.posted().await, [cmid.clone(), cmid]);
}

#[tokio::test]
async fn a_503_without_retry_after_waits_the_default() {
    let setup = setup().await;
    setup.answer_once(503, None).await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let waiting = send(&setup, &mut store, &row).await;

    assert_eq!(waiting.state, OutboxState::Pending);
    let default = i64::try_from(DEFAULT_RETRY_WAIT.as_millis()).unwrap();
    assert_eq!(waiting.not_before, Some(setup.now() + default));
    assert_eq!(setup.posted().await.len(), 1);
}

#[tokio::test]
async fn a_throttled_row_is_given_up_after_its_last_attempt() {
    let setup = setup().await;
    setup.answer(429).await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let mut last = send(&setup, &mut store, &row).await;
    while last.state == OutboxState::Pending {
        setup.advance(Duration::from_secs(60));
        last = send(&setup, &mut store, &row).await;
    }

    assert_eq!(last.state, OutboxState::Failed);
    assert_eq!(last.attempts, MAX_ATTEMPTS);
    assert_eq!(setup.posted().await.len(), usize::try_from(MAX_ATTEMPTS).unwrap());
}

#[tokio::test]
async fn any_other_4xx_fails_the_row_without_a_retry() {
    for status in [400, 403, 404, 409, 413] {
        let setup = setup().await;
        setup.answer(status).await;
        let mut store = setup.open_store();
        let row = enqueue(&mut store, &hello()).unwrap();

        let failed = send(&setup, &mut store, &row).await;

        assert_eq!(failed.state, OutboxState::Failed, "HTTP {status}");
        assert_eq!(failed.last_error, Some(format!("HTTP {status}")));
        assert_eq!(failed.attempts, 1);
        assert_eq!(setup.posted().await.len(), 1, "HTTP {status}: no retry");
        assert!(store.outbox_due(i64::MAX).unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_timeout_whose_message_arrived_is_found_by_its_client_message_id_and_not_sent_again() {
    let setup = setup().await;
    setup.created().await;
    setup.fault(Fault::LoseAnswer);
    setup.sync.finds.store(true, Ordering::SeqCst);
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(
        sent.message_id.as_deref(),
        Some("2000"),
        "the id the sync found it under"
    );
    assert_eq!(sent.attempts, 1);
    assert_eq!(
        setup.posted().await,
        std::slice::from_ref(&row.client_message_id),
        "sent once"
    );
    assert_eq!(setup.sync.calls(), [1], "the chat was synced after the timeout");
}

#[tokio::test]
async fn a_timeout_whose_message_didnt_arrive_is_sent_again_with_the_same_client_message_id() {
    let setup = setup().await;
    setup.created().await;
    setup.fault(Fault::DropRequest);
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.message_id.as_deref(), Some("1001"));
    assert_eq!(sent.attempts, 2);
    assert_eq!(
        setup.sync.calls(),
        [0],
        "the sync came first, before anything reached the service"
    );
    assert_eq!(setup.posted().await, std::slice::from_ref(&row.client_message_id));
    assert_eq!(setup.members.reads(), 2, "the new attempt asked the fence again");
}

#[tokio::test]
async fn a_5xx_other_than_503_is_checked_like_a_timeout() {
    let setup = setup().await;
    setup.answer_once(500, None).await;
    setup.created().await;
    // the service stored the message although it answered 500
    setup.sync.finds.store(true, Ordering::SeqCst);
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let sent = send(&setup, &mut store, &row).await;

    assert_eq!(
        sent.state,
        OutboxState::Sent,
        "the service had stored it: found by the sync"
    );
    assert_eq!(setup.posted().await.len(), 1, "not sent again");
    assert_eq!(setup.sync.calls(), [1]);
}

#[tokio::test]
async fn an_unknown_outcome_is_given_up_after_the_last_attempt() {
    let setup = setup().await;
    setup.answer(502).await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let failed = send(&setup, &mut store, &row).await;

    assert_eq!(failed.state, OutboxState::Failed);
    assert_eq!(failed.attempts, MAX_ATTEMPTS);
    assert!(
        failed
            .last_error
            .as_deref()
            .unwrap()
            .contains("whether it arrived is unknown")
    );
    assert_eq!(setup.posted().await.len(), usize::try_from(MAX_ATTEMPTS).unwrap());
    assert_eq!(
        setup.sync.calls().len(),
        usize::try_from(MAX_ATTEMPTS - 1).unwrap(),
        "a check before each new one"
    );
}

#[tokio::test]
async fn a_check_that_cant_sync_sends_nothing_and_leaves_the_row_waiting() {
    let setup = setup().await;
    setup.created().await;
    setup.fault(Fault::DropRequest);
    setup.sync.fails.store(true, Ordering::SeqCst);
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    let error = setup.outbox.send(&mut store, &setup.sync, row.id).await.unwrap_err();

    assert!(
        matches!(error, OutboxError::Sync { id, .. } if id == row.id),
        "{error:?}"
    );
    let waiting = store.outbox_entry(row.id).unwrap().unwrap();
    assert_eq!(waiting.state, OutboxState::Pending);
    assert_eq!(waiting.attempts, 1);
    assert!(setup.posted().await.is_empty());

    // once the sync works again, the check runs first and the row goes out
    setup.sync.fails.store(false, Ordering::SeqCst);
    let sent = send(&setup, &mut store, &row).await;
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(setup.sync.calls(), [0, 0]);
    assert_eq!(setup.posted().await.len(), 1);
}

// --- our own message coming back ---

#[tokio::test]
async fn our_message_coming_back_is_matched_to_its_row_and_stored_once() {
    let setup = setup().await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();
    send(&setup, &mut store, &row).await;
    let back = message(CHAT, "1001", 1, &row.client_message_id, ME);

    let by_trouter = setup.outbox.receive(&mut store, &back, None).unwrap();
    let by_sync = setup.outbox.receive(&mut store, &back, None).unwrap();

    assert_eq!(by_trouter.applied, Applied::Inserted);
    assert_eq!(by_sync.applied, Applied::Ignored, "never stored twice");
    for received in [&by_trouter, &by_sync] {
        let matched = received.outbox.as_ref().unwrap();
        assert_eq!(matched.id, row.id);
        assert_eq!(matched.state, OutboxState::Sent);
        assert_eq!(matched.message_id.as_deref(), Some("1001"));
    }
    let page = store.messages_before(CHAT, None, 10).unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(
        page.messages[0].client_message_id.as_deref(),
        Some(row.client_message_id.as_str())
    );
}

#[tokio::test]
async fn our_message_coming_back_before_the_answer_wins_over_the_answer() {
    let setup = setup().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();
    // the attempt is in flight when Trouter brings the message
    store.begin_attempt(row.id).unwrap();

    let received = setup
        .outbox
        .receive(&mut store, &message(CHAT, "3003", 1, &row.client_message_id, ME), None)
        .unwrap();
    assert_eq!(received.outbox.as_ref().unwrap().state, OutboxState::Sent);

    let after = store
        .finish_attempt(
            row.id,
            &AttemptOutcome::Retry {
                not_before: START,
                error: "timed out".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(after.state, OutboxState::Sent, "the late answer changes nothing");
    assert_eq!(after.message_id.as_deref(), Some("3003"));
}

#[tokio::test]
async fn a_later_version_of_our_message_keeps_its_row_as_it_is() {
    let setup = setup().await;
    setup.created().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();
    let sent = send(&setup, &mut store, &row).await;

    let edited = message(CHAT, "1001", 2, &row.client_message_id, ME);
    let received = setup.outbox.receive(&mut store, &edited, None).unwrap();

    assert_eq!(received.applied, Applied::Inserted);
    assert_eq!(received.outbox, Some(sent));
}

#[tokio::test]
async fn a_message_that_isnt_ours_is_stored_but_matched_to_nothing() {
    let setup = setup().await;
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();

    // another sender, with our row's clientmessageid
    let theirs = message(CHAT, "4004", 1, &row.client_message_id, PEER);
    let received = setup.outbox.receive(&mut store, &theirs, None).unwrap();

    assert_eq!(received.applied, Applied::Inserted);
    assert_eq!(received.outbox, None);
    assert_eq!(store.outbox_entry(row.id).unwrap().unwrap().state, OutboxState::Pending);
}

#[tokio::test]
async fn our_message_in_another_conversation_is_matched_to_nothing() {
    let setup = setup().await;
    let mut store = setup.open_store();
    let other = Conversation {
        id: "19:other-chat@thread.v2".to_owned(),
        kind: ConversationKind::Group,
        topic: None,
        version: 1,
        last_activity: None,
    };
    store.upsert_conversation(&other).unwrap();
    let row = enqueue(&mut store, &hello()).unwrap();

    let elsewhere = message(&other.id, "5005", 1, &row.client_message_id, ME);
    let received = setup.outbox.receive(&mut store, &elsewhere, None).unwrap();

    assert_eq!(received.outbox, None);
    assert_eq!(store.outbox_entry(row.id).unwrap().unwrap().state, OutboxState::Pending);
}

// --- no resend across runs ---

#[tokio::test]
async fn rows_an_earlier_run_left_pending_are_not_sent_on_start_and_go_out_only_on_an_explicit_resend() {
    let setup = setup().await;
    setup.created().await;
    let row = {
        let mut earlier = setup.open_store();
        enqueue(&mut earlier, &hello()).unwrap()
    };
    let mut store = setup.open_store();

    assert!(
        setup.outbox.send_due(&mut store, &setup.sync).await.unwrap().is_empty(),
        "nothing on start"
    );
    let error = setup.outbox.send(&mut store, &setup.sync, row.id).await.unwrap_err();
    assert!(
        matches!(error, OutboxError::OtherRun { id, .. } if id == row.id),
        "{error:?}"
    );
    assert!(setup.posted().await.is_empty());
    assert_eq!(setup.members.reads(), 0, "not even the fence was asked");
    let left = store.outbox_left_by_earlier_runs().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, row.id);

    let adopted = store.resend(row.id).unwrap();
    assert_eq!(adopted.client_message_id, row.client_message_id);
    let sent = setup.outbox.send_due(&mut store, &setup.sync).await.unwrap();

    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].state, OutboxState::Sent);
    assert_eq!(setup.posted().await, std::slice::from_ref(&row.client_message_id));
    assert!(
        setup.sync.calls().is_empty(),
        "it had never been attempted: nothing to look for"
    );
}

#[tokio::test]
async fn a_row_an_earlier_run_left_in_flight_whose_message_arrived_isnt_sent_again_on_resend() {
    let setup = setup().await;
    setup.created().await;
    let row = {
        let mut earlier = setup.open_store();
        let row = enqueue(&mut earlier, &hello()).unwrap();
        // the earlier process got as far as sending and ended before the answer
        earlier.begin_attempt(row.id).unwrap();
        row
    };
    setup.sync.seed(&row.client_message_id);
    let mut store = setup.open_store();

    store.resend(row.id).unwrap();
    let after = send(&setup, &mut store, &row).await;

    assert_eq!(after.state, OutboxState::Sent);
    assert_eq!(after.message_id.as_deref(), Some("2000"));
    assert!(setup.posted().await.is_empty(), "found by the sync: not sent twice");
    assert_eq!(setup.sync.calls(), [0]);
}

#[tokio::test]
async fn a_row_an_earlier_run_left_in_flight_that_didnt_arrive_goes_out_with_its_client_message_id() {
    let setup = setup().await;
    setup.created().await;
    let row = {
        let mut earlier = setup.open_store();
        let row = enqueue(&mut earlier, &hello()).unwrap();
        earlier.begin_attempt(row.id).unwrap();
        row
    };
    let mut store = setup.open_store();

    store.resend(row.id).unwrap();
    let after = send(&setup, &mut store, &row).await;

    assert_eq!(after.state, OutboxState::Sent);
    assert_eq!(after.attempts, 2);
    assert_eq!(setup.sync.calls(), [0], "looked for first");
    assert_eq!(setup.posted().await, std::slice::from_ref(&row.client_message_id));
}

// --- the engine's runtime ---

#[test]
fn sending_is_a_future_the_engine_can_spawn() {
    fn spawnable<F: Future + Send>(_: &F) {}
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let setup = runtime.block_on(setup());
    let mut store = setup.open_store();
    let row = enqueue(&mut store, &hello()).unwrap();
    let sending = setup.outbox.send(&mut store, &setup.sync, row.id);
    spawnable(&sending);
    drop(sending);
}
