//! The `ab-call` scenario: account a calls account b from their 1:1 chat in the web client, and the machinery
//! `abc-call` ([`super::abc_call`]) shares, where a calls the lab's a–b–c group chat. Each run is recorded
//! (`captures/<scenario>/<ts>/`, one capture per account): the call's HTTP and WebSocket signaling on every side, the
//! fence's verdicts and the WebRTC hooks' reports are the material of `docs/protocol/calling.md`.
//!
//! Every account runs under the lab rule with members read live, Chrome's fake camera and a microphone playing a tone,
//! muted ([`crate::browser::BASE_ARGS`]). The fence judges a call by its entry: a's start, a callee's answer, decline
//! or join; what a call sends once it was let in (its follow-ups, the callee's ring-time sends, the delivery
//! responses) passes. What a run does is its step ([`CallStep`], `proto-lab run ab-call --call <step>`):
//! - `start-held`: only a's browser runs. a clicks the chat's audio call button, then its video call button; each call
//!   start is held back by a forced deny labelled [`START_HELD`], so nobody is invited and nothing rings. The capture
//!   keeps the starts' bodies: what a start to a lab callee names (its `participants.to`) and what it names of its
//!   chat.
//! - `answer`, `decline`, `cancel`, `timeout`, `video`, `dropped`, `join`, `keep-alive`: a's browser and each callee's.
//!   Each callee opens the Calls app (the client starts its calling stack only then) and waits for its calling Trouter
//!   socket. a's audio call start goes to the fence, which permits it only as a start to lab callees in a lab-only chat;
//!   each callee must show the incoming call within [`RING_WAIT`] (a's call ended by the service as one in a chat whose
//!   call still runs fails the step at once: [`running_call_conflict`]). Then:
//!   - `answer`: each callee clicks answer ([`ACCEPT_AUDIO`]), which the fence permits only on its own offered
//!     invitation; the call must connect with audio both ways on every page (getStats; whether each page heard the
//!     other side's tone is only noted), and then every participant hangs up;
//!   - `decline`: each callee clicks decline ([`DECLINE`]); its rejection must have been let through; a hangs up after
//!     [`AFTER_ACTION`];
//!   - `cancel`: a hangs up after [`RING_BEFORE_CANCEL`] while the callees ring: the caller's cancel as they receive it;
//!   - `timeout`: nobody acts for [`UNANSWERED`] (an unanswered call goes to the callee's voicemail, a service of
//!     the callee's own account, which accepts it for a); then a hangs up;
//!   - `video`: as `answer`, and once the call is up every page turns its camera on and must receive video frames;
//!   - `dropped`: as `answer`, and once the call is up the last callee's page goes offline (new requests fail) and the
//!     run closes its calling Trouter socket itself, which must close; once the page is back every page must still
//!     show the call;
//!   - `join` (`abc-call` only): every callee but the last answers; once the last one's ring ended it loads the chat
//!     and clicks the header's join of the running call, which the fence judges by the chat it names; it must then show
//!     the call screen, and its capture the join let through.
//!   - `keep-alive`: as `answer`, and the call is then held [`KEEP_ALIVE_HOLD`], past its keep-alive interval, every
//!     page's requests recorded: the run that captures the call's keep-alive. A call that ends before the hold is over,
//!     or a page whose fence let no keep-alive through ([`KEEP_ALIVE_SENDS`]), fails the run.
//!
//!   The run records [`AFTER_RING`] more and notes when each callee's incoming call went.
//!
//! Before anything loads, every page force-denies an earlier chat scenario's unsent probe ([`probe_rules`]), as
//! `echo-call` does. A callee clicks answer, decline or join only on the run's own call, naming only lab accounts
//! ([`ring_check`]): every invitation its page received is a's call placed in this run (from a, to this callee, in the
//! run's chat, nobody else named), all of one call; for an answer or a decline the page shows one incoming call, for a
//! join the page shows the run's chat ([`chat_shown`]) with one join. Anything else and the step fails without a
//! click. Whatever a click sends, the fence decides it again on its own: an answer or a decline passes only on the
//! account's own offered invitation from a lab caller, a join only in a lab-only chat.
//!
//! Judged by [`policy`] per account: no deny but the reviewed expected-deny list's and the run's own forced ones; a's
//! start allowed, each callee's page receiving the run's own call's invitation ([`own_invitations`]) and its attach let
//! through, and what its step sends ([`judge_step_sends`]: a decline's rejection let through, a join let through). A
//! failed run hangs up every page that shows its call, a's first, and waits up to [`HANG_UP_WAIT`] for each one's
//! `leave` ([`hang_up_all`]), so no call is left running in the chat; it leaves each page's screenshot as
//! `failure.png` in its capture.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use clap::ValueEnum;
use serde_json::{Value, json};

use crate::browser::Media;
use crate::capture::EVENTS_FILE;
use crate::classify::Classifier;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_http::{Denial, DenyCause, ForcedDeny, without_query};
use crate::fence_ws::delivery_of;
use crate::session::{LabSession, SessionError, SessionOptions, TEAMS_URL};

use super::ab_chat::{EARLIER_RUN_PROBE, chat_link, load_at, probe_rules, save_screenshot, save_screenshot_as, setup};
use super::dsl::{Driver, Selector, js_string};
use super::echo_call::{
    AudioSummary, CALL_START, CALL_START_URL, CALLS_APP, CALLS_APP_WARM, CONTROL_WAIT, HANG_UP, PAGE_OPEN, PAGE_STEADY,
    ToneWait, USER_AGENT, call_starts, linger, size_page, turn_off_noise_suppression, wait_for_tone, write_burst_tone,
};
use super::expected::Review;
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// What a run of `ab-call` or `abc-call` does (see the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum CallStep {
    /// a calls with audio, then with video; each start held back.
    StartHeld,
    /// a calls with audio; the callees click answer, and the call runs with audio both ways.
    #[default]
    Answer,
    /// a calls with audio; the callees click decline.
    Decline,
    /// a calls with audio and hangs up while the callees ring.
    Cancel,
    /// a calls with audio; nobody acts until the ring ends by itself.
    Timeout,
    /// As `answer`, and once the call is up every page turns its camera on.
    Video,
    /// As `answer`, and once the call is up the last callee's page drops off the network for a while (its new
    /// requests fail and the run closes its calling Trouter socket; the call's media isn't affected) and comes back.
    Dropped,
    /// `abc-call` only: a calls, every callee but the last answers, and once the last one's ring ended it joins the
    /// running call from the chat's header.
    Join,
    /// As `answer`, and the call is then held [`KEEP_ALIVE_HOLD`], past the call's keep-alive interval, to record its
    /// keep-alive, which the fence permits on the call's own `callLeg` link: every page must have sent one.
    KeepAlive,
    /// `ab-call-app` only: b's page places the call to a, whose app answers it (`ab-call` and `abc-call` refuse it).
    Place,
}

impl CallStep {
    /// The step's name, as on the command line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StartHeld => "start-held",
            Self::Answer => "answer",
            Self::Decline => "decline",
            Self::Cancel => "cancel",
            Self::Timeout => "timeout",
            Self::Video => "video",
            Self::Dropped => "dropped",
            Self::Join => "join",
            Self::KeepAlive => "keep-alive",
            Self::Place => "place",
        }
    }

    /// What each callee does in this step: answer ([`ACCEPT_AUDIO`]) or decline ([`DECLINE`]); `None` for a step where
    /// no callee acts.
    #[must_use]
    pub fn callee_action(self) -> Option<CalleeAction> {
        match self {
            Self::Answer | Self::Video | Self::Dropped | Self::Join | Self::KeepAlive => Some(CalleeAction {
                button: ACCEPT_AUDIO_CSS,
                what: "answer",
            }),
            Self::Decline => Some(CalleeAction {
                button: DECLINE_CSS,
                what: "decline",
            }),
            Self::StartHeld | Self::Cancel | Self::Timeout | Self::Place => None,
        }
    }
}

/// What a callee does on the incoming call ([`CallStep::callee_action`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalleeAction {
    /// The CSS selector of the button it clicks.
    pub button: &'static str,
    /// The action, for the notes.
    pub what: &'static str,
}

/// The account that calls.
pub const CALLER: Alias = Alias::A;

/// The Chat app's button in the app bar (its `data-tid` is the Chat app's id, `simpleCollab` in the client).
pub const CHAT_APP: Selector<'static> = Selector::Tid("3b64df9d-7e97-4d9c-ac5c-2e0a5d8e6f40");

/// Where a chat's header keeps its call and meeting buttons.
pub const CALL_BUTTONS: Selector<'static> = Selector::Tid("chat-calling-meeting-buttons");
/// The header's call options next to its call button (in a group chat, "More call options" next to its "Meet now",
/// which starts a meeting, not a call). A 1:1 chat's header shows the kind of call placed last as its button and the
/// other in its options; a group chat's options hold only the audio call ([`Modality::control`]).
pub const CALL_OPTIONS: Selector<'static> = Selector::Css(
    r#"[data-tid="dropdown-calling-toggle-more-options-btn"], [data-tid="audio-drop-in-more-options-button"]"#,
);
/// The cancel button of the screen the client shows after a call that couldn't start (its retry screen).
pub const CALL_RETRY_CANCEL: Selector<'static> = Selector::Tid("calling-retry-cancelbutton");
const CALL_RETRY_CANCEL_CSS: &str = r#"[data-tid="calling-retry-cancelbutton"]"#;
/// The confirmation a group chat's call asks for before it rings everyone.
pub const GROUP_CALL_CONFIRM: Selector<'static> = Selector::Tid("group-start-call-confirmation-btn");
const GROUP_CALL_CONFIRM_CSS: &str = r#"[data-tid="group-start-call-confirmation-btn"]"#;
/// How long a group call's confirmation gets to show after the call button was clicked.
pub const CONFIRM_WAIT: Duration = Duration::from_secs(6);

/// The incoming call as the web client shows it (seen 2026-10-24): a calling notification in the in-app notifications
/// area (`notification-wrapper`, labelled by the calling notification's content), its buttons named only by their
/// labels (the lab accounts' client is in English): accept with video, accept with audio, decline.
pub const INCOMING_CALL: Selector<'static> = Selector::Css(INCOMING_CALL_CSS);
const INCOMING_CALL_CSS: &str = r#"[data-testid="notification-wrapper"][aria-labelledby*="cn-calling-main-content"]"#;
pub const ACCEPT_AUDIO: Selector<'static> = Selector::Css(ACCEPT_AUDIO_CSS);
pub const ACCEPT_AUDIO_CSS: &str = concat!(
    r#"[data-testid="notification-wrapper"][aria-labelledby*="cn-calling-main-content"] "#,
    r#"button[aria-label="Accept with audio"]"#
);
pub const DECLINE: Selector<'static> = Selector::Css(DECLINE_CSS);
pub const DECLINE_CSS: &str = concat!(
    r#"[data-testid="notification-wrapper"][aria-labelledby*="cn-calling-main-content"] "#,
    r#"button[aria-label="Decline call"]"#
);
pub(crate) const INCOMING_SCREEN_CSS: &str = INCOMING_CALL_CSS;
/// How long a callee's capture gets to show the run's call's invitation once the page shows the incoming call.
pub const INVITATION_WAIT: Duration = Duration::from_secs(5);

/// The label of the forced deny of a's call starts in `start-held`.
pub const START_HELD: &str = "the call start, held back";

/// A Trouter socket's URL as a pattern.
pub const TROUTER_SOCKETS: &str = r"^wss://([a-z0-9-]+\.)*trouter\.teams\.microsoft\.com(:\d+)?/";

