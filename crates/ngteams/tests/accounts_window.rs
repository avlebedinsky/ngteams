//! Several accounts in the app's one window, in the UI sandbox, on the mock (`harness = false`: GPUI runs on the
//! process's main thread).
//!
//! The test runs itself again inside the sandbox ([`Sandbox::spawn_with_fds`], only the view trace's pipe passed
//! through) as the app of lab accounts a and b, each one's engine on the mock chat service and its Trouter
//! (`teams-mock`, through the `test-support` mock origins, a test roster and log), handed to the app as `ngteams`
//! hands its own ([`ngteams::app::launch`]). The parent reads the view trace: both accounts on the rail, a shown and
//! connected; screenshots the window (the rail's avatars above Settings; kept under `target/ui-shots/`); switches to b
//! with the sandbox's virtual keyboard (Ctrl+Alt+Down) and screenshots it again; then c posts on the mock to a's chat
//! with c while b is shown: a's notification reaches the sandbox's fake server naming a, and activating it shows a
//! again with that chat open. The tray's "Quit" quits the app, both engines' threads joined.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, future, io, thread};

use gpui::{App, AsyncApp, Window, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "support/window.rs"]
mod support;
use ngteams::descriptors::adopt_trace;
use support::display_name;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_fence::lab::{Alias, LabConfig, record_abc_chat};
use teams_fence::{LiveMembershipSource, Mri};
use teams_mock::{ABC_CHAT, AC_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, TrouterMock, lab_toml};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::tray::{ITEM_PATH, MENU_PATH, MENU_QUIT};
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

/// Which mode the child runs in.
const CHILD_ENV: &str = "NGTEAMS_ACCOUNTS_TEST";
/// The child's view trace descriptor.
const TRACE_FD_ENV: &str = "NGTEAMS_ACCOUNTS_TRACE_FD";
/// From the child's start to its `ready` (the mock started, the window drawn).
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(30);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 3;
/// The file in the sandbox dir the parent writes a chat's id to, for c to post there.
const POST_TRIGGER: &str = "post-as-c";
/// What c posts.
const C_SAYS: &str = "hello a, from c";

fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("window") => child_window(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("accounts window test: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Prints `line` to stdout at once.
fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent: the sandbox, and the checks from outside ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams accounts window", &options)? else {
        return Ok(());
    };
    two_accounts_in_one_window(&sandbox).map_err(|error| format!("{error}{}", sandbox.logs()))?;
    say("ngteams accounts window: ok");
    Ok(())
}

/// The child's view trace, line by line as it comes.
struct TraceReader {
    lines: mpsc::Receiver<serde_json::Value>,
    seen: Vec<serde_json::Value>,
}

impl TraceReader {
    fn start(reader: io::PipeReader) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let value = serde_json::from_str(&line).unwrap_or(serde_json::Value::String(line));
                let _ = sender.send(value);
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    /// Waits for a line `found` takes (one seen already included), named `what` in the error.
    fn wait_where(&mut self, what: &str, found: impl Fn(&serde_json::Value) -> bool) -> SandboxResult {
        if self.seen.iter().any(&found) {
            return Ok(());
        }
        self.next_where(what, found)
    }

    /// Waits for a line `found` takes among those not seen yet.
    fn next_where(&mut self, what: &str, found: impl Fn(&serde_json::Value) -> bool) -> SandboxResult {
        loop {
            let line = self
                .lines
                .recv_timeout(WAIT)
                .map_err(|_| format!("no trace line {what}; seen {:?}", self.seen))?;
            let hit = found(&line);
            self.seen.push(line);
            if hit {
                return Ok(());
            }
        }
    }
}

/// A trace line of `view` with `change` naming account `account` among its people.
fn about(line: &serde_json::Value, view: &str, change: &str, account: &str) -> bool {
    line["view"] == view && line["change"] == change && line["people"] == serde_json::json!([account])
}

/// Where the offline runs' screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Screenshots the window as `name` under [`shots_dir`], after the frames that follow the trace's line.
fn shot(sandbox: &Sandbox, name: &str) -> SandboxResult {
    thread::sleep(Duration::from_secs(1));
    let png = sandbox.screenshot(name, true)?;
    fs::copy(&png, shots_dir()?.join(name))?;
    Ok(())
}

