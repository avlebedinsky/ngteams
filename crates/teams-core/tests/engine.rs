//! The engine, as native clients of the lab accounts run it against the mock chat service and its Trouter
//! (`teams-mock`, reached through the `test-support` mock origins), each engine on its own thread and runtime: a sends
//! and b receives through its push channel, never answering a delivery; an edit, a reaction (and taking it back) and
//! a delete arrive; a read receipt; the lab's group of three under the fence's live read (and any group chat the lab
//! didn't record denied); a new chat, under a stand-in for the group member read (the live read denies a group chat
//! the native client made, which is checked too); the catch-up after a dropped socket, each message stored
//! once; a write throttled once that goes out from the engine's tick; a write whose check can't sync, answered as the
//! row that waits; a post whose answer was lost and whose check couldn't sync, settled later by a catch-up or by its
//! own echo over the push channel and told once either way; such a post whose copy no read can take, met first by a
//! check from the tick and told last as the row stands, waiting or given up; a delivery whose conversation can't be
//! read, caught up from the tick; and an engine without a push channel that syncs on demand and refuses what the
//! store says can't be done.
//!
//! Every write goes through each account's own fence over the lab roster, the 1:1 chats' members read live from the
//! mock. Every id and token here is the mock's made-up one, never a real one.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fmt::Write as _;
use std::future::{self, Future};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{
    self, Action, Cause, EngineError, EngineHandle, EngineOptions, Event, MessageChange, Parts, Push, Source,
};
use teams_core::{Clock, MAX_FAILED_CHECKS, Outbox, Transient, TransientSent};
use teams_fence::lab::{LabConfig, record_abc_chat};
use teams_fence::{
    Fence, FenceLog, LiveMembershipSource, Membership, MembershipFuture, MembershipSource, Mri, Roster, Scope,
    is_group_chat_id,
};
use teams_mock::{
    AB_CHAT, ABC_CHAT, CHAT_BASE, ChatServiceMock, EventKind, LAB_A, LAB_B, LAB_C, TrouterMock, lab_toml,
};
use teams_proto::chat::MessageText;
use teams_proto::{
    Author, Authorized, AuthorizedPostRead, Client, Credentials, HttpError, HttpTransport, Mutating, OwnState,
    PostRead, Read, Response, Transport,
};
use teams_store::{Applied, Message, OutboxEntry, OutboxKind, OutboxState, Store, StoreError};
use teams_trouter::{Options, RegistrationReport, Trouter, TrouterCredentials};
use tokio::sync::broadcast;

/// How long a test waits for an event before it fails.
const WAIT: Duration = Duration::from_secs(30);

/// a's message of `messages.json` that is still standing, with a `clientmessageid`.
const A_EDITED: &str = "1760000010000";
/// a's message of `messages.json` that was deleted.
const A_DELETED: &str = "1760000060000";

// --- fakes ---

/// One account's mock token, for the transport and the push channel alike; it never runs out in a test.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Tokens {
    fn of(mock: &ChatServiceMock, mri: &str) -> Arc<Self> {
        Arc::new(Self {
            token: mock.token(mri).unwrap(),
            renewals: broadcast::channel(4).0,
        })
    }

    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

/// The fence's live member read, except that every group chat reads as complete with a, b and c. A stand-in for the
/// tests of a group chat the native client creates: the live read admits only the group chat the lab recorded
/// (`chats.toml`'s `abc`), so a write to a new one is denied
/// ([`a_group_chat_made_natively_is_denied_its_first_message_under_the_live_read`]); with the stand-in those tests
/// reach the engine's three-way delivery. Test-only, as any source but the crate's own is.
struct GroupsOfTheLab(LiveMembershipSource);

impl MembershipSource for GroupsOfTheLab {
    fn members<'a>(&'a self, scope: &'a Scope) -> MembershipFuture<'a> {
        match scope {
            Scope::Conversation { id } if is_group_chat_id(id) => Box::pin(future::ready(Membership::Complete(
                [LAB_A, LAB_B, LAB_C].map(Mri::new).to_vec(),
            ))),
            _ => self.0.members(scope),
        }
    }
}

/// The reads' token, which fails while `failing` is set: every read of the engine's client (the catch-up's, the
/// outbox's check, a thread read) fails before it goes out, as an unreachable service would fail it.
struct Reads {
    tokens: Arc<Tokens>,
    failing: Arc<AtomicBool>,
}

impl Reads {
    fn token(&self) -> Result<Secret, TokenError> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(TokenError::UnexpectedResponse { status: 503 });
        }
        Ok(Secret::new(self.tokens.token.clone()))
    }
}

impl Credentials for Reads {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(self.token())
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(self.token())
    }
}

/// The outbox's HTTP transport, whose next write can be answered before it reaches the service with the status
/// `next` holds (0: none): a 429 with a short `Retry-After`, any other with no body; or, while `lose` is set, reaches
/// the service and has its answer lost (a 500 in its place), once.
struct Interposed {
    inner: Arc<HttpTransport<Tokens>>,
    next: Arc<AtomicU16>,
    lose: Arc<AtomicBool>,
}

impl Transport for Interposed {
    type Error = HttpError;

    fn read<R: Read>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        self.inner.read(request)
    }

    fn own_state<R: OwnState>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        self.inner.own_state(request)
    }

    fn post_read<R: PostRead + Send>(
        &self,
        read: AuthorizedPostRead<R>,
    ) -> impl Future<Output = Result<Response, HttpError>> + Send {
        self.inner.post_read(read)
    }

    async fn deliver<R: Mutating + Send>(&self, write: Authorized<R>) -> Result<Response, HttpError> {
        if self.lose.swap(false, Ordering::SeqCst) {
            self.inner.deliver(write).await?;
            return Ok(Response::new(500, Vec::new()));
        }
        match self.next.swap(0, Ordering::SeqCst) {
            0 => self.inner.deliver(write).await,
            429 => Ok(Response::new(429, Vec::new()).with_retry_after(Duration::from_millis(300))),
            status => Ok(Response::new(status, Vec::new())),
        }
    }
}

/// A `fence.log` the test reads back.
#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl SharedLog {
    fn lines(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The write decisions on `conversation` of `kind`, as their verdicts.
    fn verdicts(&self, kind: &str, conversation: &str) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|line| line["kind"] == kind && line["scope"]["conversation"]["id"] == conversation)
            .map(|line| line["verdict"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The reviewed own frame of each frame the push channel logged.
    fn frames(&self) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|line| line["event"] == "frame")
            .map(|line| line["own"].as_str().unwrap().to_owned())
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

// --- setup ---

struct Lab {
    mock: ChatServiceMock,
    trouter: TrouterMock,
}

impl Lab {
    async fn start() -> Self {
        let mock = ChatServiceMock::start().await.unwrap();
        let trouter = mock.start_trouter().unwrap();
        Self { mock, trouter }
    }

    /// The posts the mock got to `conversation`'s messages.
    async fn posts_to(&self, conversation: &str) -> usize {
        let path = format!("/api/chatsvc/amer/v1/users/ME/conversations/{conversation}/messages");
        self.mock
            .received_requests()
            .await
            .iter()
            .filter(|request| request.method.as_str() == "POST" && decoded(request.url.path()) == path)
            .count()
    }
}

fn decoded(path: &str) -> String {
    path.replace("%3A", ":").replace("%40", "@")
}

/// What a native client's member read knows of group chats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Groups {
    /// The fence's live read without the lab's policy: no group chat is admitted.
    Unknown,
    /// The fence's live read with the lab's policy (its `lab.toml`), its `chats.toml` recording [`ABC_CHAT`] or not.
    Lab { recorded: bool },
    /// Every group chat reads as a, b and c ([`GroupsOfTheLab`]).
    StandIn,
}

/// How a native client is set up.
#[derive(Clone, Copy)]
struct How {
    groups: Groups,
    push: bool,
    /// The first write is throttled ([`Interposed`]).
    throttle_first_write: bool,
    /// The push channel's reconnect wait.
    backoff: Duration,
}

impl Default for How {
    fn default() -> Self {
        Self {
            groups: Groups::Unknown,
            push: true,
            throttle_first_write: false,
            backoff: Duration::from_millis(50),
        }
    }
}