/// What a Trouter delivery response starts with (socket.io 0.9's type `3`, a message).
pub const DELIVERY_RESPONSE: &str = "3:::";

/// How long a callee gets to show the incoming call after a's start went out.
pub const RING_WAIT: Duration = Duration::from_secs(45);
/// How long a callee gets to show the incoming call before the run takes a screenshot of what it shows instead.
pub const RING_SHOT: Duration = Duration::from_secs(12);
/// How often the wait for the incoming call looks at a's capture for a call the service ended at once.
pub const RING_LOOK: Duration = Duration::from_secs(2);
/// How long the callees ring before a hangs up in `cancel`.
pub const RING_BEFORE_CANCEL: Duration = Duration::from_secs(8);
/// How long the run records after the callees acted, before a hangs up.
pub const AFTER_ACTION: Duration = Duration::from_secs(12);
/// How long the answer and decline steps wait for every callee's ring to end after a hung up.
pub const RING_END: Duration = Duration::from_secs(60);
/// How long `timeout` leaves the call unanswered before a hangs up: past the moment the callee's voicemail takes an
/// unanswered call (about 22 s after the start, seen 2026-10-24).
pub const UNANSWERED: Duration = Duration::from_secs(50);
/// How long the run records after the ring.
pub const AFTER_RING: Duration = Duration::from_secs(8);
/// How long a held-back start's call screen gets to go before the run hangs it up.
pub const HELD_SETTLE: Duration = Duration::from_secs(6);
/// How long a failed run waits for each page it hung up to send its `leave` ([`hang_up_all`]).
pub const HANG_UP_WAIT: Duration = Duration::from_secs(10);

/// The forced deny of every call start ([`CALL_START_URL`]), labelled [`START_HELD`].
///
/// # Errors
/// Never: the pattern is fixed.
pub fn start_held_rule() -> Result<ForcedDeny, regex::Error> {
    Ok(ForcedDeny::new("POST", &format!("^{}(\\?|$)", regex::escape(CALL_START_URL)))?.labelled(START_HELD))
}

/// The call's keep-alive, as a URL pattern (`POST`): to the call leg's own link (`callLeg`,
/// `…/cc/v1/active/<call>/<n>/<leg>/<n>`, no operation after it).
pub const KEEP_ALIVE_SENDS: &str = concat!(
    r"^https://api\.flightproxy\.teams\.microsoft\.com/api/v2/ep/[^/?]+/",
    r"cc/v1/active/[^/?]+/[^/?]+/[^/?]+/[^/?]+(\?|$)"
);

/// The keep-alives a capture's `events.jsonl` text shows its fence letting through ([`KEEP_ALIVE_SENDS`], each
/// `Network` request once).
#[must_use]
pub fn keep_alives(text: &str) -> usize {
    allowed_requests(text, "POST", KEEP_ALIVE_SENDS)
}

/// A callee's decline, as a URL pattern (`DELETE`): the rejection of its call leg.
pub const DECLINE_SENDS: &str =
    r"^https://api\.flightproxy\.teams\.microsoft\.com/api/v2/ep/[^/?]+/cc/v1/incoming/[^?]+/reject(\?|$)";

/// An invitation's `attach` link (`…/cc/v1/forked/<call>/…/attach`), as a URL pattern.
pub const ATTACH: &str =
    r"^https://api\.flightproxy\.teams\.microsoft\.com/api/v2/ep/[^/?]+/cc/v1/forked/[^?]+/attach(\?|$)";

/// A conversation controller's own URL on the flight proxy: where a join goes.
pub const CONVERSATION_JOIN: &str =
    r"^https://api\.flightproxy\.teams\.microsoft\.com/api/v2/ep/[^/?]+/conv/[^/?]+(\?|$)";

/// The forced denies of `step` the caller's run expects: an earlier run's probe, and in `start-held` its starts.
#[must_use]
pub fn caller_forced(step: CallStep) -> &'static [&'static str] {
    match step {
        CallStep::StartHeld => &[EARLIER_RUN_PROBE, START_HELD],
        _ => &[EARLIER_RUN_PROBE],
    }
}

/// The forced denies a callee's run expects: an earlier run's probe. Nothing the callee's page sends for the call is
/// held back: the fence judges its answer, decline or join, and lets the rest of the call through.
pub const CALLEE_FORCED: &[&str] = &[EARLIER_RUN_PROBE];

/// The denies a run expects of an account with these forced `labels`: the reviewed list ([`super::expected`]) and them.
#[must_use]
pub fn policy(labels: &'static [&'static str]) -> DenyPolicy {
    DenyPolicy::Reviewed(Review::new().expecting_forced(labels))
}

/// The forced denies of `denials` labelled `label` (requests, by `Network` id when they have one: a request pauses on
/// its target's session and the browser's).
#[must_use]
pub fn held_back(denials: &[Denial], label: &str) -> usize {
    let mut seen = std::collections::BTreeSet::new();
    denials
        .iter()
        .filter(|denial| denial.cause == DenyCause::Forced && denial.label.as_deref() == Some(label))
        .filter(|denial| match &denial.network_id {
            Some(id) => seen.insert(id.clone()),
            None => true,
        })
        .count()
}

/// The chat a run calls from, and who is called.
#[derive(Clone, Debug)]
pub(crate) struct CallPlan {
    pub(crate) scenario: ScenarioName,
    /// The web client's deep link to the chat.
    pub(crate) chat_link: String,
    /// The call as each callee's invitation must name it.
    pub(crate) call: RunCall,
    /// Each callee and its MRI.
    pub(crate) callees: Vec<(Alias, String)>,
    /// The calls `start-held` places, in order.
    pub(crate) held_starts: &'static [Modality],
    pub(crate) step: CallStep,
    pub(crate) options: RunOptions,
    /// Recording pauses between actions; the live defaults remain the documented scenario timings.
    pub(crate) pauses: CallPauses,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CallPauses {
    pub(crate) page_steady: Duration,
    pub(crate) calling_warm: Duration,
    pub(crate) before_cancel: Duration,
    pub(crate) unanswered: Duration,
    pub(crate) after_action: Duration,
    pub(crate) after_ring: Duration,
    pub(crate) video_for: Duration,
    pub(crate) video_wait: Duration,
    pub(crate) keep_alive_hold: Duration,
    pub(crate) hold_check: Duration,
    pub(crate) dropped_for: Duration,
    pub(crate) after_drop: Duration,
}

impl Default for CallPauses {
    fn default() -> Self {
        Self {
            page_steady: PAGE_STEADY,
            calling_warm: CALLS_APP_WARM,
            before_cancel: RING_BEFORE_CANCEL,
            unanswered: UNANSWERED,
            after_action: AFTER_ACTION,
            after_ring: AFTER_RING,
            video_for: VIDEO_FOR,
            video_wait: VIDEO_WAIT,
            keep_alive_hold: KEEP_ALIVE_HOLD,
            hold_check: HOLD_CHECK,
            dropped_for: DROPPED_FOR,
            after_drop: AFTER_DROP,
        }
    }
}

/// Runs `ab-call` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, no MRI for a or b, no Chrome, no fence, a browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let mri_b = mri(&config, Alias::B)?;
    let plan = CallPlan {
        scenario: ScenarioName::AbCall,
        chat_link: chat_link(&mri_b),
        call: RunCall {
            caller: mri(&config, CALLER)?,
            group_chat: None,
        },
        callees: vec![(Alias::B, mri_b)],
        held_starts: &[Modality::Audio, Modality::Video],
        step: options.call,
        options,
        pauses: CallPauses::default(),
    };
    run_plan(paths, &plan).await
}

/// `alias`'s MRI from the lab config.
///
/// # Errors
/// When the lab config has none for it.
pub(crate) fn mri(config: &LabConfig, alias: Alias) -> Result<String, ScenarioError> {
    config
        .account(alias)
        .mri
        .clone()
        .ok_or_else(|| setup(format!("lab.toml has no MRI for {alias}: run `proto-lab login` for it")))
}

/// Runs a call scenario's `plan` (see the module docs).
///
/// # Errors
/// When the run can't start: `--call place` (`ab-call-app`'s only), no tone file, no Chrome, no fence, a browser that
/// can't be armed.
pub(crate) async fn run_plan(paths: &LabPaths, plan: &CallPlan) -> Result<ScenarioReport, ScenarioError> {
    if plan.step == CallStep::Place {
        return Err(setup(format!(
            "{} doesn't take --call place: only ab-call-app does",
            plan.scenario
        )));
    }
    let mut report = ScenarioReport::new(plan.scenario);
    report
        .notes
        .push(format!("{}: step {}", plan.scenario, plan.step.as_str()));
    let tone = write_burst_tone(paths).map_err(|error| setup(format!("can't write the tone: {error}")))?;
    let options = SessionOptions {
        classifier: Classifier::new(),
        live_members: true,
        capture: Some(plan.scenario.as_str().to_owned()),
        capture_request_bodies: true,
        display: plan.options.display,
        media: Media::Fake { audio: Some(tone) },
        user_agent: Some(USER_AGENT.to_owned()),
    };
    let aliases: Vec<Alias> = if plan.step == CallStep::StartHeld {
        vec![CALLER]
    } else {
        std::iter::once(CALLER)
            .chain(plan.callees.iter().map(|(alias, _)| *alias))
            .collect()
    };
    let sessions = open_all(paths, &aliases, &options).await?;
    Ok(run_sessions(plan, sessions, report).await)
}

/// Runs the opened pages and preserves diagnosis, hang-up and judgment after any phase failure.
async fn run_sessions(
    plan: &CallPlan,
    mut sessions: Vec<(Alias, LabSession)>,
    mut report: ScenarioReport,
) -> ScenarioReport {
    for (_, session) in &sessions {
        // before the page loads: the client sends an earlier run's unsent probe again as soon as it can
        match probe_rules(None) {
            Ok(rules) => rules.into_iter().for_each(|rule| session.force_deny(rule)),
            Err(error) => report.failures.push(format!("can't force the probe denies: {error}")),
        }
    }
    let mut placed = None;
    if let Err(error) = plan.drive(&mut sessions, &mut placed, &mut report).await {
        report.failures.push(error.to_string());
        diagnose(&sessions, &mut report).await;
        hang_up_all(&mut sessions, &mut report).await;
    }
    close_all(sessions, plan, placed, &mut report).await;
    judge_held(plan.step, &mut report);
    report
}

/// A conversation's `leave`, as a URL pattern (`POST`): what a page that hangs up sends to leave the call.
pub const LEAVE_SENDS: &str =
    r"^https://api\.flightproxy\.teams\.microsoft\.com/api/v2/ep/[^/?]+/conv/[^/?]+/leave(\?|$)";

/// The `leave`s a capture's `events.jsonl` text shows its fence letting through ([`LEAVE_SENDS`], each `Network`
/// request once).
#[must_use]
pub fn leaves(text: &str) -> usize {
    allowed_requests(text, "POST", LEAVE_SENDS)
}

/// The text of `session`'s capture so far (its `events.jsonl`); `None` without one.
fn capture_text(session: &LabSession) -> Option<String> {
    std::fs::read_to_string(session.capture_dir()?.join(EVENTS_FILE)).ok()
}

/// A failed run's last step before its browsers close: every page that shows its call's hang-up control hangs up, in
/// order (a's first), and the run waits up to [`HANG_UP_WAIT`] for that page's fence to let a `leave` through
/// ([`leaves`]), so no call of the run is left running in its chat (the service would refuse the chat's next call:
/// [`running_call_conflict`]). What each page did goes into the notes; nothing here fails the run further.
pub(crate) async fn hang_up_all(sessions: &mut [(Alias, LabSession)], report: &mut ScenarioReport) {
    for (alias, session) in sessions.iter_mut() {
        hang_up_after_failure(*alias, session, report).await;
    }
}

