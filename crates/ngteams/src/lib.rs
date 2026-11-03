//! The native Teams desktop client: account files, sign-in, engines, media and views.
//!
//! [`accounts_file`] owns account metadata, XDG paths and legacy file migration. [`setup`] chooses accounts,
//! [`signin`] performs host-gated sign-in, and [`engine`] starts one engine and store per account.
//! [`accounts`] shares one call floor and device set across the accounts shown by [`app`].
//!
//! [`refusal`] rejects test-support builds and binaries without the install marker before opening anything.
//! [`read_only`] runs snapshots in the UI sandbox with a get-only credential broker, no writes, calls or push.

pub mod accounts;
pub mod accounts_file;
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

/// The application's credential namespace on the desktop Secret Service; only this crate names it.
pub const KEYRING_APPLICATION: &str = "ngteams";
