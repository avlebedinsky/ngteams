//! Scenarios: scripted runs of the Teams web client in lab profiles under the armed fence (`proto-lab run`).
//!
//! - [`load`] (account a, fail-closed): opens the web client, waits for it to load and records the traffic. Every
//!   request the fence denies is expected and goes into the capture as data for mapping writes.
//! - [`ab_chat`] (accounts a and b, writes mapped, members read live): a sends a message in the a–b chat and b
//!   receives it; before that, a forced deny on one a–b message checks live that a denied write never leaves the
//!   browser.
//!
//! The steps are in [`dsl`]. A run fails (`proto-lab run` exits non-zero) when its goal isn't reached, when the fence
//! was breached, and on any **unexpected** deny: what a scenario expects is its [`DenyPolicy`].

pub mod ab_chat;
pub mod dsl;
pub mod load;

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::ValueEnum;
use url::Url;

use crate::browser::Display;
use crate::config::{Alias, LabPaths};
use crate::fence_http::{Denial, DenyCause};
use crate::fence_ws::FrameDenial;
use crate::session::{FenceReport, LabSession, SessionError};

use self::dsl::StepError;

/// The scenarios `proto-lab run` knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ScenarioName {
    /// Account a opens the web client under the fail-closed fence; the traffic is recorded.
    Load,
    /// Account a sends a message in the a–b chat and b receives it, after a live enforcement test.
    AbChat,
}

impl ScenarioName {
    /// The name, as on the command line and in the captures dir.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::AbChat => "ab-chat",
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyPolicy {
    /// Every deny is expected: a fail-closed run that records what the client tries (`load`).
    AllExpected,
    /// Only the denies the run forced, and requests to these host suffixes (matched on a dot boundary): telemetry,
    /// which no scenario needs and the fence keeps denying. No WebSocket frame deny is expected: a run forces denies
    /// on requests only, and telemetry goes out as posts.
    ForcedAnd(&'static [&'static str]),
}

/// The telemetry hosts the web client posts to, whose denies a writing run expects.
pub const TELEMETRY_HOSTS: [&str; 1] = ["events.data.microsoft.com"];

impl DenyPolicy {
    /// Whether this policy expects `denial`.
    #[must_use]
    pub fn expects(self, denial: &Denial) -> bool {
        match self {
            Self::AllExpected => true,
            Self::ForcedAnd(hosts) => {
                denial.cause == DenyCause::Forced
                    || Url::parse(&denial.url).is_ok_and(|url| {
                        url.host_str()
                            .is_some_and(|host| hosts.iter().any(|suffix| on_host(host, suffix)))
                    })
            }
        }
    }

    /// The denies of `denials` this policy doesn't expect.
    #[must_use]
    pub fn unexpected(self, denials: &[Denial]) -> Vec<Denial> {
        denials.iter().filter(|denial| !self.expects(denial)).cloned().collect()
    }

    /// Whether this policy expects a WebSocket frame deny: only [`DenyPolicy::AllExpected`] does.
    #[must_use]
    pub fn expects_frames(self) -> bool {
        self == Self::AllExpected
    }
}

/// `host` is `suffix` or a subdomain of it.
fn on_host(host: &str, suffix: &str) -> bool {
    host.eq_ignore_ascii_case(suffix)
        || host
            .len()
            .checked_sub(suffix.len() + 1)
            .is_some_and(|dot| host.as_bytes()[dot] == b'.' && host[dot + 1..].eq_ignore_ascii_case(suffix))
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
    /// fails the scenario.
    pub fn add_account(&mut self, run: AccountRun, policy: DenyPolicy) {
        let alias = run.alias;
        if run.fence.breached() {
            self.failures
                .push(format!("{alias}: the fence was breached ({})", run.fence.summary()));
        }
        for denial in policy.unexpected(&run.denials) {
            self.failures.push(format!(
                "{alias}: unexpected deny of {} {} ({:?})",
                denial.method, denial.url, denial.cause
            ));
        }
        if !policy.expects_frames() {
            for denial in &run.frame_denials {
                self.failures.push(format!(
                    "{alias}: unexpected deny of a WebSocket frame to {} ({:?})",
                    denial.url, denial.cause
                ));
            }
            // the report's count, taken at another moment, may hold frames the list doesn't
            let unlisted = run.fence.denied_frames.saturating_sub(run.frame_denials.len());
            if unlisted > 0 {
                self.failures.push(format!(
                    "{alias}: unexpected deny of {unlisted} more WebSocket frame(s)"
                ));
            }
        }
        self.accounts.push(run);
    }

    /// Closes `session` and adds its run ([`Self::add_account`]), judged by `policy`. A close that fails (the browser
    /// had to be killed, the capture lost events) fails the run but doesn't end it: the report so far and the other
    /// accounts' runs are kept. The run is judged on what the fence decided once the close has waited for the decisions
    /// still running, so a deny that lands while the browser goes down counts too (and one still undecided when that
    /// wait ends fails the run through the close).
    pub async fn close_account(&mut self, alias: Alias, session: LabSession, policy: DenyPolicy) {
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
    }
}
