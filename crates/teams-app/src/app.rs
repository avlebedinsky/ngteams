//! The app on GPUI: a bridge over each account's engine, the main window, the sign-outs, the quit.
//!
//! [`launch`] runs inside GPUI's `run`: it hands each account's engine to a [`Bridge`] of its own, opens the main
//! window over the first account's models with the view trace on the [`Desktop`] (which holds the window, the
//! notifications and the tray item, and puts the last two on the bus the app is given), adds every further account to
//! the desktop ([`Desktop::add_account`]: its shell in the window, switched to from the rail's avatars, its
//! notifications and incoming calls live whichever account is shown), watches each account's session for a sign-out
//! ([`watch_sign_out`]) and, once the desktop asks to quit (the tray's "Quit", or the window closed with no tray item
//! to stay in), shuts every engine down (off the foreground, joining their threads) and quits once all of them are
//! joined. A window that can't be opened shuts every engine down too, and the caller quits once that shutdown has
//! ended ([`LaunchFailed`]).
//!
//! The accounts share one settings file (the first account's: [`Launch::settings`]), whose top-level selected chat
//! and manual status are the first account's and the others' are kept under their MRIs; one trace; one ringtone and
//! one device choice (the first account's [`AppCalls`]), each account's calls on the same devices with a video feed
//! of its own, which the settings' video switch turns on and off for all of them. Lab accounts sign in through
//! `proto-lab auth`, so the window adds and removes none (`can_add: false`).
//!
//! Each engine is held as an [`AppEngine`]: its shutdown waits [`SHUTDOWN_GRACE`] for the engine's thread, then shuts
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
use teams_media::devices::catalog::DeviceDirectory;
use teams_ui::LogLevel;
use teams_ui::bus::BusError;
use teams_ui::window::{KitRoot, LAB_APP_ID};
use teams_ui::{
    AccountParts, Bridge, Core, Desktop, DesktopEvent, DesktopParts, EventSource, Models, Ringer, SettingsFile,
    SettingsModel, ShellParts, VideoFeed, ViewTrace,
};
use tokio::sync::watch;

use crate::calling::AppCalls;

/// One lab account the app starts with: its engine, how it reaches its broker, its session's sign-out, its calls.
pub struct LaunchAccount {
    pub alias: Alias,
    pub engine: EngineHandle,
    /// A second descriptor of the app's end of the account's broker's socket, shut down when the engine's shutdown
    /// overruns ([`AppEngine`]); `None` for an engine that reaches no broker (a test's).
    pub broker: Option<UnixStream>,
    /// The session's signed-out state ([`teams_auth::session::Session::watch_signed_out`]); `None` for an engine
    /// whose tokens can't run out (a test's).
    pub signed_out: Option<watch::Receiver<Option<SignedOut>>>,
    /// The account's calls in the window ([`crate::calling`]); `None` shows none (the engine runs without calling).
    /// Only the first account's ringer and devices are used: the app has one ringtone and one device choice.
    pub calls: Option<AppCalls>,
}

impl LaunchAccount {
    /// `alias`'s `engine`, with no broker, no sign-out to watch and no calls (a test's).
    #[must_use]
    pub fn bare(alias: Alias, engine: EngineHandle) -> Self {
        Self {
            alias,
            engine,
            broker: None,
            signed_out: None,
            calls: None,
        }
    }
}

