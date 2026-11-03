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
/// group chat, from `<data_dir>/chats.toml`), a contact's name from `lab.toml` (or `contact` when there is one: the
/// account's 1:1 chat with it), or a conversation id as it is (`19:…`).
pub type ChatArg = String;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Creates or opens a chat with the named lab accounts. Only the dedicated aliases a, b and c are accepted.
    CreateChat {
        #[arg(required = true, num_args = 1..)]
        others: Vec<Alias>,
        #[arg(long, default_value_t = DEFAULT_WAIT_SECS)]
        wait: u64,
    },
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
    /// Reads what the team pages read of a team (`docs/protocol/teams.md`), each once; prints how much each gave, never a
    /// name.
    TeamReads {
        /// The team's thread.
        team: String,
        /// The team's group.
        group: String,
    },
    /// Proves the scheduling form's series and Scheduler with an appointment series of the account's own, which invites
    /// nobody (`series.rs`): creates a weekly series from tomorrow, reads it back and prints its recurrence, reads the
    /// account's free and busy times, then deletes the series (unless `--keep`; `--cleanup` deletes what a run left).
    Series {
        /// Leave the series in the calendar.
        #[arg(long)]
        keep: bool,
        /// Only delete the probe series an earlier run left in the calendar.
        #[arg(long)]
        cleanup: bool,
    },
    /// The account's Teams meetings from yesterday on (`meetings.rs`): each one's chat and whether its join link names
    /// its organizer; `--read` reads each one's chat's thread for the meeting it names; `--cancel <prefix>` cancels the
    /// account's own meetings whose subject starts with `prefix` (what a lab run made). Prints ids and flags only.
    Meetings {
        /// Read each meeting's chat's thread.
        #[arg(long)]
        read: bool,
        /// Cancel the account's own meetings whose subject starts with this.
        #[arg(long)]
        cancel: Option<String>,
    },
    /// Asks the service's search (Substrate, `docs/protocol/search.md`) for `query`, as the title bar's search does:
    /// its suggestions, then its results of every kind; prints `search <scope> <kind> <count>` per kind found, and the
    /// ids of the chats its messages are in, never a name or a text. A read, whoever its answer lists.
    Search {
        /// What to search for.
        query: String,
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
    /// as it is received, with its receive time. Answers no delivery and no call, unless `--answer-lab-calls`; registers
    /// the calling stack only with `--calls` or `--answer-lab-calls`.
    Watch {
        /// One JSON object per line. Not with `--calls` or `--answer-lab-calls`, whose recording's summary is text.
        #[arg(long, conflicts_with_all = ["answer_lab_calls", "calls"])]
        json: bool,
        /// Also registers the calling stack and answers each call the engine offers: only an invitation from a lab
        /// account or a contact naming only them (the lab fence offers no other; every other one rings unanswered and
        /// nothing is sent for it). The call's media is files, as `call`'s; at the end it prints what the (last)
        /// call's recording holds of the tone at `--hear`. A write others see, through the fence.
        #[arg(long)]
        answer_lab_calls: bool,
        /// Also registers the calling stack, answering nothing: an offered call rings here (printed) until its ring
        /// ends (answered on another endpoint, cancelled, timed out). Its media files are as `--answer-lab-calls`'s.
        #[arg(long, conflicts_with = "answer_lab_calls")]
        calls: bool,
        #[command(flatten)]
        audio: CallAudio,
    },
    /// Places a call once the calling socket is registered, holds it `--seconds` once its media connected, then hangs
    /// it up: `echo` (the test call of lab.toml's echo service, which plays back what it hears), or a chat (`a`, `b`,
    /// `c`, `abc`, a contact or a conversation id; a catch-up first). Its media is files, never an audio server: the microphone
    /// plays a tone keyed on and off in 200 ms slots of a fixed pattern, the speaker records to `--record`. Prints each
    /// event, a summary (setup time, how it ended) and what the recording holds of the keyed tone at `--hear` (its
    /// level and round trips). A write others see, through the fence. Exits 0 only when the call connected, was held
    /// and hung up, and, for `echo`, the tone came back.
    Call {
        /// `echo`, `a`, `b`, `c`, `abc`, a contact (`contact` or its name) or a conversation id.
        target: String,
        #[command(flatten)]
        audio: CallAudio,
        /// Seconds the call is held once its media connected (the echo plays what it heard back until about 41 s
        /// after that).
        #[arg(long, default_value_t = 45)]
        seconds: u64,
        /// Seconds the call may take to connect, from its placing.
        #[arg(long, default_value_t = 60)]
        wait: u64,
    },
    /// Joins the running call in a group chat late, as `call` places one (the same media, hold, hang-up and output):
    /// once the calling socket is registered it waits for an invitation to the chat's call to ring here, lets it ring
    /// unanswered, and once its ring ended joins the running call (the machine joins only a call an invitation named).
    /// A write others see, through the fence (a join is judged like a start in that chat).
    Join {
        /// `abc` or a group chat's conversation id.
        chat: ChatArg,
        #[command(flatten)]
        audio: CallAudio,
        /// Seconds the call is held once its media connected.
        #[arg(long, default_value_t = 45)]
        seconds: u64,
        /// Seconds the join may take to connect, from its placing.
        #[arg(long, default_value_t = 60)]
        wait: u64,
        /// Seconds the invitation may take to ring here, and then its ring to end.
        #[arg(long, default_value_t = 180)]
        ring_wait: u64,
    },
    /// Goes into a meeting's call by its join link, as the calendar's Join does (the same media, hold, hang-up and
    /// output): it starts the call when nobody has, and joins it when it runs. A write others see, through the fence
    /// (judged by the meeting's chat, read fresh).
    JoinMeeting {
        /// The meeting's join link (`https://teams.microsoft.com/l/meetup-join/…`).
        link: String,
        #[command(flatten)]
        audio: CallAudio,
        /// Seconds the call is held once its media connected.
        #[arg(long, default_value_t = 45)]
        seconds: u64,
        /// Seconds the call may take to connect, from its placing (and again from waiting in the meeting's lobby).
        #[arg(long, default_value_t = 60)]
        wait: u64,
        /// Lets in everyone the meeting's lobby tells waiting, as its organizer's Admit (each through the fence).
        #[arg(long)]
        admit: bool,
    },
    /// The account's teams as the engine holds them (a read starts when the stored list is old, or a team's thread it
    /// doesn't name comes), waiting up to `--wait` seconds for one holding `--channel`: each team's id and its channels'
    /// count, never a name. Reads only.
    Teams {
        /// A channel to wait for.
        #[arg(long)]
        channel: Option<String>,
        /// Seconds to wait.
        #[arg(long, default_value_t = 20)]
        wait: u64,
    },
    /// Schedules a Teams meeting in a channel, as its header's Schedule a meeting does (`channel_schedule.rs`): on the
    /// channel's team's group calendar from the next full hour for half an hour, posted to the channel; then reads
    /// back whether the account's calendar holds it and the channel's newest posts. Prints ids and kinds only.
    ChannelSchedule {
        /// The channel's thread id (`19:…@thread.tacv2`).
        channel: String,
        /// The meeting's subject (with `--cancel`, what the titles to cancel start with).
        subject: String,
        /// Cancels the meetings the channel's posts name whose title starts with the subject, instead of scheduling.
        #[arg(long)]
        cancel: bool,
        /// Renames the meetings the channel's posts name whose title starts with the subject to this, as their
        /// details' Send update does, instead of scheduling.
        #[arg(long, conflicts_with = "cancel")]
        rename: Option<String>,
    },
    /// Goes into a channel's meeting, as the channel header's Meet now and Join do (the same media, hold, hang-up and
    /// output): a new meeting in the channel, or with `--join` the one that runs there now, as CSA names it. A write
    /// others see, through the fence (judged by the channel's team, its members read fresh).
    ChannelMeeting {
        /// The channel's thread id (`19:…@thread.tacv2`).
        channel: String,
        #[command(flatten)]
        audio: CallAudio,
        /// Joins the meeting that runs in the channel instead of starting one.
        #[arg(long)]
        join: bool,
        /// The new meeting's subject, as its pre-join's title says it (Teams' `Meeting in "<channel>"` unless edited):
        /// the meeting's post is titled so once the service posted it.
        #[arg(long, conflicts_with = "join")]
        subject: Option<String>,
        /// Seconds the call is held once its media connected.
        #[arg(long, default_value_t = 45)]
        seconds: u64,
        /// Seconds the call may take to connect, from its placing.
        #[arg(long, default_value_t = 60)]
        wait: u64,
    },
}

