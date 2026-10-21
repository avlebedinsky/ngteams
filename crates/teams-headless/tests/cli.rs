//! The lab binary's commands, offline: the binary this test build made (its `--help`, and its refusal to run, since
//! `test-support` is unified into it), which chat a command's argument names, and the commands themselves on native
//! engines of a and b against the mock chat service and its Trouter (`teams-mock`, through the `test-support` mock
//! origins): a's `sync`, `send`, `edit` and `react` print what they did, b's `watch` (as JSON and as text) prints each
//! event with its receive time, and `list` and `show` print a's store.
//!
//! Every id and token here is the mock's made-up one, never a real one.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{EngineError, Outbox};
use teams_fence::lab::Alias;
use teams_fence::{Fence, FenceLog, LiveMembershipSource, Mri, Roster, one_to_one_id};
use teams_headless::chat::{ChatError, LabNames, resolve};
use teams_headless::commands::{self, CommandError, WatchEnd};
use teams_headless::lab::display_name;
use teams_mock::{AB_CHAT, ABC_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, TrouterMock};
use teams_proto::chat::MessageText;
use teams_proto::{
    Author, Authorized, Client, Credentials, HttpError, HttpTransport, Mutating, OwnState, Read, Response, Transport,
};
use teams_store::{OutboxState, Store};
use teams_trouter::{Options, Trouter, TrouterCredentials};
use tokio::sync::{broadcast, oneshot};

/// How long a test waits for output before it fails.
const WAIT: Duration = Duration::from_secs(30);

/// The binary this test build made, with `test-support` unified in.
const BINARY: &str = env!("CARGO_BIN_EXE_teams-headless");

// --- the binary ---

#[test]
fn help_names_every_command() {
    let output = Command::new(BINARY).arg("--help").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "sync", "list", "show", "send", "edit", "delete", "react", "read", "watch",
    ] {
        assert!(
            help.lines().any(|line| line.trim_start().starts_with(command)),
            "{command}: {help}"
        );
    }
    assert!(help.contains("<ALIAS>"), "{help}");
}

#[test]
fn a_build_with_test_support_refuses_to_run_before_it_touches_the_lab() {
    assert!(
        teams_fence::test_support_enabled(),
        "this test build unifies teams-fence's test-support into the binary"
    );
    let home = tempfile::tempdir().unwrap();
    for args in [
        &["a", "list"][..],
        &["b", "watch", "--json"],
        &["a", "send", "b", "hello"],
    ] {
        let output = Command::new(BINARY)
            .args(args)
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("test-support"), "{args:?}: {stderr}");
        assert!(stderr.contains("cargo build -p teams-headless"), "{args:?}: {stderr}");
    }
    assert_eq!(
        std::fs::read_dir(home.path()).unwrap().count(),
        0,
        "nothing was created: no lab dir, no store, no fence.log"
    );
}