/// A lab account's native client: its engine, its `fence.log`, every event it took so far, and the switches of its
/// fakes.
struct Native {
    handle: EngineHandle,
    log: SharedLog,
    me: &'static str,
    seen: String,
    /// The engine's reads fail while it is set ([`Reads`]).
    reads_fail: Arc<AtomicBool>,
    /// The status the next write is answered with before it reaches the service ([`Interposed`]).
    next_write: Arc<AtomicU16>,
    /// The next write reaches the service and its answer is lost ([`Interposed`]).
    lose_answer: Arc<AtomicBool>,
    /// How far ahead of the wall clock the outbox's clock runs, in milliseconds (a row's backoff without waiting).
    ahead: Arc<AtomicI64>,
    /// An [`Event::Failed`] is taken like any other event instead of failing the test.
    failures_expected: bool,
    /// Where its store is, kept while it runs; another engine on the account opens the store there ([`try_native_in`]).
    _dir: Arc<tempfile::TempDir>,
}

fn trouter_options(backoff: Duration) -> Options {
    Options {
        ping_interval: Duration::from_secs(2),
        pong_timeout: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(5),
        hello_timeout: Duration::from_secs(5),
        backoff_base: backoff,
        backoff_cap: backoff,
        backoff_jitter_percent: 0,
        ..Options::default()
    }
}

/// `me`'s native client against the lab's mocks, set up as `how` says, on a store of its own.
fn native(lab: &Lab, me: &'static str, how: How) -> Native {
    try_native_in(lab, me, how, Arc::new(tempfile::tempdir().unwrap())).unwrap()
}

/// `me`'s native client as [`native`] makes it, on the store in `dir` (another engine's, perhaps), with a `fence.log`
/// of its own; the engine's refusal when it can't start.
fn try_native_in(lab: &Lab, me: &'static str, how: How, dir: Arc<tempfile::TempDir>) -> Result<Native, EngineError> {
    let tokens = Tokens::of(&lab.mock, me);
    let log = SharedLog::default();
    let fence = Arc::new(Fence::new(
        Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
        FenceLog::from_writer(log.clone()),
    ));
    let origin = lab.mock.origin();
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &origin).unwrap();
    let transport = Arc::new(http());
    let live = LiveMembershipSource::with_mock_origin(&origin).unwrap();
    let live = match how.groups {
        Groups::Lab { recorded } => {
            let config = LabConfig::parse(&lab_toml(dir.path()), dir.path()).unwrap();
            if recorded {
                record_abc_chat(config.paths(), ABC_CHAT).unwrap();
            }
            live.with_lab(&config)
        }
        Groups::Unknown | Groups::StandIn => live,
    };
    live.set_credential_source(transport.clone());
    let membership: Arc<dyn MembershipSource + Send> = if how.groups == Groups::StandIn {
        Arc::new(GroupsOfTheLab(live))
    } else {
        Arc::new(live)
    };
    let author = Author::new(Mri::new(me), "Lab account");
    let reads_fail = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(Reads {
        tokens: Arc::clone(&tokens),
        failing: Arc::clone(&reads_fail),
    });
    let client = Arc::new(Client::new(
        HttpTransport::with_mock_origin(reads, Arc::clone(&fence), CHAT_BASE, &origin).unwrap(),
    ));
    let push = how.push.then(|| {
        let trouter = Trouter::with_mock_origin(
            Arc::clone(&transport),
            Arc::clone(&tokens),
            Arc::clone(&fence),
            lab.trouter.origin(),
        )
        .unwrap()
        .with_options(trouter_options(how.backoff));
        Push::new(trouter)
    });
    let store = Store::open(&dir.path().join("store").join("lab.sqlite")).unwrap();
    let options = EngineOptions {
        tick: Duration::from_millis(50),
        catch_up_retry: Duration::from_millis(500),
    };
    let next_write = Arc::new(AtomicU16::new(if how.throttle_first_write { 429 } else { 0 }));
    let lose_answer = Arc::new(AtomicBool::new(false));
    let interposed = Interposed {
        inner: transport,
        next: Arc::clone(&next_write),
        lose: Arc::clone(&lose_answer),
    };
    let ahead = Arc::new(AtomicI64::new(0));
    let outbox = Outbox::new(Arc::new(interposed), fence, membership, author).with_clock(clock_ahead(&ahead));
    let handle = engine::start(Parts {
        store,
        client,
        outbox,
        push,
        options,
    })?;
    Ok(Native {
        handle,
        log,
        me,
        seen: String::new(),
        reads_fail,
        next_write,
        lose_answer,
        ahead,
        failures_expected: false,
        _dir: dir,
    })
}

impl Native {
    /// The next event `pick` takes, skipping others; fails after [`WAIT`] with every event seen.
    async fn event<T>(&mut self, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let Ok(event) = tokio::time::timeout_at(deadline, self.handle.next_event()).await else {
                panic!("{}: the event didn't come in time; seen:\n{}", self.me, self.seen);
            };
            let event = event.expect("the engine stopped");
            let _ = writeln!(self.seen, "{event:?}");
            if let Event::Failed { during, error } = &event
                && !self.failures_expected
            {
                panic!("{}: failed {during}: {error}", self.me);
            }
            if let Some(picked) = pick(&event) {
                return picked;
            }
        }
    }

    /// Waits until the push channel is registered and the catch-up after its connect has run.
    async fn ready(&mut self) {
        let (mut registered, mut caught_up) = (false, false);
        while !(registered && caught_up) {
            self.event(|event| match event {
                Event::Registration(RegistrationReport::Registered) => {
                    registered = true;
                    Some(())
                }
                Event::Registration(RegistrationReport::Failed(why)) => panic!("registration failed: {why}"),
                Event::CaughtUp { cause, report } => {
                    assert!(report.failed.is_empty(), "{report:?}");
                    assert_eq!(*cause, Cause::Connected);
                    caught_up = true;
                    Some(())
                }
                Event::CatchUpFailed { error, .. } => panic!("the catch-up failed: {error}"),
                _ => None,
            })
            .await;
        }
    }

    /// The next message event about `id`: what changed, what the store did, and the row as stored.
    async fn message_event(&mut self, conversation: &str, id: &str) -> (MessageChange, Applied, Message) {
        self.event(|event| match event {
            Event::Message {
                conversation: got,
                id: got_id,
                change,
                applied,
                message,
                ..
            } if got == conversation && got_id == id => Some((*change, *applied, *message.clone().unwrap())),
            _ => None,
        })
        .await
    }

    async fn write(&self, action: Action) -> OutboxEntry {
        self.handle.write(action).await.unwrap()
    }

    /// Sends `text` to `conversation`; the row, sent.
    async fn send(&self, conversation: &str, text: &str) -> OutboxEntry {
        let row = self.write(send(conversation, text)).await;
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        assert!(row.message_id.is_some(), "{row:?}");
        row
    }

    /// Sends `text` to `conversation`: the post reaches the service, its answer is lost, and the reads fail, so its
    /// check can't sync. The row as it waits, told as such; the reads work again afterwards.
    async fn send_unanswered(&mut self, conversation: &str, text: &str) -> OutboxEntry {
        self.lose_answer.store(true, Ordering::SeqCst);
        self.reads_fail.store(true, Ordering::SeqCst);
        let row = self.write(send(conversation, text)).await;
        assert_eq!(row.state, OutboxState::Pending, "{row:?}");
        assert_eq!((row.attempts, row.failed_checks), (1, 1), "{row:?}");
        let told = self
            .event(|event| match event {
                Event::Outbox(entry) if entry.id == row.id => Some(entry.clone()),
                _ => None,
            })
            .await;
        assert_eq!(told, row);
        self.reads_fail.store(false, Ordering::SeqCst);
        row
    }

    /// Moves the outbox's clock past row `id`'s backoff, so the tick checks it again; the row as that check left it,
    /// told with `failed_checks` checks that couldn't tell whether it arrived.
    async fn checked_from_the_tick(&mut self, id: i64, failed_checks: u32) -> OutboxEntry {
        self.ahead.fetch_add(3_600_000, Ordering::SeqCst);
        self.event(|event| match event {
            Event::Outbox(entry) if entry.id == id && entry.failed_checks == failed_checks => Some(entry.clone()),
            _ => None,
        })
        .await
    }

    /// The events waiting now, taken: the outbox row `id` as each of them told it. A request answered first makes
    /// sure the engine is done with what came before it.
    async fn told_rows(&mut self, id: i64) -> Vec<OutboxEntry> {
        self.handle.conversations(1).await.unwrap();
        let mut rows = Vec::new();
        while let Some(event) = self.handle.try_next_event() {
            let _ = writeln!(self.seen, "{event:?}");
            if let Event::Outbox(row) = event
                && row.id == id
            {
                rows.push(row);
            }
        }
        rows
    }

    /// Every event waiting now, taken; a request answered first makes sure the engine is done with what came before.
    async fn drain(&mut self) -> Vec<Event> {
        self.handle.conversations(1).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = self.handle.try_next_event() {
            let _ = writeln!(self.seen, "{event:?}");
            events.push(event);
        }
        events
    }

    async fn stored(&self, conversation: &str, id: &str) -> Message {
        self.handle.message(conversation, id).await.unwrap().unwrap()
    }

    /// How many times the store holds message `id` of `conversation`.
    async fn copies(&self, conversation: &str, id: &str) -> usize {
        let page = self.handle.messages(conversation, None, 1_000).await.unwrap();
        page.messages.iter().filter(|message| message.id == id).count()
    }

    fn shutdown(self) {
        self.handle.shutdown();
    }
}

