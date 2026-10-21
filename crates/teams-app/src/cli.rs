//! The command line: `teams-app --lab <alias> --secrets-fd <n> [--trace-fd <n>]` (the app) and
//! `teams-app secrets --lab <alias> --fd <n>` (its credential broker).

use std::os::fd::RawFd;

use clap::{Args, Parser, Subcommand};
use teams_fence::lab::Alias;

#[derive(Debug, Parser)]
#[command(
    name = "teams-app",
    about = "The lab's Teams app for one lab account, and its credential broker. Prints no token.",
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    #[command(flatten)]
    pub app: AppArgs,
}

/// The app: one lab account's window, in the UI sandbox only.
#[derive(Debug, Args)]
pub struct AppArgs {
    /// The lab account: a, b or c.
    #[arg(long = "lab", value_name = "ALIAS", required = true)]
    pub alias: Option<Alias>,
    /// The credential broker's socket (one end of a socket pair, the broker holding the other), handed to this process
    /// at its start (3 or above): the only way the app reaches the account's tokens.
    #[arg(long, value_name = "N", required = true)]
    pub secrets_fd: Option<RawFd>,
    /// Where the view trace goes: a descriptor open for writing, handed to this process at its start (3 or above).
    #[arg(long, value_name = "N")]
    pub trace_fd: Option<RawFd>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// The credential broker: serves get, put and delete of the account's own `teams-lab` Secret Service items on
    /// descriptor `--fd` (one end of a socket pair) and refuses any other account or application; exits when the
    /// socket closes. Run outside the UI sandbox; opens no window and no other desktop client.
    Secrets {
        /// The lab account: a, b or c.
        #[arg(long = "lab", value_name = "ALIAS")]
        alias: Alias,
        /// The socket's descriptor, handed to this process at its start (3 or above).
        #[arg(long, value_name = "N")]
        fd: RawFd,
    },
}

/// What the command line asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The app for `alias`, its broker's socket on `secrets_fd`, its view trace on `trace_fd`.
    App {
        alias: Alias,
        secrets_fd: RawFd,
        trace_fd: Option<RawFd>,
    },
    /// The credential broker for `alias` on `fd`.
    Secrets { alias: Alias, fd: RawFd },
}

impl Cli {
    /// What the command line asks for. The parser has made sure the app's arguments are there when no subcommand is.
    #[must_use]
    pub fn mode(&self) -> Option<Mode> {
        match (&self.command, self.app.alias, self.app.secrets_fd) {
            (Some(Command::Secrets { alias, fd }), _, _) => Some(Mode::Secrets { alias: *alias, fd: *fd }),
            (None, Some(alias), Some(secrets_fd)) => Some(Mode::App {
                alias,
                secrets_fd,
                trace_fd: self.app.trace_fd,
            }),
            (None, _, _) => None,
        }
    }
}