/// What the app starts with.
pub struct Launch {
    /// The first account: shown first, its settings the top-level ones of the settings file.
    pub first: LaunchAccount,
    /// The further accounts, in the order the rail shows them.
    pub others: Vec<LaunchAccount>,
    pub trace: ViewTrace,
    /// The settings file (`LabPaths::ui_settings_file` of the first account).
    pub settings: SettingsFile,
    /// The session bus the app is given ([`teams_ui::bus::address_from_env`]), for its notifications and tray item.
    pub bus: Result<String, BusError>,
    /// Where "copy diagnostics" reads from: the diagnostic log [`install_log`] opened (or why it couldn't), the
    /// lab's `fence.log`; `None` for an app without them (a test's).
    pub diagnostics: Option<DiagnosticsSource>,
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

/// The desktop and every account's bridge, held by the app for its whole life: nothing else holds the desktop once its
/// window is closed, and dropping a bridge would stop its engine.
struct Held {
    _desktop: Entity<Desktop>,
    _bridges: Vec<Entity<Bridge>>,
}

impl Global for Held {}

/// What [`launch`] started: the main window, the bridges holding the engines, and the desktop holding the window, the
/// notifications and the tray item.
pub struct Launched {
    pub window: WindowHandle<KitRoot>,
    /// The first account's bridge.
    pub bridge: Entity<Bridge>,
    /// Every account's bridge, the first account's first.
    pub bridges: Vec<Entity<Bridge>>,
    pub desktop: Entity<Desktop>,
}

/// What a lab account signed out is told to do.
#[must_use]
pub fn sign_in_hint(alias: Alias) -> SharedString {
    format!("run `proto-lab auth {alias}`").into()
}

/// The window's title for one account.
#[must_use]
pub fn title(alias: Alias) -> SharedString {
    titled(&[alias])
}

/// The window's title for `aliases`, the first account's first: `ngteams (lab a)`, `ngteams (lab a, b)`.
#[must_use]
pub fn titled(aliases: &[Alias]) -> SharedString {
    let names: Vec<&str> = aliases.iter().map(|alias| alias.as_str()).collect();
    format!("ngteams (lab {})", names.join(", ")).into()
}

/// The name a lab account's avatar on the rail is shown by until its profile names it: `Lab A`.
#[must_use]
pub fn account_name(alias: Alias) -> SharedString {
    format!("Lab {}", alias.as_str().to_uppercase()).into()
}

/// What tells a lab account apart beside its name: `lab a`.
#[must_use]
pub fn account_detail(alias: Alias) -> SharedString {
    format!("lab {alias}").into()
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

/// The app's settings from `file`, with "copy diagnostics" reading from `diagnostics` and, when the calls carry video,
/// the switch for its codec, which every account's feed in `videos` follows.
fn settings_model(
    file: SettingsFile,
    diagnostics: Option<DiagnosticsSource>,
    videos: Vec<VideoFeed>,
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
    if !videos.is_empty() {
        // the user's control over Cisco's OpenH264: its switch, shown beside the attribution its licence asks for
        settings = settings.with_video_switch();
        for feed in &videos {
            feed.set_enabled(settings.settings().video_calls);
        }
    }
    let settings = cx.new(|_| settings);
    if !videos.is_empty() {
        cx.observe(&settings, move |model, cx| {
            let enabled = model.read(cx).settings().video_calls;
            for feed in &videos {
                feed.set_enabled(enabled);
            }
        })
        .detach();
    }
    settings
}

/// One account as [`launch`] started it: its bridge over its engine, its models, and what its calls show and play.
struct Started {
    alias: Alias,
    bridge: Entity<Bridge>,
    models: Models,
    ringer: Option<Arc<dyn Ringer>>,
    video: Option<VideoFeed>,
    devices: Option<Arc<dyn DeviceDirectory>>,
}

/// Hands `account`'s engine to a bridge of its own over models with its calls, and watches its session for a sign-out.
fn start_account(account: LaunchAccount, cx: &mut App) -> Started {
    let LaunchAccount {
        alias,
        engine,
        broker,
        signed_out,
        calls,
    } = account;
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
    Started {
        alias,
        bridge,
        models,
        ringer,
        video,
        devices,
    }
}

/// The shutdown of every one of `bridges`, ended once all of them are.
fn shut_down_all(bridges: &[Entity<Bridge>], cx: &mut App) -> Task<()> {
    let stopping: Vec<Task<()>> = bridges
        .iter()
        .map(|bridge| bridge.update(cx, Bridge::shutdown))
        .collect();
    cx.spawn(async move |_: &mut AsyncApp| {
        for stopped in stopping {
            stopped.await;
        }
    })
}

/// The window couldn't be opened: why, and the engines' shutdown, which ends once every engine's thread is joined (the
/// caller quits after it).
pub struct LaunchFailed {
    pub error: String,
    pub stopped: Task<()>,
}

/// Starts the app inside GPUI's `run` (see the module docs): the main window and a bridge holding each account's
/// engine.
///
/// # Errors
/// The window can't be opened: every bridge is shut down then, and [`LaunchFailed::stopped`] ends once all of them are.
pub fn launch(cx: &mut App, launch: Launch) -> Result<Launched, LaunchFailed> {
    let Launch {
        first,
        others,
        trace,
        settings,
        bus,
        diagnostics,
    } = launch;
    // GPUI would quit on its own as the last window closes (`QuitMode::Default` on Linux), before the engines are shut
    // down, and closing the window keeps the app in the tray: the app quits itself when the desktop asks
    // (`quit_when_asked`), once every engine is shut down
    cx.set_quit_mode(QuitMode::Explicit);
    let first = start_account(first, cx);
    let others: Vec<Started> = others.into_iter().map(|account| start_account(account, cx)).collect();
    let aliases: Vec<Alias> = std::iter::once(first.alias)
        .chain(others.iter().map(|other| other.alias))
        .collect();
    let bridge = first.bridge.clone();
    let bridges: Vec<Entity<Bridge>> = std::iter::once(bridge.clone())
        .chain(others.iter().map(|other| other.bridge.clone()))
        .collect();
    let videos = std::iter::once(&first)
        .chain(&others)
        .filter_map(|started| started.video.clone())
        .collect();
    let settings = settings_model(settings, diagnostics, videos, cx);
    if !others.is_empty() {
        // the top-level selected chat and manual status are the first account's, the others' kept under their MRIs
        let me = bridge.read(cx).core().me().to_owned();
        settings.update(cx, |settings, cx| settings.set_first_account(&me, cx));
    }
    if let Some(devices) = first.devices.clone() {
        // the calls' devices as the settings choose them, from the start
        teams_ui::settings::attach_devices(&settings, devices, cx);
    }
    let desktop = desktop(first, &aliases, settings, trace, cx);
    for other in others {
        let account = AccountParts {
            core: other.bridge.read(cx).core(),
            models: other.models,
            name: account_name(other.alias),
            detail: account_detail(other.alias),
        };
        desktop.update(cx, |desktop, cx| desktop.add_account(account, cx));
    }
    let window = match desktop.update(cx, Desktop::open_window) {
        Ok(window) => window,
        Err(error) => {
            log::error!("the window can't be opened: {error:#}");
            return Err(LaunchFailed {
                error: format!("{error:#}"),
                stopped: shut_down_all(&bridges, cx),
            });
        }
    };
    // held for the app's life: the desktop outlives its window (the app stays in the tray), and the bridges with it
    cx.set_global(Held {
        _desktop: desktop.clone(),
        _bridges: bridges.clone(),
    });
    quit_when_asked(&desktop, bridges.clone(), cx);
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
        bridges,
        desktop,
    })
}

