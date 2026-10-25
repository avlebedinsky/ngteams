//! `teams-headless`: one lab account's native Teams client from the command line, the M1 core (`teams-core`) without a
//! UI. `teams-headless <alias> sync|list|show|send|edit|delete|react|read|watch|presence|profile|availability|call`.
//!
//! - The fence is [`teams_fence::Fence::open_lab_account`]'s: the roster from `lab.toml` (and the account's own MRI,
//!   whose own availability alone it permits), every decision in the lab's
//!   `fence.log`, which every account's process appends to at once (under the file's lock); every write goes out
//!   through the engine's outbox under a fresh `Fence::check`, its target's members read live.
//! - The tokens come from the Secret Service (`teams-lab`), where `proto-lab auth` stored them, through the account's
//!   session; nothing printed carries one.
//! - The store is the account's `<data_dir>/store/<alias>.sqlite`, outside the repo.
//! - A build with `teams-fence`'s `test-support` feature (which every mock origin turns on; `cargo test` unifies it
//!   in) refuses to run: the binary is built with `cargo build -p teams-headless`.
//!
//! [`commands`] (and [`availability`], the account's own availability, and [`call`], one call with its media on the
//! file backend, its tone keyed and found again by [`echo`]) run on any engine and print to any writer;
//! [`lab`] builds the lab's engine; [`print`] is the output;
//! [`debug_log`] is an opt-in diagnostic log on stderr (`TEAMS_HEADLESS_LOG=debug`).
//!
//! M1 Task 19 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`).

pub mod availability;
pub mod call;
pub mod chat;
pub mod cli;
pub mod commands;
pub mod debug_log;
pub mod echo;
pub mod lab;
pub mod print;
