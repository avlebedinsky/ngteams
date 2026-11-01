//! The `ab-call-app` scenario (M3 Task 21): account b's web client in the armed browser, the far end of a call with
//! account a's app. The app isn't started here: the live test (`crates/teams-app/tests/call_live.rs`) runs it in its UI
//! sandbox, drives it by its keyboard and reads its view trace, and spawns this scenario (a normally built
//! `proto-lab run ab-call-app --headless --call <step>`) for the part of the run where b is the web client. The two
//! keep in step by this scenario's progress lines ([`progress`]: `ab-call-app: <step>` on stdout, as each step is
//! reached) and the app's trace lines; every write of the page goes through the browser fence (members read live), the
//! app's through its engine's outbox, both into the lab's `fence.log`.
//!
//! b's page loads at the a–b chat, starts its calling stack and plays Chrome's fake microphone (the tone in bursts) and
//! camera, as `ab-call-native`'s does. By `--call`:
//! - **`answer`** (the default): once the page's calling socket is up it says `ready` and waits for a's call; it clicks
//!   answer only on the run's own call (`ab-call`'s ring check, every invitation the page received a's call to b in no
//!   group chat), says `in-call`, must hear a's tone (`heard`) and decode a's video once its own camera is on (`saw`),
//!   and then stays in the call until a hangs up (`ended`), at most [`APP_HOLD`].
//! - **`place`**: once the page's calling socket is up it says `ready` and calls a from the chat's header (its start
//!   decided by the fence); once a's app answered it says `in-call`, must hear a's tone and decode a's video as above,
//!   holds the call [`PAGE_HOLD`] and hangs up (`ended`).
//!
//! Judged by b's fence: no deny but the reviewed list's and the run's forced ones; in `answer`, the run's invitation
//! reaching the page and its attach let through; in `place`, its own start let through.

use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use crate::browser::Media;
use crate::capture::EVENTS_FILE;
use crate::classify::Classifier;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::session::{LabSession, SessionOptions};

use super::ab_call::{
    CONNECT_WAIT, CallPauses, CallStep, CalleeRole, EarlierRings, HANG_UP_CSS, INCOMING_CALL, Modality, RunCall,
    back_to_chat, check_ring, hang_up_page, judge_callee, mri, note_microphone, place_call, policy,
    stand_up_calling_with, switch_microphone, wait_calling_socket, watch_controls,
};
use super::ab_call_native::{ANSWER, hear_on_page, now_ms, see_on_page};
use super::ab_chat::{EARLIER_RUN_PROBE, chat_link, load_at, probe_rules, save_screenshot, setup};
use super::dsl::{Selector, js_string};
use super::echo_call::{HANG_UP, USER_AGENT, call_starts, linger, size_page, write_burst_tone};
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The forced denies of `answer`: an earlier run's probe.
pub const FORCED_ANSWER: &[&str] = &[EARLIER_RUN_PROBE];
/// The forced denies of `place`: an earlier run's probe.
pub const FORCED_PLACE: &[&str] = &[EARLIER_RUN_PROBE];

/// How long the page waits for a's call once it said `ready`: the live test calls once it read the line.
pub const APP_RING_WAIT: Duration = Duration::from_secs(90);
/// How long the page, in `answer`, stays in the call for a to hang up once it saw a's video.
pub const APP_HOLD: Duration = Duration::from_secs(180);
/// How long the page, in `place`, waits for a's app to answer once it placed the call.
pub const ANSWER_WAIT: Duration = Duration::from_secs(90);
/// How long the page, in `place`, holds the call once it saw a's video, while the live test checks a's side.
pub const PAGE_HOLD: Duration = Duration::from_secs(40);

/// The prefix of every progress line.
pub const PROGRESS: &str = "ab-call-app: ";

/// How many times the page switches its call's microphone again while its own microphone stays silent.
pub const MIC_TRIES: usize = 3;
/// How long the page's microphone gets to sound after a switch.
const MIC_WAIT: Duration = Duration::from_secs(5);
/// A level of the page's own microphone (WebRTC's `media-source` `audioLevel`, 0–1) at which it plays the tone.
pub const MIC_SOUNDS: f64 = 0.05;

