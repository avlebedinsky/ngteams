//! Records meeting pre-join UI and request shapes. Explicit entry/start holds keep the capture phase from entering a meeting until the scenario releases its holds.

use std::time::Duration;

use crate::browser::Media;
use crate::capture_hold::Hold;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{CHAT_OPEN, COMPOSE, chat_link, load, load_at, nonce, one_to_one_id, save_screenshot_as, setup};
use super::ab_meeting::{self, created_join_link};
use super::dsl::{Driver, RequestPattern};
use super::echo_call::{USER_AGENT, size_page, write_burst_tone};
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The label of the capture holds of what follows the pre-join's subscription.
pub const ENTRY_HELD: &str = "the meeting's entry, held back";

/// The conversation's entry and its callback links: `POST …/conv/<id>` and `…/conv/<id>/notificationLinks`.
const CONVERSATION_ENTRY: &str = r"\.conv\.skype\.com/conv/[^/?]+(/notificationLinks)?(\?|$)";

/// A start on the call controller's conversations endpoint with media.
const STARTS: &str = r"/api/v2/epconv(\?|$)";

/// How long the pre-join gets to show.
const PREJOIN: Duration = Duration::from_secs(90);

/// How long the run records after Join now: the client's tries, one after another.
const AFTER_JOIN: Duration = Duration::from_secs(45);

/// Runs `ab-meeting-join` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, no Chrome, a browser whose capture setup fails.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let tone = write_burst_tone(paths).map_err(|error| setup(format!("can't write the tone: {error}")))?;
    let session_options = SessionOptions {
        capture: Some(ScenarioName::AbMeetingJoin.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::Fake { audio: Some(tone) },
        user_agent: Some(USER_AGENT.to_owned()),
    };
    let mut a = LabSession::open_with(paths, Alias::A, session_options).await?;
    let nonce = nonce();
    let mut report = ScenarioReport::new(ScenarioName::AbMeetingJoin);
    let mri = |alias| config.account(alias).mri.as_deref().unwrap_or_default();
    let steps = Steps {
        mri_b: mri(Alias::B),
        upn_b: &config.account(Alias::B).upn,
        nonce: &nonce,
        options,
    };
    if let Err(error) = hold_entry(&a) {
        report.failures.push(error.to_string());
    }
    if let Err(error) = steps.run(&mut a, mri(Alias::A), &mut report).await {
        report.failures.push(error.to_string());
        if let Some(note) = save_screenshot_as(Alias::A, &a, "failure.png").await {
            report.notes.push(note);
        }
    }
    report.close_account(Alias::A, a).await;
    Ok(report)
}

struct Steps<'a> {
    mri_b: &'a str,
    upn_b: &'a str,
    nonce: &'a str,
    options: RunOptions,
}

impl Steps<'_> {
    async fn run(&self, a: &mut LabSession, me: &str, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        // Require the account and peer MRIs before opening their chat.
        one_to_one_id(me, self.mri_b)
            .ok_or_else(|| setup("lab.toml has no MRI for a or b: run `proto-lab login` for them".to_owned()))?;
        let mut drive = a.driver();
        load(Alias::A, a, self.options.load_timeout).await?;
        size_page(a).await?;
        drive
            .navigate("a opens the chat with b", &chat_link(self.mri_b))
            .await?;
        drive
            .wait_for("a's compose box", COMPOSE, self.options.load_timeout.max(CHAT_OPEN))
            .await?;

        let title = format!("lab meeting join {}", self.nonce);
        let body = ab_meeting::schedule(a, &mut drive, report, &title, self.upn_b, self.options).await?;
        let joined = self.join(a, body.as_deref(), report).await;
        if joined.is_err()
            && let Some(note) = save_screenshot_as(Alias::A, a, "join-failure.png").await
        {
            report.notes.push(note);
        }
        let mut drive = a.driver();
        let cancelled = match ab_meeting::created_chat(body.as_deref()) {
            Some(chat) => ab_meeting::cancel_scheduled(a, &mut drive, report, &chat, self.options).await,
            None => Err(setup("the event's creation named no meeting chat".to_owned())),
        };
        joined.and(cancelled)
    }

    /// Steps 4 and 5 on the meeting whose creation's request body is `body`.
    async fn join(
        &self,
        a: &mut LabSession,
        body: Option<&str>,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let link =
            created_join_link(body).ok_or_else(|| setup("the event's creation named no join link".to_owned()))?;
        load_at(Alias::A, a, &link, self.options.load_timeout).await?;
        let mut drive = a.driver();
        pre_join(a, &mut drive, report).await
    }
}

/// Holds back what follows the pre-join's subscription: the conversation's entry and its callback links.
fn hold_entry(a: &LabSession) -> Result<(), ScenarioError> {
    let rule = Hold::new("POST", CONVERSATION_ENTRY).map_err(|error| setup(error.to_string()))?;
    a.hold(rule.labelled(ENTRY_HELD));
    Ok(())
}

/// Steps 4 and 5: the pre-join, then Join now, its every start with media held back.
async fn pre_join(a: &mut LabSession, drive: &mut Driver, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    ab_meeting::wait_for_button(drive, "the pre-join's Join now", &["Join now"], PREJOIN).await?;
    if let Some(note) = save_screenshot_as(Alias::A, a, "prejoin.png").await {
        report.notes.push(note);
    }
    let rule = Hold::new("POST", STARTS)
        .map_err(|error| setup(error.to_string()))?
        .with_body_containing("callInvitation");
    a.hold(rule.labelled(ENTRY_HELD));
    ab_meeting::click_button(drive, "a clicks Join now on the run's own meeting", &["Join now"]).await?;
    tokio::time::sleep(AFTER_JOIN).await;
    if let Some(note) = save_screenshot_as(Alias::A, a, "after-join.png").await {
        report.notes.push(note);
    }
    drive.seen_so_far();
    let subscribed = RequestPattern::new(Some("POST"), STARTS).map_err(|error| setup(error.to_string()))?;
    if let Some(status) = drive
        .requests()
        .iter()
        .filter(|seen| subscribed.matches_request(seen))
        .find_map(|seen| seen.status.filter(|status| (200..300).contains(status)))
    {
        report
            .notes
            .push(format!("a: the pre-join's subscription was answered {status}"));
        return Ok(());
    }
    Err(setup(
        "the pre-join's subscription was never answered with a 2xx".to_owned(),
    ))
}