#[test]
fn a_usage_error_is_reported_by_the_parser() {
    let output = Command::new(BINARY).args(["d", "list"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let output = Command::new(BINARY).args(["a", "send", "b"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

// --- which chat ---

fn names() -> LabNames {
    LabNames::new([Some(LAB_A), Some(LAB_B), Some(LAB_C)])
}

#[test]
fn a_lab_alias_names_the_one_to_one_chat_with_that_account() {
    assert_eq!(resolve("b", Alias::A, &names(), None).unwrap(), AB_CHAT);
    assert_eq!(
        resolve("a", Alias::B, &names(), None).unwrap(),
        AB_CHAT,
        "the same chat from b"
    );
    assert_eq!(one_to_one_id(LAB_B, LAB_A).as_deref(), Some(AB_CHAT));
    assert_eq!(resolve("a", Alias::A, &names(), None), Err(ChatError::Own(Alias::A)));
    let unknown_c = LabNames::new([Some(LAB_A), Some(LAB_B), None]);
    assert_eq!(
        resolve("c", Alias::A, &unknown_c, None),
        Err(ChatError::NoMri(Alias::C))
    );
    let bot = LabNames::new([Some(LAB_A), Some("28:a-bot"), Some(LAB_C)]);
    assert_eq!(
        resolve("b", Alias::A, &bot, None),
        Err(ChatError::NoOneToOne(Alias::A, Alias::B))
    );
}

#[test]
fn abc_is_the_recorded_group_chat_and_an_id_is_taken_as_it_is() {
    assert_eq!(resolve("abc", Alias::C, &names(), Some(ABC_CHAT)).unwrap(), ABC_CHAT);
    assert_eq!(resolve("abc", Alias::C, &names(), None), Err(ChatError::NoGroupChat));
    assert_eq!(resolve(AB_CHAT, Alias::C, &names(), None).unwrap(), AB_CHAT);
    for refused in ["d", "", "19:a b", "hello"] {
        assert_eq!(
            resolve(refused, Alias::A, &names(), None),
            Err(ChatError::NotAChat(refused.to_owned())),
            "{refused:?}"
        );
    }
}

#[test]
fn a_post_carries_the_upn_s_local_part_as_its_name() {
    assert_eq!(display_name("lab-a@example.invalid"), "lab-a");
    assert_eq!(display_name("no-domain"), "no-domain");
}

// --- the commands on the mock ---

/// One account's mock token, for the transport and the push channel alike; it never runs out in a test.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
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

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

/// What a command printed, shared with the test while it runs.
#[derive(Clone, Default)]
struct Printed(Arc<Mutex<Vec<u8>>>);

impl Printed {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()).unwrap()
    }

    fn json_lines(&self) -> Vec<Value> {
        self.text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Waits until a JSON line satisfies `pick`; fails after [`WAIT`] with everything printed.
    async fn json_line(&self, mut pick: impl FnMut(&Value) -> bool) -> Value {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(line) = self.json_lines().into_iter().find(|line| pick(line)) {
                return line;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "not printed in time:\n{}",
                self.text()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Write for Printed {
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

struct Lab {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    dir: tempfile::TempDir,
}

impl Lab {
    async fn start() -> Self {
        let mock = ChatServiceMock::start().await.unwrap();
        let trouter = mock.start_trouter().unwrap();
        Self {
            mock,
            trouter,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn store_file(&self, me: &str) -> std::path::PathBuf {
        self.dir
            .path()
            .join("store")
            .join(format!("{}.sqlite", me.replace(':', "_")))
    }

    /// `me`'s engine against the mocks, as the lab binary builds it but for the mock origins, a test roster and a
    /// test log: its push channel with `push`.
    fn engine(&self, me: &str, push: bool) -> EngineHandle {
        self.engine_with(me, push, false)
    }

    /// As [`Self::engine`], its first write answered 429 with a short `Retry-After` with `throttle_first_write`.
    fn engine_with(&self, me: &str, push: bool, throttle_first_write: bool) -> EngineHandle {
        let tokens = Arc::new(Tokens {
            token: self.mock.token(me).unwrap(),
            renewals: broadcast::channel(4).0,
        });
        let fence = Arc::new(Fence::new(
            Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
            FenceLog::from_writer(io::sink()),
        ));
        let origin = self.mock.origin();
        let http =
            || HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &origin).unwrap();
        let transport = Arc::new(http());
        let membership = LiveMembershipSource::with_mock_origin(&origin).unwrap();
        membership.set_credential_source(transport.clone());
        let push = push.then(|| {
            let trouter = Trouter::with_mock_origin(
                Arc::clone(&transport),
                Arc::clone(&tokens),
                Arc::clone(&fence),
                self.trouter.origin(),
            )
            .unwrap()
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
            Push::new(trouter)
        });
        let author = Author::new(Mri::new(me), display_name("lab@example.invalid"));
        let store = Store::open(&self.store_file(me)).unwrap();
        let client = Arc::new(Client::new(http()));
        let membership = Arc::new(membership);
        if throttle_first_write {
            let throttled = Arc::new(ThrottledOnce {
                inner: transport,
                throttled: AtomicBool::new(false),
            });
            start(store, client, Outbox::new(throttled, fence, membership, author), push)
        } else {
            start(store, client, Outbox::new(transport, fence, membership, author), push)
        }
    }
}

fn start<T: Transport<Error = HttpError> + Send + 'static>(
    store: Store,
    client: Arc<Client<HttpTransport<Tokens>>>,
    outbox: Outbox<T>,
    push: Option<Push>,
) -> EngineHandle {
    engine::start(Parts {
        store,
        client,
        outbox,
        push,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            catch_up_retry: Duration::from_millis(500),
        },
    })
    .unwrap()
}

/// The HTTP transport, whose first write is answered 429 with a short `Retry-After` before it reaches the service.
struct ThrottledOnce {
    inner: Arc<HttpTransport<Tokens>>,
    throttled: AtomicBool,
}

impl Transport for ThrottledOnce {
    type Error = HttpError;

    fn read<R: Read>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        self.inner.read(request)
    }

    fn own_state<R: OwnState>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        self.inner.own_state(request)
    }

    async fn deliver<R: Mutating + Send>(&self, write: Authorized<R>) -> Result<Response, HttpError> {
        if !self.throttled.swap(true, Ordering::SeqCst) {
            return Ok(Response::new(429, Vec::new()).with_retry_after(Duration::from_millis(300)));
        }
        self.inner.deliver(write).await
    }
}

/// `watch` on `handle` in a task of its own, until the returned sender fires.
fn watching(
    mut handle: EngineHandle,
    json: bool,
) -> (
    Printed,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<(EngineHandle, WatchEnd)>,
) {
    let printed = Printed::default();
    let (stop, stopped) = oneshot::channel::<()>();
    let mut out = printed.clone();
    let task = tokio::spawn(async move {
        let end = commands::watch(&mut handle, json, &names(), &mut out, async {
            let _ = stopped.await;
        })
        .await
        .unwrap();
        (handle, end)
    });
    (printed, stop, task)
}

/// Waits until a watch has its push channel registered and its first catch-up done.
async fn ready(watch: &Printed) {
    watch
        .json_line(|line| line["event"] == "registration" && line["registered"] == true)
        .await;
    watch
        .json_line(|line| line["event"] == "caught_up" && line["cause"] == "connected" && line["failed"] == 0)
        .await;
}

/// A write command's printed lines as `(key, value)`.
fn fields(printed: &str) -> Vec<(String, String)> {
    printed
        .lines()
        .map(|line| {
            let (key, value) = line.split_once(' ').unwrap();
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

fn field<'a>(fields: &'a [(String, String)], key: &str) -> &'a str {
    &fields
        .iter()
        .find(|(name, _)| name == key)
        .unwrap_or_else(|| panic!("no {key}: {fields:?}"))
        .1
}

/// A post of `text` to the a–b chat.
fn post(text: &str) -> Action {
    Action::Send {
        conversation: AB_CHAT.to_owned(),
        text: MessageText::plain(text).unwrap(),
    }
}

async fn write(handle: &mut EngineHandle, action: Action) -> (OutboxState, Vec<(String, String)>) {
    let mut out = Vec::new();
    let state = commands::write(handle, action, WAIT, &mut out).await.unwrap();
    (state, fields(&String::from_utf8(out).unwrap()))
}

#[tokio::test]
async fn a_s_send_edit_and_reaction_reach_b_s_watch_with_their_times() {
    let lab = Lab::start().await;
    let (watch, stop, task) = watching(lab.engine(LAB_B, true), true);
    ready(&watch).await;
    let mut a = lab.engine(LAB_A, false);

    let mut synced = Vec::new();
    let report = commands::sync(&a, &mut synced).await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    let synced = String::from_utf8(synced).unwrap();
    assert!(synced.starts_with("caught_up conversations="), "{synced}");

    let before = teams_headless::print::now_ms();
    let (state, sent) = write(&mut a, post("hello b, from teams-headless")).await;
    assert_eq!(state, OutboxState::Sent, "{sent:?}");
    let keys: Vec<&str> = sent.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        keys,
        ["start_ms", "original_arrival_time", "message_id", "state"],
        "{sent:?}"
    );
    let started: i64 = field(&sent, "start_ms").parse().unwrap();
    assert!(started >= before, "{sent:?}");
    let arrival: i64 = field(&sent, "original_arrival_time").parse().unwrap();
    let id = field(&sent, "message_id").to_owned();
    assert_eq!(arrival.to_string(), id, "a message's id is its arrival time");
    assert_eq!(field(&sent, "state"), "sent");

    let received = watch
        .json_line(|line| line["event"] == "message" && line["id"] == id.as_str())
        .await;
    assert_eq!(received["conversation"], AB_CHAT);
    assert_eq!(received["change"], "new");
    assert_eq!(received["applied"], "inserted");
    assert_eq!(received["sender"], "a", "a lab account is printed by its alias");
    assert_eq!(received["text"], "hello b, from teams-headless");
    assert_eq!(received["arrival_time"], arrival);
    assert!(received["received_ms"].as_i64().unwrap() >= started, "{received}");

    let edit = Action::Edit {
        conversation: AB_CHAT.to_owned(),
        message_id: id.clone(),
        text: MessageText::plain("hello b, edited").unwrap(),
    };
    let (state, edited) = write(&mut a, edit).await;
    assert_eq!(state, OutboxState::Sent, "{edited:?}");
    assert_eq!(field(&edited, "message_id"), id);
    assert!(
        edited.iter().all(|(key, _)| key != "original_arrival_time"),
        "{edited:?}"
    );
    let changed = watch
        .json_line(|line| line["event"] == "message" && line["id"] == id.as_str() && line["change"] == "edit")
        .await;
    assert_eq!(changed["text"], "hello b, edited");

    let react = Action::React {
        conversation: AB_CHAT.to_owned(),
        message_id: id.clone(),
        key: "like".to_owned(),
    };
    let (state, reacted) = write(&mut a, react).await;
    assert_eq!(state, OutboxState::Sent, "{reacted:?}");
    watch
        .json_line(|line| line["event"] == "message" && line["id"] == id.as_str() && line["change"] == "reaction")
        .await;

    let _ = stop.send(());
    let (b, end) = task.await.unwrap();
    assert_eq!(end, WatchEnd::Stopped);
    assert!(
        watch.json_lines().iter().all(|line| line["received_ms"].is_i64()),
        "every event has its receive time:\n{}",
        watch.text()
    );
    b.shutdown();
    a.shutdown();
    lists_and_shows_the_edited_message(&lab, &id, arrival);
}

#[tokio::test]
async fn b_s_read_moves_its_read_horizon_up_to_a_s_message() {
    let lab = Lab::start().await;
    let mut a = lab.engine(LAB_A, false);
    commands::sync(&a, &mut Vec::new()).await.unwrap();
    let (state, sent) = write(&mut a, post("read me, b")).await;
    assert_eq!(state, OutboxState::Sent, "{sent:?}");
    let id = field(&sent, "message_id").to_owned();
    let mut b = lab.engine(LAB_B, false);
    commands::sync(&b, &mut Vec::new()).await.unwrap();
    let read = Action::Read {
        conversation: AB_CHAT.to_owned(),
        message_id: id.clone(),
    };
    let (state, printed) = write(&mut b, read).await;
    assert_eq!(state, OutboxState::Sent, "{printed:?}");
    let keys: Vec<&str> = printed.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(keys, ["start_ms", "message_id", "state"], "{printed:?}");
    let horizons = lab
        .mock
        .received_requests()
        .await
        .into_iter()
        .filter(|request| request.method.as_str() == "PUT" && request.url.query() == Some("name=consumptionhorizon"))
        .count();
    assert_eq!(horizons, 1, "b's read receipt reached the service once");
    a.shutdown();
    b.shutdown();
}

/// a's store as `list` and `show` print it: the a–b chat, and in it a's message `id`, edited, with a's like.
fn lists_and_shows_the_edited_message(lab: &Lab, id: &str, arrival: i64) {
    let store = Store::open(&lab.store_file(LAB_A)).unwrap();
    let mut listed = Vec::new();
    commands::list(&store, 50, &names(), &mut listed).unwrap();
    let listed = String::from_utf8(listed).unwrap();
    let chat = listed
        .lines()
        .find(|line| line.contains(AB_CHAT))
        .unwrap_or_else(|| panic!("{listed}"));
    assert!(chat.contains(" one_to_one "), "{chat}");
    let mut shown = Vec::new();
    commands::show(&store, AB_CHAT, 50, &names(), &mut shown).unwrap();
    let shown = String::from_utf8(shown).unwrap();
    let ours = shown
        .lines()
        .find(|line| line.split(' ').nth(1) == Some(id))
        .unwrap_or_else(|| panic!("{shown}"));
    assert_eq!(
        ours,
        format!("{arrival} {id} a (edited): hello b, edited [like: a]"),
        "{shown}"
    );
    let arrivals: Vec<i64> = shown
        .lines()
        .map(|line| line.split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert!(arrivals.is_sorted(), "the oldest first:\n{shown}");
}

#[tokio::test]
async fn watch_as_text_prints_each_event_on_a_line_with_its_receive_time() {
    let lab = Lab::start().await;
    let (watch, stop, task) = watching(lab.engine(LAB_B, true), false);
    let deadline = tokio::time::Instant::now() + WAIT;
    while !watch.text().lines().any(|line| line.contains(" caught_up ")) {
        assert!(tokio::time::Instant::now() < deadline, "{}", watch.text());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = stop.send(());
    let (b, end) = task.await.unwrap();
    assert_eq!(end, WatchEnd::Stopped);
    b.shutdown();
    let text = watch.text();
    for line in text.lines() {
        let (received, rest) = line.split_once(' ').unwrap();
        assert!(received.parse::<i64>().is_ok(), "{line}");
        assert!(!rest.is_empty(), "{line}");
    }
    let connected = text
        .lines()
        .find(|line| line.contains(" connected "))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(connected.contains("endpoint=\""), "{connected}");
    assert!(
        !text.contains(&lab.mock.token(LAB_B).unwrap()),
        "no token printed:\n{text}"
    );
}

#[tokio::test]
async fn a_throttled_send_is_waited_for_until_the_engine_s_tick_sends_it() {
    let lab = Lab::start().await;
    let mut a = lab.engine_with(LAB_A, false, true);
    commands::sync(&a, &mut Vec::new()).await.unwrap();
    let (state, sent) = write(&mut a, post("after a short wait")).await;
    assert_eq!(state, OutboxState::Sent, "{sent:?}");
    assert_eq!(
        field(&sent, "original_arrival_time"),
        field(&sent, "message_id"),
        "{sent:?}"
    );
    assert!(sent.iter().all(|(key, _)| key != "error"), "{sent:?}");

    // a wait that ends first leaves the row as it stands, and says so
    let mut b = lab.engine_with(LAB_B, false, true);
    commands::sync(&b, &mut Vec::new()).await.unwrap();
    let mut out = Vec::new();
    let state = commands::write(&mut b, post("not waited for"), Duration::ZERO, &mut out)
        .await
        .unwrap();
    assert_eq!(state, OutboxState::Pending);
    let printed = fields(&String::from_utf8(out).unwrap());
    assert_eq!(field(&printed, "state"), "pending");
    assert_eq!(field(&printed, "original_arrival_time"), "unknown");
    assert_eq!(field(&printed, "message_id"), "-");
    a.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn a_write_the_store_refuses_fails_and_show_needs_a_known_chat() {
    let lab = Lab::start().await;
    let mut a = lab.engine(LAB_A, false);
    let mut out = Vec::new();
    commands::sync(&a, &mut out).await.unwrap();
    let edit = Action::Edit {
        conversation: AB_CHAT.to_owned(),
        message_id: "1".to_owned(),
        text: MessageText::plain("nothing to edit").unwrap(),
    };
    let mut printed = Vec::new();
    let refused = commands::write(&mut a, edit, WAIT, &mut printed).await;
    assert!(
        matches!(refused, Err(CommandError::Engine(EngineError::UnknownMessage { .. }))),
        "{refused:?}"
    );
    assert!(printed.is_empty(), "nothing printed for a refused write");
    a.shutdown();

    let store = Store::open(&lab.store_file(LAB_A)).unwrap();
    let unknown = commands::show(&store, "19:unknown@thread.v2", 10, &names(), &mut Vec::new());
    assert!(matches!(unknown, Err(CommandError::Store(_))), "{unknown:?}");
    assert!(Path::new(&lab.store_file(LAB_A)).exists());
}