/// Existing live waits, carried by the concrete-session driver so offline pages need no recording pauses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pauses {
    calling: CallPauses,
    ring: Duration,
    app_hold: Duration,
    answer: Duration,
    page_hold: Duration,
    connect: Duration,
    microphone: Duration,
}

impl Default for Pauses {
    fn default() -> Self {
        Self {
            calling: CallPauses::default(),
            ring: APP_RING_WAIT,
            app_hold: APP_HOLD,
            answer: ANSWER_WAIT,
            page_hold: PAGE_HOLD,
            connect: CONNECT_WAIT,
            microphone: MIC_WAIT,
        }
    }
}

/// Whether the capture's `text` holds a statistics report of the page's own audio source at [`MIC_SOUNDS`] or louder:
/// what its microphone sends into the call.
#[must_use]
pub fn microphone_sounds(text: &str) -> bool {
    text.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry["type"] == "rtc" && entry["payload"]["event"] == "stats")
        .filter_map(|entry| entry["payload"]["data"].as_array().cloned())
        .flatten()
        .any(|report| {
            report["type"] == "media-source"
                && report["kind"] == "audio"
                && report["audioLevel"].as_f64().is_some_and(|level| level >= MIC_SOUNDS)
        })
}

/// Says on stdout that the page reached `step` (`ready`, `in-call`, `heard`, `mic`: its own microphone plays the tone
/// into the call, `saw`, `ended`), at once, as `ab-call-app: <step> at <Unix ms>`: the live test reads it while the run
/// goes on, and its parser (`crates/teams-app/tests/call_live/web.rs`) is the only reader of the line.
pub fn progress(step: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{PROGRESS}{step} at {}", now_ms());
    let _ = out.flush();
}

/// The forced denies of `step`'s run: `answer`'s or `place`'s.
///
/// # Errors
/// Another step: `ab-call-app` takes `answer` or `place` only.
pub fn forced(step: CallStep) -> Result<&'static [&'static str], String> {
    match step {
        CallStep::Answer => Ok(FORCED_ANSWER),
        CallStep::Place => Ok(FORCED_PLACE),
        other => Err(format!(
            "ab-call-app takes --call answer or --call place, not {}",
            other.as_str()
        )),
    }
}

/// Runs `ab-call-app` (see the module docs).
///
/// # Errors
/// When the run can't start: a `--call` step it doesn't take, no lab config or no MRI for a or b, no Chrome, no fence,
/// a browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let forced = forced(options.call).map_err(setup)?;
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let (mri_a, mri_b) = (mri(&config, Alias::A)?, mri(&config, Alias::B)?);
    let mut report = ScenarioReport::new(ScenarioName::AbCallApp);
    let tone = write_burst_tone(paths).map_err(|error| setup(format!("can't write the tone: {error}")))?;
    let session_options = SessionOptions {
        classifier: Classifier::new(),
        live_members: true,
        capture: Some(ScenarioName::AbCallApp.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::Fake { audio: Some(tone) },
        user_agent: Some(USER_AGENT.to_owned()),
    };
    let mut session = LabSession::open_with(paths, Alias::B, session_options).await?;
    for rule in probe_rules(None).map_err(|error| setup(error.to_string()))? {
        session.force_deny(rule);
    }
    let call = RunCall {
        caller: mri_a.clone(),
        group_chat: None,
    };
    let rang = drive(&mut session, (&call, &mri_b), options, &mut report, &Pauses::default()).await;
    let dir = session.capture_dir().map(Path::to_path_buf);
    report.close_account(Alias::B, session, &policy(forced)).await;
    if let Some(dir) = dir {
        judge(options.call, (&call, &mri_b, rang), &dir, &mut report);
        report.notes.push(format!("b: recorded into {}", dir.display()));
    }
    Ok(report)
}

