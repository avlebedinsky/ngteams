//! The lab binary's commands, offline: the binary this test build made (its `--help`, and its refusal to run, since
//! `test-support` is unified into it), which chat a command's argument names, and the commands themselves on native
//! engines of a and b against the mock chat service and its Trouter (`teams-mock`, through the `test-support` mock
//! origins): a's `sync`, `send`, `edit` and `react` print what they did, b's `watch` (as JSON and as text) prints each
//! event with its receive time, `list` and `show` print a's store, and `presence` and `profile` print anyone asked
//! about by MRI.
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

use clap::Parser as _;
use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{EngineError, MAX_WATCHED, NameFrom, Outbox};
use teams_fence::lab::{Alias, LabConfig, record_abc_chat};
use teams_fence::{Fence, FenceLog, LiveMembershipSource, Mri, Roster, one_to_one_id};
use teams_headless::availability;
use teams_headless::chat::{ChatError, LabNames, resolve};
use teams_headless::cli::{Cli, Command as CliCommand};
use teams_headless::commands::{self, CommandError, WatchEnd};
use teams_headless::lab::display_name;
use teams_headless::print::name_source_word;
use teams_mock::{
    AB_CHAT, ABC_CHAT, AvailabilityChange, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, OUTSIDER, PRESENCE_BASE,
    TrouterMock, lab_toml,
};
use teams_proto::ManualAvailability;
use teams_proto::chat::MessageText;
use teams_proto::{
    Author, Authorized, Client, Credentials, HttpError, HttpTransport, Mutating, Read, Request as ProtoRequest,
    Response, Transport,
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
        "sync",
        "list",
        "show",
        "send",
        "edit",
        "delete",
        "react",
        "read",
        "watch",
        "presence",
        "profile",
        "availability",
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
    // a lookup names whom it looks up
    let output = Command::new(BINARY).args(["a", "profile"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

#[test]
fn presence_takes_anyone_by_mri_and_profile_takes_their_mris() {
    let parsed = Cli::try_parse_from(["teams-headless", "a", "presence", "--mri", OUTSIDER, "--mri", LAB_B]).unwrap();
    match parsed.command {
        CliCommand::Presence { wait, mris } => {
            assert_eq!(wait, 30);
            assert_eq!(mris, [OUTSIDER, LAB_B]);
        }
        other => panic!("{other:?}"),
    }
    let parsed = Cli::try_parse_from(["teams-headless", "a", "presence"]).unwrap();
    assert!(matches!(parsed.command, CliCommand::Presence { ref mris, .. } if mris.is_empty()));
    let parsed = Cli::try_parse_from(["teams-headless", "b", "profile", OUTSIDER, LAB_C, "--wait", "5"]).unwrap();
    match parsed.command {
        CliCommand::Profile { mris, wait } => {
            assert_eq!(mris, [OUTSIDER, LAB_C]);
            assert_eq!(wait, 5);
        }
        other => panic!("{other:?}"),
    }
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
        let dir = tempfile::tempdir().unwrap();
        // the lab's `chats.toml` records the mock's a–b–c chat, as `abc-chat` records the lab's
        let config = LabConfig::parse(&lab_toml(dir.path()), dir.path()).unwrap();
        record_abc_chat(config.paths(), ABC_CHAT).unwrap();
        Self { mock, trouter, dir }
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
        self.build(me, push, throttle_first_write, false)
    }

    /// `me`'s engine with its push channel, as the lab binary builds it for its own availability: its fence's own
    /// account `me` (`Fence::open_lab_account`'s), its transport given the mock's presence base.
    fn engine_owning(&self, me: &str) -> EngineHandle {
        self.build(me, true, false, true)
    }

    /// As [`Self::engine_owning`], its first write answered 429 with a short `Retry-After`.
    fn engine_owning_throttled(&self, me: &str) -> EngineHandle {
        self.build(me, true, true, true)
    }

    fn build(&self, me: &str, push: bool, throttle_first_write: bool, own: bool) -> EngineHandle {
        let tokens = Arc::new(Tokens {
            token: self.mock.token(me).unwrap(),
            renewals: broadcast::channel(4).0,
        });
        let fence = Fence::new(
            Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
            FenceLog::from_writer(io::sink()),
        );
        let fence = Arc::new(if own {
            fence.with_own_account(Mri::new(me))
        } else {
            fence
        });
        let origin = self.mock.origin();
        let http = || {
            let transport =
                HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &origin).unwrap();
            if own {
                transport.with_presence(PRESENCE_BASE).unwrap()
            } else {
                transport
            }
        };
        let transport = Arc::new(http());
        let membership = LiveMembershipSource::with_mock_origin(&origin).unwrap();
        membership.set_credential_source(transport.clone());
        let push = push.then(|| {
            let trouter = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), self.trouter.origin())
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
        calls: None,
        store,
        client,
        outbox,
        push,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            catch_up_retry: Duration::from_millis(500),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
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

    fn send<R: ProtoRequest>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        self.inner.send(request)
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

/// A message event the push channel delivered: a read's (a catch-up's) event for the same message doesn't count.
fn delivered(line: &Value) -> bool {
    line["event"] == "message" && line["source"] == "delivery"
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
        .json_line(|line| delivered(line) && line["id"] == id.as_str())
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
        .json_line(|line| delivered(line) && line["id"] == id.as_str() && line["change"] == "edit")
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
        .json_line(|line| delivered(line) && line["id"] == id.as_str() && line["change"] == "reaction")
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

#[tokio::test]
async fn a_s_send_to_the_lab_s_group_chat_reaches_b_s_and_c_s_watch() {
    let lab = Lab::start().await;
    let (watch_b, stop_b, task_b) = watching(lab.engine(LAB_B, true), true);
    let (watch_c, stop_c, task_c) = watching(lab.engine(LAB_C, true), true);
    ready(&watch_b).await;
    ready(&watch_c).await;
    let mut a = lab.engine(LAB_A, false);

    let to_group = Action::Send {
        conversation: ABC_CHAT.to_owned(),
        text: MessageText::plain("hello b and c, from teams-headless").unwrap(),
    };
    let (state, sent) = write(&mut a, to_group).await;
    assert_eq!(state, OutboxState::Sent, "{sent:?}");
    let id = field(&sent, "message_id").to_owned();
    for watch in [&watch_b, &watch_c] {
        let received = watch
            .json_line(|line| delivered(line) && line["id"] == id.as_str())
            .await;
        assert_eq!(received["conversation"], ABC_CHAT);
        assert_eq!(received["sender"], "a");
        assert_eq!(received["text"], "hello b and c, from teams-headless");
    }

    for (stop, task) in [(stop_b, task_b), (stop_c, task_c)] {
        let _ = stop.send(());
        let (handle, end) = task.await.unwrap();
        assert_eq!(end, WatchEnd::Stopped);
        handle.shutdown();
    }
    a.shutdown();
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

#[tokio::test]
async fn presence_prints_each_account_as_the_engine_has_it_unknown_without_a_read() {
    let lab = Lab::start().await;
    let mut a = lab.engine(LAB_A, false);
    let mut out = Vec::new();

    // the mock's reads have no presence base: nothing is read, and each stays unknown once the wait is over
    let whom = [LAB_B.to_owned(), LAB_C.to_owned()];
    commands::presence(&mut a, &whom, Duration::from_millis(300), &names(), &mut out)
        .await
        .unwrap();

    assert_eq!(
        String::from_utf8(out).unwrap(),
        "presence b Unknown\npresence c Unknown\n"
    );
    a.shutdown();
}

#[tokio::test]
async fn presence_and_profile_print_anyone_asked_about_by_mri_never_a_name() {
    let lab = Lab::start().await;
    let mut a = lab.engine(LAB_A, false);

    // an outsider is printed by MRI, a lab account by alias; the mock's reads have no presence base, so each stays
    // unknown once the wait is over
    let whom = [LAB_B.to_owned(), OUTSIDER.to_owned()];
    let mut out = Vec::new();
    commands::presence(&mut a, &whom, Duration::from_millis(300), &names(), &mut out)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("presence b Unknown\npresence {OUTSIDER} Unknown\n")
    );

    // nothing gives the mock's engine a name for either (no Graph or middle-tier base, no message, no known names):
    // each is printed with where its name came from, `none`
    let mut out = Vec::new();
    commands::profile(&mut a, &whom, Duration::from_millis(300), &names(), &mut out)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("profile b none\nprofile {OUTSIDER} none\n")
    );

    // asked again: the store has both fresh, no lookup starts, so nothing is waited for however long `--wait` is
    let started = tokio::time::Instant::now();
    let mut out = Vec::new();
    commands::profile(&mut a, &whom, WAIT, &names(), &mut out)
        .await
        .unwrap();
    assert!(started.elapsed() < WAIT / 2, "{:?}", started.elapsed());
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("profile b none\nprofile {OUTSIDER} none\n")
    );
    a.shutdown();
}

#[tokio::test]
async fn presence_refuses_more_people_than_the_engine_watches_before_asking_anyone() {
    let lab = Lab::start().await;
    let mut a = lab.engine(LAB_A, false);
    let whom: Vec<String> = (0..=MAX_WATCHED)
        .map(|n| format!("8:orgid:00000000-0000-4000-8000-{n:012}"))
        .collect();
    let mut out = Vec::new();
    let refused = commands::presence(&mut a, &whom, WAIT, &names(), &mut out).await;
    assert!(
        matches!(refused, Err(CommandError::TooMany { asked, most }) if asked == MAX_WATCHED + 1 && most == MAX_WATCHED),
        "{refused:?}"
    );
    assert!(out.is_empty());
    a.shutdown();
}

#[test]
fn a_name_s_source_is_printed_as_one_word() {
    assert_eq!(name_source_word(Some(NameFrom::Graph)), "graph");
    assert_eq!(name_source_word(Some(NameFrom::ShortProfile)), "short_profile");
    assert_eq!(name_source_word(Some(NameFrom::Message)), "message");
    assert_eq!(name_source_word(Some(NameFrom::Known)), "known");
    assert_eq!(name_source_word(None), "none");
}

#[tokio::test]
async fn availability_sets_the_endpoint_then_the_manual_status_and_its_reset_printing_each_step() {
    let lab = Lab::start().await;
    let mut a = lab.engine_owning(LAB_A);
    let mut out = Vec::new();
    let plan = availability::Plan {
        manual: Some((ManualAvailability::Busy, Duration::from_millis(100))),
        reset_after: Some(Duration::from_millis(200)),
        hold: Duration::from_millis(300),
        register_wait: WAIT,
    };

    let end = availability::run(&mut a, plan, &mut out, future::pending())
        .await
        .unwrap();

    assert_eq!(end, availability::End::Done { all_sent: true });
    let printed = String::from_utf8(out).unwrap();
    let steps: Vec<String> = printed
        .lines()
        .map(|line| line.rsplit_once(" at_ms=").unwrap().0.to_owned())
        .collect();
    assert_eq!(
        steps,
        [
            "availability registered",
            "availability available sent",
            "availability manual Busy sent",
            "availability reset sent",
        ],
        "{printed}"
    );
    let changes: Vec<AvailabilityChange> = lab
        .mock
        .own_availability()
        .into_iter()
        .map(|taken| {
            assert_eq!(taken.account, LAB_A);
            taken.change
        })
        .collect();
    assert_eq!(
        changes,
        [
            AvailabilityChange::Endpoint {
                availability: "Available".to_owned(),
                activity: "Available".to_owned(),
            },
            AvailabilityChange::Manual("Busy".to_owned()),
            AvailabilityChange::Reset,
        ]
    );
    a.shutdown();
}

#[tokio::test]
async fn availability_without_its_own_account_prints_the_refusal_and_sets_nothing() {
    let lab = Lab::start().await;
    // a fence of no account: what a binary built on `Fence::open_lab` would have; its push channel's endpoint is
    // registered for no account, so no account's availability can name it
    let mut a = lab.engine(LAB_A, true);
    let mut out = Vec::new();
    let plan = availability::Plan {
        manual: None,
        reset_after: None,
        hold: Duration::ZERO,
        register_wait: WAIT,
    };

    let end = availability::run(&mut a, plan, &mut out, future::pending())
        .await
        .unwrap();

    assert_eq!(end, availability::End::Done { all_sent: false });
    let printed = String::from_utf8(out).unwrap();
    assert!(
        printed.contains("availability available dropped: ") && printed.contains("wasn't registered for this account"),
        "{printed}"
    );
    assert!(lab.mock.own_availability().is_empty());
    a.shutdown();
}

#[tokio::test]
async fn availability_counts_a_throttled_step_sent_once_it_went_out_after_its_wait() {
    let lab = Lab::start().await;
    // the endpoint's `PUT` is answered 429 with a `Retry-After` of 300 ms, and goes out from the engine's tick after it
    let mut a = lab.engine_owning_throttled(LAB_A);
    let mut out = Vec::new();
    let plan = availability::Plan {
        manual: None,
        reset_after: None,
        hold: Duration::from_secs(3),
        register_wait: WAIT,
    };

    let end = availability::run(&mut a, plan, &mut out, future::pending())
        .await
        .unwrap();

    let printed = String::from_utf8(out).unwrap();
    assert_eq!(end, availability::End::Done { all_sent: true }, "{printed}");
    assert!(
        printed.contains("availability available waiting not_before_ms="),
        "{printed}"
    );
    assert!(
        printed.contains("availability endpoint_available sent after_its_wait"),
        "{printed}"
    );
    assert_eq!(lab.mock.own_availability().len(), 1, "UPS took the `PUT` once");
    a.shutdown();
}

#[tokio::test]
async fn availability_counts_a_step_still_waiting_when_the_hold_ends_as_not_sent() {
    let lab = Lab::start().await;
    let mut a = lab.engine_owning_throttled(LAB_A);
    let mut out = Vec::new();
    // the hold ends before the `PUT`'s 300 ms `Retry-After` does
    let plan = availability::Plan {
        manual: None,
        reset_after: None,
        hold: Duration::ZERO,
        register_wait: WAIT,
    };

    let end = availability::run(&mut a, plan, &mut out, future::pending())
        .await
        .unwrap();

    let printed = String::from_utf8(out).unwrap();
    assert_eq!(end, availability::End::Done { all_sent: false }, "{printed}");
    assert!(
        printed.contains("availability available waiting not_before_ms="),
        "{printed}"
    );
    assert!(!printed.contains("after_its_wait"), "{printed}");
    a.shutdown();
}

#[tokio::test]
async fn availability_ends_on_its_stop_and_without_a_push_channel_sets_nothing() {
    let lab = Lab::start().await;
    let mut a = lab.engine(LAB_A, false);
    let mut out = Vec::new();
    let plan = availability::Plan {
        manual: None,
        reset_after: None,
        hold: Duration::from_secs(60),
        register_wait: Duration::from_millis(200),
    };
    // no push channel, so no registration
    let end = availability::run(&mut a, plan, &mut out, future::pending())
        .await
        .unwrap();
    assert_eq!(end, availability::End::NotRegistered);
    let end = availability::run(&mut a, plan, &mut out, future::ready(()))
        .await
        .unwrap();
    assert_eq!(end, availability::End::Stopped);
    assert!(out.is_empty());
    assert!(lab.mock.own_availability().is_empty());
    a.shutdown();
}
