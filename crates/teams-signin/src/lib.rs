//! The app's guarded sign-in window: WPE `WebKit` drawn in GPUI on Linux, native `WebView2` on Windows.
//! [`hosts::SignInHosts`] binds every production window to Entra and the account's identity provider hosts.
//! [`egress::HostGate`] is its network session's only way out; [`guard`] decides and logs page requests without
//! bodies or credentials. The window catches the redirect and hands it directly to `teams_auth::signin` to verify
//! the request's state and redeem its code. There is no standalone sign-in binary or pipe protocol.
//!
//! `WebKit` follows some redirects without asking the page guard; the network host gate still applies to them.
//! Windows uses all-source request filters and response restrictions in a disposable native worker.
//! Offline loopback windows and synthetic TLS rules exist only with `test-support`.

#[cfg(any(windows, test))]
mod webview_policy;

pub mod classify;
mod diagnostics;
pub mod egress;
#[cfg(unix)]
pub mod engine;
pub mod guard;
pub mod hosts;
#[cfg(unix)]
pub mod input;
#[cfg(unix)]
mod view;
#[cfg(unix)]
pub mod window;
#[cfg(windows)]
#[path = "native/mod.rs"]
pub mod window;

/// Whether offline test guards or window modes were compiled in. Production ngteams refuses such a build.
#[must_use]
pub const fn test_support_enabled() -> bool {
    cfg!(feature = "test-support")
}
