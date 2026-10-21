//! The app's window in the UI sandbox, on the mock (`harness = false`: GPUI runs on the process's main thread).
//!
//! The test runs itself again inside the sandbox ([`Sandbox::spawn_with_fds`]: the sandbox's compositor, bus and
//! opener, nothing of the desktop's, only the view trace's pipe passed through), in one of two modes:
//!
//! - `window`: lab account a's engine on the mock chat service and its Trouter (`teams-mock`, through the
//!   `test-support` mock origins, a test roster and log), handed to the app as `teams-app` hands its own
//!   ([`teams_app::app::launch`]). The parent waits for the window, checks its rem size is the theme's font size,
//!   reads the view trace (the window opened, the push channel connected), screenshots the window (its background fills it; the shot is kept under
//!   `target/ui-shots/`), closes it through niri, and checks that the app quit and its engine's thread was joined (the
//!   account's engine lock is free).
//! - `http`: a window with an `img()` of an `https` URL on a loopback listener the child holds. GPUI's HTTP client is
//!   the null one: the image loads nothing, a request through `cx.http_client()` is refused, and the listener never
//!   sees a connection.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, future, io, thread};

use gpui::{
    App, AppContext as _, AsyncApp, Bounds, Context, IntoElement, ParentElement as _, Render, Styled as _, Window,
    WindowBounds, WindowOptions, WindowingModes, div, img, px, rgb, size,
};
use teams_app::app::{self, Launch};
use teams_app::trace::adopt_trace;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::lab::display_name;
use teams_fence::lab::{Alias, LabConfig, record_abc_chat};
use teams_fence::{Fence, FenceLog, LiveMembershipSource, Mri, Roster};
use teams_mock::{ABC_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, TrouterMock, lab_toml};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::view::BACKGROUND;
use tokio::sync::broadcast;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

/// Which mode the child runs in.
const CHILD_ENV: &str = "TEAMS_APP_WINDOW_TEST";
/// The child's view trace descriptor.
const TRACE_FD_ENV: &str = "TEAMS_APP_WINDOW_TRACE_FD";
/// From the child's start to its `ready` (the mock started, the window drawn).
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(30);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 3;

fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("window") => child_window(),
        Ok("http") => child_http(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("window test: {error}");
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
    let Some(sandbox) = sandbox_or_skip("teams-app window", &options)? else {
        return Ok(());
    };
    an_empty_window_opens_on_the_mock(&sandbox).map_err(|error| format!("{error}{}", sandbox.logs()))?;
    gpui_s_http_client_is_the_null_one(&sandbox)?;
    say("teams-app window: ok");
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
                let Ok(value) = serde_json::from_str(&line) else {
                    // a line that isn't JSON is a failure the parent reports: kept as a string
                    let _ = sender.send(serde_json::Value::String(line));
                    continue;
                };
                let _ = sender.send(value);
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    /// Waits for a line of `view` with `change`.
    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult {
        let found = |line: &serde_json::Value| line["view"] == view && line["change"] == change;
        if self.seen.iter().any(found) {
            return Ok(());
        }
        loop {
            let line = self
                .lines
                .recv_timeout(WAIT)
                .map_err(|_| format!("no trace line {view} {change}; seen {:?}", self.seen))?;
            let hit = found(&line);
            self.seen.push(line);
            if hit {
                return Ok(());
            }
        }
    }
}

/// The share of the screenshot's pixels within 8 of `color` (0xRRGGBB) on every channel.
fn colour_share(png: &Path, color: u32) -> SandboxResult<f64> {
    let wanted = color.to_be_bytes();
    let shot = image::open(png)?.to_rgb8();
    let near = shot
        .pixels()
        .filter(|pixel| {
            pixel
                .0
                .iter()
                .zip(&wanted[1..])
                .all(|(&got, &want)| got.abs_diff(want) <= 8)
        })
        .count();
    let total = u64::from(shot.width()) * u64::from(shot.height());
    Ok(f64::from(u32::try_from(near)?) / f64::from(u32::try_from(total)?))
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

fn an_empty_window_opens_on_the_mock(sandbox: &Sandbox) -> SandboxResult {
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
    let rem = running
        .line_where(STARTUP, |line| line.starts_with("rem-size "))
        .map_err(|error| running.fail(&format!("no rem-size line: {error:?}")))?;
    // the theme's compact font size reached the window drawn on the real platform (GPUI's own default is 16 px)
    let compact = format!("rem-size {}", teams_ui::theme::FONT_SIZE);
    if rem != compact {
        return Err(running.fail(&format!(
            "the window's rem size isn't the theme's: {rem}, not {compact}"
        )));
    }
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    trace.wait_for("window", "opened")?;
    trace.wait_for("status", "connected")?;

    let windows = sandbox.niri_msg(&["windows"])?;
    if !windows.contains(teams_ui::window::LAB_APP_ID) {
        return Err(running.fail(&format!("no window with the app id: {windows}")));
    }
    let png = sandbox.screenshot("teams-app-window.png", true)?;
    fs::copy(&png, shots_dir()?.join("teams-app-window.png"))?;
    let share = colour_share(&png, BACKGROUND)?;
    if share < 0.8 {
        return Err(running.fail(&format!("the window's background fills {share:.2} of it")));
    }

    sandbox.niri_msg(&["action", "close-window"])?;
    running
        .line_where(WAIT, |line| line == "engine-released")
        .map_err(|error| running.fail(&format!("the engine wasn't released: {error:?}")))?;
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the app exited {status}: {stderr}").into());
    }
    if let Some(odd) = trace.seen.iter().find(|line| !line.is_object()) {
        return Err(format!("a trace line isn't JSON: {odd}").into());
    }
    Ok(())
}

