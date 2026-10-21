//! `teams-signin`: the sign-in window (M2 Task 24). Microsoft's sign-in page in a `WebKitGTK` 6.0 webview, in a process
//! of its own (GTK's main loop apart from GPUI's), behind a request guard; it hands the redirect that carries the
//! authorization code to its parent, and `teams-auth` does the rest (`teams_auth::signin`: the redirect tied to the
//! request, the code redeemed only for the account the ID token names).
//!
//! - [`guard`]: every request a page sends is decided as proto-lab's `sign_in` classifier decides it for Chrome, and
//!   logged to `fence.log` before it goes; the redirect is caught, never let through.
//! - [`window`]: the webview set up behind the guard (`teams-signin-extension` in every web process), with what the
//!   guard can't see switched off.
//! - [`cli`]: the binary's refusals (a `test-support` build, a window outside the UI sandbox, a page that isn't local)
//!   and its answer channel.
//!
//! **Not a complete fence** (decided with Codex, the plan's Task 24): a synchronous load's redirects and a kept-alive
//! load's redirects after its page is gone are followed by `WebKit`'s network process without reaching the guard. So
//! the binary opens only a local page ([`cli::NOT_LIVE`]): the window is tested offline, and the lab keeps signing in
//! through `proto-lab auth` (`docs/backlog/webkit-sign-in-guard-redirects.md`).

pub mod cli;
pub mod guard;
pub mod window;