fn send(conversation: &str, text: &str) -> Action {
    Action::Send {
        conversation: conversation.to_owned(),
        text: MessageText::plain(text).unwrap(),
    }
}

/// The outbox's clock: the wall clock, `ahead` milliseconds on.
fn clock_ahead(ahead: &Arc<AtomicI64>) -> Clock {
    let ahead = Arc::clone(ahead);
    Arc::new(move || now_ms() + ahead.load(Ordering::SeqCst))
}

/// The wall clock in milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(since.as_millis()).unwrap()
}

fn id_of(row: &OutboxEntry) -> String {
    row.message_id.clone().unwrap()
}

/// Waits until none of `account`'s sockets on the Trouter mock is open.
async fn sockets_closed(trouter: &TrouterMock, account: &str) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let open = trouter
            .sockets()
            .iter()
            .any(|socket| socket.account.as_deref() == Some(account) && socket.open);
        if !open {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "{account}'s socket stayed open");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// --- tests ---

#[tokio::test]
async fn a_message_from_a_reaches_b_over_its_push_channel_and_no_delivery_is_answered() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;

    let row = a.send(AB_CHAT, "hello b, from the engine of a").await;
    let id = id_of(&row);
    let (change, applied, message) = b.message_event(AB_CHAT, &id).await;
    assert_eq!((change, applied), (MessageChange::New, Applied::Inserted));
    assert!(
        message.content.contains("hello b, from the engine of a"),
        "{}",
        message.content
    );
    assert_eq!(message.sender.as_deref(), Some(LAB_A));
    assert_eq!(
        message.client_message_id.as_deref(),
        Some(row.client_message_id.as_str())
    );
    assert_eq!(b.copies(AB_CHAT, &id).await, 1);

    // a's own message is in a's store too (the sync after the write), with the row's clientmessageid
    let mine = a.stored(AB_CHAT, &id).await;
    assert_eq!(mine.client_message_id.as_deref(), Some(row.client_message_id.as_str()));
    // a's fence decided the one write, and allowed it
    assert_eq!(a.log.verdicts("message", AB_CHAT), ["allow"]);

    // b's socket sent only reviewed own frames, each logged before it went out, and nothing that answers a delivery
    let reviewed = [
        "trouter_authenticate",
        "trouter_ping",
        "trouter_message_loss_ack",
        "trouter_disconnect",
    ];
    let logged = b.log.frames();
    assert!(logged.iter().all(|own| reviewed.contains(&own.as_str())), "{logged:?}");
    let sent: Vec<String> = lab
        .trouter
        .sockets()
        .into_iter()
        .filter(|socket| socket.account.as_deref() == Some(LAB_B))
        .flat_map(|socket| socket.frames)
        .collect();
    assert!(!sent.is_empty() && sent.len() <= logged.len(), "{sent:?} / {logged:?}");
    assert!(
        sent.iter()
            .all(|frame| !frame.starts_with("3:") && !frame.starts_with("6:")),
        "{sent:?}"
    );

    // stopping b's engine ends its push channel
    b.shutdown();
    sockets_closed(&lab.trouter, LAB_B).await;
    a.shutdown();
}

#[tokio::test]
async fn an_edit_a_reaction_its_take_back_and_a_delete_arrive_on_b() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;
    let id = id_of(&a.send(AB_CHAT, "a first draft").await);
    b.message_event(AB_CHAT, &id).await;

    let edit = a
        .write(Action::Edit {
            conversation: AB_CHAT.to_owned(),
            message_id: id.clone(),
            text: MessageText::plain("the second draft").unwrap(),
        })
        .await;
    assert_eq!(
        (edit.kind, edit.state),
        (OutboxKind::Edit, OutboxState::Sent),
        "{edit:?}"
    );
    let (change, applied, message) = b.message_event(AB_CHAT, &id).await;
    assert_eq!((change, applied), (MessageChange::Edit, Applied::Updated));
    assert!(message.content.contains("the second draft"), "{}", message.content);
    assert!(message.edit_time.is_some());
    assert!(a.stored(AB_CHAT, &id).await.content.contains("the second draft"));

    let react = Action::React {
        conversation: AB_CHAT.to_owned(),
        message_id: id.clone(),
        key: "like".to_owned(),
    };
    assert_eq!(a.write(react).await.state, OutboxState::Sent);
    let (change, applied, _) = b.message_event(AB_CHAT, &id).await;
    assert_eq!((change, applied), (MessageChange::Reaction, Applied::Updated));
    let reactions = b.handle.reactions(AB_CHAT, &id).await.unwrap();
    assert_eq!(
        reactions
            .iter()
            .map(|reaction| (reaction.key.as_str(), reaction.user.as_str()))
            .collect::<Vec<_>>(),
        [("like", LAB_A)]
    );

    let unreact = Action::Unreact {
        conversation: AB_CHAT.to_owned(),
        message_id: id.clone(),
        key: "like".to_owned(),
    };
    assert_eq!(a.write(unreact).await.kind, OutboxKind::RemoveReaction);
    let (change, _, _) = b.message_event(AB_CHAT, &id).await;
    assert_eq!(change, MessageChange::Reaction);
    assert!(b.handle.reactions(AB_CHAT, &id).await.unwrap().is_empty());

    let delete = Action::Delete {
        conversation: AB_CHAT.to_owned(),
        message_id: id.clone(),
    };
    assert_eq!(a.write(delete).await.state, OutboxState::Sent);
    let (change, applied, message) = b.message_event(AB_CHAT, &id).await;
    assert_eq!((change, applied), (MessageChange::Delete, Applied::Updated));
    assert!(
        message.delete_time.is_some() && message.content.is_empty(),
        "{message:?}"
    );
    assert_eq!(b.copies(AB_CHAT, &id).await, 1);

    // every write was a fresh decision of a's fence, each allowed
    assert_eq!(a.log.verdicts("edit", AB_CHAT), ["allow"]);
    assert_eq!(a.log.verdicts("reaction", AB_CHAT), ["allow", "allow"]);
    assert_eq!(a.log.verdicts("delete", AB_CHAT), ["allow"]);
    a.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn a_read_receipt_moves_b_s_horizon_as_the_service_sees_it() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;
    let sent = a.send(AB_CHAT, "read me").await;
    let id = id_of(&sent);
    b.message_event(AB_CHAT, &id).await;

    let read = b
        .write(Action::Read {
            conversation: AB_CHAT.to_owned(),
            message_id: id.clone(),
        })
        .await;
    assert_eq!(
        (read.kind, read.state),
        (OutboxKind::ReadReceipt, OutboxState::Sent),
        "{read:?}"
    );
    // a is told nothing over the push channel (no capture showed it), but the service has b's horizon
    let told = lab.mock.take_events(LAB_A);
    let receipt = told
        .iter()
        .find(|event| event.kind == EventKind::ReadReceipt && event.from == LAB_B)
        .unwrap();
    let horizon = receipt.resource["consumptionhorizon"].as_str().unwrap();
    assert!(horizon.starts_with(&format!("{id};")), "{horizon}");
    assert!(horizon.ends_with(&format!(";{}", sent.client_message_id)), "{horizon}");
    assert_eq!(b.log.verdicts("read_receipt", AB_CHAT), ["allow"]);
    a.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn a_message_to_the_lab_s_group_reaches_both_others() {
    let lab = Lab::start().await;
    let how = How {
        groups: Groups::Lab { recorded: true },
        ..How::default()
    };
    let mut a = native(&lab, LAB_A, how);
    let mut b = native(&lab, LAB_B, how);
    let mut c = native(&lab, LAB_C, how);
    for native in [&mut a, &mut b, &mut c] {
        native.ready().await;
    }

    let from_a = id_of(&a.send(ABC_CHAT, "hello b and c").await);
    for native in [&mut b, &mut c] {
        let (change, applied, message) = native.message_event(ABC_CHAT, &from_a).await;
        assert_eq!((change, applied), (MessageChange::New, Applied::Inserted));
        assert_eq!(message.sender.as_deref(), Some(LAB_A));
    }
    let from_c = id_of(&c.send(ABC_CHAT, "hello a and b").await);
    for native in [&mut a, &mut b] {
        let (_, applied, message) = native.message_event(ABC_CHAT, &from_c).await;
        assert_eq!(applied, Applied::Inserted);
        assert_eq!(message.sender.as_deref(), Some(LAB_C));
    }
    assert_eq!(a.log.verdicts("message", ABC_CHAT), ["allow"]);
    assert_eq!(c.log.verdicts("message", ABC_CHAT), ["allow"]);
    for native in [a, b, c] {
        native.shutdown();
    }
}

