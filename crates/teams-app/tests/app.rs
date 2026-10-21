//! The app (`teams-app --lab <alias> --secrets-fd <n>`) up to its window: its command line, what it refuses before it
//! touches the lab (a `test-support` build, a window outside the UI sandbox or with `DISPLAY` set, a trace
//! descriptor it can't write), its engine built as `teams-headless` builds one (`teams_core::lab::start`) with its
//! tokens only through the broker's socket, another engine on the account reported, and the signed-out state: a
//! session that can't renew its tokens shows the account signed out and stops the engine, so nothing more is sent.
//!
//! The lab here is a made-up one in a temp dir (the mock's `lab.toml`, a region record with the mock's chat base), and
//! the broker serves a `MemorySecretStore` holding no refresh token: every token request ends at the session, which
//! sends nothing without one, so no request leaves the process.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::os::fd::{AsRawFd as _, IntoRawFd as _};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use clap::Parser as _;
use gpui::{AppContext as _, TestAppContext};
use teams_app::app::{AppEngine, sign_in_hint, start_failure, watch_sign_out};
use teams_app::cli::{Cli, Mode, ModeError};
use teams_app::guard::window_allowed;
use teams_app::trace::{TraceFdError, adopt_trace};
use teams_auth::broker::{BrokerScope, BrokerSecretStore, Op, serve};
use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::SignedOut;
use teams_core::lab::{self, LabEngine, LabError};
use teams_core::{EngineError, Reply};
use teams_fence::Fence;
use teams_fence::lab::{Alias, LabConfig, LabPaths};
use teams_mock::{CHAT_BASE, lab_toml};
use teams_ui::{Account, Bridge, Core, Engine, Models};

const BINARY: &str = env!("CARGO_BIN_EXE_teams-app");
const UPN_A: &str = "bot-a@lab.example.test";
const WAIT: Duration = Duration::from_secs(30);

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

// --- the command line ---

#[test]
fn the_command_line_takes_the_app_or_the_broker() -> TestResult {
    let app = Cli::try_parse_from(["teams-app", "--lab", "b", "--secrets-fd", "5", "--trace-fd", "6"])?;
    assert_eq!(
        app.mode(),
        Ok(Mode::App {
            alias: Alias::B,
            secrets_fd: 5,
            trace_fd: Some(6)
        })
    );
    let untraced = Cli::try_parse_from(["teams-app", "--lab", "a", "--secrets-fd", "3"])?;
    assert_eq!(
        untraced.mode(),
        Ok(Mode::App {
            alias: Alias::A,
            secrets_fd: 3,
            trace_fd: None
        })
    );
    let broker = Cli::try_parse_from(["teams-app", "secrets", "--lab", "c", "--fd", "4"])?;
    assert_eq!(broker.mode(), Ok(Mode::Secrets { alias: Alias::C, fd: 4 }));
    let shared = Cli::try_parse_from(["teams-app", "--lab", "a", "--secrets-fd", "3", "--trace-fd", "3"])?;
    assert_eq!(shared.mode(), Err(ModeError::SharedDescriptor(3)));

    for refused in [
        &["teams-app"][..],
        &["teams-app", "--lab", "a"],
        &["teams-app", "--secrets-fd", "3"],
        &["teams-app", "--lab", "d", "--secrets-fd", "3"],
        &["teams-app", "--lab", "a", "--secrets-fd", "three"],
        &[
            "teams-app",
            "--lab",
            "a",
            "--secrets-fd",
            "3",
            "secrets",
            "--lab",
            "a",
            "--fd",
            "4",
        ],
    ] {
        assert!(Cli::try_parse_from(refused).is_err(), "{refused:?}");
    }
    Ok(())
}

#[test]
fn without_the_broker_s_socket_the_app_does_not_start() -> TestResult {
    let output = Command::new(BINARY).args(["--lab", "a"]).output()?;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8(output.stderr)?.contains("--secrets-fd"));
    Ok(())
}

#[test]
fn one_descriptor_named_for_both_the_broker_and_the_trace_is_refused_before_either_is_taken() -> TestResult {
    let home = tempfile::tempdir()?;
    let output = Command::new(BINARY)
        .args(["--lab", "a", "--secrets-fd", "3", "--trace-fd", "3"])
        .env_clear()
        .env("HOME", home.path())
        .output()?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("both name descriptor 3"), "{stderr}");
    assert!(!stderr.contains("test-support"), "refused first: {stderr}");
    assert_eq!(std::fs::read_dir(home.path())?.count(), 0, "nothing was created");
    Ok(())
}

#[test]
fn a_build_with_test_support_refuses_to_run_before_it_touches_the_lab() -> TestResult {
    assert!(
        teams_fence::test_support_enabled(),
        "this test build unifies teams-fence's test-support into the binary"
    );
    let home = tempfile::tempdir()?;
    let (ours, theirs) = UnixStream::pair()?;
    let fd = theirs.as_raw_fd().to_string();
    let output = Command::new(BINARY)
        .args(["--lab", "a", "--secrets-fd", &fd])
        .env_clear()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .output()?;
    drop((ours, theirs));
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("test-support"), "{stderr}");
    assert!(stderr.contains("cargo build -p teams-app"), "{stderr}");
    assert_eq!(std::fs::read_dir(home.path())?.count(), 0, "nothing was created");
    Ok(())
}

