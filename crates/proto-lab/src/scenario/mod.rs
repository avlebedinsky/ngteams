//! Scenarios: scripted runs of the Teams web client in lab profiles under the armed fence (`proto-lab run`).
//!
//! - [`load`] (account a, fail-closed): opens the web client, waits for it to load and records the traffic. Every
//!   request the fence denies is expected and goes into the capture as data for mapping writes.
//! - [`ab_chat`] (accounts a and b, writes mapped, members read live): a sends a message in the a–b chat and b
//!   receives it; before that, a forced deny on one a–b message checks live that a denied write never leaves the
//!   browser.
//! - [`ab_chat_full`] (accounts a and b, as `ab-chat`): the a–b chat beyond a plain message, recorded: a sends, edits,
//!   reacts and takes the reaction back, mentions b (picked from the mention picker only once it is b), replies with a
//!   quote and reacts to it, and deletes; b reads (the mention among it) and reacts.
//! - [`ab_image`] (accounts a and b, as `ab-chat`, every media service write force-denied): what the web client sends
//!   when a sends an image in the a–b chat; recorded.
//! - [`abc_chat`] (accounts a, b and c, writes mapped, members read live): the group chat of a, b and c, created once
//!   (its id kept in the lab's `chats.toml`); messaging it (a sends, b and c receive, after a live enforcement test)
//!   is refused before anything opens until the fence can read a group chat's members in full.
//! - [`ab_trouter`] (accounts a and b, writes mapped, members read live, the push channel's own frames and the
//!   messaging endpoint's registration let out): b's Trouter session while a sends, edits and reacts in the a–b chat,
//!   recorded; `ab-trouter-quiet` holds back the client's own `user.activity` as well.
//! - [`echo_call`] (account a, writes mapped, members read live, fake microphone): a places the web client's test call
//!   to the echo service, the one approved participant outside the lab, stays in it a while and hangs up; recorded.
//!
//! The steps are in [`dsl`]. A run fails (`proto-lab run` exits non-zero) when its goal isn't reached, when the fence
//! was breached, and on any **unexpected** deny: what a scenario expects is its [`DenyPolicy`]. The writing scenarios
//! judge by the reviewed expected-deny list ([`expected`]), which never changes what the fence denies.

pub mod ab_chat;
pub mod ab_chat_full;
pub mod ab_image;
pub mod ab_trouter;
pub mod abc_chat;
pub mod dsl;
pub mod echo_call;
pub mod expected;
pub mod load;

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::ValueEnum;

use crate::browser::Display;
use crate::config::{Alias, LabPaths};
use crate::fence_http::Denial;
use crate::fence_ws::FrameDenial;
use crate::session::{FenceReport, LabSession, SessionError};

use self::dsl::StepError;
use self::expected::Review;

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
    /// Account a creates the a–b–c group chat (once); later runs: a messages it, b and c show it, after the same
    /// enforcement test.
    AbcChat,
    /// b's push channel while a sends, edits and reacts in the a–b chat; recorded.
    AbTrouter,
    /// As `ab-trouter`, with every `user.activity` of the client held back.
    AbTrouterQuiet,
    /// Account a places the test call to the echo service with a fake microphone playing a tone; recorded.
    EchoCall,
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
            Self::AbcChat => "abc-chat",
            Self::AbTrouter => "ab-trouter",
            Self::AbTrouterQuiet => "ab-trouter-quiet",
            Self::EchoCall => "echo-call",
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
}

/// Which denies a scenario expects; any other one fails the run.
#[derive(Clone, Debug)]
pub enum DenyPolicy {
    /// Every deny is expected: a fail-closed run that records what the client tries (`load`).
    AllExpected,
    /// The reviewed expected-deny list ([`expected`]), with the forced denies and the mapped targets of the scenario.
    Reviewed(Review),
}

impl DenyPolicy {
    /// Whether this policy expects `denial`.
    #[must_use]
    pub fn expects(&self, denial: &Denial) -> bool {
        match self {
            Self::AllExpected => true,
            Self::Reviewed(review) => review.expects(denial),
        }
    }

    /// The denies of `denials` this policy doesn't expect.
    #[must_use]
    pub fn unexpected(&self, denials: &[Denial]) -> Vec<Denial> {
        denials.iter().filter(|denial| !self.expects(denial)).cloned().collect()
    }

    /// Whether this policy expects the WebSocket frame deny `denial`.
    #[must_use]
    pub fn expects_frame(&self, denial: &FrameDenial) -> bool {
        match self {
            Self::AllExpected => true,
            Self::Reviewed(review) => review.expects_frame(denial),
        }
    }

