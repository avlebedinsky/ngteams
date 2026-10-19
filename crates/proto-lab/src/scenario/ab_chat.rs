//! The `ab-chat` scenario: account a sends a message in the a–b chat and account b receives it.
//!
//! Both accounts run with the write mapping ([`Classifier::mapped`]) and members read live (`LiveMembershipSource`,
//! fed each account's chat service credential from its own page's requests): a write passes only when the chat's
//! members, read fresh, are all lab accounts. The steps:
//! 1. a and b open the web client; both must load.
//! 2. a opens the a–b chat (the chat deep link with b's UPN), b the same chat with a's.
//! 3. **Live enforcement test**: a forced deny ([`ForcedDeny`]) on a's message post holding a probe text; a sends the
//!    probe. The post must fail in a's page with `net::ERR_BLOCKED_BY_CLIENT`, and b must not see the probe within
//!    [`PROBE_WATCH`].
//! 4. a sends the real message; b must see it within [`DELIVERY`].
//! 5. After the run: no deny but the forced one and telemetry ([`DenyPolicy::ForcedAnd`] with [`TELEMETRY_HOSTS`]).
//!
//! **Not run yet**: the lab accounts have no Teams license, so the web client never loads for them
//! (`docs/backlog/lab-accounts-have-no-teams-license.md`), and this stops at step 1. The page selectors below
//! ([`COMPOSE`]) and the deep link are from the web client's conventions, unverified on a loaded client.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::classify::Classifier;
use crate::commands::status::account_status;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_http::{DenyCause, ForcedDeny};
use crate::session::{LabSession, SessionOptions, SessionState, TEAMS_URL};

use super::dsl::{RequestPattern, Selector};
use super::{AccountRun, DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport, TELEMETRY_HOSTS};

/// The message compose box: the web client's rich-text editor (unverified on a loaded client).
pub const COMPOSE: Selector<'static> = Selector::Css(r#"[contenteditable="true"][role="textbox"]"#);

/// A message post to the chat service: `POST …/v1/users/ME/conversations/<chat>/messages`.
pub const MESSAGE_POST: &str = r"/v1/users/ME/conversations/[^/?]+/messages(\?|$)";

/// How long the chat gets to open.
pub const CHAT_OPEN: Duration = Duration::from_secs(60);
/// How long b is watched for the probe that must not arrive.
pub const PROBE_WATCH: Duration = Duration::from_secs(20);
/// How long the real message gets to reach b.
pub const DELIVERY: Duration = Duration::from_secs(60);

/// The denies a run of this scenario expects.
pub const POLICY: DenyPolicy = DenyPolicy::ForcedAnd(&TELEMETRY_HOSTS);

/// The web client's deep link to the 1:1 chat with `upn` (opened, or started if there is none yet).
#[must_use]
pub fn chat_link(upn: &str) -> String {
    let users: String = url::form_urlencoded::byte_serialize(upn.as_bytes()).collect();
    format!("https://teams.microsoft.com/l/chat/0/0?users={users}")
}

/// A text no earlier run used.
#[must_use]
pub fn nonce() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{:x}-{:x}", now.as_millis(), std::process::id())
}

/// The probe a's forced deny stops: it must never reach b.
#[must_use]
pub fn probe_text(nonce: &str) -> String {
    format!("lab fence probe {nonce}: must not arrive")
}

/// The message that must reach b.
#[must_use]
pub fn message_text(nonce: &str) -> String {
    format!("lab message {nonce}")
}