/// Drives an already armed page; browser admission, forced rules and capture closing stay in `run`.
async fn drive(
    session: &mut LabSession,
    (call, mri_b): (&RunCall, &str),
    options: RunOptions,
    report: &mut ScenarioReport,
    pauses: &Pauses,
) -> Option<u64> {
    let outcome = match options.call {
        CallStep::Place => place(session, &call.caller, options, report, pauses)
            .await
            .map(|()| None),
        _ => answer(session, (call, mri_b), options, report, pauses).await,
    };
    let rang = match outcome {
        Ok(rang) => rang,
        Err(error) => {
            report.failures.push(error.to_string());
            if let Some(note) = save_screenshot(Alias::B, session).await {
                report.notes.push(note);
            }
            None
        }
    };
    if hang_up_page(Alias::B, session, report).await.unwrap_or(false) {
        report.notes.push("b: hung up at the run's end".to_owned());
    }
    progress("ended");
    rang
}

/// The page loaded at the a–b chat with its calling stack up, its ring controls watched.
async fn stand_up(
    session: &mut LabSession,
    mri_a: &str,
    options: RunOptions,
    report: &mut ScenarioReport,
    pauses: &Pauses,
) -> Result<(), ScenarioError> {
    load_at(Alias::B, session, &chat_link(mri_a), options.load_timeout).await?;
    size_page(session).await?;
    report.notes.push("b: the web client loaded at the a-b chat".to_owned());
    stand_up_calling_with(Alias::B, &mut session.driver(), report, pauses.calling).await?;
    back_to_chat(Alias::B, &mut session.driver(), report).await?;
    wait_calling_socket(Alias::B, session, report).await?;
    watch_controls(Alias::B, session).await
}

/// `answer` (see the module docs); when the page said `ready` (Unix ms): the run's call rings after it.
async fn answer(
    session: &mut LabSession,
    (call, mri_b): (&RunCall, &str),
    options: RunOptions,
    report: &mut ScenarioReport,
    pauses: &Pauses,
) -> Result<Option<u64>, ScenarioError> {
    stand_up(session, &call.caller, options, report, pauses).await?;
    let since = now_ms();
    progress("ready");
    session
        .driver()
        .wait_for("b shows a's incoming call", INCOMING_CALL, pauses.ring)
        .await?;
    report
        .notes
        .push(format!("b: the incoming call showed at {}", now_ms()));
    check_ring(
        (Alias::B, &mut *session),
        ANSWER,
        (call, mri_b),
        since,
        EarlierRings::OwnLeftOut,
    )
    .await?;
    session
        .driver()
        .click("b clicks answer", Selector::Css(ANSWER.button))
        .await?;
    report.notes.push(format!("b: clicked answer at {}", now_ms()));
    session
        .driver()
        .wait_for("b's call screen", HANG_UP, pauses.connect)
        .await?;
    in_call(session, CallStep::Answer, report, pauses).await?;
    if !screen_gone_within(session, pauses.app_hold).await {
        report.failures.push(format!(
            "a didn't hang up within {:?} of b's seeing its video",
            pauses.app_hold
        ));
    }
    Ok(Some(since))
}

/// `place` (see the module docs).
async fn place(
    session: &mut LabSession,
    mri_a: &str,
    options: RunOptions,
    report: &mut ScenarioReport,
    pauses: &Pauses,
) -> Result<(), ScenarioError> {
    stand_up(session, mri_a, options, report, pauses).await?;
    back_to_chat(Alias::B, &mut session.driver(), report).await?;
    progress("ready");
    place_call((Alias::B, session), Modality::Audio, report).await?;
    session
        .driver()
        .wait_for("b's call screen, a having answered", HANG_UP, pauses.answer)
        .await?;
    in_call(session, CallStep::Place, report, pauses).await?;
    linger(session, pauses.page_hold).await;
    if !hang_up_page(Alias::B, session, report).await? {
        report
            .failures
            .push("b's call screen was gone before b hung up".to_owned());
    }
    if !screen_gone_within(session, pauses.connect).await {
        report
            .failures
            .push("b's call screen stayed after b hung up".to_owned());
    }
    Ok(())
}

