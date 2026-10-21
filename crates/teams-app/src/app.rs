//! The app on GPUI: the bridge over the account's engine, the main window, the sign-out, the quit.
//!
//! [`launch`] runs inside GPUI's `run`: it hands the engine to a [`Bridge`], opens the main window over the bridge's
//! models with the view trace, watches the session for a sign-out ([`watch_sign_out`]) and, once the last window
//! closes, shuts the engine down (off the foreground, joining its thread) and quits. A window that can't be opened
//! shuts the engine down too, and the caller quits once that shutdown has ended ([`LaunchFailed`]).
//!
//! The engine is held as an [`AppEngine`]: its shutdown waits [`SHUTDOWN_GRACE`] for the engine's thread, then shuts
//! the broker's socket down, so an exchange waiting on the broker (the Secret Service asking the user to unlock it,
//! with the window already closed) ends and the engine with it, instead of keeping a windowless app and the account's
//! engine lock until the prompt is answered.

use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, AppContext as _, AsyncApp, Entity, QuitMode, SharedString, Task, WeakEntity, WindowHandle};
use teams_auth::session::SignedOut;
use teams_core::EngineError;
use teams_core::lab::LabError;
use teams_core::{EngineHandle, Event};
use teams_fence::lab::Alias;
use teams_ui::window::{self, KitRoot, LAB_APP_ID};
use teams_ui::{Bridge, Core, EventSource, Models, SettingsFile, SettingsModel, ShellParts, ViewTrace};
use tokio::sync::watch;

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
pub fn launch(cx: &mut App, launch: Launch) -> Result<(WindowHandle<KitRoot>, Entity<Bridge>), LaunchFailed> {
    let Launch {
        alias,
        engine,
        broker,
        signed_out,
        trace,
        settings,
    } = launch;
    // GPUI would quit on its own as the last window closes (`QuitMode::Default` on Linux), before the engine is shut
    // down: the app quits itself once it is (`quit_when_closed`)
    cx.set_quit_mode(QuitMode::Explicit);
    let models = Models::new(cx);
    let core: Arc<dyn Core> = Arc::new(engine.requests());
    let drained = models.clone();
    let engine = AppEngine::new(engine, broker, SHUTDOWN_GRACE);
    let bridge = cx.new(|cx| Bridge::start(engine, core, drained, cx));
    if let Some(signed_out) = signed_out {
        watch_sign_out(signed_out, bridge.downgrade(), sign_in_hint(alias), cx);
    }
    let settings = SettingsModel::load(settings);
    for problem in settings.problems() {
        eprintln!("teams-app: {problem}");
    }
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings: cx.new(|_| settings),
        trace: Arc::new(trace),
    };
    let window = match window::open(cx, LAB_APP_ID, title(alias), parts) {
        Ok(window) => window,
        Err(error) => {
            return Err(LaunchFailed {
                error: format!("{error:#}"),
                stopped: bridge.update(cx, Bridge::shutdown),
            });
        }
    };
    quit_when_closed(bridge.clone(), cx);
    Ok((window, bridge))
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

/// Once the last window closes: the engine shut down, then the app quits.
fn quit_when_closed(bridge: Entity<Bridge>, cx: &mut App) {
    cx.on_window_closed(move |cx, _| {
        if cx.windows().is_empty() {
            let stopped = bridge.update(cx, Bridge::shutdown);
            quit_after(stopped, cx);
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
        LabError::Engine(EngineError::AlreadyRunning { lock }) => format!(
            "another engine runs on lab account {alias} (a teams-app or teams-headless holds {}): only one at a \
             time; this one exits",
            lock.display()
        ),
        other => format!("lab account {alias}: {other}"),
    }
}
