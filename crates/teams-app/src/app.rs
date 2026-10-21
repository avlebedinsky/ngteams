//! The app on GPUI: the bridge over the account's engine, the main window, the sign-out, the quit.
//!
//! [`launch`] runs inside GPUI's `run`: it hands the engine to a [`Bridge`], opens the main window over the bridge's
//! models with the view trace, watches the session for a sign-out ([`watch_sign_out`]) and, once the last window
//! closes, shuts the engine down (off the foreground, joining its thread) and quits.

use std::sync::Arc;

use gpui::{App, AppContext as _, AsyncApp, Entity, QuitMode, SharedString, WeakEntity, WindowHandle};
use teams_auth::session::SignedOut;
use teams_core::EngineError;
use teams_core::EngineHandle;
use teams_core::lab::LabError;
use teams_fence::lab::Alias;
use teams_ui::window::{self, LAB_APP_ID};
use teams_ui::{Bridge, Core, Models, Root, ViewTrace};
use tokio::sync::watch;

/// What the app starts with.
pub struct Launch {
    pub alias: Alias,
    pub engine: EngineHandle,
    /// The session's signed-out state ([`teams_auth::session::Session::watch_signed_out`]); `None` for an engine
    /// whose tokens can't run out (a test's).
    pub signed_out: Option<watch::Receiver<Option<SignedOut>>>,
    pub trace: ViewTrace,
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

/// Starts the app inside GPUI's `run` (see the module docs): the main window and the bridge holding the engine.
///
/// # Errors
/// The window can't be opened (the bridge is shut down then).
pub fn launch(cx: &mut App, launch: Launch) -> gpui::Result<(WindowHandle<Root>, Entity<Bridge>)> {
    let Launch {
        alias,
        engine,
        signed_out,
        trace,
    } = launch;
    // GPUI would quit on its own as the last window closes (`QuitMode::Default` on Linux), before the engine is shut
    // down: the app quits itself once it is (`quit_when_closed`)
    cx.set_quit_mode(QuitMode::Explicit);
    let models = Models::new(cx);
    let core: Arc<dyn Core> = Arc::new(engine.requests());
    let drained = models.clone();
    let bridge = cx.new(|cx| Bridge::start(engine, core, drained, cx));
    if let Some(signed_out) = signed_out {
        watch_sign_out(signed_out, bridge.downgrade(), sign_in_hint(alias), cx);
    }
    let window = match window::open(cx, LAB_APP_ID, title(alias), models, Arc::new(trace)) {
        Ok(window) => window,
        Err(error) => {
            bridge.update(cx, Bridge::shutdown).detach();
            return Err(error);
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
            cx.spawn(async move |cx: &mut AsyncApp| {
                stopped.await;
                cx.update(|cx| cx.quit());
            })
            .detach();
        }
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
