//! `teams`: the personal build, the user's own Teams account on the user's desktop (`docs/personal-build.md`, approved
//! by the user on 2026-10-23; M2 Task 27 of `docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`).
//!
//! - [`refusal`]: what both binaries refuse before anything else, the window included: a build with `test-support`
//!   (any `cargo test`, `--all-targets` or `--all-features` build), and a build `scripts/install-personal.sh` didn't
//!   make (its install marker, read at compile time). Agents never produce a runnable personal binary.
//! - [`setup`]: its files (`~/.config/teams/account.toml`, `~/.local/share/teams/`), kept apart from the lab's: a data
//!   dir overlapping the lab's and an account `lab.toml` names are refused.
//! - [`signin`]: the sign-in through `teams-signin-personal` (the window bound to Entra's and the identity provider's
//!   hosts), the code redeemed only for the account `account.toml` names (its object id and tenant once known), the
//!   refresh token stored under [`KEYRING_APPLICATION`] on the desktop's Secret Service, the region from authz.
//! - [`engine`]: the account's engine (`teams_core::account`) under the personal fence (`Fence::open_personal`) and its
//!   own membership source, the store at `~/.local/share/teams/store.sqlite`.
//! - [`app`]: the window (`teams-ui`'s shell with the first run's screen of the writes that happen on their own), the
//!   tray, and the sign-out: a session that can't renew its tokens shows "signed out", stops the engine (nothing more
//!   is sent) and opens the sign-in window again; a sign-in that goes through starts the app afresh.
//! - [`cli`]: `teams [--account <UPN>]`, `teams --forget`.
//!
//! No broker and no sandbox: it is the user's own app. It never opens the lab's fence, data dir or keyring items.

pub mod app;
pub mod cli;
pub mod engine;
pub mod refusal;
pub mod setup;
pub mod signin;

pub use refusal::refusal;

/// The `application` attribute of the personal build's items on the desktop's Secret Service (the lab's is
/// `teams-lab`): only this crate names it (`tests/apart.rs`).
pub const KEYRING_APPLICATION: &str = "teams-personal";