#[tokio::test]
async fn a_write_to_a_group_chat_the_lab_didn_t_record_is_denied() {
    let lab = Lab::start().await;
    // the lab's policy with nothing recorded, and a read that doesn't know the lab
    for groups in [Groups::Lab { recorded: false }, Groups::Unknown] {
        let mut a = native(
            &lab,
            LAB_A,
            How {
                groups,
                ..How::default()
            },
        );
        a.ready().await;
        let row = a.write(send(ABC_CHAT, "never sent")).await;
        assert_eq!(row.state, OutboxState::Denied, "{groups:?}: {row:?}");
        assert_eq!(a.log.verdicts("message", ABC_CHAT), ["deny"], "{groups:?}");
        a.shutdown();
    }
    assert_eq!(lab.posts_to(ABC_CHAT).await, 0);
}

#[tokio::test]
async fn a_group_chat_made_natively_is_denied_its_first_message_under_the_live_read() {
    // the live read admits only the group chat chats.toml records, and a native create records nothing
    let lab = Lab::start().await;
    let how = How {
        groups: Groups::Lab { recorded: true },
        ..How::default()
    };
    let mut a = native(&lab, LAB_A, how);
    a.ready().await;
    let created = a
        .write(Action::CreateChat {
            others: vec![Mri::new(LAB_B), Mri::new(LAB_C)],
        })
        .await;
    assert_eq!(
        (created.kind, created.state),
        (OutboxKind::CreateChat, OutboxState::Sent),
        "{created:?}"
    );
    let chat = id_of(&created);
    assert!(is_group_chat_id(&chat) && chat != ABC_CHAT, "{chat}");

    let row = a.write(send(&chat, "never sent")).await;
    assert_eq!(row.state, OutboxState::Denied, "{row:?}");
    assert_eq!(a.log.verdicts("message", &chat), ["deny"]);
    assert_eq!(lab.posts_to(&chat).await, 0);
    a.shutdown();
}

#[tokio::test]
async fn a_new_group_chat_is_created_and_its_first_message_brings_it_to_b_and_c() {
    let lab = Lab::start().await;
    let how = How {
        groups: Groups::StandIn,
        ..How::default()
    };
    let mut a = native(&lab, LAB_A, how);
    let mut b = native(&lab, LAB_B, how);
    let mut c = native(&lab, LAB_C, how);
    for native in [&mut a, &mut b, &mut c] {
        native.ready().await;
    }
    let refused = a.handle.write(Action::CreateChat { others: Vec::new() }).await;
    assert!(matches!(refused, Err(EngineError::NotAChat)), "{refused:?}");
    let refused = a
        .handle
        .write(Action::CreateChat {
            others: vec![Mri::new(LAB_A)],
        })
        .await;
    assert!(matches!(refused, Err(EngineError::NotAChat)), "{refused:?}");

    let created = a
        .write(Action::CreateChat {
            others: vec![Mri::new(LAB_B), Mri::new(LAB_C)],
        })
        .await;
    assert_eq!(
        (created.kind, created.state),
        (OutboxKind::CreateChat, OutboxState::Sent),
        "{created:?}"
    );
    let chat = id_of(&created);
    assert!(is_group_chat_id(&chat), "{chat}");
    // the new chat is in a's store with its members (synced after the write)
    let mut members: Vec<String> = a
        .handle
        .members(chat.as_str())
        .await
        .unwrap()
        .into_iter()
        .map(|member| member.mri)
        .collect();
    members.sort();
    assert_eq!(members, [LAB_A, LAB_B, LAB_C]);

    let first = id_of(&a.send(&chat, "the new chat's first message").await);
    for native in [&mut b, &mut c] {
        let (_, applied, _) = native.message_event(&chat, &first).await;
        assert_eq!(applied, Applied::Inserted);
        let conversations = native.handle.conversations(100).await.unwrap();
        assert!(conversations.iter().any(|conversation| conversation.id == chat));
        assert_eq!(native.copies(&chat, &first).await, 1);
    }
    let created: Vec<Value> = a
        .log
        .lines()
        .into_iter()
        .filter(|line| line["kind"] == "chat_create")
        .collect();
    assert_eq!(created.len(), 1, "{created:?}");
    assert_eq!(created[0]["verdict"], "allow");
    for native in [a, b, c] {
        native.shutdown();
    }
}

#[tokio::test]
async fn a_dropped_socket_is_caught_up_after_the_reconnect_each_message_stored_once() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    let mut b = native(
        &lab,
        LAB_B,
        How {
            backoff: Duration::from_secs(2),
            ..How::default()
        },
    );
    a.ready().await;
    b.ready().await;

    lab.trouter.drop_sockets(LAB_B);
    b.event(|event| matches!(event, Event::Disconnected(_)).then_some(()))
        .await;
    let mut missed = Vec::new();
    for n in 0..3 {
        missed.push(id_of(&a.send(AB_CHAT, &format!("sent while b was away, {n}")).await));
    }

    // the reconnect is caught up (for the connect itself, before any delivery after it is taken); then come the
    // message-loss report's catch-up, the deliveries the service kept for b, and the replays of earlier ones
    b.event(|event| matches!(event, Event::Connected { .. }).then_some(()))
        .await;
    // what the catch-up stored is told as it is stored, marked as brought by it, before it ends
    let mut told_caught_up = 0;
    let mut caught_up = b
        .event(|event| match event {
            Event::CaughtUp { cause, report } => {
                assert_eq!(*cause, Cause::Connected, "{report:?}");
                Some(report.messages.inserted)
            }
            Event::Message {
                id, source, applied, ..
            } if missed.contains(id) => {
                assert_eq!(*source, Source::CatchUp, "a delivery came before the catch-up");
                assert_eq!(*applied, Applied::Inserted);
                told_caught_up += 1;
                None
            }
            _ => None,
        })
        .await;
    assert_eq!(told_caught_up, caught_up, "each message the catch-up stored is told");
    let mut delivered = 0;
    let mut seen: Vec<String> = Vec::new();
    while seen.len() < missed.len() {
        b.event(|event| match event {
            Event::CaughtUp { report, .. } => {
                assert!(report.failed.is_empty(), "{report:?}");
                caught_up += report.messages.inserted;
                Some(())
            }
            // one a later catch-up stored is counted by its report
            Event::Message {
                id, applied, source, ..
            } if missed.contains(id) && *source != Source::CatchUp => {
                if !seen.contains(id) {
                    seen.push(id.clone());
                }
                if *applied == Applied::Inserted {
                    delivered += 1;
                }
                Some(())
            }
            _ => None,
        })
        .await;
    }
    assert_eq!(caught_up + delivered, missed.len(), "every missed message stored once");
    for id in &missed {
        assert_eq!(b.copies(AB_CHAT, id).await, 1);
    }
    a.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn a_throttled_write_goes_out_from_the_tick() {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            throttle_first_write: true,
            ..How::default()
        },
    );
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;

    let row = a.write(send(AB_CHAT, "after a short wait")).await;
    assert_eq!(row.state, OutboxState::Pending, "{row:?}");
    assert!(row.not_before.is_some());
    let sent = a
        .event(|event| match event {
            Event::Outbox(entry) if entry.id == row.id && entry.state == OutboxState::Sent => Some(entry.clone()),
            _ => None,
        })
        .await;
    assert_eq!(sent.attempts, 2);
    assert_eq!(sent.client_message_id, row.client_message_id);
    let (_, applied, _) = b.message_event(AB_CHAT, &id_of(&sent)).await;
    assert_eq!(applied, Applied::Inserted);
    // the throttled attempt never reached the service; each attempt was a decision of its own
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);
    assert_eq!(a.log.verdicts("message", AB_CHAT), ["allow", "allow"]);
    a.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn without_a_push_channel_the_engine_syncs_on_demand_and_refuses_what_the_store_rules_out() {
    let lab = Lab::start().await;
    let b = native(
        &lab,
        LAB_B,
        How {
            push: false,
            ..How::default()
        },
    );
    assert!(b.handle.conversations(100).await.unwrap().is_empty());
    let report = b.handle.sync().await.unwrap();
    assert!(report.failed.is_empty() && report.conversations > 0, "{report:?}");
    let conversations = b.handle.conversations(100).await.unwrap();
    assert!(conversations.iter().any(|conversation| conversation.id == AB_CHAT));
    assert_eq!(b.stored(AB_CHAT, A_EDITED).await.sender.as_deref(), Some(LAB_A));

    let refusals = [
        Action::Edit {
            conversation: AB_CHAT.to_owned(),
            message_id: A_EDITED.to_owned(),
            text: MessageText::plain("not b's to edit").unwrap(),
        },
        Action::Delete {
            conversation: AB_CHAT.to_owned(),
            message_id: A_EDITED.to_owned(),
        },
        Action::React {
            conversation: AB_CHAT.to_owned(),
            message_id: A_DELETED.to_owned(),
            key: "like".to_owned(),
        },
        Action::Read {
            conversation: AB_CHAT.to_owned(),
            message_id: "1".to_owned(),
        },
    ];
    let mut refused = Vec::new();
    for action in refusals {
        refused.push(b.handle.write(action).await.unwrap_err());
    }
    assert!(
        matches!(refused[0], EngineError::NotOwn { action: "edited", .. }),
        "{:?}",
        refused[0]
    );
    assert!(
        matches!(refused[1], EngineError::NotOwn { action: "deleted", .. }),
        "{:?}",
        refused[1]
    );
    assert!(matches!(refused[2], EngineError::Deleted { .. }), "{:?}", refused[2]);
    assert!(
        matches!(refused[3], EngineError::UnknownMessage { .. }),
        "{:?}",
        refused[3]
    );
    // nothing of those reached the fence or the service
    assert!(b.log.lines().is_empty(), "{:?}", b.log.lines());
    let writes = lab
        .mock
        .received_requests()
        .await
        .iter()
        .filter(|request| request.method.as_str() != "GET")
        .count();
    assert_eq!(writes, 0);

    let read = b
        .write(Action::Read {
            conversation: AB_CHAT.to_owned(),
            message_id: A_EDITED.to_owned(),
        })
        .await;
    assert_eq!(read.state, OutboxState::Sent, "{read:?}");
    let id = id_of(&b.send(AB_CHAT, "sent without a push channel").await);
    assert!(
        b.stored(AB_CHAT, &id)
            .await
            .content
            .contains("sent without a push channel")
    );
    b.handle.load_older(AB_CHAT).await.unwrap();
    b.shutdown();
}

