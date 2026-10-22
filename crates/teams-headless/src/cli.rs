//! The command line: `teams-headless <alias> <command>`.

use clap::{Parser, Subcommand};
use teams_fence::lab::Alias;

/// How long a one-shot write waits by default for a row that waits (a `Retry-After`) to settle, in seconds.
pub const DEFAULT_WAIT_SECS: u64 = 120;

/// How many conversations `list` and messages `show` print by default.
pub const DEFAULT_LIMIT: usize = 20;

#[derive(Debug, Parser)]
#[command(
    name = "teams-headless",
    about = "One lab account's native Teams client, headless: its writes go only through the fence (Fence::open_lab), \
             its tokens come from the Secret Service (`proto-lab auth` stored them), its store is \
             <data_dir>/store/<alias>.sqlite. Prints no token."
)]
pub struct Cli {
    /// The lab account: a, b or c.
    pub alias: Alias,
    #[command(subcommand)]
    pub command: Command,
}

/// A chat, as a command names it: `a`, `b` or `c` (the account's 1:1 chat with that lab account), `abc` (the lab's
/// group chat, from `<data_dir>/chats.toml`), or a conversation id as it is (`19:…`).
pub type ChatArg = String;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Catch up: the conversation list and every changed conversation's messages, into the store. Reads only.
    Sync,
    /// The stored conversations, the latest activity first. Reads the store only; nothing goes out.
    List {
        /// How many.
        #[arg(long, default_value_t = DEFAULT_LIMIT)]
        limit: usize,
    },
    /// A chat's newest stored messages, the oldest of them first, with their reactions. Reads the store only.
    Show {
        /// `a`, `b`, `c`, `abc` or a conversation id.
        chat: ChatArg,
        /// How many messages.
        #[arg(long, default_value_t = DEFAULT_LIMIT)]
        limit: usize,
    },
    /// Sends a message (plain text) after a catch-up; prints its start time, its `OriginalArrivalTime` and its id.
    Send {
        /// `a`, `b`, `c`, `abc` or a conversation id.
        chat: ChatArg,
        text: String,
        /// Seconds to wait for a write that waits (a `Retry-After`) to settle.
        #[arg(long, default_value_t = DEFAULT_WAIT_SECS)]
        wait: u64,
    },
    /// Edits one of the account's messages (its text now plain `text`), after a catch-up.
    Edit {
        /// `a`, `b`, `c`, `abc` or a conversation id.
        chat: ChatArg,
        /// The message's id.
        message: String,
        text: String,
        /// Seconds to wait for a write that waits to settle.
        #[arg(long, default_value_t = DEFAULT_WAIT_SECS)]
        wait: u64,
    },
    /// Deletes one of the account's messages, after a catch-up.
    Delete {
        /// `a`, `b`, `c`, `abc` or a conversation id.
        chat: ChatArg,
        /// The message's id.
        message: String,
        /// Seconds to wait for a write that waits to settle.
        #[arg(long, default_value_t = DEFAULT_WAIT_SECS)]
        wait: u64,
    },
    /// Reacts to a message (`like`, `heart`, …), or takes the reaction back with `--remove`, after a catch-up.
    React {
        /// `a`, `b`, `c`, `abc` or a conversation id.
        chat: ChatArg,
        /// The message's id.
        message: String,
        /// The reaction's key.
        key: String,
        /// Take the account's reaction back instead.
        #[arg(long)]
        remove: bool,
        /// Seconds to wait for a write that waits to settle.
        #[arg(long, default_value_t = DEFAULT_WAIT_SECS)]
        wait: u64,
    },
    /// Moves the account's read horizon in a chat up to a message (a read receipt, which the chat's members may see),
    /// after a catch-up.
    Read {
        /// `a`, `b`, `c`, `abc` or a conversation id.
        chat: ChatArg,
        /// The message's id.
        message: String,
        /// Seconds to wait for a write that waits to settle.
        #[arg(long, default_value_t = DEFAULT_WAIT_SECS)]
        wait: u64,
    },
    /// The other lab accounts' presence, read once from UPS (a read by `POST` under the fence's permit, lab accounts
    /// only); prints `presence <alias> <availability>` per account. Reads only.
    Presence {
        /// Seconds to wait for the read.
        #[arg(long, default_value_t = 30)]
        wait: u64,
    },
    /// Listens on the push channel (Trouter) until SIGINT or SIGTERM, catching up on every connect; prints each event
    /// as it is received, with its receive time. Answers no delivery and no call.
    Watch {
        /// One JSON object per line.
        #[arg(long)]
        json: bool,
    },
}