/// One page of a failed run hangs up, if it shows the call's hang-up control, and the run waits up to
/// [`HANG_UP_WAIT`] for its fence to let a `leave` through ([`hang_up_all`]'s step for one page, also the native
/// scenarios' for their pages).
pub(crate) async fn hang_up_after_failure(alias: Alias, session: &mut LabSession, report: &mut ScenarioReport) {
    let before = capture_text(session).map_or(0, |text| leaves(&text));
    match hang_up_page(alias, session, report).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            report
                .notes
                .push(format!("{alias}: couldn't hang up after the failure: {error}"));
            return;
        }
    }
    let deadline = tokio::time::Instant::now() + HANG_UP_WAIT;
    loop {
        if capture_text(session).is_some_and(|text| leaves(&text) > before) {
            report.notes.push(format!("{alias}: its leave went by {}", unix_ms()));
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            report
                .notes
                .push(format!("{alias}: no leave went within {HANG_UP_WAIT:?} of its hang-up"));
            return;
        }
        linger(session, Duration::from_millis(500)).await;
    }
}

/// What a native client's call says when the service ended it at once because the chat already has a running call
/// (`teams-headless`'s end word for a `conversationEnd` of 409, 5704), the running call's conversation after it.
pub const NATIVE_CONFLICT: &str = "remote 409 chat has a running call ";

/// Whether a native client's output shows its call ended at once because the chat already has a running call
/// ([`NATIVE_CONFLICT`]): the running call's conversation, as the end names it; `None` when no such end came.
#[must_use]
pub fn native_call_conflict(output: &str) -> Option<String> {
    let (_, rest) = output.split_once(NATIVE_CONFLICT)?;
    let conversation: String = rest.chars().take_while(|c| !c.is_whitespace() && *c != '"').collect();
    Some(if conversation.is_empty() {
        "?".to_owned()
    } else {
        conversation
    })
}

/// Whether a capture's `events.jsonl` text shows its page's call ended by the service at once because the chat already
/// has a running call: a Trouter delivery at a `conversationEnd` callback whose body names code 409, sub-code 5704
/// ("failure to resolve ownership", seen 2026-10-26: a start in a chat whose earlier call still runs). The running
/// call's conversation, as the end names it (`conversationUrl.Location`, its query left out), or `"?"` when it names
/// none; `None` when no such end came.
#[must_use]
pub fn running_call_conflict(text: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["type"] == "cdp" && entry["method"] == "Network.webSocketFrameReceived")
        .find_map(|entry| {
            let delivery = delivery_of("", entry["params"]["response"]["payloadData"].as_str()?)?;
            if !delivery.path.ends_with("/conversationEnd/") {
                return None;
            }
            let end: Value = serde_json::from_slice(&delivery.body).ok()?;
            let conflict = end["code"].as_i64() == Some(409) && end["subCode"].as_i64() == Some(5704);
            conflict.then(|| {
                end.pointer("/conversationUrl/Location")
                    .and_then(Value::as_str)
                    .map_or_else(|| "?".to_owned(), |url| without_query(url).to_owned())
            })
        })
}

/// A failed run's diagnosis: each page's screenshot and the call controls it shows.
async fn diagnose(sessions: &[(Alias, LabSession)], report: &mut ScenarioReport) {
    for (alias, session) in sessions {
        if let Some(note) = save_screenshot(*alias, session).await {
            report.notes.push(note);
        }
        if let Some(note) = call_controls(*alias, session).await {
            report.notes.push(note);
        }
    }
}

/// Closes every browser and finishes every capture, whatever happened to any, each account judged by its step's
/// forced denies and its capture (a callee's by the invitations of the call a placed at `placed`, Unix ms, and by what
/// its role in the step sends).
async fn close_all(
    sessions: Vec<(Alias, LabSession)>,
    plan: &CallPlan,
    placed: Option<u64>,
    report: &mut ScenarioReport,
) {
    let step = plan.step;
    for (alias, session) in sessions {
        let capture = session.capture_dir().map(Path::to_path_buf);
        let labels = if alias == CALLER {
            caller_forced(step)
        } else {
            CALLEE_FORCED
        };
        report.close_account(alias, session, &policy(labels)).await;
        if let Some(dir) = capture {
            if alias == CALLER {
                judge_caller(&dir, step, report);
            } else {
                let last = plan.callees.last().is_some_and(|(callee, _)| *callee == alias);
                let callee = (alias, plan.callee_mri(alias), CalleeRole::of(step, last));
                judge_callee(callee, &plan.call, placed, &dir, report);
            }
            report.notes.push(format!("{alias}: recorded into {}", dir.display()));
        }
    }
}

/// Opens a browser for each of `aliases`, in order; one that can't open closes those opened before it.
async fn open_all(
    paths: &LabPaths,
    aliases: &[Alias],
    options: &SessionOptions,
) -> Result<Vec<(Alias, LabSession)>, ScenarioError> {
    open_all_with(aliases, options, async |alias, options| {
        LabSession::open_with(paths, alias, options).await
    })
    .await
}

async fn open_all_with(
    aliases: &[Alias],
    options: &SessionOptions,
    mut open: impl AsyncFnMut(Alias, SessionOptions) -> Result<LabSession, SessionError>,
) -> Result<Vec<(Alias, LabSession)>, ScenarioError> {
    let mut sessions: Vec<(Alias, LabSession)> = Vec::new();
    for alias in aliases {
        match open(*alias, options.clone()).await {
            Ok(session) => sessions.push((*alias, session)),
            Err(error) => {
                for (_, opened) in sessions {
                    let _ = opened.close().await;
                }
                return Err(error.into());
            }
        }
    }
    Ok(sessions)
}

impl CallPlan {
    /// `alias`'s MRI, `""` (nobody's) for an account the plan doesn't call.
    fn callee_mri(&self, alias: Alias) -> &str {
        self.callees
            .iter()
            .find(|(callee, _)| *callee == alias)
            .map_or("", |(_, mri)| mri.as_str())
    }

    async fn drive(
        &self,
        sessions: &mut [(Alias, LabSession)],
        placed: &mut Option<u64>,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let (caller, callees) = sessions
            .split_first_mut()
            .ok_or_else(|| setup("no caller's browser".to_owned()))?;
        let caller = &mut caller.1;
        self.load_pages(caller, callees, report).await?;
        if self.step == CallStep::StartHeld {
            return start_held(caller, self.held_starts, report).await;
        }
        wait_calling_socket(CALLER, caller, report).await?;
        for (alias, session) in callees.iter_mut() {
            wait_calling_socket(*alias, session, report).await?;
        }
        for (alias, session) in callees.iter() {
            watch_controls(*alias, session).await?;
        }
        self.ring(caller, callees, placed, report).await
    }

    /// Loads every page (a's at the chat, the callees' at the client's start, a joiner's at the chat) and stands up
    /// each one's calling stack.
    async fn load_pages(
        &self,
        caller: &mut LabSession,
        callees: &mut [(Alias, LabSession)],
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        load_at(CALLER, caller, &self.chat_link, self.options.load_timeout).await?;
        size_page(caller).await?;
        report
            .notes
            .push(format!("{CALLER}: the web client loaded at the chat"));
        let joiner = (self.step == CallStep::Join).then(|| callees.len().saturating_sub(1));
        for (index, (alias, session)) in callees.iter_mut().enumerate() {
            // the one who joins later starts at the chat, which the Chat app shows again after the Calls app
            let at = if joiner == Some(index) {
                self.chat_link.as_str()
            } else {
                TEAMS_URL
            };
            load_at(*alias, session, at, self.options.load_timeout).await?;
            size_page(session).await?;
            report.notes.push(format!("{alias}: the web client loaded"));
        }
        stand_up_calling_with(CALLER, &mut caller.driver(), report, self.pauses).await?;
        back_to_chat(CALLER, &mut caller.driver(), report).await?;
        for (alias, session) in callees.iter_mut() {
            stand_up_calling_with(*alias, &mut session.driver(), report, self.pauses).await?;
        }
        Ok(())
    }

    /// a calls with audio (the moment it does into `placed`, Unix ms), each callee must ring, and the step plays out.
    async fn ring(
        &self,
        caller: &mut LabSession,
        callees: &mut [(Alias, LabSession)],
        placed: &mut Option<u64>,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let at = now_ms();
        *placed = Some(at);
        place_call((CALLER, caller), Modality::Audio, report).await?;
        match self.step.callee_action() {
            Some(action) => {
                if let Err(error) = wait_incoming(callees, caller.capture_dir(), report).await {
                    note_ring_controls(callees, report).await;
                    return Err(error);
                }
                self.act(action, at, caller, callees, report).await?;
                wait_ring_end(callees, RING_END, report).await;
            }
            None if self.step == CallStep::Cancel => {
                linger(caller, self.pauses.before_cancel).await;
                hang_up(caller, report).await?;
            }
            None => {
                linger(caller, self.pauses.unanswered).await;
                hang_up(caller, report).await?;
            }
        }
        linger(caller, self.pauses.after_ring).await;
        note_ring_controls(callees, report).await;
        Ok(())
    }

    /// Each callee clicks `action`'s button, but only on the run's own call ([`check_ring`]); a hangs up after
    /// [`AFTER_ACTION`].
    async fn act(
        &self,
        action: CalleeAction,
        placed: u64,
        caller: &mut LabSession,
        callees: &mut [(Alias, LabSession)],
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let answering = if self.step == CallStep::Join {
            callees.len().saturating_sub(1)
        } else {
            callees.len()
        };
        for (alias, session) in callees.iter_mut().take(answering) {
            check_ring(
                (*alias, &mut *session),
                action,
                (&self.call, self.callee_mri(*alias)),
                placed,
                EarlierRings::Judged,
            )
            .await?;
            session
                .driver()
                .click(&format!("{alias} clicks {}", action.what), Selector::Css(action.button))
                .await?;
            report
                .notes
                .push(format!("{alias}: clicked {} at {}", action.what, unix_ms()));
        }
        if matches!(
            self.step,
            CallStep::Answer | CallStep::Video | CallStep::Dropped | CallStep::KeepAlive
        ) {
            converse(self.step, caller, callees, report, self.pauses).await?;
        }
        if self.step == CallStep::Join {
            self.join_late(placed, callees, report).await?;
        }
        linger(caller, self.pauses.after_action).await;
        hang_up(caller, report).await
    }
}

/// Whether `alias` may click `action`'s button on `call`'s ring at `callee` (its MRI), placed at `placed` (Unix ms):
/// within [`INVITATION_WAIT`], its capture must pass [`phase_ring_check`] (every invitation the page received is the
/// run's call, `earlier` saying which earlier ones are left out) and its page must show one incoming call with one such
/// button. Anything else fails the step, nothing clicked.
pub(crate) async fn check_ring(
    (alias, session): (Alias, &mut LabSession),
    action: CalleeAction,
    (call, callee): (&RunCall, &str),
    placed: u64,
    earlier: EarlierRings,
) -> Result<(), ScenarioError> {
    let refuse = |reason: String| setup(format!("{alias} doesn't click {}: {reason}", action.what));
    let file = session
        .capture_dir()
        .map(|dir| dir.join(EVENTS_FILE))
        .ok_or_else(|| refuse("the run records no capture to bind the incoming call to".to_owned()))?;
    let deadline = tokio::time::Instant::now() + INVITATION_WAIT;
    loop {
        let text =
            std::fs::read_to_string(&file).map_err(|error| refuse(format!("can't read the capture: {error}")))?;
        match phase_ring_check(&invitations(&text), call, callee, placed, earlier) {
            RingCheck::Ready => break,
            RingCheck::Refused(reason) => return Err(refuse(reason)),
            RingCheck::Pending if tokio::time::Instant::now() >= deadline => {
                return Err(refuse(format!(
                    "no invitation of the run's call in the capture within {INVITATION_WAIT:?}"
                )));
            }
            RingCheck::Pending => {
                linger(session, Duration::from_millis(500)).await;
            }
        }
    }
    let shown = session
        .driver()
        .evaluate(
            &format!("{alias}'s incoming calls"),
            &format!(
                "document.querySelectorAll({}).length <= 1 && document.querySelectorAll({}).length === 1",
                js_string(INCOMING_SCREEN_CSS),
                js_string(action.button)
            ),
        )
        .await?;
    if shown != Value::Bool(true) {
        return Err(refuse("the page doesn't show exactly one incoming call".to_owned()));
    }
    Ok(())
}