#[tokio::test]
async fn whether_the_whole_history_is_stored_tells_a_chat_never_synced_from_its_beginning() {
    let lab = Lab::start().await;
    let b = native(&lab, LAB_B, QUIET);
    // never synced: nothing older can be read, and that isn't the chat's beginning
    assert!(b.handle.load_older(AB_CHAT).await.unwrap().is_none());
    assert_eq!(b.handle.history_complete(AB_CHAT).await.unwrap(), None);

    b.handle.sync().await.unwrap();
    let mut pages = 0;
    while b.handle.load_older(AB_CHAT).await.unwrap().is_some() {
        pages += 1;
        assert!(pages < 20, "older pages never ran out");
    }
    assert_eq!(b.handle.history_complete(AB_CHAT).await.unwrap(), Some(true));
    b.shutdown();
}

#[tokio::test]
async fn a_write_whose_check_cant_sync_is_answered_as_the_row_that_waits() {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            push: false,
            ..How::default()
        },
    );
    a.handle.sync().await.unwrap();

    // the post's answer is a 500, so whether it arrived is unknown, and the chat can't be read to look for it
    a.next_write.store(500, Ordering::SeqCst);
    a.reads_fail.store(true, Ordering::SeqCst);
    let row = a.handle.write(send(AB_CHAT, "did it arrive?")).await;

    // the reply is the row as it waits for its next check, not an error a caller would send the message again on
    let row = row.unwrap();
    assert_eq!(row.state, OutboxState::Pending, "{row:?}");
    assert_eq!((row.attempts, row.failed_checks), (1, 1), "{row:?}");
    assert!(row.not_before.is_some(), "{row:?}");
    let told = a
        .event(|event| match event {
            Event::Outbox(entry) => Some(entry.clone()),
            _ => None,
        })
        .await;
    assert_eq!(told, row);
    assert_eq!(lab.posts_to(AB_CHAT).await, 0);
    a.shutdown();
}

/// A post whose answer was lost has `failed_before` checks that couldn't sync; then the service's copy of it turns
/// into an entry no read can take, and the next check, from the tick, meets it first: the row records it and the check
/// fails. The last [`Event::Outbox`] of the row is the row as the store now has it, whether the check left it waiting
/// or gave it up (`state`), never the copy the read kept before the check changed the row.
async fn an_unreadable_entry_a_check_meets_first(failed_before: u32, state: OutboxState) {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            push: false,
            ..How::default()
        },
    );
    a.handle.sync().await.unwrap();
    let row = a
        .send_unanswered(AB_CHAT, "did it arrive? its copy can't be read")
        .await;

    // more checks that can't sync, each from the tick once the row's backoff is over
    a.reads_fail.store(true, Ordering::SeqCst);
    for failed in 2..=failed_before {
        a.checked_from_the_tick(row.id, failed).await;
    }
    a.reads_fail.store(false, Ordering::SeqCst);
    let unreadable = lab.mock.change_message(AB_CHAT, &row.client_message_id, |message| {
        message["properties"] = json!("unreadable");
    });
    assert!(unreadable);

    let told = a.checked_from_the_tick(row.id, failed_before + 1).await;
    let stored = told_last_as_it_stands(&mut a, told).await;
    assert_eq!(stored.state, state, "{stored:?}");
    assert!(stored.may_have_arrived(), "{stored:?}");
    assert_eq!(lab.posts_to(AB_CHAT).await, 1, "never sent again on its own");
    a.shutdown();
}

/// Checks that `told`, the [`Event::Outbox`] of a row just taken, is the last one told of it and the row as the store
/// has it; the row.
async fn told_last_as_it_stands(a: &mut Native, told: OutboxEntry) -> OutboxEntry {
    let later = a.told_rows(told.id).await;
    let last = later.last().unwrap_or(&told);
    let stored = a.handle.outbox_entry(told.id).await.unwrap().unwrap();
    assert_eq!(
        last, &stored,
        "the last event is the row as it stands; told after it: {later:?}"
    );
    assert!(later.is_empty(), "told once: {later:?}");
    stored
}

#[tokio::test]
async fn an_unreadable_entry_a_check_meets_first_leaves_the_row_waiting_and_told_as_it_stands() {
    an_unreadable_entry_a_check_meets_first(1, OutboxState::Pending).await;
}

#[tokio::test]
async fn an_unreadable_entry_a_check_meets_first_at_its_last_check_fails_the_row_told_as_it_stands() {
    an_unreadable_entry_a_check_meets_first(MAX_FAILED_CHECKS - 1, OutboxState::Failed).await;
}

/// A post whose answer was lost, and whose check couldn't sync; then the service holds two entries of it, one no read
/// can take and the readable one, and the next check, from the tick, meets both on one page: the unreadable one first
/// (the row records it, still pending), then the readable one (the row is sent). The last [`Event::Outbox`] of the row
/// is the row as sent, never the pending copy the same read kept before it.
#[tokio::test]
async fn a_check_meeting_an_unreadable_and_a_readable_entry_of_its_post_tells_the_row_last_as_sent() {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            push: false,
            ..How::default()
        },
    );
    a.handle.sync().await.unwrap();
    let row = a
        .send_unanswered(AB_CHAT, "did it arrive? one of its entries can't be read")
        .await;
    let copied = lab.mock.copy_message(AB_CHAT, &row.client_message_id, |message| {
        message["id"] = json!("9999999999999");
        message["properties"] = json!("unreadable");
    });
    assert!(copied);

    // the check, from the tick once the row's backoff is over
    a.ahead.fetch_add(3_600_000, Ordering::SeqCst);
    let told = a
        .event(|event| match event {
            Event::Outbox(entry) if entry.id == row.id => Some(entry.clone()),
            _ => None,
        })
        .await;
    let stored = told_last_as_it_stands(&mut a, told).await;
    assert_eq!(stored.state, OutboxState::Sent, "{stored:?}");
    assert!(stored.message_id.is_some(), "{stored:?}");
    assert_eq!(lab.posts_to(AB_CHAT).await, 1, "never sent again");
    a.shutdown();
}

