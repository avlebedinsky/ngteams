//! `teams-app`: the lab's GPUI app for one lab account (from Task 6 of
//! `docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`), and its credential broker.
//!
//! The app runs in the UI sandbox, whose session bus is the sandbox's own, so it can't reach the Secret Service where
//! the lab's tokens are. `teams-app secrets --lab <alias> --fd <n>` ([`secrets`]) runs outside the sandbox, with the
//! user's bus, and serves that account's own `teams-lab` items, nothing else, on descriptor `n`, one end of a socket
//! pair; the app gets the other end and reads and stores its tokens through `teams_auth::broker::BrokerSecretStore`.
//!
//! [`cli`] is the command line; [`secrets`] the broker.

pub mod cli;
pub mod secrets;