#[test]
fn a_window_opens_only_on_the_sandbox_s_compositor_and_never_with_display_set() {
    let sandbox = Some("/tmp/.tmpSandbox".as_ref());
    let inside = Some("/tmp/.tmpSandbox/niri-run/wayland-1".as_ref());
    assert_eq!(window_allowed(inside, sandbox, None), Ok(()));
    assert_eq!(window_allowed(inside, sandbox, Some("".as_ref())), Ok(()));

    let refusals = [
        (inside, sandbox, Some(":0".as_ref()), "DISPLAY"),
        (None, sandbox, None, "WAYLAND_DISPLAY"),
        (Some("wayland-1".as_ref()), sandbox, None, "UI_SANDBOX_DIR"),
        (
            Some("/run/user/1000/wayland-1".as_ref()),
            sandbox,
            None,
            "UI_SANDBOX_DIR",
        ),
        (inside, None, None, "UI_SANDBOX_DIR"),
    ];
    for (wayland, sandbox, display, named) in refusals {
        let refusal = window_allowed(wayland, sandbox, display).expect_err("refused");
        assert!(refusal.contains(named), "{refusal}");
    }
}

#[test]
fn the_trace_descriptor_must_be_handed_in_open_for_writing() -> TestResult {
    assert!(matches!(adopt_trace(2), Err(TraceFdError::Reserved(2))));
    let dir = tempfile::tempdir()?;
    let read_only = std::fs::File::create(dir.path().join("trace"))
        .and_then(|_| std::fs::File::open(dir.path().join("trace")))?
        .into_raw_fd();
    assert!(matches!(adopt_trace(read_only), Err(TraceFdError::NotWritable { .. })));
    // the refused descriptor was closed
    assert!(matches!(adopt_trace(read_only), Err(TraceFdError::NotOpen { .. })));

    let (reader, writer) = std::io::pipe()?;
    let mut adopted = adopt_trace(writer.into_raw_fd())?;
    std::io::Write::write_all(&mut adopted, b"{}\n")?;
    drop(adopted);
    let mut read = String::new();
    std::io::Read::read_to_string(&mut &reader, &mut read)?;
    assert_eq!(read, "{}\n");
    Ok(())
}

// --- the engine, as the app builds it ---

/// A made-up lab in a temp dir: the mock's `lab.toml`, a's region record with the mock's chat base.
struct TempLab {
    _dir: tempfile::TempDir,
    config: LabConfig,
}

impl TempLab {
    fn new() -> TestResult<Self> {
        let dir = tempfile::tempdir()?;
        let config = LabConfig::parse(&lab_toml(dir.path()), dir.path())?;
        let region = serde_json::json!({
            "region": "amer",
            "partition": "amer03",
            "regionGtms": {"chatServiceAfd": CHAT_BASE},
        });
        let file = config.paths().region_file(Alias::A);
        std::fs::create_dir_all(file.parent().ok_or("a parent dir")?)?;
        std::fs::write(&file, region.to_string())?;
        Ok(Self { _dir: dir, config })
    }

    fn paths(&self) -> &LabPaths {
        self.config.paths()
    }
}

/// What the broker was asked: the operation, the account and the audience of each request.
type Asked = Arc<Mutex<Vec<(Op, String, String)>>>;

/// The broker serving an empty `MemorySecretStore` for a's UPN on `runtime`, and the app's end of its socket.
fn broker(runtime: &tokio::runtime::Runtime) -> TestResult<(BrokerSecretStore, Asked)> {
    let (server, client) = UnixStream::pair()?;
    server.set_nonblocking(true)?;
    let asked: Asked = Arc::default();
    let noted = Arc::clone(&asked);
    let _guard = runtime.enter();
    let server = tokio::net::UnixStream::from_std(server)?;
    runtime.spawn(async move {
        let store = MemorySecretStore::new();
        let scope = BrokerScope::new(LabPaths::KEYRING_APPLICATION, UPN_A);
        let _ = serve(server, &store, &scope, |note| {
            noted.lock().unwrap_or_else(PoisonError::into_inner).push((
                note.op,
                note.key.account().to_owned(),
                note.key.audience().to_owned(),
            ));
        })
        .await;
    });
    Ok((BrokerSecretStore::new(client, LabPaths::KEYRING_APPLICATION)?, asked))
}

fn start(lab: &TempLab, secrets: BrokerSecretStore) -> Result<LabEngine<BrokerSecretStore>, LabError> {
    let fence = Arc::new(Fence::open(&lab.config).expect("the lab's fence"));
    lab::start(&lab.config, Alias::A, fence, secrets, false)
}

