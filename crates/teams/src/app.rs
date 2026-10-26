//! The personal app on GPUI: the bridge over the account's engine, the main window (`teams-ui`'s shell, with the first
//! run's screen of the writes that happen on their own, [`settings`]), the notifications and the tray on the desktop's
//! session bus, the calls when the engine has its calling ([`PersonalCalls`]: a call button and "Test call", an
//! incoming call's ringtone, notification and banner, the in-call view, the video switch and the devices in the
//! settings), the sign-out and the quit.
//!
//! **Signed out** ([`watch_sign_out`]): once the session can't renew its tokens (`invalid_grant`: a sign-in frequency
//! policy, a password change, a revoked session; or no refresh token stored), the window shows [`SIGNED_OUT_HINT`], the
//! engine is shut down (nothing more is sent: every request answers that it stopped), and the sign-in window opens
//! again ([`SignInAgain`]). A sign-in that goes through starts the app afresh (`main`: the process runs itself again),
//! so a new engine starts only once it is signed in again.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, AppContext as _, AsyncApp, Entity, Global, QuitMode, SharedString, Task, WeakEntity, WindowHandle};
use teams_auth::session::SignedOut;
use teams_core::EngineHandle;
use teams_log::diagnostics::Source as DiagnosticsSource;
use teams_ui::bus::BusError;
use teams_ui::window::{KitRoot, PERSONAL_APP_ID};
use teams_ui::{
    Bridge, Core, Desktop, DesktopEvent, DesktopParts, LogLevel, Models, SettingsFile, SettingsModel, ShellParts,
    VideoFeed, ViewTrace,
};
use tokio::sync::watch;

use crate::calling::PersonalCalls;

/// The window's title and the app's name on its notifications and tray item.
pub const TITLE: &str = "Teams";

/// What the window says once the account is signed out.
pub const SIGNED_OUT_HINT: &str = "Signed out: sign in again in the window that opened";

/// Opens the sign-in window again once the account is signed out (see the module docs).
pub type SignInAgain = Rc<dyn Fn(&mut App)>;

/// What the app starts with.
pub struct Launch {
    pub engine: EngineHandle,
    /// The session's signed-out state (`Session::watch_signed_out`).
    pub signed_out: watch::Receiver<Option<SignedOut>>,
    /// The settings, the first run's screen asked for ([`settings`]).
    pub settings: SettingsModel,
    /// The desktop's session bus, for the notifications and the tray item.
    pub bus: Result<String, BusError>,
    pub sign_in_again: SignInAgain,
    /// The window's calls, when the engine has its calling (`crate::calling::open`); `None` shows no call.
    pub calls: Option<PersonalCalls>,
}

/// The settings in `file` (`~/.local/share/teams/settings.toml`) with the first run's screen asked for: none of
/// typing, read receipts and presence is sent until the user confirms it (`docs/personal-build.md`, "The settings that
/// write on their own"); with where "copy diagnostics" reads from.
#[must_use]
pub fn settings(file: SettingsFile, diagnostics: Option<DiagnosticsSource>) -> SettingsModel {
    let settings = SettingsModel::load(file).requiring_first_run();
    match diagnostics {
        Some(diagnostics) => settings.with_diagnostics(diagnostics),
        None => settings,
    }
}

/// Installs the diagnostic log in `dir` at the settings' `setting` level (`TEAMS_APP_LOG` overriding it) before the
/// engine starts. It is the user's data, like the store: agents never read it. A log that can't be opened is said once
/// on stderr and in the settings view, and the app runs without it.
#[must_use]
pub fn install_log(dir: PathBuf, setting: LogLevel, fence_log: PathBuf) -> DiagnosticsSource {
    let config = teams_log::Config::from_env(dir, setting);
    let (log_file, log_problem) = match teams_log::install(&config) {
        Ok(log) => (Some(log.path().to_owned()), None),
        Err(error) => {
            eprintln!("teams: {error}");
            (None, Some(error.to_string()))
        }
    };
    log::info!("teams {} starting", env!("CARGO_PKG_VERSION"));
    DiagnosticsSource {
        app: format!("teams {}", env!("CARGO_PKG_VERSION")),
        log_file,
        log_problem,
        fence_log: Some(fence_log),
    }
}

/// The desktop and the bridge, held for the app's life.
struct Held {
    _desktop: Entity<Desktop>,
    _bridge: Entity<Bridge>,
}

impl Global for Held {}

/// What [`launch`] started.
pub struct Launched {
    pub window: WindowHandle<KitRoot>,
    pub bridge: Entity<Bridge>,
    pub desktop: Entity<Desktop>,
}

