//! The `ab-meeting-join` scenario (`docs/protocol/calling.md`, "Joining a meeting"): what the web client sends when it
//! goes into a meeting nobody started. Account a schedules a Teams meeting with b from the scheduling form, opens its
//! join link, and clicks the pre-join's Join now, only on the meeting the run made.
//!
//! The start's row is the fence's `meeting_call_start`: a's pre-join subscription goes through it, judged by the meeting's
//! chat, read fresh. What the client sends next (its callback links and its entry at the conversation) is held back by
//! labelled forced denies, as `ab-call --call start-held` holds a start: the run records it, and the client then tries
//! its one start with media, which goes through the row too, unless the run holds that back as well.
//!
//! The steps:
//! 1. Before anything loads, a's fence force-denies every write carrying the probe marker, as in `ab-chat`, and the
//!    conversation's own writes.
//! 2. a opens the web client, opens the chat with b and proves enforcement live with this run's probe.
//! 3. a schedules the meeting ([`super::ab_meeting`]'s form).
//! 4. a loads at the meeting's join link; its pre-join must show Join now, its subscription the fence let through.
//! 5. a clicks Join now; every start with media is held back with a labelled deny, so nothing enters a call.

use std::time::Duration;

use crate::browser::Media;
use crate::classify::Classifier;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_http::ForcedDeny;
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{
    CHAT_OPEN, COMPOSE, EXPECTED_FORCED, chat_link, chat_url, deny_probes, enforce, load, load_at, nonce,
    one_to_one_id, probe_text, save_screenshot_as, setup,
};
use super::ab_meeting::{self, created_join_link};
use super::dsl::{Driver, RequestPattern};
use super::echo_call::{USER_AGENT, size_page, write_burst_tone};
use super::expected::Review;
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The label of the forced denies of what follows the pre-join's subscription.
pub const ENTRY_HELD: &str = "the meeting's entry, held back";

/// The denies the run expects: `ab-chat`'s, and its own.
const FORCED: [&str; 3] = [EXPECTED_FORCED[0], EXPECTED_FORCED[1], ENTRY_HELD];

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
/// When the run can't start: no lab config, no Chrome, no fence, a browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let tone = write_burst_tone(paths).map_err(|error| setup(format!("can't write the tone: {error}")))?;
    let session_options = SessionOptions {
        classifier: Classifier::new(),
        live_members: true,
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
    if let Err(error) = steps.prepare(&a).and(hold_entry(&a)) {
        report.failures.push(error.to_string());
    }
    if let Err(error) = steps.run(&mut a, mri(Alias::A), &mut report).await {
        report.failures.push(error.to_string());
        if let Some(note) = save_screenshot_as(Alias::A, &a, "failure.png").await {
            report.notes.push(note);
        }
    }
    report.close_account(Alias::A, a, &policy()).await;
    Ok(report)
}

/// How a run judges its denies: its own forced ones, every other by the reviewed list.
#[must_use]
pub fn policy() -> DenyPolicy {
    DenyPolicy::Reviewed(Review::new().expecting_forced(&FORCED))
}

struct Steps<'a> {
    mri_b: &'a str,
    upn_b: &'a str,
    nonce: &'a str,
    options: RunOptions,
}

impl Steps<'_> {
    fn prepare(&self, a: &LabSession) -> Result<(), ScenarioError> {
        deny_probes(a, Some(&probe_text(self.nonce)))
    }

    async fn run(&self, a: &mut LabSession, me: &str, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        let chat_id = one_to_one_id(me, self.mri_b)
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
        let names_chat = chat_url(&chat_id).map_err(|error| setup(error.to_string()))?;
        enforce(a, &mut drive, &probe_text(self.nonce), &names_chat).await?;
        report
            .notes
            .push("enforcement: a's probe post was denied by the fence and failed in a's page".to_owned());
        let title = format!("lab meeting join {}", self.nonce);
        let body = ab_meeting::schedule(a, &mut drive, report, &title, self.upn_b, self.options).await?;
        let link = created_join_link(body.as_deref())
            .ok_or_else(|| setup("the event's creation named no join link".to_owned()))?;
        load_at(Alias::A, a, &link, self.options.load_timeout).await?;
        let mut drive = a.driver();
        pre_join(a, &mut drive, report).await
    }
}

/// Holds back what follows the pre-join's subscription: the conversation's entry and its callback links.
fn hold_entry(a: &LabSession) -> Result<(), ScenarioError> {
    let rule = ForcedDeny::new("POST", CONVERSATION_ENTRY).map_err(|error| setup(error.to_string()))?;
    a.force_deny(rule.labelled(ENTRY_HELD));
    Ok(())
}

/// Steps 4 and 5: the pre-join, then Join now, its every start with media held back.
async fn pre_join(a: &mut LabSession, drive: &mut Driver, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    ab_meeting::wait_for_button(drive, "the pre-join's Join now", &["Join now"], PREJOIN).await?;
    if let Some(note) = save_screenshot_as(Alias::A, a, "prejoin.png").await {
        report.notes.push(note);
    }
    let rule = ForcedDeny::new("POST", STARTS)
        .map_err(|error| setup(error.to_string()))?
        .with_body_containing("callInvitation");
    a.force_deny(rule.labelled(ENTRY_HELD));
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
