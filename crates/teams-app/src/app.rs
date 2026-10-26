//! The app on GPUI: the bridge over the account's engine, the main window, the sign-out, the quit.
//!
//! [`launch`] runs inside GPUI's `run`: it hands the engine to a [`Bridge`], opens the main window over the bridge's
//! models with the view trace on the [`Desktop`] (which holds the window, the notifications and the tray item, and
//! puts the last two on the bus the app is given), watches the session for a sign-out ([`watch_sign_out`]) and, once
//! the desktop asks to quit (the tray's "Quit", or the window closed with no tray item to stay in), shuts the engine
//! down (off the foreground, joining its thread) and quits. A window that can't be opened shuts the engine down too,
//! and the caller quits once that shutdown has ended ([`LaunchFailed`]).
//!
//! The engine is held as an [`AppEngine`]: its shutdown waits [`SHUTDOWN_GRACE`] for the engine's thread, then shuts
//! the broker's socket down, so an exchange waiting on the broker (the Secret Service asking the user to unlock it,
//! with the window already closed) ends and the engine with it, instead of keeping a windowless app and the account's
//! engine lock until the prompt is answered.

use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use std::cell::Cell;
use std::rc::Rc;

use gpui::{App, AppContext as _, AsyncApp, Entity, Global, QuitMode, SharedString, Task, WeakEntity, WindowHandle};
use teams_auth::session::SignedOut;
use teams_core::EngineError;
use teams_core::account::AccountError;
use teams_core::lab::LabError;
use teams_core::{EngineHandle, Event};
use teams_fence::lab::Alias;
use teams_log::diagnostics::Source as DiagnosticsSource;
use teams_ui::LogLevel;
use teams_ui::bus::BusError;
use teams_ui::window::{KitRoot, LAB_APP_ID};
use teams_ui::{
    Bridge, Core, Desktop, DesktopEvent, DesktopParts, EventSource, Models, SettingsFile, SettingsModel, ShellParts,
    VideoFeed, ViewTrace,
};
use tokio::sync::watch;

use crate::calling::AppCalls;

/// What the app starts with.
pub struct Launch {
    pub alias: Alias,
    pub engine: EngineHandle,
    /// A second descriptor of the app's end of the broker's socket, shut down when the engine's shutdown overruns
    /// ([`AppEngine`]); `None` for an engine that reaches no broker (a test's).
    pub broker: Option<UnixStream>,
    /// The session's signed-out state ([`teams_auth::session::Session::watch_signed_out`]); `None` for an engine
    /// whose tokens can't run out (a test's).
    pub signed_out: Option<watch::Receiver<Option<SignedOut>>>,
    pub trace: ViewTrace,
    /// The account's settings file (`LabPaths::ui_settings_file`).
    pub settings: SettingsFile,
    /// The session bus the app is given ([`teams_ui::bus::address_from_env`]), for its notifications and tray item.
    pub bus: Result<String, BusError>,
    /// Where "copy diagnostics" reads from: the diagnostic log [`install_log`] opened (or why it couldn't), the
    /// account's `fence.log`; `None` for an app without them (a test's).
    pub diagnostics: Option<DiagnosticsSource>,
    /// The window's calls ([`crate::calling`]); `None` shows none (the engine runs without calling).
    pub calls: Option<AppCalls>,
}

/// Installs the account's diagnostic log in `dir` (the lab's `LabPaths::app_log_dir`) at the settings' `setting`
/// level, `TEAMS_APP_LOG` overriding it, before the engine starts; where "copy diagnostics" reads from, `fence_log`
/// among it. A log that can't be opened is said once on stderr and in the settings view, and the app runs without it.
#[must_use]
pub fn install_log(dir: PathBuf, setting: LogLevel, fence_log: PathBuf) -> DiagnosticsSource {
    let config = teams_log::Config::from_env(dir, setting);
    let (log_file, log_problem) = match teams_log::install(&config) {
        Ok(log) => (Some(log.path().to_owned()), None),
        Err(error) => {
            eprintln!("teams-app: {error}");
            (None, Some(error.to_string()))
        }
    };
    log::info!("teams-app {} starting", env!("CARGO_PKG_VERSION"));
    DiagnosticsSource {
        app: format!("teams-app {}", env!("CARGO_PKG_VERSION")),
        log_file,
        log_problem,
        fence_log: Some(fence_log),
    }
}

