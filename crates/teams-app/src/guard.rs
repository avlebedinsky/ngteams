//! What the app refuses before it touches the lab: a build with `teams-fence`'s `test-support` (its fence API takes
//! any roster and any member list, its transports a mock origin) or its `personal` (the personal build's fence), and a
//! window anywhere but the UI sandbox.
//!
//! The app opens a window only on a Wayland compositor whose socket lies in the dir `UI_SANDBOX_DIR` names (the UI
//! sandbox sets both, `ui_sandbox::window_allowed`), and only with `DISPLAY` unset: GPUI's X11 backend, which the UI
//! kit turns on (Task 7), must have no display to fall back to.

use std::ffi::OsStr;

use ui_sandbox::{SANDBOX_ENV, in_sandbox, names_wayland_display};

/// Why a build with `test-support` doesn't run, or `None` for a normal build.
#[must_use]
pub fn test_support_refusal() -> Option<&'static str> {
    teams_fence::test_support_enabled().then_some(
        "built with teams-fence's test-support feature (cargo test, --all-targets or --all-features unify it in); \
         rebuild with `cargo build -p teams-app`",
    )
}

/// Why a build with the personal build's fence doesn't run (`teams-fence`'s `personal`, unified in by a build with
/// `teams`), or `None` for a lab build.
#[must_use]
pub fn personal_refusal() -> Option<&'static str> {
    teams_fence::personal_enabled().then_some(
        "built with teams-fence's personal feature (a build with `teams`, --workspace or --all-features unify it in); \
         rebuild with `cargo build -p teams-app`",
    )
}

/// Whether a window may open with this `WAYLAND_DISPLAY`, sandbox opt-in (`UI_SANDBOX_DIR`) and `DISPLAY`.
///
/// # Errors
/// Why not, as the message to print.
pub fn window_allowed(wayland: Option<&OsStr>, sandbox: Option<&OsStr>, display: Option<&OsStr>) -> Result<(), String> {
    if display.is_some_and(|display| !display.is_empty()) {
        return Err(
            "DISPLAY is set: refusing to start where GPUI could fall back to X11 (run in the UI sandbox)".into(),
        );
    }
    if !names_wayland_display(wayland) {
        return Err("no Wayland compositor: WAYLAND_DISPLAY is unset or empty".into());
    }
    if !in_sandbox(wayland, sandbox) {
        return Err(format!(
            "not the UI sandbox's compositor: WAYLAND_DISPLAY must be an absolute socket path inside the dir \
             {SANDBOX_ENV} names; refusing to open a window on the desktop session"
        ));
    }
    Ok(())
}

/// [`window_allowed`] for this process's environment.
///
/// # Errors
/// Why not.
pub fn window_allowed_here() -> Result<(), String> {
    window_allowed(
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        std::env::var_os(SANDBOX_ENV).as_deref(),
        std::env::var_os("DISPLAY").as_deref(),
    )
}
