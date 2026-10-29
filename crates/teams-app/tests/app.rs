//! The app (`teams-app --lab <alias> --secrets-fd <n> ...`) up to its window: its command line (one account, or
//! several paired with their brokers' sockets in order), what it refuses before it
//! touches the lab (a `test-support` build, a window outside the UI sandbox or with `DISPLAY` set, a trace
//! descriptor it can't write), its engine built as `teams-headless` builds one (`teams_core::lab::start`) with its
//! tokens only through the broker's socket, another engine on the account reported, and the signed-out state: a
//! session that can't renew its tokens shows the account signed out and stops the engine, so nothing more is sent; its
//! calls (M3 Task 17) relayed through Teams' relay and processed as a call's, on the UI sandbox's PipeWire only, the
//! test call's echo lab.toml's; several accounts launched in one window on GPUI's test context, each on the rail,
//! the quit shutting every engine down.
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
use teams_app::app::{self, AppEngine, Launch, LaunchAccount, sign_in_hint, start_failure, watch_sign_out};
use teams_app::calling;
use teams_app::cli::{Cli, Mode, ModeError};
use teams_app::guard::window_allowed;
use teams_app::trace::{TraceFdError, adopt_trace};
use teams_auth::broker::{BrokerScope, BrokerSecretStore, Op, serve};
use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::SignedOut;
use teams_core::account::AccountError;
use teams_core::lab::{self, LabEngine, LabError};
use teams_core::{EngineError, Reply};
use teams_fence::lab::{Alias, LabConfig, LabPaths};
use teams_fence::{CallFloor, Fence};
use teams_mock::{CHAT_BASE, lab_toml};
use teams_ui::bus::BusError;
use teams_ui::tray::TrayEvent;
use teams_ui::{Account, Bridge, Core, Engine, Models, SettingsFile, ViewTrace};

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
            accounts: vec![(Alias::B, 5)],
            trace_fd: Some(6)
        })
    );
    let untraced = Cli::try_parse_from(["teams-app", "--lab", "a", "--secrets-fd", "3"])?;
    assert_eq!(
        untraced.mode(),
        Ok(Mode::App {
            accounts: vec![(Alias::A, 3)],
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

/// The command line `line`, split at its spaces.
fn parsed(line: &str) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(line.split_whitespace())
}

#[test]
fn several_accounts_pair_each_lab_with_its_broker_s_socket_in_order() -> TestResult {
    let app = parsed("teams-app --lab a --secrets-fd 3 --lab c --secrets-fd 5 --lab b --secrets-fd 4 --trace-fd 6")?;
    assert_eq!(
        app.mode(),
        Ok(Mode::App {
            accounts: vec![(Alias::A, 3), (Alias::C, 5), (Alias::B, 4)],
            trace_fd: Some(6)
        })
    );
    // the flags' order between the pairs doesn't matter: the n-th --secrets-fd is the n-th --lab's
    let grouped = parsed("teams-app --lab b --lab a --secrets-fd 7 --secrets-fd 8")?;
    assert_eq!(
        grouped.mode(),
        Ok(Mode::App {
            accounts: vec![(Alias::B, 7), (Alias::A, 8)],
            trace_fd: None
        })
    );

    let refusals = [
        (
            "teams-app --lab a --lab b --secrets-fd 3",
            ModeError::Unpaired { labs: 2, sockets: 1 },
        ),
        (
            "teams-app --lab a --secrets-fd 3 --secrets-fd 4",
            ModeError::Unpaired { labs: 1, sockets: 2 },
        ),
        (
            "teams-app --lab a --secrets-fd 3 --lab a --secrets-fd 4",
            ModeError::RepeatedAccount(Alias::A),
        ),
        (
            "teams-app --lab a --secrets-fd 3 --lab b --secrets-fd 3",
            ModeError::SharedDescriptor(3),
        ),
        (
            "teams-app --lab a --secrets-fd 3 --lab b --secrets-fd 4 --trace-fd 4",
            ModeError::SharedDescriptor(4),
        ),
    ];
    for (refused, error) in refusals {
        assert_eq!(parsed(refused)?.mode(), Err(error), "{refused}");
    }
    Ok(())
}

#[test]
fn several_accounts_are_titled_and_named_by_their_aliases() {
    assert_eq!(app::title(Alias::A).as_ref(), "ngteams (lab a)");
    assert_eq!(app::titled(&[Alias::A]).as_ref(), "ngteams (lab a)");
    assert_eq!(app::titled(&[Alias::A, Alias::B]).as_ref(), "ngteams (lab a, b)");
    assert_eq!(
        app::titled(&[Alias::C, Alias::A, Alias::B]).as_ref(),
        "ngteams (lab c, a, b)"
    );
    assert_eq!(app::account_name(Alias::B).as_ref(), "Lab B");
    assert_eq!(app::account_detail(Alias::B).as_ref(), "lab b");
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
    assert!(stderr.contains("descriptor 3 is named twice"), "{stderr}");
    assert!(!stderr.contains("test-support"), "refused first: {stderr}");
    assert_eq!(std::fs::read_dir(home.path())?.count(), 0, "nothing was created");
    Ok(())
}

#[test]
fn an_account_given_twice_is_refused_before_any_descriptor_is_taken() -> TestResult {
    let home = tempfile::tempdir()?;
    let output = Command::new(BINARY)
        .args(["--lab", "a", "--secrets-fd", "3", "--lab", "a", "--secrets-fd", "4"])
        .env_clear()
        .env("HOME", home.path())
        .output()?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("--lab a is given twice"), "{stderr}");
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
fn a_build_with_the_personal_fence_refuses_to_run() {
    // `cargo test -p teams-app` builds without it; a workspace build with `ngteams` unifies it in
    let refusal = teams_app::guard::personal_refusal();
    assert_eq!(refusal.is_some(), teams_fence::personal_enabled());
    if let Some(refusal) = refusal {
        assert!(
            refusal.contains("personal") && refusal.contains("cargo build -p teams-app"),
            "{refusal}"
        );
    }
    let main = include_str!("../src/main.rs");
    let (test_support, personal) = (
        main.find("guard::test_support_refusal()").unwrap(),
        main.find("guard::personal_refusal()").unwrap(),
    );
    let prepared = main.find("prepare(accounts").unwrap();
    assert!(
        test_support < personal && personal < prepared,
        "both refusals before the app touches anything"
    );
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

// --- the calls (M3 Task 17) ---

#[test]
fn the_app_s_calls_go_through_teams_relay_processed_as_a_call_its_client_the_web_client_s() {
    let devices = Arc::new(teams_media::devices::file::FileDevices {
        input: teams_media::devices::file::AudioInput::Silence,
        recording: None,
        camera: teams_media::devices::file::PatternCamera::default(),
        pacing: teams_media::devices::file::Pacing::RealTime,
    });
    let echo = teams_fence::Mri::new("28:00000000-0000-4000-8000-0000000000ec");
    let (engine, window) = calling::on_devices(devices, Some(echo.clone()), false);
    assert_eq!(engine.media.relay, teams_core::Relay::Teams);
    assert_eq!(
        engine.media.session.processing,
        teams_media::processing::ProcessingConfig::CALL,
        "echo cancellation, noise suppression and gain control on"
    );
    assert_eq!(engine.client, lab::web_client_info());
    assert_eq!(engine.language_id.as_deref(), Some(lab::LAB_LANGUAGE));
    assert!(window.ringer.is_some(), "the ringtone on the same devices");
    assert_eq!(window.echo, Some(echo));
    assert!(
        engine.media.video.is_none() && window.video.is_none(),
        "no video asked, none"
    );
}

#[test]
fn with_video_the_app_s_calls_show_their_video_on_the_window_s_feed_off_until_the_camera_is_turned_on() {
    let devices = Arc::new(teams_media::devices::file::FileDevices {
        input: teams_media::devices::file::AudioInput::Silence,
        recording: None,
        camera: teams_media::devices::file::PatternCamera::default(),
        pacing: teams_media::devices::file::Pacing::RealTime,
    });
    let (engine, window) = calling::on_devices(devices, None, true);
    let video = engine.media.video.expect("the calls' video");
    assert_eq!(video.max_size, teams_media::video::convert::MaxSize::HD);
    let feed = window.video.expect("the window's feed");
    assert!(
        teams_core::VideoOutput::enabled(&*video.output),
        "the codec on by default"
    );
    feed.set_enabled(false);
    assert!(
        !teams_core::VideoOutput::enabled(&*video.output),
        "the settings' switch is the engine's"
    );
}

#[test]
fn the_test_call_s_echo_is_lab_toml_s() -> TestResult {
    let dir = tempfile::tempdir()?;
    let without = LabConfig::parse(&lab_toml(dir.path()), dir.path())?;
    assert_eq!(calling::echo_of(&without), None, "not recorded yet");
    let recorded = format!(
        "{}\n[[service_participants]]\nname = \"echo\"\nmri = \"28:00000000-0000-4000-8000-0000000000ec\"\n",
        lab_toml(dir.path())
    );
    let with = LabConfig::parse(&recorded, dir.path())?;
    assert_eq!(
        calling::echo_of(&with).map(|mri| mri.as_str().to_owned()).as_deref(),
        Some("28:00000000-0000-4000-8000-0000000000ec")
    );
    Ok(())
}

#[test]
fn outside_the_sandbox_s_pipewire_the_app_has_no_calls() -> TestResult {
    // a test process has no UI sandbox: its PipeWire is the only one a lab build may open
    if std::env::var_os(ui_sandbox::SANDBOX_ENV).is_some() {
        return Ok(());
    }
    let dir = tempfile::tempdir()?;
    let config = LabConfig::parse(&lab_toml(dir.path()), dir.path())?;
    let refused = calling::open(&config).err().ok_or("calls opened outside the sandbox")?;
    assert!(
        matches!(refused, teams_media::devices::DeviceError::Refused(_)),
        "{refused}"
    );
    Ok(())
}

// --- the engine, as the app builds it ---

/// A made-up lab in a temp dir: the mock's `lab.toml`, every account's region record with the mock's chat base.
struct TempLab {
    dir: tempfile::TempDir,
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
        // every account's: the launch of several starts more than a's engine
        for alias in Alias::ALL {
            let file = config.paths().region_file(alias);
            std::fs::create_dir_all(file.parent().ok_or("a parent dir")?)?;
            std::fs::write(&file, region.to_string())?;
        }
        Ok(Self { dir, config })
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
    lab::start(&lab.config, Alias::A, fence, secrets, false, None)
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
        matches!(
            &refused,
            LabError::Account(AccountError::Engine(EngineError::AlreadyRunning { .. }))
        ),
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

// --- several accounts in one window ---

/// `alias`'s engine on the made-up lab, its fence on `floor`, its tokens from an empty store: nothing leaves the
/// process.
fn engine_on(lab: &TempLab, alias: Alias, floor: &CallFloor) -> TestResult<teams_core::EngineHandle> {
    let fence = Fence::open(&lab.config)?.sharing_calls(floor);
    let started = lab::start(
        &lab.config,
        alias,
        Arc::new(fence),
        MemorySecretStore::new(),
        false,
        None,
    )?;
    Ok(started.handle)
}

/// The MRI lab.toml gives `alias`.
fn mri_of(lab: &TempLab, alias: Alias) -> TestResult<String> {
    Ok(lab.config.account(alias).mri.clone().ok_or("an MRI in lab.toml")?)
}

#[test]
fn several_accounts_run_in_one_window_and_the_quit_shuts_every_engine_down() -> TestResult {
    let lab = TempLab::new()?;
    let floor = CallFloor::new();
    let (a, b) = (engine_on(&lab, Alias::A, &floor)?, engine_on(&lab, Alias::B, &floor)?);
    let cx = TestAppContext::single();
    let settings = SettingsFile::new(lab.dir.path().join("ui").join("a.toml"));
    let launched = cx.update(|cx| {
        app::launch(
            cx,
            Launch {
                first: LaunchAccount::bare(Alias::A, a),
                others: vec![LaunchAccount::bare(Alias::B, b)],
                trace: ViewTrace::off(),
                settings,
                // no bus: nothing is put on the desktop's
                bus: Err(BusError::NotGiven),
                diagnostics: None,
                playback: teams_media::player::PlayerSetup::default(),
            },
        )
    });
    let launched = launched.map_err(|failed| failed.error)?;
    cx.run_until_parked();

    let (mri_a, mri_b) = (mri_of(&lab, Alias::A)?, mri_of(&lab, Alias::B)?);
    let tiles = cx.read(|cx| {
        let accounts = launched.desktop.read(cx).accounts().read(cx);
        let tiles: Vec<(String, String, String)> = accounts
            .tiles()
            .iter()
            .map(|tile| (tile.key.clone(), tile.name.to_string(), tile.detail.to_string()))
            .collect();
        (tiles, accounts.active_key().map(str::to_owned), accounts.can_add())
    });
    assert_eq!(
        tiles.0,
        [
            (mri_a.clone(), "Lab A".to_owned(), "lab a".to_owned()),
            (mri_b, "Lab B".to_owned(), "lab b".to_owned()),
        ],
        "each account on the rail, the first one first"
    );
    assert_eq!(tiles.1.as_deref(), Some(mri_a.as_str()), "the first account shown");
    assert!(!tiles.2, "lab accounts are added only through `proto-lab auth`");
    let first = cx.read(|cx| {
        launched
            .desktop
            .read(cx)
            .settings()
            .read(cx)
            .settings()
            .first_account
            .clone()
    });
    assert_eq!(first.as_deref(), Some(mri_a.as_str()), "the settings' top level is a's");
    assert_eq!(launched.bridges.len(), 2);
    assert!(cx.read(|cx| launched.bridges.iter().all(|bridge| bridge.read(cx).running())));

    cx.update(|cx| {
        launched
            .desktop
            .update(cx, |desktop, cx| desktop.tray_asked(TrayEvent::Quit, cx));
    });
    settle(&cx, |cx| {
        cx.read(|cx| launched.bridges.iter().all(|bridge| !bridge.read(cx).running()))
    });
    // every engine's thread was joined: their locks are free for the next ones
    let (again_a, again_b) = (engine_on(&lab, Alias::A, &floor)?, engine_on(&lab, Alias::B, &floor)?);
    again_a.shutdown();
    again_b.shutdown();
    Ok(())
}
