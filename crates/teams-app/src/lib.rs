//! `teams-app`: the lab's GPUI app for one lab account, and its credential broker.
//!
//! `teams-app --lab <alias> --secrets-fd <n> [--trace-fd <n>]` is the app ([`app`]): one lab account's window, built
//! as `teams-headless` builds its client (`teams_core::lab::start`: the fence from `Fence::open_lab`, the account's
//! `Session`, the region's chat base, the live member read with the chat transport's credential, the store at
//! `LabPaths::store_file`, Trouter), its tokens only through the credential broker's socket (`--secrets-fd`, a
//! `teams_auth::broker::BrokerSecretStore`; the app never opens the Secret Service itself and has no fallback). It
//! refuses a `test-support` build and a window anywhere but the UI sandbox ([`guard`]), reports another engine on the
//! account and exits, writes the view trace to `--trace-fd` ([`trace`]) and prints no token. When the account's
//! session can't renew its tokens, the window shows it signed out (run `proto-lab auth <alias>`) and the engine is
//! stopped, so nothing more is sent.
//!
//! The app runs in the UI sandbox, whose session bus is the sandbox's own, so it can't reach the Secret Service where
//! the lab's tokens are. `teams-app secrets --lab <alias> --fd <n>` ([`secrets`]) runs outside the sandbox, with the
//! user's bus, and serves that account's own `teams-lab` items, nothing else, on descriptor `n`, one end of a socket
//! pair; the app gets the other end.
//!
//! [`cli`] is the command line. M2 Tasks 4 and 6 (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`).

pub mod app;
pub mod cli;
pub mod guard;
pub mod secrets;
pub mod trace;