/// The desktop and the bridge, held by the app for its whole life: nothing else holds the desktop once its window is
/// closed, and dropping the bridge would stop the engine.
struct Held {
    _desktop: Entity<Desktop>,
    _bridge: Entity<Bridge>,
}

impl Global for Held {}

/// What [`launch`] started: the main window, the bridge holding the engine, and the desktop holding the window, the
/// notifications and the tray item.
pub struct Launched {
    pub window: WindowHandle<KitRoot>,
    pub bridge: Entity<Bridge>,
    pub desktop: Entity<Desktop>,
}

/// What a lab account signed out is told to do.
#[must_use]
pub fn sign_in_hint(alias: Alias) -> SharedString {
    format!("run `proto-lab auth {alias}`").into()
}

/// The window's title.
#[must_use]
pub fn title(alias: Alias) -> SharedString {
    format!("Teams (lab {alias})").into()
}

/// How long the engine's shutdown waits for its thread before it shuts the broker's socket down ([`AppEngine`]).
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// The engine as the app holds it: its handle, and the broker's socket to shut down when its shutdown overruns its
/// grace (see the module docs).
pub struct AppEngine {
    engine: EngineHandle,
    broker: Option<UnixStream>,
    grace: Duration,
}

impl AppEngine {
    #[must_use]
    pub fn new(engine: EngineHandle, broker: Option<UnixStream>, grace: Duration) -> Self {
        Self { engine, broker, grace }
    }
}

impl EventSource for AppEngine {
    fn next_event(&mut self) -> impl Future<Output = Option<Event>> {
        self.engine.next_event()
    }

    fn shutdown(self) {
        let Self { engine, broker, grace } = self;
        engine.shutdown_within(grace, || {
            if let Some(broker) = &broker {
                // a socket already closed by the broker has nothing left to end
                let _ = broker.shutdown(Shutdown::Both);
            }
        });
    }
}

/// The account's settings from `file`, with "copy diagnostics" reading from `diagnostics` and, when the calls carry
/// video, the switch for its codec, which `video` follows.
fn settings_model(
    file: SettingsFile,
    diagnostics: Option<DiagnosticsSource>,
    video: Option<VideoFeed>,
    cx: &mut App,
) -> Entity<SettingsModel> {
    let mut settings = SettingsModel::load(file);
    for problem in settings.problems() {
        eprintln!("teams-app: {problem}");
        log::warn!("{problem}");
    }
    if let Some(diagnostics) = diagnostics {
        settings = settings.with_diagnostics(diagnostics);
    }
    if let Some(feed) = &video {
        // the user's control over Cisco's OpenH264: its switch, shown beside the attribution its licence asks for
        settings = settings.with_video_switch();
        feed.set_enabled(settings.settings().video_calls);
    }
    let settings = cx.new(|_| settings);
    if let Some(feed) = video {
        cx.observe(&settings, move |model, cx| {
            feed.set_enabled(model.read(cx).settings().video_calls);
        })
        .detach();
    }
    settings
}

/// The window couldn't be opened: why, and the engine's shutdown, which ends once its thread is joined (the caller
/// quits after it).
pub struct LaunchFailed {
    pub error: String,
    pub stopped: Task<()>,
}

