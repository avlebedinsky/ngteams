//! The protocol lab harness: lab config, browser profiles, the armed browser fence, captures and scenarios. The
//! `proto-lab` binary is the CLI over this library.

pub mod accounts;
pub mod arming;
pub mod browser;
pub mod bundles;
pub mod capture;
pub mod cdp;
pub mod classify;
pub mod commands;
pub mod config;
pub mod fence_http;
pub mod fence_ws;
pub mod sanitize;
pub mod scenario;
pub mod session;
pub mod stop;

/// Whether this build includes the harness's test-only mock origins.
/// Normal binaries refuse such a build before reading credentials or opening browsers.
#[must_use]
pub const fn test_support_enabled() -> bool {
    cfg!(feature = "test-support")
}