/// The service's echo of `from`'s message to `to`'s endpoints, which the mock queues for the chat's other members
/// alone: the one queued for `other`, queued for `to` too.
fn echo(lab: &Lab, from: &str, other: &str, to: &str) {
    echo_changed(lab, from, other, to, |_| {});
}

/// [`echo`], its message changed by `change` first.
fn echo_changed(lab: &Lab, from: &str, other: &str, to: &str, change: impl FnOnce(&mut Value)) {
    let mut event = lab
        .mock
        .take_events(other)
        .into_iter()
        .find(|event| event.kind == EventKind::Message && event.from == from)
        .unwrap();
    change(&mut event.resource);
    lab.mock.queue_event(to, event);
}

#[tokio::test]
async fn a_post_whose_answer_was_lost_is_settled_by_a_later_catch_up_and_told_once() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    let row = a.send_unanswered(AB_CHAT, "did it arrive? ask the catch-up").await;
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);

    // the catch-up finds the message: its row is sent, and told so once
    let report = a.handle.sync().await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    let told = a.told_rows(row.id).await;
    assert_eq!(told.len(), 1, "{told:?}\n{}", a.seen);
    let sent = &told[0];
    assert_eq!(sent.state, OutboxState::Sent, "{sent:?}");
    assert_eq!(sent.client_message_id, row.client_message_id);
    let id = id_of(sent);
    assert_eq!(a.handle.outbox_entry(row.id).await.unwrap().as_ref(), Some(sent));
    assert_eq!(
        a.stored(AB_CHAT, &id).await.client_message_id.as_deref(),
        Some(row.client_message_id.as_str())
    );

    // its echo over the push channel changes nothing, and tells nothing more; nor does another catch-up
    echo(&lab, LAB_A, LAB_B, LAB_A);
    let (change, applied, _) = a.message_event(AB_CHAT, &id).await;
    assert_eq!((change, applied), (MessageChange::New, Applied::Ignored));
    a.handle.sync().await.unwrap();
    let told = a.told_rows(row.id).await;
    assert!(told.is_empty(), "{told:?}");
    assert_eq!(a.copies(AB_CHAT, &id).await, 1);
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);
    assert_eq!(a.log.verdicts("message", AB_CHAT), ["allow"]);
    a.shutdown();
}

#[tokio::test]
async fn a_post_whose_answer_was_lost_is_settled_by_its_echo_over_the_push_channel_and_told_once() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    let row = a.send_unanswered(AB_CHAT, "did it arrive? ask the push channel").await;

    // the echo settles the row, told before the message itself
    echo(&lab, LAB_A, LAB_B, LAB_A);
    let sent = a
        .event(|event| match event {
            Event::Outbox(entry) if entry.id == row.id => Some(entry.clone()),
            Event::Message { .. } => panic!("the message was told before its row"),
            _ => None,
        })
        .await;
    assert_eq!(sent.state, OutboxState::Sent, "{sent:?}");
    let id = id_of(&sent);
    let (change, applied, _) = a.message_event(AB_CHAT, &id).await;
    assert_eq!((change, applied), (MessageChange::New, Applied::Inserted));

    // a catch-up after it finds the row sent already, and tells nothing more
    a.handle.sync().await.unwrap();
    let told = a.told_rows(row.id).await;
    assert!(told.is_empty(), "{told:?}");
    assert_eq!(a.handle.outbox_entry(row.id).await.unwrap(), Some(sent));
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);
    a.shutdown();
}

#[tokio::test]
async fn a_post_whose_answer_was_lost_is_settled_by_an_echo_it_cant_store_and_told_once() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    a.ready().await;
    let row = a
        .send_unanswered(AB_CHAT, "did it arrive? its echo has no version")
        .await;

    // an echo whose version can't be read isn't stored, but it is a's own: its row is sent, and told so
    echo_changed(&lab, LAB_A, LAB_B, LAB_A, |message| {
        message.as_object_mut().unwrap().remove("version");
    });
    let sent = a
        .event(|event| match event {
            Event::Outbox(entry) if entry.id == row.id => Some(entry.clone()),
            _ => None,
        })
        .await;
    assert_eq!(sent.state, OutboxState::Sent, "{sent:?}");
    assert!(a.handle.message(AB_CHAT, id_of(&sent)).await.unwrap().is_none());
    let told = a.told_rows(row.id).await;
    assert!(told.is_empty(), "{told:?}");
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);
    a.shutdown();
}

#[tokio::test]
async fn a_post_whose_answer_was_lost_is_settled_by_the_sync_after_another_write_and_told_once() {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            push: false,
            ..How::default()
        },
    );
    a.handle.sync().await.unwrap();
    let row = a.send_unanswered(AB_CHAT, "did it arrive? ask the next write").await;

    // the next post goes through, and the sync after it brings the first one back too
    let next = a.send(AB_CHAT, "the next one").await;
    let told = a.told_rows(row.id).await;
    assert_eq!(told.len(), 1, "{told:?}\n{}", a.seen);
    assert_eq!(told[0].state, OutboxState::Sent, "{:?}", told[0]);
    assert_ne!(told[0].message_id, next.message_id);
    assert_eq!(a.copies(AB_CHAT, &id_of(&told[0])).await, 1);
    assert_eq!(lab.posts_to(AB_CHAT).await, 2);
    a.shutdown();
}

#[tokio::test]
async fn a_delivery_whose_conversation_cant_be_read_is_caught_up_from_the_tick() {
    let lab = Lab::start().await;
    let how = How {
        groups: Groups::StandIn,
        ..How::default()
    };
    let mut a = native(&lab, LAB_A, how);
    let mut b = native(&lab, LAB_B, how);
    a.ready().await;
    b.ready().await;

    // b can't read the new chat its first message names, so the delivery doesn't reach b's store
    b.reads_fail.store(true, Ordering::SeqCst);
    b.failures_expected = true;
    let chat = id_of(
        &a.write(Action::CreateChat {
            others: vec![Mri::new(LAB_B), Mri::new(LAB_C)],
        })
        .await,
    );
    let first = id_of(&a.send(&chat, "the first message, missed by b").await);
    b.event(|event| match event {
        Event::Failed { during, .. } => (*during == "adding a delivery's conversation").then_some(()),
        Event::Message { id, .. } if *id == first => panic!("the message reached b's store"),
        _ => None,
    })
    .await;
    assert!(b.handle.message(chat.as_str(), first.as_str()).await.unwrap().is_none());

    // once b can read again, the tick's catch-up brings the chat and its message while the socket stays up
    b.reads_fail.store(false, Ordering::SeqCst);
    b.event(|event| match event {
        Event::CaughtUp {
            cause: Cause::Retry,
            report,
        } => report.failed.is_empty().then_some(()),
        Event::Disconnected(_) => panic!("b's socket dropped"),
        _ => None,
    })
    .await;
    assert_eq!(b.copies(&chat, &first).await, 1);
    a.shutdown();
    b.shutdown();
}

// --- the request side, the account's lock, the queries the UI reads ---

/// A native client without a push channel.
const QUIET: How = How {
    groups: Groups::Unknown,
    push: false,
    throttle_first_write: false,
    backoff: Duration::from_millis(50),
};

#[tokio::test]
async fn the_request_side_is_shared_from_any_thread_and_answers_stopped_once_the_engine_is_shut_down() {
    let lab = Lab::start().await;
    let mut b = native(&lab, LAB_B, QUIET);
    let requests = b.handle.requests();

    // from a thread that runs no executor, blocking; from a task, awaiting
    let from_thread = {
        let requests = requests.clone();
        std::thread::spawn(move || requests.sync().blocking_wait())
            .join()
            .unwrap()
            .unwrap()
    };
    assert!(
        from_thread.failed.is_empty() && from_thread.conversations > 0,
        "{from_thread:?}"
    );
    let from_task = tokio::spawn({
        let requests = requests.clone();
        async move { requests.conversations(100).await }
    })
    .await
    .unwrap()
    .unwrap();
    assert!(from_task.iter().any(|conversation| conversation.id == AB_CHAT));

    // a write made through a shared handle is told on the owner's events
    let read = Action::Read {
        conversation: AB_CHAT.to_owned(),
        message_id: A_EDITED.to_owned(),
    };
    let row = requests.write(read).await.unwrap();
    let told = b
        .event(|event| match event {
            Event::Outbox(entry) if entry.id == row.id => Some(entry.clone()),
            _ => None,
        })
        .await;
    assert_eq!(told, row);

    b.shutdown();
    assert!(matches!(requests.conversations(1).await, Err(EngineError::Stopped)));
}