fn two_accounts_in_one_window(sandbox: &Sandbox) -> SandboxResult {
    let (reader, writer) = io::pipe()?;
    let fd = writer.as_raw_fd().to_string();
    let child = sandbox.spawn_with_fds(
        &std::env::current_exe()?,
        &[],
        &[(CHILD_ENV, "window"), (TRACE_FD_ENV, &fd)],
        &[writer.as_fd()],
    )?;
    // the child holds the only writer from here, so the trace ends when it exits
    drop(writer);
    let mut running = Running::new(child)?;
    let mut trace = TraceReader::start(reader);
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    for account in [LAB_A, LAB_B] {
        trace
            .wait_where("account added", |line| about(line, "account", "added", account))
            .map_err(|error| running.fail(&error.to_string()))?;
    }
    trace
        .wait_where("status connected", |line| {
            line["view"] == "status" && line["change"] == "connected"
        })
        .map_err(|error| running.fail(&error.to_string()))?;
    trace
        .wait_where("a's chat list", |line| {
            line["view"] == "chat-list" && line["change"] == "shown"
        })
        .map_err(|error| running.fail(&error.to_string()))?;
    shot(sandbox, "ngteams-accounts-a.png")?;

    let mut input = sandbox.input()?;
    input.key_with(&[Modifier::Ctrl, Modifier::Alt], keys::DOWN)?;
    trace
        .next_where("switched to b", |line| about(line, "account", "switched", LAB_B))
        .map_err(|error| running.fail(&error.to_string()))?;
    shot(sandbox, "ngteams-accounts-b.png")?;

    a_s_message_notifies_naming_a_and_shows_a(sandbox, &mut trace, &running)?;
    shot(sandbox, "ngteams-accounts-notified.png")?;
    quit(sandbox, &mut trace, &running)?;
    running
        .line_where(WAIT, |line| line == "engines-released")
        .map_err(|error| running.fail(&format!("the engines weren't released: {error:?}")))?;
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the app exited {status}: {stderr}").into());
    }
    Ok(())
}

/// c posts to a's chat with c while b is shown: a's notification names a (`<summary> · <a's name>`), and activating
/// it shows a with the chat open.
fn a_s_message_notifies_naming_a_and_shows_a(
    sandbox: &Sandbox,
    trace: &mut TraceReader,
    running: &Running,
) -> SandboxResult {
    let staged = sandbox.path("post-as-c.staged");
    fs::write(&staged, AC_CHAT)?;
    fs::rename(&staged, sandbox.path(POST_TRIGGER))?;
    running
        .line_where(WAIT, |line| line.starts_with("posted "))
        .map_err(|error| format!("c didn't post: {error:?}"))?;
    trace.wait_where("notification shown for c's message", |line| {
        line["view"] == "notification" && line["change"] == "shown" && line["conversation"] == AC_CHAT
    })?;
    let notifications = sandbox.bus().notifications();
    let notification = notifications
        .iter()
        .find(|notification| notification.body == C_SAYS)
        .ok_or_else(|| format!("no notification of c's message: {notifications:?}"))?;
    // a's name: "Lab A", or what its profile on the mock says once read
    if !notification.summary.contains(" · ") {
        return Err(format!("the notification doesn't name a: {notification:?}").into());
    }
    sandbox.bus().invoke_action(notification.id, "default")?;
    trace.next_where("switched to a", |line| about(line, "account", "switched", LAB_A))?;
    trace.wait_where("its chat opened", |line| {
        line["view"] == "chat" && line["change"] == "opened" && line["conversation"] == AC_CHAT
    })?;
    Ok(())
}

/// The tray menu's "Quit".
fn quit(sandbox: &Sandbox, trace: &mut TraceReader, running: &Running) -> SandboxResult {
    let name = format!("org.kde.StatusNotifierItem-{}-1", running.pid());
    let host = sandbox.bus().connect()?;
    let item = zbus::blocking::Proxy::new(&host, name.as_str(), ITEM_PATH, "org.kde.StatusNotifierItem")?;
    let title: String = item.get_property("Title")?;
    if !title.starts_with(app::TITLE) {
        return Err(format!("the item's title: {title}").into());
    }
    let menu = zbus::blocking::Proxy::new(&host, name.as_str(), MENU_PATH, "com.canonical.dbusmenu")?;
    menu.call_method("Event", &(MENU_QUIT, "clicked", zbus::zvariant::Value::from(""), 0_u32))?;
    trace.next_where("tray quit", |line| line["view"] == "tray" && line["change"] == "quit")?;
    Ok(())
}