fn gpui_s_http_client_is_the_null_one(sandbox: &Sandbox) -> SandboxResult {
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, "http")])?;
    let running = Running::new(child)?;
    let refused = running
        .line_where(STARTUP, |line| line.starts_with("client-refused "))
        .map_err(|error| format!("no client-refused line: {error:?}"))?;
    if !refused.contains("No HttpClient available") {
        return Err(format!("GPUI's HTTP client isn't the null one: {refused}").into());
    }
    let connections = running
        .line_where(WAIT, |line| line.starts_with("connections "))
        .map_err(|error| format!("no connections line: {error:?}"))?;
    if connections != "connections 0" {
        return Err(format!("a request left the process: {connections}").into());
    }
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the http child exited {status}: {stderr}").into());
    }
    Ok(())
}

// --- the children ---

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

/// The mock and a's engine on it, as `teams_core::lab::start` builds one but for the mock origins, a test roster and
/// a test log; its push channel on the Trouter mock.
struct MockLab {
    _runtime: tokio::runtime::Runtime,
    _mock: ChatServiceMock,
    _trouter: TrouterMock,
    dir: tempfile::TempDir,
}

impl MockLab {
    fn start() -> SandboxResult<(Self, EngineHandle)> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let dir = tempfile::tempdir()?;
        let config = LabConfig::parse(&lab_toml(dir.path()), dir.path())?;
        record_abc_chat(config.paths(), ABC_CHAT)?;
        let tokens = Arc::new(Tokens {
            token: mock.token(LAB_A).ok_or("the mock's token for a")?,
            renewals: broadcast::channel(4).0,
        });
        let fence = Arc::new(Fence::new(
            Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
            FenceLog::from_writer(io::sink()),
        ));
        let origin = mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &origin);
        let transport = Arc::new(http()?);
        let membership = LiveMembershipSource::with_mock_origin(&origin)?.with_lab(&config);
        membership.set_credential_source(transport.clone());
        let trouter_client = Trouter::with_mock_origin(
            Arc::clone(&transport),
            Arc::clone(&tokens),
            Arc::clone(&fence),
            trouter.origin(),
        )?
        .with_options(Options {
            backoff_base: Duration::from_millis(50),
            backoff_cap: Duration::from_millis(50),
            backoff_jitter_percent: 0,
            ..Options::default()
        });
        let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
        let engine = engine::start(Parts {
            store: Store::open(&config.paths().store_file(Alias::A))?,
            client: Arc::new(Client::new(http()?)),
            outbox: Outbox::new(transport, fence, Arc::new(membership), author),
            push: Some(Push::new(trouter_client)),
            options: EngineOptions::default(),
        })?;
        let lab = Self {
            _runtime: runtime,
            _mock: mock,
            _trouter: trouter,
            dir,
        };
        Ok((lab, engine))
    }

    /// Whether the account's engine lock is free: the engine's thread has ended.
    fn engine_released(&self) -> SandboxResult<bool> {
        let config = LabConfig::parse(&lab_toml(self.dir.path()), self.dir.path())?;
        let store = Store::open(&config.paths().store_file(Alias::A))?;
        Ok(store.engine_lock().is_ok())
    }
}

/// The trace descriptor the parent passed.
fn passed_trace_fd() -> SandboxResult<RawFd> {
    Ok(std::env::var(TRACE_FD_ENV)?.parse()?)
}

fn child_window() -> SandboxResult {
    teams_app::guard::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(passed_trace_fd()?)?);
    let (lab, engine) = MockLab::start()?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                alias: Alias::A,
                engine,
                broker: None,
                signed_out: None,
                trace,
            },
        );
        match launched {
            Ok((window, _)) => {
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, READY_FRAMES, |window, _| {
                        say(&format!("rem-size {}", window.rem_size().as_f32()));
                        say("ready");
                    });
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
    if lab.engine_released()? {
        say("engine-released");
    }
    Ok(())
}

/// A window with one image whose source is an `https` URL.
struct Picture {
    url: String,
}

impl Render for Picture {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(BACKGROUND))
            .child(img(self.url.clone()).size(px(64.0)))
    }
}

/// How many connections `listener` (non-blocking) has waiting.
fn accepted(listener: &TcpListener) -> usize {
    std::iter::from_fn(|| listener.accept().ok()).count()
}

fn child_http() -> SandboxResult {
    teams_app::guard::window_allowed_here()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let url = format!("https://127.0.0.1:{}/picture.png", listener.local_addr()?.port());
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(320.0), px(240.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        let picture_url = url.clone();
        let opened = cx.open_window(options, move |_, cx| cx.new(|_| Picture { url: picture_url }));
        let Ok(window) = opened else {
            say("no window");
            cx.quit();
            return;
        };
        let _ = window.update(cx, move |_, window, _| {
            after_frames(window, 10, move |_, cx| check_http(cx, url, listener));
        });
    });
    Ok(())
}

/// After the image had its chance to load: a request through GPUI's own client, then the listener's count.
fn check_http(cx: &mut App, url: String, listener: TcpListener) {
    let client = cx.http_client();
    cx.spawn(async move |cx: &mut AsyncApp| {
        cx.background_executor().timer(Duration::from_secs(1)).await;
        match client.get(&url, ().into(), true).await {
            Ok(response) => say(&format!("client-answered {}", response.status())),
            Err(error) => say(&format!("client-refused {error:#}")),
        }
        say(&format!("connections {}", accepted(&listener)));
        cx.update(|cx| cx.quit());
    })
    .detach();
}
