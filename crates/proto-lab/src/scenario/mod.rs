//! Browser capture scenarios and their reports. A run records setup/shutdown failures and failures of its actual steps; explicit capture holds are observations rather than authorization denials.

pub mod ab_call;
pub mod ab_chat;
pub mod ab_chat_full;
pub mod ab_file;
pub mod ab_image;
pub mod ab_meeting;
pub mod ab_meeting_join;
pub mod ab_trouter;
pub mod abc_call;
pub mod abc_chat;
pub mod abc_everyone;
pub mod channel_create;
pub mod chat_shapes;
pub mod dsl;
pub mod echo_call;
pub mod load;
pub mod meeting_steps;
pub mod own_presence;
pub mod search;
pub mod team_manage;

#[cfg(test)]
#[path = "../../tests/support/scenario/test_page.rs"]
pub(crate) mod test_page;

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::ValueEnum;

use crate::browser::Display;
use crate::capture_hold::HeldRequest;
use crate::config::{Alias, LabPaths};
use crate::session::{LabSession, SessionError, SessionReport};

use self::dsl::StepError;

/// The scenarios `proto-lab run` knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ScenarioName {
    /// Account a opens the web client under the fail-closed fence; the traffic is recorded.
    Load,
    /// Account a sends a message in the a–b chat and b receives it, after a live enforcement test.
    AbChat,
    /// Accounts a and b in the a–b chat beyond a plain message (an edit, reactions, a quoted reply, a delete, b's read
    /// and reaction), after the same enforcement test; recorded.
    AbChatFull,
    /// Account a sends a made-up image in the a–b chat with every write to the media service denied, after the same
    /// enforcement test; recorded.
    AbImage,
    /// Account a sends a made-up file in the a–b chat with every write of it denied, after the same enforcement test;
    /// recorded.
    AbFile,
    /// Account a creates the a–b–c group chat (once); later runs: a messages it, b and c show it, after the same
    /// enforcement test.
    AbcChat,
    /// b's push channel while a sends, edits and reacts in the a–b chat; recorded.
    AbTrouter,
    /// As `ab-trouter`, with every `user.activity` of the client held back.
    AbTrouterQuiet,
    /// Accounts a and b, as `ab-trouter`: every quick reaction, typing and its indicator, a message with line breaks
    /// and a link, and a mention of b in the group chat; recorded.
    ChatShapes,
    /// Accounts a and b in the a–b–c group chat: a mentions everyone, first held back by a capture hold and read, then
    /// sent once the lab rule reads it as a message to the chat adding nobody; b shows it; recorded.
    AbcEveryone,
    /// Account a places the test call to the echo service with a fake microphone playing a tone; recorded.
    EchoCall,
    /// Account a calls b from their 1:1 chat, as the run's step (`--call`) says; recorded.
    AbCall,
    /// Account a calls the a–b–c group chat, as the run's step (`--call`) says; recorded.
    AbcCall,
    /// Account a schedules a Teams meeting with b, changes its title and cancels it, after the same enforcement test;
    /// recorded.
    AbMeeting,
    /// Account a goes into a meeting nobody started, from its pre-join, after the same enforcement test; what follows
    /// the pre-join's subscription held back; recorded.
    AbMeetingJoin,
    /// Account a searches from the title bar: the suggestions, the results and their tabs; recorded.
    Search,
    /// Account a fills the web client's "Create a channel" dialog for each channel type and layout, every write that
    /// could make a channel held back; recorded.
    ChannelCreate,
    /// One account's page driven step by step over a socket (`PROTO_LAB_STEPS_SOCKET`), to look at its team management:
    /// every write on a team held back, or with `PROTO_LAB_TEAM_WRITES=1` only the lab rule's; recorded.
    TeamManage,
    /// One account's page in a meeting, driven step by step over a socket (`PROTO_LAB_STEPS_SOCKET`), after the
    /// enforcement test; every write judged by the lab rule; recorded.
    MeetingSteps,
    /// Account a's availability reported by the `user.activity` frame alone (the presence endpoint denied); recorded.
    OwnPresenceActivity,
    /// Account a's availability reported by the presence endpoint alone (every `user.activity` held back); recorded.
    OwnPresenceEndpoint,
    /// Account a's availability reported by both; recorded.
    OwnPresenceBoth,
    /// As `own-presence-both`, and a sets its status by hand with each request denied, to capture it.
    OwnPresenceManualHeld,
    /// As `own-presence-both`, and a sets its status by hand, the mapped request let through.
    OwnPresenceManual,
}

impl ScenarioName {
    /// The name, as on the command line and in the captures dir.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::AbChat => "ab-chat",
            Self::AbChatFull => "ab-chat-full",
            Self::AbImage => "ab-image",
            Self::AbFile => "ab-file",
            Self::AbcChat => "abc-chat",
            Self::AbTrouter => "ab-trouter",
            Self::AbTrouterQuiet => "ab-trouter-quiet",
            Self::ChatShapes => "chat-shapes",
            Self::AbcEveryone => "abc-everyone",
            Self::EchoCall => "echo-call",
            Self::AbCall => "ab-call",
            Self::AbcCall => "abc-call",
            Self::AbMeeting => "ab-meeting",
            Self::AbMeetingJoin => "ab-meeting-join",
            Self::Search => "search",
            Self::ChannelCreate => "channel-create",
            Self::TeamManage => "team-manage",
            Self::MeetingSteps => "meeting-steps",
            Self::OwnPresenceActivity => "own-presence-activity",
            Self::OwnPresenceEndpoint => "own-presence-endpoint",
            Self::OwnPresenceBoth => "own-presence-both",
            Self::OwnPresenceManualHeld => "own-presence-manual-held",
            Self::OwnPresenceManual => "own-presence-manual",
        }
    }
}