/// Waits up to [`RING_WAIT`] for each callee to show the incoming call; one that doesn't within [`RING_SHOT`] leaves a
/// screenshot of what it shows while it rings (`ringing.png` in its capture). Every [`RING_LOOK`] meanwhile it reads the
/// caller's capture, `caller` (its directory), when there is one: a call the service ended at once because the chat
/// already has a running call ([`running_call_conflict`]) fails the step then, naming that call, rather than when the
/// ring wait runs out.
pub(crate) async fn wait_incoming(
    callees: &mut [(Alias, LabSession)],
    caller: Option<&Path>,
    report: &mut ScenarioReport,
) -> Result<(), ScenarioError> {
    let conflict = || {
        let text = std::fs::read_to_string(caller?.join(EVENTS_FILE)).ok()?;
        running_call_conflict(&text)
    };
    for (alias, session) in callees.iter_mut() {
        let what = format!("{alias} shows the incoming call");
        let started = tokio::time::Instant::now();
        let mut shot = false;
        loop {
            let left = RING_WAIT.saturating_sub(started.elapsed());
            let shown = session
                .driver()
                .wait_for(&what, INCOMING_CALL, left.min(RING_LOOK))
                .await;
            let Err(error) = shown else { break };
            if let Some(conversation) = conflict() {
                return Err(setup(format!(
                    "chat has a running call {conversation}: the service ended {CALLER}'s call at once (409, 5704)"
                )));
            }
            if started.elapsed() >= RING_WAIT {
                return Err(error.into());
            }
            if !shot && started.elapsed() >= RING_SHOT {
                shot = true;
                note_ringing(*alias, session, report).await;
            }
        }
        report
            .notes
            .push(format!("{alias}: the incoming call showed at {}", unix_ms()));
    }
    Ok(())
}

/// What a callee's page shows while it doesn't show the incoming call: its screenshot (`ringing.png` in its capture)
/// and the in-app notifications' controls, into the notes.
async fn note_ringing(alias: Alias, session: &LabSession, report: &mut ScenarioReport) {
    if let Some(note) = save_screenshot_as(alias, session, "ringing.png").await {
        report.notes.push(note);
    }
    let toast = session
        .driver()
        .evaluate(
            "the in-app notifications' controls",
            "JSON.stringify([...document.querySelectorAll('[data-tid=\"app-layout-area--in-app-notifications\"] \
             button, [data-tid=\"app-layout-area--in-app-notifications\"] [role]')].slice(0, 40)\
             .map(e => Object.fromEntries([...e.attributes].filter(a => a.name !== 'style' && a.name !== 'class')\
             .map(a => [a.name, a.value.slice(0, 80)]))))",
        )
        .await
        .unwrap_or_default();
    report
        .notes
        .push(format!("{alias}: the in-app notifications' controls: {toast}"));
}

/// `start-held`: a places each call of `modalities` (an audio call, a video call); each start must be held back (the
/// client tries again by itself: every try is), and the screen each leaves is closed.
async fn start_held(
    caller: &mut LabSession,
    modalities: &[Modality],
    report: &mut ScenarioReport,
) -> Result<(), ScenarioError> {
    caller.force_deny(start_held_rule().map_err(|error| setup(error.to_string()))?);
    for &modality in modalities {
        let before = held_back(&caller.denials(), START_HELD);
        place_call((CALLER, caller), modality, report).await?;
        wait_held(caller, before + 1).await?;
        report
            .notes
            .push(format!("{CALLER}: the start of {} was held back", modality.what()));
        linger(caller, HELD_SETTLE).await;
        let starts = held_back(&caller.denials(), START_HELD) - before;
        report.notes.push(format!(
            "{CALLER}: {starts} start(s) of {} held back (the client tries again by itself)",
            modality.what()
        ));
        hang_up_if_up(caller, report).await?;
        give_up_retry(caller, report).await?;
        caller
            .driver()
            .wait_for(
                &format!("the chat's call buttons after {}", modality.what()),
                CALL_BUTTONS,
                PAGE_OPEN,
            )
            .await?;
    }
    Ok(())
}

/// How a call is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modality {
    Audio,
    Video,
}

impl Modality {
    fn what(self) -> &'static str {
        match self {
            Self::Audio => "an audio call",
            Self::Video => "a video call",
        }
    }

    /// The header's control that places this call: its button, the plain button a narrower header shows, or its item
    /// in the call options (a 1:1 chat's, or a group chat's).
    #[must_use]
    pub fn control(self) -> &'static str {
        match self {
            Self::Audio => {
                r#"[data-tid="default-chat-call-audio-button"], [data-tid="chat-call-audio-button"], [data-tid="dropdown-audio-call-btn"], [data-tid="audio-drop-in-dropdown-menu-item-audio-button"]"#
            }
            Self::Video => {
                r#"[data-tid="default-chat-call-video-button"], [data-tid="chat-call-video-button"], [data-tid="dropdown-video-call-btn"], [data-tid="audio-drop-in-dropdown-menu-item-video-button"]"#
            }
        }
    }
}

/// `alias` (a, but in `ab-call-native` b) places a call from the chat's header ([`Modality::control`]): its button when the header shows one, else its item
/// in the call options; a group chat's confirmation, when it asks for one within [`CONFIRM_WAIT`], is confirmed.
pub(crate) async fn place_call(
    (alias, caller): (Alias, &mut LabSession),
    modality: Modality,
    report: &mut ScenarioReport,
) -> Result<(), ScenarioError> {
    let mut driver = caller.driver();
    let control = Selector::Css(modality.control());
    let shown = driver
        .evaluate(
            "the call's button",
            &format!("document.querySelector({}) !== null", js_string(modality.control())),
        )
        .await?;
    if shown != Value::Bool(true) {
        driver
            .click(&format!("{alias} opens the call options"), CALL_OPTIONS)
            .await?;
        driver
            .wait_for(&format!("the item of {}", modality.what()), control, CONTROL_WAIT)
            .await?;
    }
    driver
        .click(&format!("{alias} starts {}", modality.what()), control)
        .await?;
    report
        .notes
        .push(format!("{alias}: {} clicked at {}", modality.what(), unix_ms()));
    let asks = driver
        .wait_until_true(
            "a group call's confirmation",
            &format!("document.querySelector({}) !== null", js_string(GROUP_CALL_CONFIRM_CSS)),
            "the confirmation",
            CONFIRM_WAIT,
        )
        .await
        .is_ok();
    if asks {
        driver
            .click(&format!("{alias} confirms the group call"), GROUP_CALL_CONFIRM)
            .await?;
        report
            .notes
            .push(format!("{alias}: confirmed the group call at {}", unix_ms()));
    }
    Ok(())
}

/// Clicks the cancel button of the screen the client shows after a call that couldn't start
/// ([`CALL_RETRY_CANCEL`]), if it shows it.
async fn give_up_retry(caller: &mut LabSession, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    let mut driver = caller.driver();
    let shows = driver
        .evaluate(
            "the retry screen",
            &format!("document.querySelector({}) !== null", js_string(CALL_RETRY_CANCEL_CSS)),
        )
        .await?;
    if shows == Value::Bool(true) {
        driver
            .click("a cancels the call that didn't start", CALL_RETRY_CANCEL)
            .await?;
        report
            .notes
            .push(format!("{CALLER}: cancelled the retry screen at {}", unix_ms()));
    }
    Ok(())
}

/// Waits up to [`CALL_START`] until `caller`'s fence has held back `count` call starts.
async fn wait_held(caller: &mut LabSession, count: usize) -> Result<(), ScenarioError> {
    let deadline = tokio::time::Instant::now() + CALL_START;
    loop {
        if held_back(&caller.denials(), START_HELD) >= count {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(setup(format!(
                "a's fence held back no call start number {count} within {CALL_START:?}"
            )));
        }
        linger(caller, Duration::from_millis(500)).await;
    }
}

/// Starts the web client's calling stack in `alias`'s page: the Calls app starts it (the client defers it until then;
/// `echo-call`'s step 2).
pub(crate) async fn stand_up_calling_with(
    alias: Alias,
    driver: &mut Driver,
    report: &mut ScenarioReport,
    pauses: CallPauses,
) -> Result<(), ScenarioError> {
    // the client may load once more on its way to the origin it assigns the account: the app bar has to stay
    driver.wait_for("the Calls app", CALLS_APP, PAGE_OPEN).await?;
    tokio::time::sleep(pauses.page_steady).await;
    driver.wait_for("the Calls app", CALLS_APP, PAGE_OPEN).await?;
    driver.click(&format!("{alias} opens the Calls app"), CALLS_APP).await?;
    tokio::time::sleep(pauses.calling_warm).await;
    report
        .notes
        .push(format!("{alias}: the Calls app started the calling stack"));
    Ok(())
}

/// How long a page's calling stack gets to connect its own Trouter socket once the Calls app started it.
pub const CALLING_SOCKET_WAIT: Duration = Duration::from_secs(60);

/// Whether a capture's `events.jsonl` text shows the calling stack's Trouter socket connected and still open: a socket
/// whose URL names the calling stack's client (`SkypeSpaces`), that received `trouter.connected` and wasn't closed
/// after. Calls placed or rung before it is up reach nobody (seen 2026-10-24: a callee whose calling socket wasn't up
/// got no invitation; a caller whose socket reconnected mid-call missed its acceptance).
#[must_use]
pub fn calling_socket_up(text: &str) -> bool {
    let mut calling: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
    let trouter = regex::Regex::new(TROUTER_SOCKETS).ok();
    for entry in text.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()) {
        if entry["type"] != "cdp" {
            continue;
        }
        let params = &entry["params"];
        let id = params["requestId"].as_str().unwrap_or_default().to_owned();
        match entry["method"].as_str() {
            Some("Network.webSocketCreated") => {
                let url = params["url"].as_str().unwrap_or_default();
                if trouter.as_ref().is_some_and(|socket| socket.is_match(url)) && url.contains("SkypeSpaces") {
                    calling.insert(id, false);
                }
            }
            Some("Network.webSocketClosed") => {
                calling.remove(&id);
            }
            Some("Network.webSocketFrameReceived") => {
                let connected = params["response"]["payloadData"]
                    .as_str()
                    .is_some_and(|data| data.contains("\"name\":\"trouter.connected\""));
                if let (true, Some(up)) = (connected, calling.get_mut(&id)) {
                    *up = true;
                }
            }
            _ => {}
        }
    }
    calling.values().any(|up| *up)
}