/// The window couldn't be opened: why, and the engine's shutdown (the caller quits after it).
pub struct LaunchFailed {
    pub error: String,
    pub stopped: Task<()>,
}

/// Starts the app inside GPUI's `run`: the bridge holding the engine, the window, the sign-out watch, the bus.
///
/// # Errors
/// The window can't be opened: the bridge is shut down then.
pub fn launch(cx: &mut App, launch: Launch) -> Result<Launched, LaunchFailed> {
    let Launch {
        engine,
        signed_out,
        settings,
        bus,
        sign_in_again,
        calls,
    } = launch;
    // closing the window keeps the app in the tray: it quits itself once the engine is shut down
    cx.set_quit_mode(QuitMode::Explicit);
    let (models, bridge) = bridge(engine, calls.as_ref(), cx);
    watch_sign_out(signed_out, bridge.downgrade(), sign_in_again, cx);
    for problem in settings.problems() {
        eprintln!("teams: {problem}");
        log::warn!("{problem}");
    }
    let video = calls.as_ref().and_then(|calls| calls.video.clone());
    let settings = settings_entity(settings, video, cx);
    if let Some(devices) = calls.as_ref().and_then(|calls| calls.devices.clone()) {
        // the calls' devices as the settings choose them, from the start
        teams_ui::settings::attach_devices(&settings, devices, cx);
    }
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings,
        trace: Arc::new(ViewTrace::off()),
    };
    let desktop = cx.new(|cx| {
        Desktop::new(
            DesktopParts {
                app_id: PERSONAL_APP_ID,
                app_name: TITLE.into(),
                title: TITLE.into(),
                shell: parts,
                ringer: calls.and_then(|calls| calls.ringer),
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
    cx.set_global(Held {
        _desktop: desktop.clone(),
        _bridge: bridge.clone(),
    });
    quit_when_asked(&desktop, bridge.clone(), cx);
    let connecting = desktop.update(cx, |desktop, cx| desktop.connect(bus, cx));
    cx.spawn(async move |_: &mut AsyncApp| {
        for problem in connecting.await {
            eprintln!("teams: {problem}");
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

/// The models and the bridge holding `engine`. The models show calls when the window has them (`calls`: its test call
/// to the echo service, its calls' video on their feed); without them whatever the engine told of a call is left out.
pub fn bridge(engine: EngineHandle, calls: Option<&PersonalCalls>, cx: &mut App) -> (Models, Entity<Bridge>) {
    let models = match calls {
        Some(calls) => match calls.video.clone() {
            Some(feed) => Models::with_video_calls(Some(calls.echo.clone()), feed, cx),
            None => Models::with_calls(Some(calls.echo.clone()), cx),
        },
        None => Models::new(cx),
    };
    let core: Arc<dyn Core> = Arc::new(engine.requests());
    let drained = models.clone();
    let bridge = cx.new(|cx| Bridge::start(engine, core, drained, cx));
    (models, bridge)
}

/// The settings as an entity; when the calls carry video, with the switch for its codec (shown beside the attribution
/// its licence asks for), which `video` follows.
fn settings_entity(settings: SettingsModel, video: Option<VideoFeed>, cx: &mut App) -> Entity<SettingsModel> {
    let settings = match &video {
        Some(feed) => {
            let settings = settings.with_video_switch();
            feed.set_enabled(settings.settings().video_calls);
            settings
        }
        None => settings,
    };
    let settings = cx.new(|_| settings);
    if let Some(feed) = video {
        cx.observe(&settings, move |model, cx| {
            feed.set_enabled(model.read(cx).settings().video_calls);
        })
        .detach();
    }
    settings
}

/// Once `signed_out` says the session is signed out: shows [`SIGNED_OUT_HINT`], shuts the engine down (nothing more is
/// sent) and, once it is down, opens the sign-in window again (`again`), once.
pub fn watch_sign_out(
    mut signed_out: watch::Receiver<Option<SignedOut>>,
    bridge: WeakEntity<Bridge>,
    again: SignInAgain,
    cx: &mut App,
) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        // the session dropped without signing out: nothing to watch
        if signed_out.wait_for(Option::is_some).await.is_err() {
            return;
        }
        let Ok(stopped) = bridge.update(cx, |bridge, cx| {
            bridge.sign_out(SharedString::from(SIGNED_OUT_HINT), cx)
        }) else {
            return;
        };
        stopped.await;
        cx.update(|cx| again(cx));
    })
    .detach();
}

/// Once the desktop asks to quit: the engine shut down, then the app quits; the first ask does it.
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