/// An engine for `me` on the store in `dir`, once the one before it has let go of the account's lock (its thread ends
/// after its handle is dropped, which isn't waited for).
async fn native_once_free(lab: &Lab, me: &'static str, dir: &Arc<tempfile::TempDir>) -> Native {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        match try_native_in(lab, me, QUIET, Arc::clone(dir)) {
            Ok(native) => return native,
            Err(EngineError::AlreadyRunning { .. }) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!("the engine didn't start: {error}"),
        }
    }
}

#[tokio::test]
async fn dropping_the_owner_stops_the_engine_whatever_request_handles_are_left() {
    let lab = Lab::start().await;
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let b = try_native_in(&lab, LAB_B, QUIET, Arc::clone(&dir)).unwrap_or_else(|error| panic!("{error}"));
    let requests = b.handle.requests();
    drop(b);
    assert!(matches!(requests.conversations(1).await, Err(EngineError::Stopped)));
    // and the account's lock went with its thread
    native_once_free(&lab, LAB_B, &dir).await.shutdown();
}

#[tokio::test]
async fn a_second_engine_on_the_account_is_refused_at_once_and_starts_once_the_first_is_shut_down() {
    let lab = Lab::start().await;
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let mut first =
        try_native_in(&lab, LAB_A, How::default(), Arc::clone(&dir)).unwrap_or_else(|error| panic!("{error}"));
    first.ready().await;

    let started = std::time::Instant::now();
    let Err(error) = try_native_in(&lab, LAB_A, How::default(), Arc::clone(&dir)) else {
        panic!("a second engine started on the account");
    };
    assert!(started.elapsed() < Duration::from_secs(2), "a refusal never waits");
    assert!(
        matches!(&error, EngineError::AlreadyRunning { lock } if lock.ends_with("lab.sqlite-runs/engine.lock")),
        "{error}"
    );
    assert!(
        error.to_string().contains("another engine runs on this account"),
        "{error}"
    );
    // the refused one never started its push channel; the first one runs on
    let sockets = lab
        .trouter
        .sockets()
        .into_iter()
        .filter(|socket| socket.account.as_deref() == Some(LAB_A))
        .count();
    assert_eq!(sockets, 1);
    assert!(!first.handle.conversations(1).await.unwrap().is_empty());

    first.shutdown();
    let second = try_native_in(&lab, LAB_A, QUIET, Arc::clone(&dir)).unwrap_or_else(|error| panic!("{error}"));
    assert!(
        !second.handle.conversations(1).await.unwrap().is_empty(),
        "the same store"
    );
    second.shutdown();
}

#[tokio::test]
async fn the_unread_count_and_the_read_horizon_follow_the_store() {
    let lab = Lab::start().await;
    let a = native(&lab, LAB_A, QUIET);
    let b = native(&lab, LAB_B, QUIET);
    b.handle.sync().await.unwrap();
    let before = b.handle.unread_count(AB_CHAT).await.unwrap();

    let id = id_of(&a.send(AB_CHAT, "count me").await);
    b.handle.sync().await.unwrap();
    assert_eq!(b.handle.unread_count(AB_CHAT).await.unwrap(), before + 1);

    let read = b
        .write(Action::Read {
            conversation: AB_CHAT.to_owned(),
            message_id: id.clone(),
        })
        .await;
    assert_eq!(read.state, OutboxState::Sent, "{read:?}");
    b.handle.sync().await.unwrap();
    let horizon = b.handle.read_horizon(AB_CHAT).await.unwrap().unwrap();
    assert_eq!(horizon.user, LAB_B);
    assert_eq!(horizon.last_read_arrival, b.stored(AB_CHAT, &id).await.arrival_time);
    // what is unread now is what arrived after it (the fixture's messages carry later arrival times than the mock's
    // new ones), from a, standing
    let page = b.handle.messages(AB_CHAT, None, 100).await.unwrap();
    let after = page
        .messages
        .iter()
        .filter(|message| {
            message.arrival_time > horizon.last_read_arrival
                && message.sender.as_deref() == Some(LAB_A)
                && message.delete_time.is_none()
        })
        .count();
    assert_eq!(
        b.handle.unread_count(AB_CHAT).await.unwrap(),
        u64::try_from(after).unwrap()
    );
    assert!(b.handle.read_horizon("19:unknown@thread.v2").await.unwrap().is_none());
    a.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn the_requests_name_the_account_and_a_person_s_name_comes_from_the_store() {
    let lab = Lab::start().await;
    let b = native(&lab, LAB_B, QUIET);
    assert_eq!(b.handle.me(), LAB_B);
    assert_eq!(b.handle.requests().me(), LAB_B, "every clone names the account");
    assert_eq!(b.handle.display_name(LAB_A).await.unwrap(), None, "nothing synced yet");

    b.handle.sync().await.unwrap();
    // the a–b chat's messages are a's, each carrying a's name
    assert_eq!(b.handle.display_name(LAB_A).await.unwrap().as_deref(), Some("Lab A"));
    assert_eq!(b.handle.display_name(LAB_C).await.unwrap(), None);
    b.shutdown();
}

#[tokio::test]
async fn a_row_an_earlier_run_left_goes_out_only_by_an_explicit_resend_which_looks_for_it_first() {
    let lab = Lab::start().await;
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let mut first = try_native_in(&lab, LAB_A, QUIET, Arc::clone(&dir)).unwrap_or_else(|error| panic!("{error}"));
    let row = first.send_unanswered(AB_CHAT, "its answer was lost").await;
    assert_eq!(
        first.handle.outbox_unsettled(AB_CHAT).await.unwrap(),
        std::slice::from_ref(&row)
    );
    assert!(
        first.handle.left_by_earlier_runs().await.unwrap().is_empty(),
        "its own run's"
    );
    first.shutdown();
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);

    let mut second = native_once_free(&lab, LAB_A, &dir).await;
    let left = second.handle.left_by_earlier_runs().await.unwrap();
    assert_eq!(left.iter().map(|left| left.id).collect::<Vec<_>>(), [row.id]);
    assert_eq!(second.handle.outbox_unsettled(AB_CHAT).await.unwrap().len(), 1);
    // it doesn't go out on its own, however many ticks pass
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);

    // the explicit resend: the row's earlier attempt arrived, which its check finds, so nothing goes out again
    let resent = second.handle.resend(row.id).await.unwrap();
    assert_eq!(resent.state, OutboxState::Sent, "{resent:?}");
    assert_eq!(resent.client_message_id, row.client_message_id);
    assert_ne!(resent.run_id, row.run_id, "adopted into this run");
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);
    assert!(second.handle.outbox_unsettled(AB_CHAT).await.unwrap().is_empty());
    assert!(second.handle.left_by_earlier_runs().await.unwrap().is_empty());
    let told = second
        .event(|event| match event {
            Event::Outbox(entry) if entry.id == row.id => Some(entry.clone()),
            _ => None,
        })
        .await;
    assert_eq!(told.state, OutboxState::Sent);
    // a sent row isn't sent again
    let refused = second.handle.resend(row.id).await;
    assert!(
        matches!(refused, Err(EngineError::Store(StoreError::OutboxNotSendable { .. }))),
        "{refused:?}"
    );

    // a denied row is the conversation's to show until it is resent
    let denied = second.write(send(ABC_CHAT, "never sent")).await;
    assert_eq!(denied.state, OutboxState::Denied);
    assert_eq!(second.handle.outbox_unsettled(ABC_CHAT).await.unwrap(), [denied]);
    second.shutdown();
}