/// Starts the app inside GPUI's `run` (see the module docs): the main window and the bridge holding the engine.
///
/// # Errors
/// The window can't be opened: the bridge is shut down then, and [`LaunchFailed::stopped`] ends once it is.
pub fn launch(cx: &mut App, launch: Launch) -> Result<Launched, LaunchFailed> {
    let Launch {
        alias,
        engine,
        broker,
        signed_out,
        trace,
        settings,
        bus,
        diagnostics,
        calls,
    } = launch;
    // GPUI would quit on its own as the last window closes (`QuitMode::Default` on Linux), before the engine is shut
    // down, and closing the window keeps the app in the tray: the app quits itself when the desktop asks
    // (`quit_when_asked`), once the engine is shut down
    cx.set_quit_mode(QuitMode::Explicit);
    let (models, ringer, video, devices) = match calls {
        Some(calls) => {
            let models = match calls.video.clone() {
                Some(feed) => Models::with_video_calls(calls.echo, feed, cx),
                None => Models::with_calls(calls.echo, cx),
            };
            (models, calls.ringer, calls.video, calls.devices)
        }
        None => (Models::new(cx), None, None, None),
    };
    let core: Arc<dyn Core> = Arc::new(engine.requests());
    let drained = models.clone();
    let engine = AppEngine::new(engine, broker, SHUTDOWN_GRACE);
    let bridge = cx.new(|cx| Bridge::start(engine, core, drained, cx));
    if let Some(signed_out) = signed_out {
        watch_sign_out(signed_out, bridge.downgrade(), sign_in_hint(alias), cx);
    }
    let settings = settings_model(settings, diagnostics, video, cx);
    if let Some(devices) = devices {
        // the calls' devices as the settings choose them, from the start
        teams_ui::settings::attach_devices(&settings, devices, cx);
    }
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings,
        trace: Arc::new(trace),
    };
    let desktop = cx.new(|cx| {
        Desktop::new(
            DesktopParts {
                app_id: LAB_APP_ID,
                app_name: title(alias),
                title: title(alias),
                shell: parts,
                ringer,
            },
            cx,
        )
    });
    let window = match desktop.update(cx, Desktop::open_window) {
        Ok(window) => window,
        Err(error) => {
            log::error!("the window can't be opened: {error:#}");
            return Err(LaunchFailed {
                error: format!("{error:#}"),
                stopped: bridge.update(cx, Bridge::shutdown),
            });
        }
    };
    // held for the app's life: the desktop outlives its window (the app stays in the tray), and the bridge with it
    cx.set_global(Held {
        _desktop: desktop.clone(),
        _bridge: bridge.clone(),
    });
    quit_when_asked(&desktop, bridge.clone(), cx);
    let connecting = desktop.update(cx, |desktop, cx| desktop.connect(bus, cx));
    cx.spawn(async move |_: &mut AsyncApp| {
        for problem in connecting.await {
            eprintln!("teams-app: {problem}");
            log::warn!("{problem}");
        }
    })
    .detach();
    Ok(Launched {
        window,
        bridge,
        desktop,
    })
}

/// Once `signed_out` says the session is signed out, shows it with `hint` and shuts the engine down, so nothing more
/// is sent ([`Bridge::sign_out`]).
pub fn watch_sign_out(
    mut signed_out: watch::Receiver<Option<SignedOut>>,
    bridge: WeakEntity<Bridge>,
    hint: SharedString,
    cx: &mut App,
) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        // the session dropped without signing out: nothing to watch
        if signed_out.wait_for(Option::is_some).await.is_err() {
            return;
        }
        // a bridge that is gone has nothing left to stop
        let _ = bridge.update(cx, |bridge, cx| bridge.sign_out(hint, cx).detach());
    })
    .detach();
}

/// Once the desktop asks to quit (the tray's "Quit", or the window closed with no tray item): the engine shut down,
/// then the app quits. The first ask does it; a later one (the window closing as the app quits) changes nothing.
fn quit_when_asked(desktop: &Entity<Desktop>, bridge: Entity<Bridge>, cx: &mut App) {
    let quitting = Rc::new(Cell::new(false));
    cx.subscribe(desktop, move |_, event, cx| match event {
        DesktopEvent::Quit => {
            if !quitting.replace(true) {
                let stopped = bridge.update(cx, Bridge::shutdown);
                quit_after(stopped, cx);
            }
        }
    })
    .detach();
}

/// Quits the app once `stopped` (the engine's shutdown) has ended.
pub fn quit_after(stopped: Task<()>, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        stopped.await;
        cx.update(|cx| cx.quit());
    })
    .detach();
}

/// What the app prints when the account's engine can't start: another engine running on the account is named as
/// such (another `teams-app`, a `teams-headless`), with the lock it holds.
#[must_use]
pub fn start_failure(alias: Alias, error: &LabError) -> String {
    match error {
        LabError::Account(AccountError::Engine(EngineError::AlreadyRunning { lock })) => format!(
            "another engine runs on lab account {alias} (a teams-app or teams-headless holds {}): only one at a \
             time; this one exits",
            lock.display()
        ),
        other => format!("lab account {alias}: {other}"),
    }
}