/// In the call: says `in-call`, then must hear a's tone (`heard`) and decode a's video once its own camera is on
/// (`saw`); a step that doesn't come is a failure of the run, and the run goes on.
async fn in_call(
    session: &mut LabSession,
    step: CallStep,
    report: &mut ScenarioReport,
    pauses: &Pauses,
) -> Result<(), ScenarioError> {
    report.notes.push(format!("b: in the call at {}", now_ms()));
    progress("in-call");
    let from = super::ab_call_native::capture_len(session);
    let failures = report.failures.len();
    hear_on_page(session, from, report).await?;
    if report.failures.len() == failures {
        progress("heard");
    }
    sound_microphone(session, (from, step), report, pauses.microphone).await?;
    let failures = report.failures.len();
    see_on_page(session, from, report).await?;
    if report.failures.len() == failures {
        progress("saw");
    }
    Ok(())
}

/// The page's own microphone must play its tone into the call: when it stays silent (a call the page placed opens
/// it silent, and one switch may land on it again: seen 2026-10-26), the page switches its call's microphone again, up
/// to [`MIC_TRIES`] times. Says `mic` once it sounds. A microphone silent throughout is noted, and the run goes on: the
/// far end then can't be checked hearing it (seen 2026-10-26, in calls the page placed and, once, in one it answered:
/// the page's own audio source flat however often it was switched, its microphone button unmuted, Chrome's fake input
/// playing the tone file; the page heard the far end and decoded its video throughout).
async fn sound_microphone(
    session: &mut LabSession,
    (from, step): (u64, CallStep),
    report: &mut ScenarioReport,
    microphone_wait: Duration,
) -> Result<(), ScenarioError> {
    let Some(file) = session.capture_dir().map(|dir| dir.join(EVENTS_FILE)) else {
        return Err(setup("the run records no capture to read the microphone in".to_owned()));
    };
    for attempt in 0..=MIC_TRIES {
        if attempt > 0 {
            switch_microphone(Alias::B, &mut session.driver(), report).await?;
        }
        note_microphone(Alias::B, session, report).await;
        let since = super::ab_call_native::capture_len(session).max(from);
        let deadline = tokio::time::Instant::now() + microphone_wait;
        while tokio::time::Instant::now() < deadline {
            linger(session, Duration::from_millis(500)).await;
            if microphone_sounds(&super::ab_call_native::read_from(&file, since)) {
                report
                    .notes
                    .push(format!("b: its microphone sounds after {attempt} more switch(es)"));
                progress("mic");
                return Ok(());
            }
        }
    }
    report.notes.push(format!(
        "b: its own microphone stayed silent through {MIC_TRIES} more switches ({})",
        step.as_str()
    ));
    Ok(())
}

/// Whether b's call screen went within `limit` (the call ended), the page's events flowing.
async fn screen_gone_within(session: &mut LabSession, limit: Duration) -> bool {
    let gone = format!("document.querySelector({}) === null", js_string(HANG_UP_CSS));
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if session.driver().evaluate("b's call screen", &gone).await.ok() == Some(serde_json::Value::Bool(true)) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        linger(session, Duration::from_secs(1)).await;
    }
}

/// b's capture after the run: in `answer` the run's invitation reached the page and its attach went (a callee's
/// judgement in `ab-call`); in `place` the page's own start was let through.
fn judge(step: CallStep, (call, mri_b, rang): (&RunCall, &str, Option<u64>), dir: &Path, report: &mut ScenarioReport) {
    if step == CallStep::Place {
        let text = std::fs::read_to_string(dir.join(EVENTS_FILE)).unwrap_or_default();
        let (allowed, denied) = call_starts(&text);
        report.notes.push(format!(
            "b: call start {allowed} allowed and {denied} denied by the fence"
        ));
        if allowed == 0 {
            report.failures.push("b's fence allowed no call start".to_owned());
        }
    } else {
        judge_callee((Alias::B, mri_b, CalleeRole::Other), call, rang, dir, report);
    }
}

#[cfg(test)]
#[path = "../../tests/support/scenario/ab_call_app_driver_tests.rs"]
mod driver_tests;