// --- the child: the app of a and b on the mock ---

/// Runs `done` once `frames` more frames of `window` have been presented.
fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

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
            expires_at: std::time::SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

/// The mock with a's and b's engines on it, each as `teams_core::account::start` builds one but for the mock origins, a
/// test roster and log, both fences on one call floor; their push channels on the mock's Trouter.
struct MockLab {
    _runtime: tokio::runtime::Runtime,
    mock: Arc<ChatServiceMock>,
    trouter: TrouterMock,
    dir: tempfile::TempDir,
}

impl MockLab {
    fn start() -> SandboxResult<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let dir = tempfile::tempdir()?;
        let config = LabConfig::parse(&lab_toml(dir.path()), dir.path())?;
        record_abc_chat(config.paths(), ABC_CHAT)?;
        Ok(Self {
            _runtime: runtime,
            mock: Arc::new(mock),
            trouter,
            dir,
        })
    }

    /// `alias`'s engine (its MRI `mri`) on the mock.
    fn engine(&self, alias: Alias, mri: &str, upn: &str) -> SandboxResult<EngineHandle> {
        let config = LabConfig::parse(&lab_toml(self.dir.path()), self.dir.path())?;
        let tokens = Arc::new(Tokens {
            token: self.mock.token(mri).ok_or("the mock's token")?,
            renewals: broadcast::channel(4).0,
        });

        let origin = self.mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
        let transport = Arc::new(http()?);
        let _membership = LiveMembershipSource::with_mock_origin(&origin)?;

        let trouter = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), self.trouter.origin())?
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        let author = Author::new(Mri::new(mri), display_name(upn));
        Ok(engine::start(Parts {
            calls: None,
            store: Store::open(&config.paths().store_file(alias))?,
            client: Arc::new(Client::new(http()?)),
            outbox: Outbox::new(transport, author),
            push: Some(Push::new(trouter)),
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })?)
    }

    /// Whether both accounts' engine locks are free: their engines' threads have ended.
    fn engines_released(&self) -> SandboxResult<bool> {
        let config = LabConfig::parse(&lab_toml(self.dir.path()), self.dir.path())?;
        for alias in [Alias::A, Alias::B] {
            if Store::open(&config.paths().store_file(alias))?.engine_lock().is_err() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn child_window() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let fd: RawFd = std::env::var(TRACE_FD_ENV)?.parse()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(fd)?);
    let lab = MockLab::start()?;
    let a = support::started(Alias::A, lab.engine(Alias::A, LAB_A, "bot-a@lab.example.test")?)?;
    let b = support::started(Alias::B, lab.engine(Alias::B, LAB_B, "bot-b@lab.example.test")?)?;
    let settings = teams_ui::SettingsFile::new(lab.dir.path().join("ui").join("a.toml"));
    let runner = support::runner(lab.dir.path())?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let mock = Arc::clone(&lab.mock);
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first: a,
                others: vec![b],
                trace: Arc::new(trace),
                settings: teams_ui::SettingsModel::load(settings),
                // the sandbox's private bus, with its fake notification server and tray watcher
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                post_when_asked(mock, cx);
                let _ = launched.window.update(cx, |_, window, _| {
                    after_frames(window, READY_FRAMES, |_, _| say("ready"));
                });
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    if lab.engines_released()? {
        say("engines-released");
    }
    Ok(())
}

/// Once the parent writes a chat's id to [`POST_TRIGGER`] in the sandbox dir, c posts [`C_SAYS`] there on the mock,
/// which delivers it to the members' push channels as any post of c's.
fn post_when_asked(mock: Arc<ChatServiceMock>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let trigger = PathBuf::from(dir).join(POST_TRIGGER);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(chat) = fs::read_to_string(&trigger) else {
                continue;
            };
            let _ = fs::remove_file(&trigger);
            match mock.post_as(LAB_C, chat.trim(), &format!("<p>{C_SAYS}</p>")) {
                Some(id) => say(&format!("posted {id}")),
                None => say("post refused"),
            }
            return;
        }
    })
    .detach();
}