/// Runs `ab-chat` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, no Chrome, no fence, a browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let session_options = SessionOptions {
        classifier: Classifier::mapped(),
        live_members: true,
        capture: Some(ScenarioName::AbChat.as_str().to_owned()),
        display: options.display,
    };
    let mut a = LabSession::open_with(paths, Alias::A, session_options.clone()).await?;
    let mut b = match LabSession::open_with(paths, Alias::B, session_options).await {
        Ok(b) => b,
        Err(error) => {
            // a's browser goes down whatever happened
            let _ = a.close().await;
            return Err(error.into());
        }
    };
    let mut report = ScenarioReport::new(ScenarioName::AbChat);
    let steps = Steps {
        upn_a: &config.account(Alias::A).upn,
        upn_b: &config.account(Alias::B).upn,
        options,
    };
    let outcome = steps.run(&mut a, &mut b, &mut report).await;
    if let Err(error) = outcome {
        report.failures.push(error.to_string());
    }
    for (alias, session) in [(Alias::A, a), (Alias::B, b)] {
        let denials = session.denials();
        let (fence, capture) = session.close_recorded().await?;
        report.notes.push(format!("{alias}: {}", fence.summary()));
        report.add_account(
            AccountRun {
                alias,
                fence,
                denials,
                capture,
            },
            POLICY,
        );
    }
    Ok(report)
}

struct Steps<'a> {
    upn_a: &'a str,
    upn_b: &'a str,
    options: RunOptions,
}

impl Steps<'_> {
    async fn run(
        &self,
        a: &mut LabSession,
        b: &mut LabSession,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        for (alias, session) in [(Alias::A, &mut *a), (Alias::B, &mut *b)] {
            load(alias, session, self.options.load_timeout).await?;
            report.notes.push(format!("{alias}: the web client loaded"));
        }
        let (mut drive_a, mut drive_b) = (a.driver(), b.driver());
        drive_a.navigate("a opens the a–b chat", &chat_link(self.upn_b)).await?;
        drive_b.navigate("b opens the a–b chat", &chat_link(self.upn_a)).await?;
        drive_a.wait_for("a's compose box", COMPOSE, CHAT_OPEN).await?;
        drive_b.wait_for("b's compose box", COMPOSE, CHAT_OPEN).await?;

        // the live enforcement test: a denied write never leaves a's browser
        let nonce = nonce();
        let probe = probe_text(&nonce);
        let posts = RequestPattern::new(Some("POST"), MESSAGE_POST).map_err(|error| setup(error.to_string()))?;
        a.force_deny(
            ForcedDeny::new("POST", MESSAGE_POST)
                .map_err(|error| setup(error.to_string()))?
                .with_body_containing(&probe),
        );
        drive_a.type_into("a types the probe", COMPOSE, &probe).await?;
        drive_a.press_enter("a sends the probe").await?;
        let denied = drive_a
            .wait_for_failed_request("a's probe post fails", &posts, Duration::from_secs(30))
            .await?;
        let error = denied.failed.clone().unwrap_or_default();
        if !error.contains("ERR_BLOCKED_BY_CLIENT") {
            return Err(setup(format!(
                "a's probe post failed with {error}, not as blocked by the fence"
            )));
        }
        if !a.denials().iter().any(|denial| denial.cause == DenyCause::Forced) {
            return Err(setup("the fence has no forced deny of the probe".to_owned()));
        }
        a.clear_forced();
        if drive_b
            .wait_for_text("b must not get the probe", &probe, PROBE_WATCH)
            .await
            .is_ok()
        {
            return Err(setup("b received the probe the fence denied".to_owned()));
        }
        report
            .notes
            .push("enforcement: a's denied probe failed in the page and never reached b".to_owned());

        // the real message
        let message = message_text(&nonce);
        drive_a.type_into("a types the message", COMPOSE, &message).await?;
        drive_a.press_enter("a sends the message").await?;
        drive_b
            .wait_for_text("b receives the message", &message, DELIVERY)
            .await?;
        report.notes.push("a's message reached b".to_owned());
        Ok(())
    }
}

/// Opens the web client in `session` and waits for it to load.
async fn load(alias: Alias, session: &mut LabSession, timeout: Duration) -> Result<(), ScenarioError> {
    session.navigate(TEAMS_URL).await?;
    let state = session.settle(timeout).await;
    if state == SessionState::Loaded {
        return Ok(());
    }
    let status = account_status(&state, timeout, session.watch().last_authz_status());
    Err(setup(format!("{alias}: the web client didn't load: {status}")))
}

fn setup(reason: String) -> ScenarioError {
    ScenarioError::Setup(reason)
}