/// Waits up to [`CALLING_SOCKET_WAIT`] until `alias`'s capture shows its calling socket up ([`calling_socket_up`]).
pub(crate) async fn wait_calling_socket(
    alias: Alias,
    session: &mut LabSession,
    report: &mut ScenarioReport,
) -> Result<(), ScenarioError> {
    let file = session
        .capture_dir()
        .map(|dir| dir.join(EVENTS_FILE))
        .ok_or_else(|| setup(format!("{alias}'s run records no capture to see its calling socket in")))?;
    let deadline = tokio::time::Instant::now() + CALLING_SOCKET_WAIT;
    loop {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        if calling_socket_up(&text) {
            report.notes.push(format!("{alias}: the calling socket is up"));
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(setup(format!(
                "{alias}'s calling socket wasn't up within {CALLING_SOCKET_WAIT:?}"
            )));
        }
        linger(session, Duration::from_millis(500)).await;
    }
}

/// Back from the Calls app to the chat the page loaded at, in the same document: the Chat app shows the chat it showed
/// last, and its header's call buttons.
pub(crate) async fn back_to_chat(
    alias: Alias,
    driver: &mut Driver,
    report: &mut ScenarioReport,
) -> Result<(), ScenarioError> {
    driver.click(&format!("{alias} opens the Chat app"), CHAT_APP).await?;
    driver
        .wait_for("the chat's call buttons", CALL_BUTTONS, PAGE_OPEN)
        .await?;
    report.notes.push(format!("{alias}: back at the chat"));
    Ok(())
}

/// The chat header's join of a running call (any control there whose `data-tid` names a join).
pub(crate) const CHAT_JOIN: Selector<'static> = Selector::Css(CHAT_JOIN_CSS);
pub(crate) const CHAT_JOIN_CSS: &str = r#"[data-tid="chat-calling-meeting-buttons"] [data-tid*="join" i]"#;
/// How long a running call gets to show its join in the chat's header.
pub const JOIN_WAIT: Duration = Duration::from_secs(30);

/// The open chat's send button, which names the chat it sends to (`data-track-thread-id`, seen 2026-10-25: the page's
/// URL and the chat list's selection don't name the open chat).
const CHAT_SEND_CSS: &str = r#"[data-tid="sendMessageCommands-send"]"#;

/// The page script that reads what a page shows of the chat `chat` (a chat id) and of its header's joins
/// ([`CHAT_JOIN_CSS`]), as JSON for [`chat_shown`]: how many send buttons ([`CHAT_SEND_CSS`]) the page shows
/// (`sends`), how many of them name `chat` exactly (`ours`), how many joins the header shows (`joins`), and where the
/// page names the chat (each element's tag, `data-tid` and attribute name; at most 12, ids only, no page text).
pub(crate) fn chat_shown_script(chat: &str, joins: &str) -> String {
    format!(
        "(() => {{ const chat = {chat}; \
         const sends = [...document.querySelectorAll({sends})]; \
         const where = [...document.querySelectorAll('*')].flatMap((e) => [...e.attributes]\
         .filter((a) => a.value.includes(chat))\
         .map((a) => `${{e.tagName.toLowerCase()}}[${{e.getAttribute('data-tid') ?? ''}}].${{a.name}}`)).slice(0, 12); \
         return JSON.stringify({{ sends: sends.length, \
         ours: sends.filter((e) => e.getAttribute('data-track-thread-id') === chat).length, \
         joins: document.querySelectorAll({joins}).length, where }}); }})()",
        chat = js_string(chat),
        sends = js_string(CHAT_SEND_CSS),
        joins = js_string(joins),
    )
}

/// Whether a page shows the run's chat with one join to click, by what [`chat_shown_script`] read (`reading`, its
/// JSON): the header shows exactly one join, and the page shows one open chat's send button, naming the run's chat.
///
/// # Errors
/// The reason, when the page doesn't show the run's chat with one join, or the reading doesn't read.
pub fn chat_shown(reading: &str) -> Result<(), String> {
    let reading: Value = serde_json::from_str(reading).map_err(|error| format!("the page's reading: {error}"))?;
    let count = |key: &str| reading[key].as_u64().unwrap_or(0);
    if count("joins") != 1 {
        return Err(format!("the header shows {} join(s), not one", count("joins")));
    }
    match (count("sends"), count("ours")) {
        (1, 1) => Ok(()),
        (sends, ours) => Err(format!(
            "the page shows {sends} open chat(s), {ours} of them the run's (the chat named at {})",
            reading["where"]
        )),
    }
}

impl CallPlan {
    /// `join`: every callee but the last answered; the last one's ring ends unanswered ([`RING_END`]), its page goes
    /// back to the chat (the Chat app shows the chat it loaded at), and, only on the run's own call
    /// ([`Self::check_join`]), clicks the header's join of the running call ([`CHAT_JOIN`]); its call screen must then
    /// show within [`CONNECT_WAIT`]. Then the others hang up.
    async fn join_late(
        &self,
        placed: u64,
        callees: &mut [(Alias, LabSession)],
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let Some(last) = callees.len().checked_sub(1) else {
            return Ok(());
        };
        for (other, session) in &mut callees[..last] {
            session
                .driver()
                .wait_for(&format!("{other}'s call screen"), HANG_UP, CONNECT_WAIT)
                .await?;
            report.notes.push(format!("{other}: in the call at {}", unix_ms()));
        }
        wait_ring_end(&mut callees[last..], RING_END, report).await;
        let (alias, joiner) = &mut callees[last];
        let alias = *alias;
        open_join(alias, joiner, report).await?;
        self.check_join(alias, joiner, placed, report).await?;
        let mut driver = joiner.driver();
        driver
            .click(&format!("{alias} joins the running call"), CHAT_JOIN)
            .await?;
        report.notes.push(format!("{alias}: clicked join at {}", unix_ms()));
        driver
            .wait_for(&format!("{alias}'s call screen after the join"), HANG_UP, CONNECT_WAIT)
            .await?;
        report.notes.push(format!("{alias}: in the call at {}", unix_ms()));
        linger(joiner, self.pauses.after_action).await;
        if let Some(note) = save_screenshot_as(alias, joiner, "joined.png").await {
            report.notes.push(note);
        }
        for (other, session) in &mut callees[..last] {
            hang_up_page(*other, session, report).await?;
        }
        Ok(())
    }

    /// Whether `alias` may click the chat's join: its capture must pass [`ring_check`] (every invitation the page
    /// received is the run's call, placed at `placed`, and there is one) and its page must show the run's group chat
    /// with one join ([`chat_shown`]). Anything else fails the step, nothing clicked.
    async fn check_join(
        &self,
        alias: Alias,
        session: &LabSession,
        placed: u64,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let refuse = |reason: String| setup(format!("{alias} doesn't click join: {reason}"));
        let chat = self
            .call
            .group_chat
            .as_deref()
            .ok_or_else(|| refuse("the run calls no group chat".to_owned()))?;
        let text = session
            .capture_dir()
            .map(|dir| std::fs::read_to_string(dir.join(EVENTS_FILE)))
            .ok_or_else(|| refuse("the run records no capture to bind the running call to".to_owned()))?
            .map_err(|error| refuse(format!("can't read the capture: {error}")))?;
        match ring_check(&invitations(&text), &self.call, self.callee_mri(alias), placed) {
            RingCheck::Ready => {}
            RingCheck::Refused(reason) => return Err(refuse(reason)),
            RingCheck::Pending => return Err(refuse("no invitation of the run's call reached the page".to_owned())),
        }
        let reading = session
            .driver()
            .evaluate(&format!("{alias}'s chat"), &chat_shown_script(chat, CHAT_JOIN_CSS))
            .await?;
        chat_shown(reading.as_str().unwrap_or_default()).map_err(refuse)?;
        report
            .notes
            .push(format!("{alias}: the page shows the run's chat and one join"));
        Ok(())
    }
}

/// `alias`'s page opens the Chat app (the chat it showed last) and waits up to [`JOIN_WAIT`] for its header's join of
/// the running call; the call controls it shows go into the notes.
pub(crate) async fn open_join(
    alias: Alias,
    joiner: &LabSession,
    report: &mut ScenarioReport,
) -> Result<(), ScenarioError> {
    let mut driver = joiner.driver();
    driver.click(&format!("{alias} opens the Chat app"), CHAT_APP).await?;
    driver
        .wait_for(&format!("{alias}'s chat call buttons"), CALL_BUTTONS, PAGE_OPEN)
        .await?;
    let shown = driver
        .wait_for(&format!("{alias}'s join of the running call"), CHAT_JOIN, JOIN_WAIT)
        .await;
    if let Some(note) = call_controls(alias, joiner).await {
        report.notes.push(note);
    }
    shown.map_err(ScenarioError::from)
}

/// How long an answered call gets to show its call screen on every callee.
pub const CONNECT_WAIT: Duration = Duration::from_secs(30);
/// How long each page gets to hear the other side's tone once its noise suppression is off.
pub const TONE_WAIT: Duration = Duration::from_secs(25);

/// The answered call: every callee shows the call screen within [`CONNECT_WAIT`], every page turns its noise
/// suppression off and its call to another microphone (the client takes the steady tone for noise, and its microphone
/// starts silent), whether each page heard the other side's tone within [`TONE_WAIT`] is noted (the WebRTC hooks'
/// audio tap, in each capture) and its call audio must go both ways by the WebRTC statistics ([`judge_audio`]); in
/// `video` every page then turns its camera on and must decode video ([`VIDEO_WAIT`]), in `dropped` the last callee
/// drops off the network for [`DROPPED_FOR`], its calling socket closed, and the call must still show on every page
/// [`AFTER_DROP`] after it came back ([`drop_off`]); then each callee hangs up.
async fn converse(
    step: CallStep,
    caller: &mut LabSession,
    callees: &mut [(Alias, LabSession)],
    report: &mut ScenarioReport,
    pauses: CallPauses,
) -> Result<(), ScenarioError> {
    for (alias, session) in callees.iter_mut() {
        session
            .driver()
            .wait_for(&format!("{alias}'s call screen"), HANG_UP, CONNECT_WAIT)
            .await?;
        report.notes.push(format!("{alias}: in the call at {}", unix_ms()));
    }
    open_microphones(caller, callees, report).await?;
    note_tones(caller, callees, report).await;
    judge_audio(caller, callees, report);
    match step {
        CallStep::Video => show_video(caller, callees, report, pauses).await?,
        CallStep::Dropped => drop_off(caller, callees, report, pauses).await?,
        CallStep::KeepAlive => keep_alive(caller, callees, report, pauses).await,
        _ => {}
    }
    for (alias, session) in callees.iter_mut() {
        if !hang_up_page(*alias, session, report).await? {
            report
                .notes
                .push(format!("{alias}: the call screen was gone: nothing to hang up"));
        }
    }
    Ok(())
}

/// Every page turns its noise suppression off and its call to another microphone, and notes what its microphone
/// button says.
async fn open_microphones(
    caller: &mut LabSession,
    callees: &mut [(Alias, LabSession)],
    report: &mut ScenarioReport,
) -> Result<(), ScenarioError> {
    turn_off_noise_suppression(CALLER, &mut caller.driver(), report).await?;
    switch_microphone(CALLER, &mut caller.driver(), report).await?;
    for (alias, session) in callees.iter_mut() {
        turn_off_noise_suppression(*alias, &mut session.driver(), report).await?;
        switch_microphone(*alias, &mut session.driver(), report).await?;
    }
    note_microphone(CALLER, caller, report).await;
    for (alias, session) in callees.iter_mut() {
        note_microphone(*alias, session, report).await;
    }
    Ok(())
}