    /// Whether this policy expects frame denies it can't see (counted, but not listed): only
    /// [`DenyPolicy::AllExpected`] does.
    #[must_use]
    pub fn expects_unseen_frames(&self) -> bool {
        matches!(self, Self::AllExpected)
    }
}

/// What became of one account's browser in a run.
#[derive(Debug)]
pub struct AccountRun {
    pub alias: Alias,
    pub fence: FenceReport,
    /// Every request the fence denied.
    pub denials: Vec<Denial>,
    /// Every WebSocket frame the fence denied.
    pub frame_denials: Vec<FrameDenial>,
    /// Where the run was recorded, if it was.
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

    /// Adds an account's run, judged by `policy`: a breach or an unexpected deny, of a request or a WebSocket frame,
    /// fails the scenario. A target contained before it ran ([`FenceReport::contained_unarmed`]) is a warning in the
    /// notes: the scenario fails on it only through a step that needed it, or because the browser was killed to
    /// contain it.
    pub fn add_account(&mut self, run: AccountRun, policy: &DenyPolicy) {
        let alias = run.alias;
        if run.fence.breached() {
            self.failures
                .push(format!("{alias}: the fence was breached ({})", run.fence.summary()));
        }
        // no breach: nothing of them ran; a step that needed one of them has failed the run already
        let contained = run.fence.contained_unarmed().count();
        if contained > 0 {
            self.notes.push(format!(
                "{alias}: warning: {contained} target(s) not armed in time were contained before they ran (no breach)"
            ));
        }
        if run.fence.browser_killed() {
            self.failures.push(format!(
                "{alias}: the browser was killed to contain a target not armed in time ({})",
                run.fence.summary()
            ));
        }
        for denial in policy.unexpected(&run.denials) {
            self.failures.push(format!(
                "{alias}: unexpected deny of {} {} ({:?})",
                denial.method, denial.url, denial.cause
            ));
        }
        for denial in run.frame_denials.iter().filter(|denial| !policy.expects_frame(denial)) {
            self.failures.push(format!(
                "{alias}: unexpected deny of a WebSocket frame to {} ({:?})",
                denial.url, denial.cause
            ));
        }
        // the report's count, taken at another moment, may hold frames the list doesn't: nothing says what they were
        let unlisted = run.fence.denied_frames.saturating_sub(run.frame_denials.len());
        if unlisted > 0 && !policy.expects_unseen_frames() {
            self.failures.push(format!(
                "{alias}: unexpected deny of {unlisted} more WebSocket frame(s)"
            ));
        }
        self.accounts.push(run);
    }

    /// Closes `session` and adds its run ([`Self::add_account`]), judged by `policy`. A close that fails (the browser
    /// had to be killed, the capture lost events) fails the run but doesn't end it: the report so far and the other
    /// accounts' runs are kept. The run is judged on what the fence decided once the close has waited for the decisions
    /// still running, so a deny that lands while the browser goes down counts too (and one still undecided when that
    /// wait ends fails the run through the close).
    pub async fn close_account(&mut self, alias: Alias, session: LabSession, policy: &DenyPolicy) {
        let decisions = session.decisions();
        let capture = session.capture_dir().map(Path::to_path_buf);
        if let Err(error) = session.close().await {
            self.failures.push(format!("{alias}: {error}"));
        }
        let fence = FenceReport::of(&decisions);
        self.notes.push(format!("{alias}: {}", fence.summary()));
        self.add_account(
            AccountRun {
                alias,
                fence,
                denials: decisions.denials(),
                frame_denials: decisions.frame_denials(),
                capture,
            },
            policy,
        );
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
/// When the scenario can't run at all (no Chrome, no fence, a browser that can't be armed); a run that ran but failed
/// is a report with failures.
pub async fn run(
    scenario: ScenarioName,
    paths: &LabPaths,
    options: RunOptions,
) -> Result<ScenarioReport, ScenarioError> {
    match scenario {
        ScenarioName::Load => load::run(paths, options).await,
        ScenarioName::AbChat => ab_chat::run(paths, options).await,
        ScenarioName::AbChatFull => ab_chat_full::run(paths, options).await,
        ScenarioName::AbImage => ab_image::run(paths, options).await,
        ScenarioName::AbcChat => abc_chat::run(paths, options).await,
        ScenarioName::AbTrouter => ab_trouter::run(paths, options, ab_trouter::Activity::Reported).await,
        ScenarioName::AbTrouterQuiet => ab_trouter::run(paths, options, ab_trouter::Activity::HeldBack).await,
        ScenarioName::EchoCall => echo_call::run(paths, options).await,
    }
}