#[tokio::test]
async fn a_catch_up_tells_what_it_stored_and_changed_once_marked_as_its_own() {
    let lab = Lab::start().await;
    let a = native(&lab, LAB_A, QUIET);
    let mut b = native(&lab, LAB_B, QUIET);

    let report = b.handle.sync().await.unwrap();
    let told = b.drain().await;
    let messages: Vec<_> = told
        .iter()
        .filter_map(|event| match event {
            Event::Message {
                change,
                applied,
                message,
                source,
                ..
            } => Some((*change, *applied, message.is_some(), *source)),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), report.messages.changed(), "{messages:?}");
    assert!(
        messages
            .iter()
            .all(|told| *told == (MessageChange::New, Applied::Inserted, true, Source::CatchUp)),
        "{messages:?}"
    );
    let changed = conversations_changed(&told);
    assert!(changed.contains(&AB_CHAT.to_owned()), "{changed:?}");
    let mut once = changed.clone();
    once.sort();
    once.dedup();
    assert_eq!(once.len(), changed.len(), "each conversation once: {changed:?}");

    // nothing changed: nothing told
    b.handle.sync().await.unwrap();
    let told = b.drain().await;
    assert!(
        told.iter()
            .all(|event| !matches!(event, Event::Message { .. } | Event::ConversationChanged { .. })),
        "{told:?}"
    );

    // a's new message: told once, as new, with the conversation it changed
    let id = id_of(&a.send(AB_CHAT, "brought by a catch-up").await);
    b.handle.sync().await.unwrap();
    let told = b.drain().await;
    let messages: Vec<_> = told
        .iter()
        .filter_map(|event| match event {
            Event::Message {
                id,
                change,
                applied,
                message,
                source,
                ..
            } => Some((
                id.clone(),
                *change,
                *applied,
                *source,
                message.as_ref()?.content.clone(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    let (got, change, applied, source, content) = &messages[0];
    assert_eq!(
        (got, *change, *applied, *source),
        (&id, MessageChange::New, Applied::Inserted, Source::CatchUp)
    );
    assert!(content.contains("brought by a catch-up"), "{content}");
    assert_eq!(conversations_changed(&told), [AB_CHAT]);
    a.shutdown();
    b.shutdown();
}

/// The conversations `events` told changed, in order.
fn conversations_changed(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::ConversationChanged { conversation } => Some(conversation.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_message_a_delivery_brought_is_not_told_again_by_a_catch_up() {
    let lab = Lab::start().await;
    let mut a = native(&lab, LAB_A, How::default());
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;
    let id = id_of(&a.send(AB_CHAT, "delivered, then caught up").await);
    let (applied, source) = b
        .event(|event| match event {
            Event::Message {
                id: got,
                applied,
                source,
                ..
            } if *got == id => Some((*applied, *source)),
            _ => None,
        })
        .await;
    assert_eq!((applied, source), (Applied::Inserted, Source::Delivery));

    b.handle.sync().await.unwrap();
    let told = b.drain().await;
    assert!(
        !told
            .iter()
            .any(|event| matches!(event, Event::Message { id: got, .. } if *got == id)),
        "{told:?}"
    );
    a.shutdown();
    b.shutdown();
}

// --- transient writes ---

fn typing(conversation: &str) -> Transient {
    Transient::Typing {
        conversation: conversation.to_owned(),
    }
}

#[tokio::test]
async fn typing_reaches_b_under_a_fresh_decision_each_time_and_nothing_of_it_is_stored_or_survives_a_restart() {
    let lab = Lab::start().await;
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let mut a = try_native_in(&lab, LAB_A, How::default(), Arc::clone(&dir)).unwrap_or_else(|error| panic!("{error}"));
    let mut b = native(&lab, LAB_B, How::default());
    a.ready().await;
    b.ready().await;

    for _ in 0..2 {
        assert_eq!(a.handle.transient(typing(AB_CHAT)).await.unwrap(), TransientSent::Sent);
        let from = b
            .event(|event| match event {
                Event::Typing {
                    conversation,
                    from,
                    typing: true,
                } if conversation == AB_CHAT => Some(from.clone()),
                _ => None,
            })
            .await;
        assert_eq!(from.as_deref(), Some(LAB_A));
    }
    assert_eq!(a.log.verdicts("typing", AB_CHAT), ["allow", "allow"]);
    assert_eq!(lab.posts_to(AB_CHAT).await, 2);

    // no row: none is the conversation's, none was ever made, and none is left for a later run
    assert!(a.handle.outbox_unsettled(AB_CHAT).await.unwrap().is_empty());
    assert!(a.handle.outbox_entry(1).await.unwrap().is_none());
    a.shutdown();
    let again = native_once_free(&lab, LAB_A, &dir).await;
    assert!(again.handle.left_by_earlier_runs().await.unwrap().is_empty());
    assert!(again.handle.outbox_entry(1).await.unwrap().is_none());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lab.posts_to(AB_CHAT).await, 2, "nothing sent again after the restart");
    again.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn typing_whose_outcome_is_unknown_is_dropped_and_never_sent_again() {
    let lab = Lab::start().await;
    let a = native(&lab, LAB_A, QUIET);
    a.handle.sync().await.unwrap();

    a.lose_answer.store(true, Ordering::SeqCst);
    let sent = a.handle.transient(typing(AB_CHAT)).await.unwrap();
    assert!(
        matches!(&sent, TransientSent::Dropped { reason } if reason.contains("whether it arrived is unknown")),
        "{sent:?}"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lab.posts_to(AB_CHAT).await, 1, "no check, no second attempt");
    assert_eq!(a.log.verdicts("typing", AB_CHAT), ["allow"]);

    // a 401 gets one more attempt at once, as a row's does; any other refusal drops it
    a.next_write.store(401, Ordering::SeqCst);
    assert_eq!(a.handle.transient(typing(AB_CHAT)).await.unwrap(), TransientSent::Sent);
    assert_eq!(lab.posts_to(AB_CHAT).await, 2);
    a.next_write.store(400, Ordering::SeqCst);
    let sent = a.handle.transient(typing(AB_CHAT)).await.unwrap();
    assert_eq!(
        sent,
        TransientSent::Dropped {
            reason: "HTTP 400".to_owned()
        }
    );
    assert_eq!(a.log.verdicts("typing", AB_CHAT), ["allow"; 4]);
    assert!(a.handle.outbox_entry(1).await.unwrap().is_none());
    a.shutdown();
}

#[tokio::test]
async fn throttled_typing_waits_and_only_the_latest_goes_out_from_the_tick() {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            throttle_first_write: true,
            ..QUIET
        },
    );
    a.handle.sync().await.unwrap();

    let first = a.handle.transient(typing(AB_CHAT)).await.unwrap();
    let TransientSent::Waiting { not_before } = first else {
        panic!("{first:?}");
    };
    // asked again while it waits: it takes the first one's place, and waits as long
    let second = a.handle.transient(typing(AB_CHAT)).await.unwrap();
    assert_eq!(second, TransientSent::Waiting { not_before });
    let (transient, sent) = a
        .event(|event| match event {
            Event::Transient(transient, sent) => Some((transient.clone(), sent.clone())),
            _ => None,
        })
        .await;
    assert_eq!((transient, sent), (typing(AB_CHAT), TransientSent::Sent));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let told = a.drain().await;
    assert!(
        !told.iter().any(|event| matches!(event, Event::Transient(..))),
        "one went out from the tick: {told:?}"
    );
    // the throttled attempt never reached the service; the replaced one never went out
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);
    assert_eq!(a.log.verdicts("typing", AB_CHAT), ["allow", "allow"]);
    a.shutdown();
}

#[tokio::test]
async fn a_message_post_drops_the_typing_waiting_in_its_conversation() {
    let lab = Lab::start().await;
    let mut a = native(
        &lab,
        LAB_A,
        How {
            throttle_first_write: true,
            ..QUIET
        },
    );
    a.handle.sync().await.unwrap();

    let waiting = a.handle.transient(typing(AB_CHAT)).await.unwrap();
    assert!(matches!(waiting, TransientSent::Waiting { .. }), "{waiting:?}");
    let row = a.handle.write(send(AB_CHAT, "typed")).await.unwrap();
    assert_eq!(row.state, OutboxState::Sent);
    let (transient, sent) = a
        .event(|event| match event {
            Event::Transient(transient, sent) => Some((transient.clone(), sent.clone())),
            _ => None,
        })
        .await;
    assert_eq!(transient, typing(AB_CHAT));
    assert!(matches!(sent, TransientSent::Dropped { .. }), "{sent:?}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let told = a.drain().await;
    assert!(
        !told.iter().any(|event| matches!(event, Event::Transient(..))),
        "no typing after the message: {told:?}"
    );
    // the message alone reached the service: the throttled typing never did, and nothing followed
    assert_eq!(lab.posts_to(AB_CHAT).await, 1);
    assert_eq!(a.log.verdicts("typing", AB_CHAT), ["allow"]);
    a.shutdown();
}

#[tokio::test]
async fn typing_is_fenced_like_any_write_and_refused_where_the_store_knows_no_chat() {
    let lab = Lab::start().await;
    let a = native(&lab, LAB_A, QUIET);
    a.handle.sync().await.unwrap();
    let denied = a.handle.transient(typing(ABC_CHAT)).await.unwrap();
    assert!(matches!(denied, TransientSent::Denied { .. }), "{denied:?}");
    assert_eq!(a.log.verdicts("typing", ABC_CHAT), ["deny"]);
    assert_eq!(lab.posts_to(ABC_CHAT).await, 0);

    let refused = a.handle.transient(typing("19:unknown@thread.v2")).await;
    assert!(
        matches!(refused, Err(EngineError::Store(StoreError::UnknownConversation(_)))),
        "{refused:?}"
    );
    assert_eq!(a.log.lines().len(), 1, "the refused one reached no fence");
    a.shutdown();
}