/// Notes whether each page heard the other side's tone within [`TONE_WAIT`].
async fn note_tones(caller: &mut LabSession, callees: &mut [(Alias, LabSession)], report: &mut ScenarioReport) {
    let mut heard = vec![(CALLER, wait_for_tone(caller, TONE_WAIT).await)];
    for (alias, session) in callees.iter_mut() {
        heard.push((*alias, wait_for_tone(session, TONE_WAIT).await));
    }
    for (alias, tone) in heard {
        report.notes.push(match tone {
            ToneWait::Back(after) => format!("{alias}: heard the other side's tone {after:?} into the wait"),
            // the client's audio processing silences the fake microphone's tone in many runs (seen 2026-10-24):
            // the audio is judged by the statistics, the tone only noted
            other => format!("{alias}: didn't hear the other side's tone ({other:?})"),
        });
    }
}

/// Each page's call audio must go both ways by the WebRTC statistics in its capture; notes the packets and energy.
fn judge_audio(caller: &mut LabSession, callees: &mut [(Alias, LabSession)], report: &mut ScenarioReport) {
    let mut pages: Vec<(Alias, &mut LabSession)> = vec![(CALLER, &mut *caller)];
    pages.extend(callees.iter_mut().map(|(alias, session)| (*alias, session)));
    for (alias, session) in pages {
        let text = session
            .capture_dir()
            .and_then(|dir| std::fs::read_to_string(dir.join(EVENTS_FILE)).ok())
            .unwrap_or_default();
        let audio = AudioSummary::from_events(&text);
        report.notes.push(format!(
            "{alias}: audio {} packet(s) sent, {} received, energy {:.4}",
            audio.packets_sent, audio.packets_received, audio.received_energy
        ));
        if !audio.both_ways() {
            report
                .failures
                .push(format!("{alias}'s call audio didn't go both ways (WebRTC statistics)"));
        }
    }
}

/// Notes what `alias`'s call screen says of its microphone (the microphone button's label: muted or not).
pub(crate) async fn note_microphone(alias: Alias, session: &LabSession, report: &mut ScenarioReport) {
    let mut driver = session.driver();
    driver.set_call_timeout(Duration::from_secs(10));
    let label = driver
        .evaluate(
            &format!("{alias}'s microphone button"),
            "document.querySelector('#microphone-button')?.getAttribute('aria-label') ?? null",
        )
        .await
        .unwrap_or_default();
    report
        .notes
        .push(format!("{alias}: the microphone button says {label}"));
}

/// How long `keep-alive` holds the answered call: past the call's keep-alive interval (`callKeepAliveInterval`, 2700 s
/// in every capture; the web client sends its keep-alive at 90 % of it).
pub const KEEP_ALIVE_HOLD: Duration = Duration::from_mins(47);
/// How often a long hold looks whether the caller's call screen still shows.
const HOLD_CHECK: Duration = Duration::from_secs(60);

/// `keep-alive`: holds the answered call [`KEEP_ALIVE_HOLD`] ([`hold_long`]); a call that ended before fails the run,
/// and so does a page whose fence let no keep-alive through ([`keep_alives`]).
async fn keep_alive(
    caller: &mut LabSession,
    callees: &mut [(Alias, LabSession)],
    report: &mut ScenarioReport,
    pauses: CallPauses,
) {
    if !hold_long(caller, callees, pauses.keep_alive_hold, pauses.hold_check, report).await {
        report.failures.push(format!(
            "the call ended before it was held {:?}: no keep-alive is due before then",
            pauses.keep_alive_hold
        ));
    }
    let mut pages: Vec<(Alias, &mut LabSession)> = vec![(CALLER, &mut *caller)];
    pages.extend(callees.iter_mut().map(|(alias, session)| (*alias, session)));
    for (alias, session) in pages {
        let text = session
            .capture_dir()
            .and_then(|dir| std::fs::read_to_string(dir.join(EVENTS_FILE)).ok())
            .unwrap_or_default();
        match keep_alives(&text) {
            0 => report
                .failures
                .push(format!("{alias}'s fence let no keep-alive of the call through")),
            sent => report
                .notes
                .push(format!("{alias}: the fence let {sent} keep-alive(s) through")),
        }
    }
}

/// Holds the answered call `hold`, every page's events taken in turn, the caller's call screen looked at every
/// `check_every` ([`HOLD_CHECK`] in a live run): a call that ended meanwhile ends the hold (noted with its time).
/// Whether the call was held the whole `hold`.
async fn hold_long(
    caller: &mut LabSession,
    callees: &mut [(Alias, LabSession)],
    hold: Duration,
    check_every: Duration,
    report: &mut ScenarioReport,
) -> bool {
    let started = tokio::time::Instant::now();
    report
        .notes
        .push(format!("{CALLER}: holding the call from {}", unix_ms()));
    let share = check_every / u32::try_from(callees.len() + 1).unwrap_or(1);
    while started.elapsed() < hold {
        linger(caller, share).await;
        for (_, session) in callees.iter_mut() {
            linger(session, share).await;
        }
        let mut driver = caller.driver();
        driver.set_call_timeout(Duration::from_secs(10));
        let up = driver
            .evaluate(
                &format!("{CALLER}'s call screen"),
                &format!("document.querySelector({}) !== null", js_string(HANG_UP_CSS)),
            )
            .await;
        if !matches!(up, Ok(Value::Bool(true))) {
            report.notes.push(format!(
                "{CALLER}: the call screen went {:?} into the hold, at {}",
                started.elapsed(),
                unix_ms()
            ));
            return false;
        }
    }
    report.notes.push(format!(
        "{CALLER}: held the call {:?}, until {}",
        started.elapsed(),
        unix_ms()
    ));
    true
}

/// The call screen's camera button.
pub(crate) const CAMERA: Selector<'static> = Selector::Css("#video-button");
/// How long every page gets to decode the others' video once the cameras are on.
pub const VIDEO_WAIT: Duration = Duration::from_secs(45);
/// How long the run records with the cameras on.
pub const VIDEO_FOR: Duration = Duration::from_secs(15);

/// `video`: every page turns its camera on (Chrome's fake camera); then each page's capture must show video frames
/// decoded ([`video_frames`]) within [`VIDEO_WAIT`].
async fn show_video(
    caller: &mut LabSession,
    callees: &mut [(Alias, LabSession)],
    report: &mut ScenarioReport,
    pauses: CallPauses,
) -> Result<(), ScenarioError> {
    caller.driver().click("a turns its camera on", CAMERA).await?;
    report.notes.push(format!("{CALLER}: camera on at {}", unix_ms()));
    for (alias, session) in callees.iter_mut() {
        session
            .driver()
            .click(&format!("{alias} turns its camera on"), CAMERA)
            .await?;
        report.notes.push(format!("{alias}: camera on at {}", unix_ms()));
    }
    linger(caller, pauses.video_for).await;
    let mut pages: Vec<(Alias, &mut LabSession)> = vec![(CALLER, caller)];
    pages.extend(callees.iter_mut().map(|(alias, session)| (*alias, session)));
    for (alias, session) in pages {
        let file = session.capture_dir().map(|dir| dir.join(EVENTS_FILE));
        let deadline = tokio::time::Instant::now() + pauses.video_wait;
        loop {
            let frames = file.as_ref().map_or(0, |file| {
                video_frames(&std::fs::read_to_string(file).unwrap_or_default())
            });
            if frames > 0 {
                report.notes.push(format!("{alias}: decoded {frames} video frame(s)"));
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                report
                    .failures
                    .push(format!("{alias} decoded no video within {:?}", pauses.video_wait));
                break;
            }
            linger(session, Duration::from_secs(1)).await;
        }
    }
    Ok(())
}

/// The most video frames a capture's WebRTC statistics report decoded (`inbound-rtp`, kind `video`,
/// `framesDecoded`).
#[must_use]
pub fn video_frames(text: &str) -> u64 {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["type"] == "rtc" && entry["payload"]["event"] == "stats")
        .flat_map(|entry| entry["payload"]["data"].as_array().cloned().unwrap_or_default())
        .filter(|stat| stat["type"] == "inbound-rtp" && stat["kind"] == "video")
        .filter_map(|stat| stat["framesDecoded"].as_u64())
        .max()
        .unwrap_or(0)
}

/// Closes the calling stack's Trouter sockets in `session`'s page (every open `WebSocket` of the page whose URL names
/// the calling stack's client, `SkypeSpaces`), found through `DevTools` `Runtime.queryObjects`: a close sends no data,
/// only the socket's closing handshake. How many it closed (0 when it couldn't look).
async fn close_calling_socket(session: &LabSession) -> u64 {
    let (cdp, page) = (session.cdp(), session.page());
    let group = json!("lab-dropped");
    let Ok(prototype) = cdp
        .call(
            Some(page),
            "Runtime.evaluate",
            json!({"expression": "WebSocket.prototype", "objectGroup": group}),
        )
        .await
    else {
        return 0;
    };
    let Some(prototype) = prototype["result"]["objectId"].as_str() else {
        return 0;
    };
    let Ok(sockets) = cdp
        .call(
            Some(page),
            "Runtime.queryObjects",
            json!({"prototypeObjectId": prototype, "objectGroup": group}),
        )
        .await
    else {
        return 0;
    };
    let Some(sockets) = sockets["objects"]["objectId"].as_str() else {
        return 0;
    };
    let closed = cdp
        .call(
            Some(page),
            "Runtime.callFunctionOn",
            json!({
                "objectId": sockets,
                "functionDeclaration": "function () { const calling = this.filter(s => s.readyState === 1 && \
                    String(s.url).includes('SkypeSpaces')); calling.forEach(s => s.close()); return calling.length; }",
                "returnByValue": true,
            }),
        )
        .await
        .ok()
        .and_then(|answer| answer["result"]["value"].as_u64())
        .unwrap_or(0);
    let _ = cdp
        .call(Some(page), "Runtime.releaseObjectGroup", json!({"objectGroup": group}))
        .await;
    closed
}

/// How long the last callee's page stays off the network in `dropped`.
pub const DROPPED_FOR: Duration = Duration::from_secs(10);
/// How long the run waits after the page came back before it looks at the call.
pub const AFTER_DROP: Duration = Duration::from_secs(25);

/// How long the closed calling socket gets to show closed in the capture.
pub const SOCKET_CLOSE_WAIT: Duration = Duration::from_secs(5);

/// `dropped`: the last callee's page goes off the network for [`DROPPED_FOR`] (the `DevTools` network emulation: its
/// new requests fail, but its open sockets stay, so the run closes its calling socket itself, [`drop_calling_socket`];
/// the call's media goes on) and comes back; [`AFTER_DROP`] later every page must still show the call. No calling
/// socket closed fails the step: the run wouldn't test the interruption.
async fn drop_off(
    caller: &mut LabSession,
    callees: &mut [(Alias, LabSession)],
    report: &mut ScenarioReport,
    pauses: CallPauses,
) -> Result<(), ScenarioError> {
    let Some((alias, session)) = callees.last_mut() else {
        return Ok(());
    };
    let alias = *alias;
    let offline =
        |offline: bool| json!({"offline": offline, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1});
    session
        .cdp()
        .call(Some(session.page()), "Network.emulateNetworkConditions", offline(true))
        .await
        .map_err(|error| setup(format!("{alias} can't go off the network: {error}")))?;
    report.notes.push(format!("{alias}: off the network at {}", unix_ms()));
    let dropped = drop_calling_socket(alias, session, report).await;
    if dropped.is_ok() {
        linger(session, pauses.dropped_for).await;
    }
    session
        .cdp()
        .call(Some(session.page()), "Network.emulateNetworkConditions", offline(false))
        .await
        .map_err(|error| setup(format!("{alias} can't come back on the network: {error}")))?;
    report
        .notes
        .push(format!("{alias}: back on the network at {}", unix_ms()));
    dropped?;
    linger(caller, pauses.after_drop).await;
    let mut pages: Vec<(Alias, &mut LabSession)> = vec![(CALLER, caller)];
    pages.extend(callees.iter_mut().map(|(alias, session)| (*alias, session)));
    for (alias, session) in pages {
        let up = session
            .driver()
            .evaluate(
                &format!("{alias}'s call screen"),
                &format!("document.querySelector({}) !== null", js_string(HANG_UP_CSS)),
            )
            .await?;
        if up == Value::Bool(true) {
            report.notes.push(format!("{alias}: the call is still up"));
        } else {
            report
                .failures
                .push(format!("{alias}'s call didn't outlive the dropped page"));
        }
    }
    Ok(())
}

