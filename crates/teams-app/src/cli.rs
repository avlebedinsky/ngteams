//! The command line: `teams-app --lab <alias> --secrets-fd <n> [--lab <alias> --secrets-fd <n> ...] [--trace-fd <n>]`
//! (the app, one window over one or more lab accounts) and `teams-app secrets --lab <alias> --fd <n>` (one account's
//! credential broker).
//!
//! The app's `--lab` and `--secrets-fd` are paired in order: the n-th `--secrets-fd` is the socket of the n-th
//! `--lab`'s broker, and the first `--lab` is the app's first account. A count that doesn't match, an alias given
//! twice and a descriptor named twice (among the brokers' sockets and the trace) are refused before any descriptor is
//! taken over ([`ModeError`]).

use std::collections::BTreeSet;
use std::os::fd::RawFd;

use clap::{ArgAction, Args, Parser, Subcommand};
use teams_fence::lab::Alias;

#[derive(Debug, Parser)]
#[command(
    name = "teams-app",
    about = "The lab's Teams app for one or more lab accounts in one window, and its credential broker. Prints no \
             token.",
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    #[command(flatten)]
    pub app: AppArgs,
}

/// The app: one window over one or more lab accounts, in the UI sandbox only.
#[derive(Debug, Args)]
pub struct AppArgs {
    /// A lab account: a, b or c. Given again for each further account the window runs; the first is the app's first.
    #[arg(long = "lab", value_name = "ALIAS", required = true, action = ArgAction::Append)]
    pub aliases: Vec<Alias>,
    /// The credential broker's socket of the `--lab` in the same place (one end of a socket pair, the broker holding
    /// the other), handed to this process at its start (3 or above): the only way the app reaches that account's
    /// tokens. One per `--lab`, in their order.
    #[arg(long, value_name = "N", required = true, action = ArgAction::Append)]
    pub secrets_fd: Vec<RawFd>,
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The app for `accounts` (each alias with its broker's socket, the first account first), its view trace on
    /// `trace_fd`.
    App {
        accounts: Vec<(Alias, RawFd)>,
        trace_fd: Option<RawFd>,
    },
    /// The credential broker for `alias` on `fd`.
    Secrets { alias: Alias, fd: RawFd },
}

/// A command line the parser took but the app refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ModeError {
    /// The app's arguments are missing: the parser requires them when no subcommand is given, so a parsed command line
    /// never has this.
    #[error("give --lab and --secrets-fd, or a subcommand")]
    Incomplete,
    /// The `--lab`s and the `--secrets-fd`s don't pair up: each account needs its own broker's socket.
    #[error("{labs} --lab but {sockets} --secrets-fd: each --lab needs the --secrets-fd of its own broker, in order")]
    Unpaired { labs: usize, sockets: usize },
    /// An account given twice: it would run two engines, which its engine lock refuses.
    #[error("--lab {0} is given twice: each account runs once")]
    RepeatedAccount(Alias),
    /// Two of the brokers' sockets and the view trace name the same descriptor: each is taken over as a descriptor of
    /// its own, and trace lines (or another account's requests) would go into a broker's protocol.
    #[error("descriptor {0} is named twice among --secrets-fd and --trace-fd: each needs a descriptor of its own")]
    SharedDescriptor(RawFd),
}

impl Cli {
    /// What the command line asks for.
    ///
    /// # Errors
    /// See [`ModeError`].
    pub fn mode(&self) -> Result<Mode, ModeError> {
        if let Some(Command::Secrets { alias, fd }) = &self.command {
            return Ok(Mode::Secrets { alias: *alias, fd: *fd });
        }
        let AppArgs {
            aliases,
            secrets_fd,
            trace_fd,
        } = &self.app;
        if aliases.is_empty() || secrets_fd.is_empty() {
            return Err(ModeError::Incomplete);
        }
        if aliases.len() != secrets_fd.len() {
            return Err(ModeError::Unpaired {
                labs: aliases.len(),
                sockets: secrets_fd.len(),
            });
        }
        let mut seen = BTreeSet::new();
        if let Some(repeated) = aliases.iter().find(|alias| !seen.insert(**alias)) {
            return Err(ModeError::RepeatedAccount(*repeated));
        }
        let mut named = BTreeSet::new();
        if let Some(shared) = secrets_fd.iter().chain(trace_fd).find(|fd| !named.insert(**fd)) {
            return Err(ModeError::SharedDescriptor(*shared));
        }
        Ok(Mode::App {
            accounts: aliases.iter().copied().zip(secrets_fd.iter().copied()).collect(),
            trace_fd: *trace_fd,
        })
    }
}
