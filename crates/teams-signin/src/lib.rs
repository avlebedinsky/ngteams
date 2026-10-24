//! `teams-signin`: the sign-in window (M2 Task 24). Microsoft's sign-in page in a `WebKitGTK` 6.0 webview, in a process
//! of its own (GTK's main loop apart from GPUI's), behind a request guard; it hands the redirect that carries the
//! authorization code to its parent, and `teams-auth` does the rest (`teams_auth::signin`: the redirect tied to the
//! request, the code redeemed only for the account the ID token names).
//!
//! - [`guard`]: every request a page sends is decided by the sign-in rules ([`classify`]: reads, the token endpoint,
//!   authz and Entra's sign-in form in their captured shapes, nothing else), and logged to `fence.log` before it goes;
//!   the redirect is caught, never let through.
//! - [`window`]: the webview set up behind the guard (`teams-signin-extension` in every web process), with what the
//!   guard can't see switched off.
//! - [`egress`]: the network session's only way off loopback, a proxy that refuses everything; for the personal
//!   sign-in window (`personal`), a host gate opening tunnels only to the hosts of its bound (`hosts`).
//! - [`cli`]: the binary's refusals (a `test-support` build, a window outside the UI sandbox, a page that isn't local)
//!   and its answer channel.
//!
//! **Not a complete fence** (decided with Codex, the plan's Task 24): a synchronous load's redirects and a kept-alive
//! load's redirects after its page is gone are followed by `WebKit`'s network process without reaching the guard. So
//! the binary opens only a local page ([`cli::NOT_LIVE`]) and the window stays on loopback for its whole run: the
//! guard denies, and the navigation policy refuses, everything off a loopback address, and the network session goes
//! anywhere else only through [`egress::BlackHole`], which refuses it. The window is tested offline, and the lab keeps
//! signing in through `proto-lab auth` (`docs/backlog/webkit-sign-in-guard-redirects.md`).
//!
//! **The personal sign-in window** (the `personal` feature, which only `crates/teams` enables, for its
//! `teams-signin-personal` binary; `docs/personal-build.md`, Choice C, approved on 2026-10-23): the same window, guard
//! and hardening, bound to Entra's sign-in hosts and the identity provider's hosts the user names (`hosts`) instead of
//! loopback: the guard denies every request off that bound (`Guard::hosts_only`), the navigation policy refuses every
//! navigation off it, and the network session's only way out is the host gate (`egress::HostGate`), which opens a
//! tunnel only to those hosts. The guard's known gaps stay (the two redirects, an unarmed later web process), accepted
//! for the user's own sign-in because they reach no Teams host. The lab's `teams-signin` binary refuses to run in a
//! build that has the feature.

pub mod classify;
pub mod cli;
pub mod egress;
pub mod guard;
#[cfg(feature = "personal")]
pub mod hosts;
pub mod window;