impl fmt::Display for ScenarioName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a scenario runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunOptions {
    pub display: Display,
    /// How long the web client gets to load, per account.
    pub load_timeout: Duration,
    /// What a call scenario (`ab-call`, `abc-call`) does; the others don't read it.
    pub call: ab_call::CallStep,
}

/// One account's capture in a scenario.
#[derive(Debug)]
pub struct AccountRun {
    pub alias: Alias,
    pub browser: SessionReport,
    pub held_requests: Vec<HeldRequest>,
    pub capture: Option<PathBuf>,
}

/// What a scenario run did, and whether it failed.
#[derive(Debug)]
pub struct ScenarioReport {
    pub scenario: ScenarioName,
    pub accounts: Vec<AccountRun>,
    /// Lines for the operator: what happened, step by step.
    pub notes: Vec<String>,
    /// Why the run failed (its goal not reached, a breach, an unexpected deny), if it did.
    pub failures: Vec<String>,
}

impl ScenarioReport {
    #[must_use]
    pub fn new(scenario: ScenarioName) -> Self {
        Self {
            scenario,
            accounts: Vec::new(),
            notes: Vec::new(),
            failures: Vec::new(),
        }
    }

    /// Whether the run failed.
    #[must_use]
    pub fn failed(&self) -> bool {
        !self.failures.is_empty()
    }

    /// Records capture setup failures alongside the scenario's own steps.
    pub fn add_account(&mut self, run: AccountRun) {
        for failure in &run.browser.failures {
            self.failures.push(format!("{}: {failure}", run.alias));
        }
        self.accounts.push(run);
    }
    /// Closes a session and preserves its capture even when a scenario step failed.
    pub async fn close_account(&mut self, alias: Alias, session: LabSession) {
        let status = session.status();
        let capture = session.capture_dir().map(Path::to_path_buf);
        if let Err(error) = session.close().await {
            self.failures.push(format!("{alias}: {error}"));
        }
        self.add_account(AccountRun {
            alias,
            browser: SessionReport::of(&status),
            held_requests: status.held_requests(),
            capture,
        });
    }
}

/// Why a scenario couldn't run at all.
#[derive(Debug, thiserror::Error)]
pub enum ScenarioError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Step(#[from] StepError),
    #[error("can't read the lab config: {0}")]
    Config(#[from] crate::config::ConfigError),
    #[error("{0}")]
    Setup(String),
}

/// Runs one scenario.
///
/// # Errors
/// When the scenario can't run at all (no Chrome, a browser whose capture setup fails); a run that ran but failed
/// is a report with failures.
pub async fn run(
    scenario: ScenarioName,
    paths: &LabPaths,
    options: RunOptions,
) -> Result<ScenarioReport, ScenarioError> {
    match scenario {
        ScenarioName::EchoCall | ScenarioName::AbCall | ScenarioName::AbcCall => {
            run_call(scenario, paths, options).await
        }
        ScenarioName::OwnPresenceActivity
        | ScenarioName::OwnPresenceEndpoint
        | ScenarioName::OwnPresenceBoth
        | ScenarioName::OwnPresenceManualHeld
        | ScenarioName::OwnPresenceManual => own_presence::run_named(paths, options, scenario).await,
        ScenarioName::ChannelCreate => channel_create::run(paths, options).await,
        ScenarioName::TeamManage => team_manage::run(paths, options).await,
        ScenarioName::MeetingSteps => meeting_steps::run(paths, options).await,
        _ => run_other(scenario, paths, options).await,
    }
}

/// Runs one of the scenarios that are neither a call nor own presence ([`run`]).
async fn run_other(
    scenario: ScenarioName,
    paths: &LabPaths,
    options: RunOptions,
) -> Result<ScenarioReport, ScenarioError> {
    match scenario {
        ScenarioName::Load => load::run(paths, options).await,
        ScenarioName::AbChat => ab_chat::run(paths, options).await,
        ScenarioName::AbChatFull => ab_chat_full::run(paths, options).await,
        ScenarioName::AbImage => ab_image::run(paths, options).await,
        ScenarioName::AbFile => ab_file::run(paths, options).await,
        ScenarioName::AbcChat => abc_chat::run(paths, options).await,
        ScenarioName::AbTrouter => ab_trouter::run(paths, options, ab_trouter::Activity::Reported).await,
        ScenarioName::AbTrouterQuiet => ab_trouter::run(paths, options, ab_trouter::Activity::HeldBack).await,
        ScenarioName::ChatShapes => chat_shapes::run(paths, options).await,
        ScenarioName::AbcEveryone => abc_everyone::run(paths, options).await,
        ScenarioName::AbMeeting => ab_meeting::run(paths, options).await,
        ScenarioName::AbMeetingJoin => ab_meeting_join::run(paths, options).await,
        ScenarioName::Search => search::run(paths, options).await,
        // a call or own presence: `run` sends those to `run_call` and `own_presence`
        _ => Err(ScenarioError::Setup(format!(
            "{scenario:?} isn't a scenario of `run_other`"
        ))),
    }
}

/// Runs one of the call scenarios ([`run`]).
async fn run_call(
    scenario: ScenarioName,
    paths: &LabPaths,
    options: RunOptions,
) -> Result<ScenarioReport, ScenarioError> {
    match scenario {
        ScenarioName::EchoCall => echo_call::run(paths, options).await,
        ScenarioName::AbCall => ab_call::run(paths, options).await,
        ScenarioName::AbcCall => abc_call::run(paths, options).await,
        _ => Err(ScenarioError::Setup(format!("{scenario:?} is not a call scenario"))),
    }
}
