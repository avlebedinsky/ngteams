//! `teams-headless`: one lab account's native Teams client from the command line, the M1 core (`teams-core`) without a
//! UI. `teams-headless <alias> sync|list|show|send|edit|delete|react|watch`.
//!
//! - The fence is [`teams_fence::Fence::open_lab`]'s: the roster from `lab.toml`, every decision in the lab's
//!   `fence.log`, which every account's process appends to at once (under the file's lock); every write goes out
//!   through the engine's outbox under a fresh `Fence::check`, its target's members read live.
//! - The tokens come from the Secret Service (`teams-lab`), where `proto-lab auth` stored them, through the account's
//!   session; nothing printed carries one.
//! - The store is the account's `<data_dir>/store/<alias>.sqlite`, outside the repo.
//! - A build with `teams-fence`'s `test-support` feature (which every mock origin turns on; `cargo test` unifies it
//!   in) refuses to run: the binary is built with `cargo build -p teams-headless`.
//!
//! [`commands`] run on any engine and print to any writer; [`lab`] builds the lab's engine; [`print`] is the output.
//!
//! M1 Task 19 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`).

pub mod chat;
pub mod cli;
pub mod commands;
pub mod lab;
pub mod print;