/// Closes `alias`'s calling socket ([`close_calling_socket`]; the network emulation keeps open sockets) and waits up
/// to [`SOCKET_CLOSE_WAIT`] for its capture to show no calling socket up ([`calling_socket_up`]).
///
/// # Errors
/// When it closed none, or the capture still shows one up.
async fn drop_calling_socket(
    alias: Alias,
    session: &mut LabSession,
    report: &mut ScenarioReport,
) -> Result<(), ScenarioError> {
    let closed = close_calling_socket(session).await;
    report
        .notes
        .push(format!("{alias}: closed {closed} calling socket(s) at {}", unix_ms()));
    if closed == 0 {
        return Err(setup(format!(
            "{alias}'s page closed no calling socket: the run can't test the interruption"
        )));
    }
    let file = session.capture_dir().map(|dir| dir.join(EVENTS_FILE)).ok_or_else(|| {
        setup(format!(
            "{alias}'s run records no capture to see its calling socket close in"
        ))
    })?;
    let deadline = tokio::time::Instant::now() + SOCKET_CLOSE_WAIT;
    loop {
        if !calling_socket_up(&std::fs::read_to_string(&file).unwrap_or_default()) {
            report
                .notes
                .push(format!("{alias}: the calling socket closed by {}", unix_ms()));
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(setup(format!(
                "{alias}'s calling socket didn't close within {SOCKET_CLOSE_WAIT:?}"
            )));
        }
        linger(session, Duration::from_millis(250)).await;
    }
}

/// The call screen's microphone choices (in its audio options) and one not chosen.
const MICROPHONE_CHOICE: Selector<'static> =
    Selector::Css(r#"[data-tid="microphone-settings"] [role="option"][aria-selected="false"]"#);
const MICROPHONE_CHOSEN_CSS: &str = r#"[data-tid="microphone-settings"] [role="option"][aria-selected="true"]"#;

/// Switches `alias`'s call to another (fake) microphone from the call screen's audio options: the call's microphone
/// stays silent until it is opened again with the noise suppression off (seen 2026-10-24: the level meter flat and
/// the sent track silent while Chrome's fake input plays the tone), and choosing another one opens it again. A setting
/// of the account's own client, reaching nobody.
pub(crate) async fn switch_microphone(
    alias: Alias,
    driver: &mut Driver,
    report: &mut ScenarioReport,
) -> Result<(), ScenarioError> {
    driver
        .click(
            &format!("{alias} opens the audio options"),
            super::echo_call::AUDIO_OPTIONS,
        )
        .await?;
    driver
        .wait_for(&format!("{alias}'s other microphone"), MICROPHONE_CHOICE, CONTROL_WAIT)
        .await?;
    let before = format!("document.querySelector({})?.id", js_string(MICROPHONE_CHOSEN_CSS));
    let chosen = driver.evaluate(&format!("{alias}'s microphone"), &before).await?;
    driver
        .click(&format!("{alias} chooses another microphone"), MICROPHONE_CHOICE)
        .await?;
    driver
        .wait_until_true(
            &format!("{alias}'s microphone"),
            &format!("{before} !== {chosen}"),
            "another microphone chosen",
            CONTROL_WAIT,
        )
        .await?;
    driver
        .click(
            &format!("{alias} closes the audio options"),
            super::echo_call::AUDIO_OPTIONS,
        )
        .await?;
    report
        .notes
        .push(format!("{alias}: switched the call to another microphone"));
    Ok(())
}

/// a hangs up the call it placed.
async fn hang_up(caller: &mut LabSession, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    if !hang_up_if_up(caller, report).await? {
        report
            .notes
            .push(format!("{CALLER}: the call screen was gone: nothing to hang up"));
    }
    Ok(())
}

/// Clicks a's hang-up button if the call screen shows it; whether it did.
async fn hang_up_if_up(caller: &mut LabSession, report: &mut ScenarioReport) -> Result<bool, ScenarioError> {
    hang_up_page(CALLER, caller, report).await
}

/// Clicks `alias`'s hang-up button if its call screen shows it; whether it did.
pub(crate) async fn hang_up_page(
    alias: Alias,
    session: &LabSession,
    report: &mut ScenarioReport,
) -> Result<bool, ScenarioError> {
    let mut driver = session.driver();
    let up = driver
        .evaluate(
            &format!("{alias}'s call screen"),
            &format!("document.querySelector({}) !== null", js_string(HANG_UP_CSS)),
        )
        .await?;
    if up != Value::Bool(true) {
        return Ok(false);
    }
    driver.click(&format!("{alias} hangs up"), HANG_UP).await?;
    report.notes.push(format!("{alias}: hung up at {}", unix_ms()));
    Ok(true)
}

pub(crate) const HANG_UP_CSS: &str = r#"#hangup-button, [data-tid="hangup-main-btn"]"#;

/// Waits up to `limit` until no callee shows the incoming call any more, noting when each one's went (or that it
/// didn't).
async fn wait_ring_end(callees: &mut [(Alias, LabSession)], limit: Duration, report: &mut ScenarioReport) {
    let deadline = tokio::time::Instant::now() + limit;
    let shows = format!("document.querySelector({}) !== null", js_string(INCOMING_CALL_CSS));
    for (alias, session) in callees.iter_mut() {
        let mut driver = session.driver();
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let gone = driver
            .wait_until_true(
                &format!("{alias}'s ring ends"),
                &format!("!({shows})"),
                "the incoming call to go",
                left,
            )
            .await;
        report.notes.push(match gone {
            Ok(()) => format!("{alias}: the incoming call went by {}", unix_ms()),
            Err(error) => format!("{alias}: the incoming call stayed: {error}"),
        });
    }
}

/// The page script that records, from now on, the first moment each element `data-tid` the page didn't show when it
/// started shows in the page (`window.__labRingControls`, at most 300): what a callee's page showed while it rang,
/// however briefly. Ids of the client's own, no text of the page.
const WATCH_CONTROLS: &str = "(() => { const seen = window.__labRingControls = window.__labRingControls || {};      let count = 0; const note = (at) => document.querySelectorAll('[data-tid]').forEach(e => {        const t = e.getAttribute('data-tid'); if (!(t in seen) && count < 300) { seen[t] = at; if (at) count++; } });      note(0); new MutationObserver(() => note(Date.now())).observe(document.documentElement, {subtree: true,        childList: true, attributes: true, attributeFilter: ['data-tid']}); return true; })()";

/// Starts recording what `alias`'s page shows while it rings ([`WATCH_CONTROLS`]).
pub(crate) async fn watch_controls(alias: Alias, session: &LabSession) -> Result<(), ScenarioError> {
    session
        .driver()
        .evaluate(&format!("{alias}'s ring controls"), WATCH_CONTROLS)
        .await?;
    Ok(())
}

/// Notes what each callee's page showed while it rang ([`WATCH_CONTROLS`]): each control's id and when it first showed.
async fn note_ring_controls(callees: &[(Alias, LabSession)], report: &mut ScenarioReport) {
    for (alias, session) in callees {
        let mut driver = session.driver();
        driver.set_call_timeout(Duration::from_secs(10));
        if let Ok(seen) = driver
            .evaluate(
                &format!("{alias}'s ring controls"),
                "JSON.stringify(Object.fromEntries(Object.entries(window.__labRingControls || {}).filter(([, at]) => at)))",
            )
            .await
        {
            report
                .notes
                .push(format!("{alias}: controls shown since the call was placed: {seen}"));
        }
    }
}

/// The call controls `session`'s page shows (each element's `data-tid` naming a call, a ring, an answer or a decline)
/// and the app bar's buttons, for a failed run's diagnosis: ids of the client's own, no text of the page.
async fn call_controls(alias: Alias, session: &LabSession) -> Option<String> {
    let mut driver = session.driver();
    driver.set_call_timeout(Duration::from_secs(10));
    let tids = driver
        .evaluate(
            "the call controls",
            "[...new Set([...document.querySelectorAll('[data-tid]')].map(e => e.getAttribute('data-tid'))\
             .filter(t => /call|ring|accept|reject|hangup/i.test(t)))].slice(0, 80)",
        )
        .await
        .ok()?;
    let header = driver
        .evaluate(
            "the chat header's call buttons",
            "[...document.querySelectorAll('[data-tid=\"chat-calling-meeting-buttons\"] button, \
             [role=\"menu\"] [role=\"menuitem\"]')]\
             .map(e => [e.getAttribute('data-tid'), e.getAttribute('aria-label')]).slice(0, 40)",
        )
        .await
        .unwrap_or_default();
    Some(format!(
        "{alias}: call controls in the page: {tids}; the header's call buttons and menu items: {header}"
    ))
}

/// The caller's capture: in `start-held` no start may have been let through; otherwise exactly the fence's permit of
/// the audio call's start.
fn judge_caller(capture: &Path, step: CallStep, report: &mut ScenarioReport) {
    let text = match std::fs::read_to_string(capture.join(EVENTS_FILE)) {
        Ok(text) => text,
        Err(error) => {
            report.failures.push(format!("can't read a's capture: {error}"));
            return;
        }
    };
    let (allowed, denied) = call_starts(&text);
    report.notes.push(format!(
        "call start: {allowed} allowed and {denied} denied by the fence"
    ));
    match step {
        CallStep::StartHeld if allowed > 0 => report
            .failures
            .push(format!("the fence let {allowed} call start(s) through in start-held")),
        CallStep::StartHeld => {}
        _ if allowed == 0 => report.failures.push("the fence allowed no call start".to_owned()),
        _ => {}
    }
}

/// A callee's capture: the run's call's invitation must have reached its page ([`own_invitations`]; any other is only
/// noted), its attach let through, and what its `role` sends ([`judge_step_sends`]).
pub(crate) fn judge_callee(
    (alias, callee, role): (Alias, &str, CalleeRole),
    call: &RunCall,
    placed: Option<u64>,
    capture: &Path,
    report: &mut ScenarioReport,
) {
    match std::fs::read_to_string(capture.join(EVENTS_FILE)) {
        Ok(text) => {
            let all = invitations(&text);
            let own = placed.map_or(0, |placed| own_invitations(&all, call, callee, placed));
            report.notes.push(format!(
                "{alias}: {own} invitation delivery(ies) of the run's call received, {} other(s)",
                all.len() - own
            ));
            if own == 0 {
                report
                    .failures
                    .push(format!("{alias}'s page received no invitation of the run's call"));
            }
            let sends = ring_sends(&text);
            report.notes.push(format!(
                "{alias}: the fence let {} attach(es) and {} invitation receipt(s) through",
                sends.attaches, sends.receipts
            ));
            if own > 0 && sends.attaches == 0 {
                report
                    .failures
                    .push(format!("{alias}'s fence let no attach of the run's call through"));
            }
            match judge_step_sends(role, &text) {
                Ok(Some(note)) => report.notes.push(format!("{alias}: {note}")),
                Ok(None) => {}
                Err(failure) => report.failures.push(format!("{alias}: {failure}")),
            }
        }
        Err(error) => report.failures.push(format!("can't read {alias}'s capture: {error}")),
    }
}

/// What a callee does in a run, for what its capture must show it sent ([`judge_step_sends`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalleeRole {
    /// It declines (`decline`).
    Declines,
    /// It joins the running call late (`join`'s last callee).
    Joins,
    /// Anything else: its sends are judged elsewhere (an answered call's by the call itself) or it sends nothing.
    Other,
}

