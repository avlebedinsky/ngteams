//! The protocol lab harness: lab config, browser profiles, captures and scenarios. The
//! `proto-lab` binary is the CLI over this library.

pub mod accounts;
mod action_targets;
pub mod arming;
pub mod browser;
#[cfg(unix)]
pub mod bundles;
pub mod capture;
pub mod capture_hold;
pub mod cdp;
pub mod classify;
pub mod commands;
pub mod config;
pub mod frame_hold;
pub mod protocol;
pub mod redirect_catch;
#[cfg(unix)]
pub mod sanitize;
#[cfg(unix)]
pub mod scenario;
pub mod session;
#[cfg(unix)]
pub mod stop;

/// Whether this build includes the harness's test-only mock origins.
/// Normal binaries refuse such a build before reading credentials or opening browsers.
#[must_use]
pub const fn test_support_enabled() -> bool {
    cfg!(feature = "test-support")
}

#[cfg(windows)]
#[path = "scenario/windows.rs"]
pub mod scenario;

#[cfg(windows)]
pub mod private;