/// The desktop over the `first` account's shell, titled for `aliases`, its ringtone the first account's.
fn desktop(
    first: Started,
    aliases: &[Alias],
    settings: Entity<SettingsModel>,
    trace: ViewTrace,
    cx: &mut App,
) -> Entity<Desktop> {
    let parts = ShellParts {
        core: first.bridge.read(cx).core(),
        models: first.models,
        settings,
        trace: Arc::new(trace),
    };
    cx.new(|cx| {
        Desktop::new(
            DesktopParts {
                app_id: LAB_APP_ID,
                app_name: titled(aliases),
                title: titled(aliases),
                shell: parts,
                account_name: account_name(first.alias),
                account_detail: account_detail(first.alias),
                // lab accounts sign in through `proto-lab auth`, never from the window
                can_add: false,
                ringer: first.ringer,
            },
            cx,
        )
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

/// Once the desktop asks to quit (the tray's "Quit", or the window closed with no tray item): every engine shut down,
/// then the app quits once all of them are. The first ask does it; a later one (the window closing as the app quits)
/// changes nothing. The lab app adds and removes no account, so the desktop's other asks change nothing either.
fn quit_when_asked(desktop: &Entity<Desktop>, bridges: Vec<Entity<Bridge>>, cx: &mut App) {
    let quitting = Rc::new(Cell::new(false));
    cx.subscribe(desktop, move |_, event, cx| match event {
        DesktopEvent::Quit => {
            if !quitting.replace(true) {
                let stopped = shut_down_all(&bridges, cx);
                quit_after(stopped, cx);
            }
        }
        DesktopEvent::AddAccount | DesktopEvent::RemoveAccount(_) => {}
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