impl CalleeRole {
    /// The role of a callee in `step`, `last` when it is the run's last callee.
    #[must_use]
    pub fn of(step: CallStep, last: bool) -> Self {
        match step {
            CallStep::Decline => Self::Declines,
            CallStep::Join if last => Self::Joins,
            _ => Self::Other,
        }
    }
}

/// What a callee's capture (`text`, its `events.jsonl`) must show it sent in its `role`: one that declines, its
/// rejection let through by the fence ([`DECLINE_SENDS`], [`allowed_requests`]); one that joins, its join of the running
/// call let through ([`CONVERSATION_JOIN`]). A note of what it shows, or the failure.
///
/// # Errors
/// The failure, when the capture doesn't show it.
pub fn judge_step_sends(role: CalleeRole, text: &str) -> Result<Option<String>, String> {
    match role {
        CalleeRole::Declines => match allowed_requests(text, "DELETE", DECLINE_SENDS) {
            0 => Err("the fence let no rejection of the call through".to_owned()),
            rejects => Ok(Some(format!("the fence let {rejects} rejection(s) through"))),
        },
        CalleeRole::Joins => match allowed_requests(text, "POST", CONVERSATION_JOIN) {
            0 => Err("the fence let no join of the running call through".to_owned()),
            joins => Ok(Some(format!(
                "the fence let {joins} join(s) of the running call through"
            ))),
        },
        CalleeRole::Other => Ok(None),
    }
}

/// The requests a capture's `events.jsonl` text shows its fence letting through: each `method` request whose URL
/// matches `pattern`, allowed, once per `Network` request (the browser decides a request on two layers).
#[must_use]
pub fn allowed_requests(text: &str, method: &str, pattern: &str) -> usize {
    let Ok(pattern) = regex::Regex::new(pattern) else {
        return 0;
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["type"] == "verdict" && entry["verdict"] == "allow" && entry["channel"] == "http")
        .filter(|entry| entry["method"] == method && pattern.is_match(entry["url"].as_str().unwrap_or_default()))
        .map(|entry| entry["network_id"].to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

/// What a callee's fence let through while it rang ([`ring_sends`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RingSends {
    /// `attach` requests allowed (each `Network` request once).
    pub attaches: usize,
    /// Delivery responses (`3:::`) allowed on a Trouter socket.
    pub receipts: usize,
}

/// The ring-time sends a capture's `events.jsonl` text shows its fence letting through: each `POST` to an `attach`
/// link ([`ATTACH`]) allowed, and each delivery response allowed on a Trouter socket.
#[must_use]
pub fn ring_sends(text: &str) -> RingSends {
    let trouter = regex::Regex::new(TROUTER_SOCKETS).ok();
    let receipts = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["type"] == "verdict" && entry["verdict"] == "allow" && entry["channel"] == "web_socket")
        .filter(|entry| {
            let response = entry["frame"]["data"]
                .as_str()
                .is_some_and(|data| data.starts_with(DELIVERY_RESPONSE));
            let url = entry["url"].as_str().unwrap_or_default();
            response && trouter.as_ref().is_some_and(|socket| socket.is_match(url))
        })
        .count();
    RingSends {
        attaches: allowed_requests(text, "POST", ATTACH),
        receipts,
    }
}

/// The call a run places, as each callee's invitation names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunCall {
    /// The caller's MRI (a's).
    pub caller: String,
    /// The group chat the call is placed in (`19:<id>@thread.v2`); `None` for a 1:1 call, whose invitation names no
    /// group chat.
    pub group_chat: Option<String>,
}

/// A call invitation a page received: a Trouter delivery whose body's `evt` is one of [`INVITATION_EVENTS`] and whose
/// `gp`, decoded, holds a `callNotification`.
#[derive(Clone, Debug, PartialEq)]
pub struct Invitation {
    /// When the capture recorded it (its entry's `ts_ms`; `0` when it has none).
    pub at_ms: u64,
    pub event: u64,
    /// The decoded `gp`.
    pub payload: Value,
}

impl Invitation {
    /// The conversation it invites to (`conversationInvitation.conversationController`): the same in each delivery of
    /// one call.
    #[must_use]
    pub fn conversation(&self) -> Option<&str> {
        self.payload
            .pointer("/conversationInvitation/conversationController")?
            .as_str()
    }

    /// Whether it is `call`'s invitation to `callee` (an MRI), naming nobody else: from the caller, to the callee, a
    /// conversation to join, no call it replaces, no group context; a group call's in the run's group chat, its
    /// initiator (when named) the caller; a 1:1 call's in no group chat.
    #[must_use]
    pub fn is_of(&self, call: &RunCall, callee: &str) -> bool {
        let text = |pointer: &str| self.payload.pointer(pointer).and_then(Value::as_str);
        let unset = |pointer: &str| self.payload.pointer(pointer).is_none_or(Value::is_null);
        let caller_or_unset = |pointer: &str| unset(pointer) || text(pointer) == Some(call.caller.as_str());
        let chat = match &call.group_chat {
            Some(thread) => text("/groupChat/threadId") == Some(thread.as_str()),
            None => unset("/groupChat"),
        };
        INVITATION_EVENTS.contains(&self.event)
            && !callee.is_empty()
            && text("/callNotification/from/id") == Some(call.caller.as_str())
            && text("/callNotification/to/id") == Some(callee)
            && unset("/callNotification/replaces")
            && unset("/groupContext")
            && self.conversation().is_some()
            && chat
            && caller_or_unset("/conversationInvitation/groupCallInitiator")
            && caller_or_unset("/conversationInvitation/groupCallInitiatorParticipant/id")
    }
}

/// Whether a callee may click on its incoming call ([`ring_check`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RingCheck {
    /// Every invitation the page received is the run's call, and there is one.
    Ready,
    /// No invitation yet, and none of anything else.
    Pending,
    /// An invitation that isn't the run's call, one from before it was placed, or invitations of more than one call:
    /// no click.
    Refused(String),
}

/// Whether a callee (`callee`, its MRI) may click on its incoming call, by `invitations`, every one its page received
/// since it loaded: each must be `call`'s invitation to it ([`Invitation::is_of`]), received after the call was placed
/// at `placed` (Unix ms), all to one conversation. Anything else refuses, even an invitation that already went: the
/// page may still show it.
#[must_use]
pub fn ring_check(invitations: &[Invitation], call: &RunCall, callee: &str, placed: u64) -> RingCheck {
    let mut conversation = None;
    for invitation in invitations {
        if !invitation.is_of(call, callee) {
            return RingCheck::Refused("an invitation that isn't the run's call reached the page".to_owned());
        }
        if invitation.at_ms < placed {
            return RingCheck::Refused("an invitation reached the page before a placed the call".to_owned());
        }
        match (conversation, invitation.conversation()) {
            (None, Some(this)) => conversation = Some(this),
            (Some(first), Some(this)) if first == this => {}
            _ => return RingCheck::Refused("invitations of more than one call reached the page".to_owned()),
        }
    }
    if conversation.is_some() {
        RingCheck::Ready
    } else {
        RingCheck::Pending
    }
}

/// Which of a page's invitations a ring check judges ([`phase_ring_check`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EarlierRings {
    /// Every one its page received since it loaded: the page rings for one call in its run (`ab-call`, `abc-call`).
    Judged,
    /// Every one but the run's own call's invitations to the callee ([`Invitation::is_of`]) the capture timed before
    /// the call was placed: an earlier phase of the run rang the page with a call of the same caller
    /// (`ab-call-native`). Any other invitation from before, an outsider's included, still refuses.
    OwnLeftOut,
}

/// [`ring_check`] on the invitations `earlier` says a ring check judges: all of them, or all but the run's own call's
/// from before it was placed at `placed` (Unix ms).
#[must_use]
pub fn phase_ring_check(
    invitations: &[Invitation],
    call: &RunCall,
    callee: &str,
    placed: u64,
    earlier: EarlierRings,
) -> RingCheck {
    let own_earlier = |invitation: &Invitation| {
        earlier == EarlierRings::OwnLeftOut
            && invitation.at_ms != 0
            && invitation.at_ms < placed
            && invitation.is_of(call, callee)
    };
    let judged: Vec<Invitation> = invitations
        .iter()
        .filter(|invitation| !own_earlier(invitation))
        .cloned()
        .collect();
    ring_check(&judged, call, callee, placed)
}

/// The deliveries of `invitations` that are `call`'s invitation to `callee` ([`Invitation::is_of`]) received after the
/// call was placed at `placed` (Unix ms).
#[must_use]
pub fn own_invitations(invitations: &[Invitation], call: &RunCall, callee: &str, placed: u64) -> usize {
    invitations
        .iter()
        .filter(|invitation| invitation.at_ms >= placed && invitation.is_of(call, callee))
        .count()
}

/// The call invitations a capture's `events.jsonl` text shows its page receiving: Trouter deliveries (socket.io `3:::`
/// messages) whose body is `{"evt": <one of INVITATION_EVENTS>, "gp": <base64 JSON>}` with a `callNotification` in it
/// (seen 2026-10-24; the service delivers one again while it goes unanswered, and each is one).
#[must_use]
pub fn invitations(text: &str) -> Vec<Invitation> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["type"] == "cdp" && entry["method"] == "Network.webSocketFrameReceived")
        .filter_map(|entry| {
            let data = entry["params"]["response"]["payloadData"]
                .as_str()?
                .strip_prefix(DELIVERY_RESPONSE)?;
            let message: Value = serde_json::from_str(data).ok()?;
            let body: Value = serde_json::from_str(message["body"].as_str()?).ok()?;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(body["gp"].as_str()?)
                .ok()?;
            let payload: Value = serde_json::from_slice(&decoded).ok()?;
            let event = body["evt"].as_u64()?;
            (INVITATION_EVENTS.contains(&event) && payload.get("callNotification").is_some()).then(|| Invitation {
                at_ms: entry["ts_ms"].as_u64().unwrap_or(0),
                event,
                payload,
            })
        })
        .collect()
}

/// The event numbers of a call invitation delivered over Trouter: a 1:1 call's (107) and a group call's (109, the
/// call's mixer dialling the callee out).
pub const INVITATION_EVENTS: [u64; 2] = [107, 109];

/// The held-back starts, per account, into the notes; `start-held` must have held back both starts.
fn judge_held(step: CallStep, report: &mut ScenarioReport) {
    let mut notes = Vec::new();
    for run in &report.accounts {
        let requests = held_back(&run.denials, START_HELD);
        if requests > 0 {
            notes.push(format!("{}: {START_HELD}: {requests} request(s)", run.alias));
        }
        if step == CallStep::StartHeld && run.alias == CALLER && held_back(&run.denials, START_HELD) == 0 {
            report.failures.push("a's fence held back no call start".to_owned());
        }
    }
    report.notes.extend(notes);
}

/// The time now in Unix milliseconds, to line the notes up with the captures.
fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// The time now in Unix milliseconds, as a capture stamps its entries (`ts_ms`).
fn now_ms() -> u64 {
    u64::try_from(unix_ms()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "../../tests/support/scenario/ab_call_tests.rs"]
mod tests;