#[test]
fn the_engine_is_built_as_the_headless_binary_builds_it_its_tokens_only_through_the_broker() -> TestResult {
    let lab = TempLab::new()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let (secrets, asked) = broker(&runtime)?;

    let LabEngine { handle, session } = start(&lab, secrets)?;

    assert!(
        lab.paths().store_file(Alias::A).exists(),
        "the store at LabPaths::store_file"
    );
    assert_eq!(session.account(), UPN_A);
    assert_eq!(session.service_base("chatServiceAfd").as_deref(), Some(CHAT_BASE));
    assert!(session.signed_out().is_none());

    // a read needs a token: the session asks the broker for the refresh token, finds none, and sends nothing
    let error = handle.sync().blocking_wait().expect_err("no token");
    assert!(error.to_string().contains("sign in"), "{error}");
    assert_eq!(session.signed_out(), Some(SignedOut::NotSignedIn));
    let asked = asked.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(!asked.is_empty(), "the session asked the broker");
    assert!(
        asked.iter().all(|(op, account, _)| *op == Op::Get && account == UPN_A),
        "{asked:?}"
    );
    assert!(
        format!("{session:?}").contains(UPN_A) && !format!("{session:?}").contains("token-"),
        "{session:?}"
    );
    handle.shutdown();
    Ok(())
}

#[test]
fn another_engine_on_the_account_is_reported_and_this_one_exits() -> TestResult {
    let lab = TempLab::new()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let (first, _) = broker(&runtime)?;
    let (second, _) = broker(&runtime)?;
    let running = start(&lab, first)?;

    let refused = start(&lab, second).expect_err("one engine per account");

    assert!(
        matches!(&refused, LabError::Engine(EngineError::AlreadyRunning { .. })),
        "{refused:?}"
    );
    let message = start_failure(Alias::A, &refused);
    assert!(message.contains("another engine runs on lab account a"), "{message}");
    assert!(message.contains("engine.lock"), "{message}");
    running.handle.shutdown();
    Ok(())
}

#[test]
fn a_shutdown_held_up_by_a_broker_that_never_answers_ends_the_exchange_after_its_grace() -> TestResult {
    let lab = TempLab::new()?;
    // the broker's end, held but never answered: the Secret Service waiting on an unlock prompt nobody answers
    let (server, client) = UnixStream::pair()?;
    let closer = client.try_clone()?;
    let secrets = BrokerSecretStore::new(client, LabPaths::KEYRING_APPLICATION)?;
    let LabEngine { handle, .. } = start(&lab, secrets)?;
    let waiting = handle.sync();
    // the session's request for the refresh token reached the broker: the engine waits on its answer
    let mut header = [0; 4];
    std::io::Read::read_exact(&mut &server, &mut header)?;

    let (done, ended) = std::sync::mpsc::channel();
    let engine = AppEngine::new(handle, Some(closer), Duration::from_millis(200));
    std::thread::spawn(move || {
        teams_ui::EventSource::shutdown(engine);
        let _ = done.send(());
    });

    assert!(
        ended.recv_timeout(WAIT).is_ok(),
        "the shutdown ended though the broker never answered"
    );
    assert!(waiting.blocking_wait().is_err(), "the read got no token");
    // the engine's thread was joined: its lock is free for the next one
    let runtime = tokio::runtime::Runtime::new()?;
    let (again, _) = broker(&runtime)?;
    start(&lab, again)?.handle.shutdown();
    drop(server);
    Ok(())
}

// --- signed out ---

/// Steps GPUI's test context until `done` holds: the engine and the broker answer on threads of their own.
fn settle(cx: &TestAppContext, mut done: impl FnMut(&TestAppContext) -> bool) {
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(start.elapsed() < WAIT, "not settled in time");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_session_that_cannot_renew_shows_signed_out_and_stops_the_engine() -> TestResult {
    let lab = TempLab::new()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let (secrets, _) = broker(&runtime)?;
    let LabEngine { handle, session } = start(&lab, secrets)?;
    let cx = TestAppContext::single();
    let models = cx.update(Models::new);
    let core: Arc<dyn Core> = Arc::new(handle.requests());
    let drained = models.clone();
    let bridge = cx.update(|cx| cx.new(|cx| Bridge::start(handle, Arc::clone(&core), drained, cx)));
    cx.update(|cx| {
        watch_sign_out(
            session.watch_signed_out(),
            bridge.downgrade(),
            sign_in_hint(Alias::A),
            cx,
        );
    });

    // the first request that needs a token finds the account signed out
    let failed: Reply<_> = core.sync();
    assert!(failed.blocking_wait().is_err());
    settle(&cx, |cx| cx.read(|cx| models.status.read(cx).engine == Engine::Stopped));

    let status = cx.read(|cx| models.status.read(cx).clone());
    assert_eq!(
        status.account,
        Account::SignedOut {
            hint: "run `proto-lab auth a`".into()
        }
    );
    assert!(!cx.read(|cx| bridge.read(cx).running()));
    // nothing more is sent: the engine is gone, and every request answers that it stopped
    let after = core.sync().blocking_wait();
    assert!(matches!(after, Err(EngineError::Stopped)), "{after:?}");
    // the engine's thread was joined: its lock is free for the next one
    let (again, _) = broker(&runtime)?;
    start(&lab, again)?.handle.shutdown();
    Ok(())
}
