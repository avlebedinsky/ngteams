//! `ngteams`: the personal build, the user's own Teams accounts (one or several, all running at once) on the user's
//! desktop (`docs/personal-build.md`, approved by the user on 2026-10-23; M2 Task 27 of
//! `docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`).
//!
//! - [`refusal`]: what the binary refuses before anything else, the window included: a build with `test-support`
//!   (any `cargo test`, `--all-targets` or `--all-features` build), and a build `scripts/install-personal.sh` didn't
//!   make (its install marker, read at compile time). Agents never produce a runnable personal binary.
//! - [`setup`]: its files (`~/.config/ngteams/accounts.toml`, the user's accounts in the order added;
//!   `~/.local/share/ngteams/`), kept apart from the lab's: a data dir overlapping the lab's and an account `lab.toml`
//!   names are refused. A single account's `account.toml` of an earlier build is moved into them.
//! - [`signin`]: one account's sign-in through the app's own sign-in window (`teams_signin::window`, bound to Entra's
//!   hosts and that account's identity provider hosts), the code redeemed only for that account (its object id and
//!   tenant once known), the refresh token stored under its UPN and [`KEYRING_APPLICATION`] on the desktop's Secret
//!   Service, its region from authz (`regions/<oid>.json`).
//! - [`engine`]: an account's engine (`teams_core::account`) under its personal fence (`Fence::open_personal`, which
//!   logs every send and denies none, so it reads no members), the account's own store at
//!   `~/.local/share/ngteams/store/<oid>.sqlite`; one per account.
//! - [`calling`]: the calls (`docs/personal-build.md`, "Calls"; M3 Task 22): the engine's calling on the desktop's
//!   PipeWire and camera portal, the window's ringtone, test call, video and devices, every call send logged by the
//!   personal fence, one call at a time across the accounts (the call floor).
//! - [`accounts`]: the accounts the app runs at once ([`accounts::Runner`]: each one's engine over its own fence on the
//!   app's call floor, its calls on the app's one set of devices), and the sign-in and forget the window runs to add
//!   and remove one.
//! - [`app`]: the window (`teams-ui`'s desktop with a shell per account, switched from the rail's avatars, the first
//!   run's screen of the writes that happen on their own, and the calls), the tray, adding and removing an account,
//!   and the sign-out: a session that can't renew its tokens shows "signed out", stops the
//!   engine (nothing more is sent) and opens the sign-in window again; a sign-in that goes through starts the app
//!   afresh.
//! - [`cli`]: `ngteams [--account <UPN>]` (without it, a window asks for the account before the first sign-in),
//!   `ngteams --forget <UPN>` ([`forget`]: one account, refused while its engine runs).
//!
//! - [`read_only`]: the read-only mode (`ngteams --read-only`), the only way agents run it on their own: on a snapshot
//!   ([`snapshot`], `ngteams snapshot --to <dir>`) in the UI sandbox only, its tokens through a `get`-only broker
//!   (`ngteams secrets`, its descriptors taken over by [`descriptors`]), sending nothing that writes, with no push
//!   channel and no calls (`scripts/run-personal-read-only.sh`).
//!
//! No broker and no sandbox but in the read-only mode: it is the user's own app. It never opens the lab's fence, data
//! dir or keyring items.

pub mod accounts;
pub mod app;
pub mod calling;
pub mod cli;
pub mod descriptors;
pub mod engine;
pub mod forget;
pub mod read_only;
pub mod refusal;
pub mod setup;
pub mod signin;
pub mod snapshot;

pub use refusal::refusal;

/// The `application` attribute of the personal build's items on the desktop's Secret Service (the lab's is
/// `teams-lab`): only this crate names it (`tests/apart.rs`).
pub const KEYRING_APPLICATION: &str = "ngteams";
