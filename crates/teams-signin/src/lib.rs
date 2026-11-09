//! The app's sign-in window: Microsoft's sign-in page in a WPE `WebKit` view, drawn in GPUI.
//! [`hosts::SignInHosts`] binds every production window to Entra and the account's identity provider hosts.
//! [`egress::HostGate`] is its network session's only way out; [`guard`] decides and logs page requests without
//! bodies or credentials. The window catches the redirect and hands it directly to `teams_auth::signin` to verify
//! the request's state and redeem its code. There is no standalone sign-in binary or pipe protocol.
//!
//! `WebKit` follows some redirects without asking the page guard; the network host gate still applies to them.
//! The offline loopback window and test rules exist only with `test-support`.

pub mod classify;
mod diagnostics;
pub mod egress;
pub mod engine;
pub mod guard;
pub mod hosts;
pub mod input;
mod view;
pub mod window;

/// Whether offline test guards or window modes were compiled in. Production ngteams refuses such a build.
#[must_use]
pub const fn test_support_enabled() -> bool {
    cfg!(feature = "test-support")
}
