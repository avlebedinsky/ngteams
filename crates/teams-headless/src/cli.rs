//! The command line: `teams-headless <alias> <command>`.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use teams_fence::lab::Alias;
use teams_proto::ManualAvailability;

/// How long a one-shot write waits by default for a row that waits (a `Retry-After`) to settle, in seconds.
pub const DEFAULT_WAIT_SECS: u64 = 120;

/// How many conversations `list` and messages `show` print by default.
pub const DEFAULT_LIMIT: usize = 20;

#[derive(Debug, Parser)]
#[command(
    name = "teams-headless",
    about = "One lab account's native Teams client, headless: its writes go only through the fence (Fence::open_lab_account), \
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
    /// The other lab accounts' presence, and anyone's `--mri` names, read once from UPS (a read sent as a `POST`,
    /// which reaches nobody whoever it names); prints `presence <alias or MRI> <availability>` per person. Reads only;
    /// refused for more people, all told, than the engine watches at once (`teams_core::MAX_WATCHED`).
    Presence {
        /// Seconds to wait for the read.
        #[arg(long, default_value_t = 30)]
        wait: u64,
        /// Someone else to read too, by MRI (`8:orgid:…`); repeatable.
        #[arg(long = "mri", value_name = "MRI")]
        mris: Vec<String>,
    },
    /// People's names as the engine looks them up (Graph's user record, then the middle tier's profile lookup, a read
    /// sent as a `POST`, for a name Graph doesn't give), by MRI, whoever they are; prints `profile <alias or MRI>
    /// <source>` per person: where the name the engine has came from (`graph`, `short_profile`, `message`, `known`,
    /// or `none`), never the name. Reads only.
    Profile {
        /// Their MRIs (`8:orgid:…`).
        #[arg(required = true, value_name = "MRI")]
        mris: Vec<String>,
        /// Seconds to wait for the lookups.
        #[arg(long, default_value_t = 30)]
        wait: u64,
    },
    /// Sets the account's own availability from its push channel's endpoint (M2 Task 19): once the endpoint is
    /// registered, its presence `PUT` (available), then, if asked, a manual status and its reset, each counted in
    /// seconds from the `PUT`; ends `--hold` seconds after the `PUT` (or on SIGINT or SIGTERM), which takes the
    /// endpoint down. A write others see, under the fence's permit for the account's own availability (the user's
    /// exception of 2026-10-21); prints each step with its time.
    Availability {
        /// A manual status to set.
        #[arg(long, value_enum)]
        manual: Option<ManualArg>,
        /// Seconds after the `PUT` the manual status is set.
        #[arg(long, default_value_t = 60)]
        manual_after: u64,
        /// Seconds after the `PUT` the manual status is cleared.
        #[arg(long)]
        reset_after: Option<u64>,
        /// Seconds after the `PUT` the command ends.
        #[arg(long, default_value_t = 300)]
        hold: u64,
    },
    /// Listens on the push channel (Trouter) until SIGINT or SIGTERM, catching up on every connect; prints each event
    /// as it is received, with its receive time. Answers no delivery and no call.
    Watch {
        /// One JSON object per line.
        #[arg(long)]
        json: bool,
    },
    /// Places a call once the calling socket is registered, holds it `--seconds` once its media connected, then hangs
    /// it up: `echo` (the test call of lab.toml's echo service, which plays back what it hears), or a chat (`a`, `b`,
    /// `c`, `abc` or a conversation id; a catch-up first). Its media is files, never an audio server: the microphone
    /// plays a tone keyed on and off in 200 ms slots of a fixed pattern, the speaker records to `--record`. Prints each
    /// event, a summary (setup time, how it ended) and what the recording holds of the keyed tone at `--hear` (its
    /// level and round trips). A write others see, through the fence. Exits 0 only when the call connected, was held
    /// and hung up, and, for `echo`, the tone came back.
    Call {
        /// `echo`, `a`, `b`, `c`, `abc` or a conversation id.
        target: String,
        /// The microphone's tone, in Hz.
        #[arg(long, default_value_t = DEFAULT_TONE_HZ)]
        tone: f32,
        /// The keyed tone looked for in the recording, in Hz (default: `--tone`, which the echo plays back).
        #[arg(long)]
        hear: Option<f32>,
        /// Where the far end's audio is recorded (default: `<data_dir>/recordings/<alias>-call-<ms>.wav`, beside the
        /// tone sent, `…-sent.wav`).
        #[arg(long)]
        record: Option<PathBuf>,
        /// Seconds the call is held once its media connected (the echo plays what it heard back until about 41 s
        /// after that).
        #[arg(long, default_value_t = 45)]
        seconds: u64,
        /// Seconds the call may take to connect, from its placing.
        #[arg(long, default_value_t = 60)]
        wait: u64,
    },
}

/// The microphone's tone in a call by default, in Hz.
pub const DEFAULT_TONE_HZ: f32 = 1_000.0;

/// A manual status as the command line names it: the account menu's, as captured (M2 Task 18).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ManualArg {
    Busy,
    DoNotDisturb,
    Away,
    Available,
}

impl From<ManualArg> for ManualAvailability {
    fn from(arg: ManualArg) -> Self {
        match arg {
            ManualArg::Busy => Self::Busy,
            ManualArg::DoNotDisturb => Self::DoNotDisturb,
            ManualArg::Away => Self::Away,
            ManualArg::Available => Self::Available,
        }
    }
}