/// A call's audio on files: the microphone's tone and how it is keyed, the tone looked for in the far end's recording,
/// and where that goes; or a movie as the microphone and the camera.
#[derive(Clone, Debug, PartialEq, clap::Args)]
#[command(group(clap::ArgGroup::new("takes_video").args(["video", "media"]).multiple(true)))]
pub struct CallAudio {
    /// The microphone's tone, in Hz.
    #[arg(long, default_value_t = DEFAULT_TONE_HZ)]
    pub tone: f32,
    /// The microphone plays the tone in bursts (1.6 s on, 0.9 s off), as a lab browser's fake microphone does and its
    /// page hears, instead of keyed in 200 ms slots of the fixed pattern.
    #[arg(long)]
    pub bursts: bool,
    /// A tone looked for in the recording, in Hz; repeatable, one report each (default: `--tone`, which the echo plays
    /// back).
    #[arg(long)]
    pub hear: Vec<f32>,
    /// Where the far end's audio is recorded (default: `<data_dir>/recordings/<alias>-<command>-<ms>.wav`, beside the
    /// tone sent, `…-sent.wav`).
    #[arg(long)]
    pub record: Option<PathBuf>,
    /// How long the microphone's tone lasts (silence after it), in seconds (default: `call`'s and `join`'s whole call;
    /// `watch`'s [`WATCH_TONE_SECONDS`]). A long call's tone kept short keeps its files and their analysis small.
    #[arg(long)]
    pub tone_seconds: Option<u64>,
    /// The call takes video (a 1:1 call's main line, a group call's main and receiving lines), on the OpenH264 built
    /// into the binary: the frames received are decoded and counted per line, printed every 2 s while a call placed or
    /// joined runs (`video_lines received=<line>:<n>,…`) and summarised at the end (`video …`, `video line=…`). The
    /// camera stays off unless `--camera-after`.
    #[arg(long)]
    pub video: bool,
    /// Turns the camera (the test pattern, frames carrying their index, or `--media`'s video) on this many seconds
    /// after the call's media connected (for `watch`, after an answered call's).
    #[arg(long, requires = "takes_video")]
    pub camera_after: Option<u64>,
    /// A movie file as the microphone and the camera instead of the tone and the test pattern, decoded by `ffmpeg`
    /// as the call runs (its first audio stream mixed down to mono, its first video stream at
    /// [`MEDIA_CAMERA`](crate::lab::MEDIA_CAMERA)'s size and frame rate). Implies `--video`; the camera goes on as
    /// soon as the media connected unless `--camera-after` says later. The tone isn't played, so nothing is looked
    /// for in the recording.
    #[arg(long, value_name = "FILE")]
    pub media: Option<PathBuf>,
    /// Where in the movie the call starts, in seconds.
    #[arg(long, value_name = "SECONDS", default_value_t = 0.0, requires = "media")]
    pub media_from: f64,
    /// Plays the movie in a loop (else once, then silence and its last picture).
    #[arg(long, requires = "media")]
    pub media_loop: bool,
}

impl CallAudio {
    /// Whether the call takes video: `--video`, or `--media`.
    #[must_use]
    pub fn takes_video(&self) -> bool {
        self.video || self.media.is_some()
    }

    /// When the camera goes on, in seconds after the media connected: `--camera-after`, or at once with `--media`.
    #[must_use]
    pub fn camera_after(&self) -> Option<u64> {
        self.camera_after.or_else(|| self.media.as_ref().map(|_| 0))
    }
}

/// How long `watch`'s tone lasts by default, in seconds.
pub const WATCH_TONE_SECONDS: u64 = 180;

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
