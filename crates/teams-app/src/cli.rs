//! The command line: `teams-app secrets --lab <alias> --fd <n>`.

use std::os::fd::RawFd;

use clap::{Parser, Subcommand};
use teams_fence::lab::Alias;

#[derive(Debug, Parser)]
#[command(
    name = "teams-app",
    about = "The lab's Teams app for one lab account, and its credential broker. Prints no token."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
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
