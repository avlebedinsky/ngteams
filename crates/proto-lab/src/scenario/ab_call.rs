//! The `ab-call` scenario: account a calls account b from their 1:1 chat in the web client, and the machinery
//! `abc-call` ([`super::abc_call`]) shares, where a calls the lab's a–b–c group chat. Each run is recorded
//! (`captures/<scenario>/<ts>/`, one capture per account): the call's HTTP and WebSocket signaling on every side, the
//! fence's verdicts and the WebRTC hooks' reports are the material of `docs/protocol/calling.md`.
//!
//! Every account runs under the lab rule with members read live, Chrome's fake camera and a microphone playing a tone,
//! muted ([`crate::browser::BASE_ARGS`]). What a run does is its step ([`CallStep`], `proto-lab run ab-call --call
//! <step>`):
//! - `start-held`: only a's browser runs. a clicks the chat's audio call button, then its video call button; each call
//!   start is held back by a forced deny labelled [`START_HELD`], so nobody is invited and nothing rings. The capture
//!   keeps the starts' bodies: what a start to a lab callee names (its `participants.to`) and what it names of its
//!   chat.
//! - `answer`, `decline`, `cancel`, `timeout`: a's browser and each callee's. Each callee opens the Calls app (the
//!   client starts its calling stack only then) and, before a calls, holds back everything its page sends to the
//!   calling services and every Trouter delivery response it sends ([`hold_callee`], labelled [`RING_SENDS`]). a's
//!   audio call start goes to the fence, which permits it only as a start to lab callees in a lab-only chat; each callee
//!   must show the incoming call within [`RING_WAIT`]. Then:
//!   - `answer`: each callee clicks answer ([`ACCEPT_AUDIO`]); what its page sends from then on is held back under
//!     [`ANSWER_HELD`], so the call isn't answered, and rings on until a hangs up after [`AFTER_ACTION`];
//!   - `decline`: each callee clicks decline ([`DECLINE`]), held back under [`DECLINE_HELD`]; a hangs up after
//!     [`AFTER_ACTION`];
//!   - `cancel`: a hangs up after [`RING_BEFORE_CANCEL`] while the callees ring: the caller's cancel as they receive it;
//!   - `timeout`: nobody acts for [`UNANSWERED`] (an unanswered call goes to the callee's voicemail, a service of
//!     the callee's own account, which accepts it for a); then a hangs up.
//!
//!   The run records [`AFTER_RING`] more and notes when each callee's incoming call went.
//!
//! Before anything loads, every page force-denies an earlier chat scenario's unsent probe ([`probe_rules`]), as
//! `echo-call` does. A callee clicks answer or decline only on the run's own call, naming only lab accounts
//! ([`ring_check`]): every invitation its page received is a's call placed in this run (from a, to this callee, in the
//! run's chat, nobody else named), all of one call, and the page shows one incoming call. Any other invitation, or a
//! second incoming call, and the step fails without a click. The fence maps neither an answer nor a decline: both stay
//! held back.
//!
//! Judged by [`policy`] per account: no deny but the reviewed expected-deny list's and the run's own forced ones; a's
//! start allowed, and each callee's page receiving the run's own call's invitation ([`own_invitations`]).
//! A failed run leaves each page's screenshot as `failure.png` in its capture.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use clap::ValueEnum;
use serde_json::Value;

use crate::browser::Media;
use crate::capture::EVENTS_FILE;
use crate::classify::Classifier;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_http::{Denial, DenyCause, ForcedDeny};
use crate::fence_ws::ForcedFrameDeny;
use crate::session::{LabSession, SessionOptions, TEAMS_URL};

use super::ab_chat::{EARLIER_RUN_PROBE, chat_link, load_at, probe_rules, save_screenshot, setup};
use super::dsl::{Driver, Selector, js_string};
use super::echo_call::{
    CALL_START, CALL_START_URL, CALLS_APP, CALLS_APP_WARM, CONTROL_WAIT, HANG_UP, PAGE_OPEN, PAGE_STEADY, USER_AGENT,
    call_starts, linger, size_page, write_tone,
};
use super::expected::Review;
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// What a run of `ab-call` or `abc-call` does (see the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum CallStep {
    /// a calls with audio, then with video; each start held back.
    StartHeld,
    /// a calls with audio; the callees click answer, held back.
    #[default]
    Answer,
    /// a calls with audio; the callees click decline, held back.
    Decline,
    /// a calls with audio and hangs up while the callees ring.
    Cancel,
    /// a calls with audio; nobody acts until the ring ends by itself.
    Timeout,
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
        }
    }

    /// What each callee does in this step: answer ([`ACCEPT_AUDIO`], what its page sends from then on held back under
    /// [`ANSWER_HELD`]) or decline ([`DECLINE`], under [`DECLINE_HELD`]); `None` for a step where no callee acts.
    #[must_use]
    pub fn callee_action(self) -> Option<CalleeAction> {
        match self {
            Self::Answer => Some(CalleeAction {
                label: ANSWER_HELD,
                button: ACCEPT_AUDIO_TID,
                what: "answer",
            }),
            Self::Decline => Some(CalleeAction {
                label: DECLINE_HELD,
                button: DECLINE_TID,
                what: "decline",
            }),
            Self::StartHeld | Self::Cancel | Self::Timeout => None,
        }
    }
}

/// What a callee does on the incoming call ([`CallStep::callee_action`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalleeAction {
    /// The label of what its page sends once it clicked.
    pub label: &'static str,
    /// The `data-tid` of the button it clicks.
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

/// The incoming call as the web client shows it, and its answer (camera off) and decline buttons.
pub const INCOMING_CALL: Selector<'static> = Selector::Css(INCOMING_CALL_CSS);
const INCOMING_CALL_CSS: &str =
    r#"[data-tid="incoming-call-screen-wrapper"], [data-tid="accept-video-off-call-button"]"#;
pub const ACCEPT_AUDIO: Selector<'static> = Selector::Tid(ACCEPT_AUDIO_TID);
pub const ACCEPT_AUDIO_TID: &str = "accept-video-off-call-button";
pub const DECLINE: Selector<'static> = Selector::Tid(DECLINE_TID);
pub const DECLINE_TID: &str = "reject-call-button";
const INCOMING_SCREEN_CSS: &str = r#"[data-tid="incoming-call-screen-wrapper"]"#;
/// How long a callee's capture gets to show the run's call's invitation once the page shows the incoming call.
pub const INVITATION_WAIT: Duration = Duration::from_secs(5);

/// The label of the forced deny of a's call starts in `start-held`.
pub const START_HELD: &str = "the call start, held back";
/// The label of what a callee's page sends to the calling services, or as a Trouter delivery response, while it rings.
pub const RING_SENDS: &str = "a callee's sends while ringing, held back";
/// The label of what a callee's page sends once it clicked answer.
pub const ANSWER_HELD: &str = "a callee's answer, held back";
/// The label of what a callee's page sends once it clicked decline.
pub const DECLINE_HELD: &str = "a callee's decline, held back";
/// The label of a's own join of the running call ([`caller_join_rule`]).
pub const CALLER_JOIN_HELD: &str = "the caller's join of the running call, held back";

/// The calling services' hosts as a URL pattern: the flight proxy, its `api.fl` name and the Skype-era call hosts
/// (`teams_fence::service_of`'s calling service).
pub const CALLING_HOSTS: &str =
    r"^https://([a-z0-9-]+\.)*(flightproxy\.teams\.microsoft\.com|api\.fl\.teams\.microsoft\.com|skype\.com)(:\d+)?/";

/// A Trouter socket's URL as a pattern.
pub const TROUTER_SOCKETS: &str = r"^wss://([a-z0-9-]+\.)*trouter\.teams\.microsoft\.com(:\d+)?/";

/// What a Trouter delivery response starts with (socket.io 0.9's type `3`, a message).
pub const DELIVERY_RESPONSE: &str = "3:::";

/// How long a callee gets to show the incoming call after a's start went out.
pub const RING_WAIT: Duration = Duration::from_secs(45);
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

/// The forced deny of every call start ([`CALL_START_URL`]), labelled [`START_HELD`].
///
/// # Errors
/// Never: the pattern is fixed.
pub fn start_held_rule() -> Result<ForcedDeny, regex::Error> {
    Ok(ForcedDeny::new("POST", &format!("^{}(\\?|$)", regex::escape(CALL_START_URL)))?.labelled(START_HELD))
}

/// The forced denies of everything a callee's page sends to the calling services ([`CALLING_HOSTS`]: every method
/// that can write) and of every Trouter delivery response it sends, all labelled `label`.
///
/// # Errors
/// Never: the patterns are fixed.
pub fn callee_rules(label: &str) -> Result<(Vec<ForcedDeny>, ForcedFrameDeny), regex::Error> {
    let requests = ["POST", "PUT", "PATCH", "DELETE"]
        .into_iter()
        .map(|method| Ok(ForcedDeny::new(method, CALLING_HOSTS)?.labelled(label)))
        .collect::<Result<_, regex::Error>>()?;
    let frames = ForcedFrameDeny::new(TROUTER_SOCKETS)?
        .with_text_starting_with(DELIVERY_RESPONSE)
        .labelled(label);
    Ok((requests, frames))
}

/// The forced deny of a's join of a running call (a `POST` to a conversation controller itself,
/// `…/api/v2/ep/<conv host>/conv/<id>`), labelled [`CALLER_JOIN_HELD`]: after a hangs up a group call its callees
/// still ring in, the client joins the call's conversation again by itself (seen 2026-10-24, `abc-call --call
/// cancel`). A join isn't mapped (CLAUDE.md: only in a chat a fresh member read finds lab-only, once a capture binds
/// the call to it), so it stays denied; the rule only names it.
///
/// # Errors
/// Never: the pattern is fixed.
pub fn caller_join_rule() -> Result<ForcedDeny, regex::Error> {
    Ok(ForcedDeny::new("POST", CONVERSATION_JOIN)?.labelled(CALLER_JOIN_HELD))
}

/// A conversation controller's own URL on the flight proxy: where a join goes.
pub const CONVERSATION_JOIN: &str =
    r"^https://api\.flightproxy\.teams\.microsoft\.com/api/v2/ep/[^/?]+/conv/[^/?]+(\?|$)";

/// The forced denies of `step` the caller's run expects: an earlier run's probe, in `start-held` its starts, and in
/// the ring's steps its own join of the running call ([`caller_join_rule`]).
#[must_use]
pub fn caller_forced(step: CallStep) -> &'static [&'static str] {
    match step {
        CallStep::StartHeld => &[EARLIER_RUN_PROBE, START_HELD],
        _ => &[EARLIER_RUN_PROBE, CALLER_JOIN_HELD],
    }
}

/// The forced denies of `step` a callee's run expects: an earlier run's probe, its sends while ringing, and its
/// answer's or decline's.
#[must_use]
pub fn callee_forced(step: CallStep) -> &'static [&'static str] {
    match step {
        CallStep::Answer => &[EARLIER_RUN_PROBE, RING_SENDS, ANSWER_HELD],
        CallStep::Decline => &[EARLIER_RUN_PROBE, RING_SENDS, DECLINE_HELD],
        _ => &[EARLIER_RUN_PROBE, RING_SENDS],
    }
}

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
/// When the run can't start: no tone file, no Chrome, no fence, a browser that can't be armed.
pub(crate) async fn run_plan(paths: &LabPaths, plan: &CallPlan) -> Result<ScenarioReport, ScenarioError> {
    let mut report = ScenarioReport::new(plan.scenario);
    report
        .notes
        .push(format!("{}: step {}", plan.scenario, plan.step.as_str()));
    let tone = write_tone(paths).map_err(|error| setup(format!("can't write the tone: {error}")))?;
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
    let mut sessions = open_all(paths, &aliases, &options).await?;
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
    }
    close_all(sessions, plan, placed, &mut report).await;
    judge_held(plan.step, &mut report);
    Ok(report)
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
/// forced denies and its capture (a callee's by the invitations of the call a placed at `placed`, Unix ms).
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
            callee_forced(step)
        };
        report.close_account(alias, session, &policy(labels)).await;
        if let Some(dir) = capture {
            if alias == CALLER {
                judge_caller(&dir, step, report);
            } else {
                judge_callee(alias, plan.callee_mri(alias), &plan.call, placed, &dir, report);
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
    let mut sessions: Vec<(Alias, LabSession)> = Vec::new();
    for alias in aliases {
        match LabSession::open_with(paths, *alias, options.clone()).await {
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
        load_at(CALLER, caller, &self.chat_link, self.options.load_timeout).await?;
        size_page(caller).await?;
        report
            .notes
            .push(format!("{CALLER}: the web client loaded at the chat"));
        for (alias, session) in callees.iter_mut() {
            load_at(*alias, session, TEAMS_URL, self.options.load_timeout).await?;
            size_page(session).await?;
            report.notes.push(format!("{alias}: the web client loaded"));
        }
        stand_up_calling(CALLER, &mut caller.driver(), report).await?;
        back_to_chat(&mut caller.driver(), report).await?;
        for (alias, session) in callees.iter_mut() {
            stand_up_calling(*alias, &mut session.driver(), report).await?;
        }
        if self.step == CallStep::StartHeld {
            return start_held(caller, self.held_starts, report).await;
        }
        caller.force_deny(caller_join_rule().map_err(|error| setup(error.to_string()))?);
        for (alias, session) in callees.iter() {
            hold_callee(session, RING_SENDS)?;
            watch_controls(*alias, session).await?;
        }
        self.ring(caller, callees, placed, report).await
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
        place_call(caller, Modality::Audio, report).await?;
        match self.step.callee_action() {
            Some(action) => {
                if let Err(error) = wait_incoming(callees, report).await {
                    note_ring_controls(callees, report).await;
                    return Err(error);
                }
                self.act(action, at, caller, callees, report).await?;
                wait_ring_end(callees, RING_END, report).await;
            }
            None if self.step == CallStep::Cancel => {
                linger(caller, RING_BEFORE_CANCEL).await;
                hang_up(caller, report).await?;
            }
            None => {
                linger(caller, UNANSWERED).await;
                hang_up(caller, report).await?;
            }
        }
        linger(caller, AFTER_RING).await;
        note_ring_controls(callees, report).await;
        Ok(())
    }

    /// Each callee clicks `action`'s button, what its page sends from then on held back under its label, but only on
    /// the run's own call ([`Self::check_ring`]); a hangs up after [`AFTER_ACTION`].
    async fn act(
        &self,
        action: CalleeAction,
        placed: u64,
        caller: &mut LabSession,
        callees: &mut [(Alias, LabSession)],
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        for (alias, session) in callees.iter_mut() {
            hold_callee(session, action.label)?;
            self.check_ring(*alias, session, action, placed).await?;
            session
                .driver()
                .click(&format!("{alias} clicks {}", action.what), Selector::Tid(action.button))
                .await?;
            report
                .notes
                .push(format!("{alias}: clicked {} at {}", action.what, unix_ms()));
        }
        linger(caller, AFTER_ACTION).await;
        hang_up(caller, report).await
    }

    /// Whether `alias` may click `action`'s button: within [`INVITATION_WAIT`], its capture must pass [`ring_check`]
    /// (every invitation the page received is the run's call, placed at `placed`) and its page must show one incoming
    /// call with one such button. Anything else fails the step, nothing clicked.
    async fn check_ring(
        &self,
        alias: Alias,
        session: &mut LabSession,
        action: CalleeAction,
        placed: u64,
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
            match ring_check(&invitations(&text), &self.call, self.callee_mri(alias), placed) {
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
                    js_string(&format!(r#"[data-tid="{}"]"#, action.button))
                ),
            )
            .await?;
        if shown != Value::Bool(true) {
            return Err(refuse("the page doesn't show exactly one incoming call".to_owned()));
        }
        Ok(())
    }
}

/// Waits up to [`RING_WAIT`] for each callee to show the incoming call.
async fn wait_incoming(callees: &mut [(Alias, LabSession)], report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    for (alias, session) in callees.iter_mut() {
        session
            .driver()
            .wait_for(&format!("{alias} shows the incoming call"), INCOMING_CALL, RING_WAIT)
            .await?;
        report
            .notes
            .push(format!("{alias}: the incoming call showed at {}", unix_ms()));
    }
    Ok(())
}

/// From now on, everything `session`'s page sends to the calling services and every Trouter delivery response it
/// sends is denied under `label` ([`callee_rules`]); the probe rules stay, and the earlier label's rules go.
///
/// # Errors
/// Never: the patterns are fixed.
pub(crate) fn hold_callee(session: &LabSession, label: &str) -> Result<(), ScenarioError> {
    let (requests, frames) = callee_rules(label).map_err(|error| setup(error.to_string()))?;
    let probes = probe_rules(None).map_err(|error| setup(error.to_string()))?;
    session.clear_forced();
    probes
        .into_iter()
        .chain(requests)
        .for_each(|rule| session.force_deny(rule));
    session.force_deny_frames(frames);
    Ok(())
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
        place_call(caller, modality, report).await?;
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

/// a places a call from the chat's header ([`Modality::control`]): its button when the header shows one, else its item
/// in the call options; a group chat's confirmation, when it asks for one within [`CONFIRM_WAIT`], is confirmed.
async fn place_call(
    caller: &mut LabSession,
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
        driver.click("a opens the call options", CALL_OPTIONS).await?;
        driver
            .wait_for(&format!("the item of {}", modality.what()), control, CONTROL_WAIT)
            .await?;
    }
    driver.click(&format!("a starts {}", modality.what()), control).await?;
    report
        .notes
        .push(format!("{CALLER}: {} clicked at {}", modality.what(), unix_ms()));
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
        driver.click("a confirms the group call", GROUP_CALL_CONFIRM).await?;
        report
            .notes
            .push(format!("{CALLER}: confirmed the group call at {}", unix_ms()));
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
async fn stand_up_calling(alias: Alias, driver: &mut Driver, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    // the client may load once more on its way to the origin it assigns the account: the app bar has to stay
    driver.wait_for("the Calls app", CALLS_APP, PAGE_OPEN).await?;
    tokio::time::sleep(PAGE_STEADY).await;
    driver.wait_for("the Calls app", CALLS_APP, PAGE_OPEN).await?;
    driver.click(&format!("{alias} opens the Calls app"), CALLS_APP).await?;
    tokio::time::sleep(CALLS_APP_WARM).await;
    report
        .notes
        .push(format!("{alias}: the Calls app started the calling stack"));
    Ok(())
}

/// Back from the Calls app to the chat the page loaded at, in the same document: the Chat app shows the chat it showed
/// last, and its header's call buttons.
async fn back_to_chat(driver: &mut Driver, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    driver.click("a opens the Chat app", CHAT_APP).await?;
    driver
        .wait_for("the chat's call buttons", CALL_BUTTONS, PAGE_OPEN)
        .await?;
    report.notes.push(format!("{CALLER}: back at the chat"));
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
    let mut driver = caller.driver();
    let up = driver
        .evaluate(
            "a's call screen",
            &format!("document.querySelector({}) !== null", js_string(HANG_UP_CSS)),
        )
        .await?;
    if up != Value::Bool(true) {
        return Ok(false);
    }
    driver.click("a hangs up", HANG_UP).await?;
    report.notes.push(format!("{CALLER}: hung up at {}", unix_ms()));
    Ok(true)
}

const HANG_UP_CSS: &str = r#"#hangup-button, [data-tid="hangup-main-btn"]"#;

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

/// The page script that records, from now on, the first moment each element `data-tid` naming a call, a ring, an
/// answer, a decline or a toast shows in the page (`window.__labRingControls`): what a callee's page showed while it
/// rang, however briefly. Ids of the client's own, no text of the page.
const WATCH_CONTROLS: &str = "(() => { const seen = window.__labRingControls = window.__labRingControls || {};      const note = () => document.querySelectorAll('[data-tid]').forEach(e => { const t = e.getAttribute('data-tid');        if (/call|ring|accept|reject|incoming|toast|notif/i.test(t) && !(t in seen)) seen[t] = Date.now(); });      note(); new MutationObserver(note).observe(document.documentElement, {subtree: true, childList: true, attributes: true,        attributeFilter: ['data-tid']}); return true; })()";

/// Starts recording what `alias`'s page shows while it rings ([`WATCH_CONTROLS`]).
async fn watch_controls(alias: Alias, session: &LabSession) -> Result<(), ScenarioError> {
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
                "JSON.stringify(window.__labRingControls || {})",
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

/// A callee's capture: the run's call's invitation must have reached its page ([`own_invitations`]); any other is only
/// noted.
fn judge_callee(
    alias: Alias,
    callee: &str,
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
        }
        Err(error) => report.failures.push(format!("can't read {alias}'s capture: {error}")),
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

/// The held-back counts by label, per account, into the notes; `start-held` must have held back both starts.
fn judge_held(step: CallStep, report: &mut ScenarioReport) {
    let labels = [START_HELD, RING_SENDS, ANSWER_HELD, DECLINE_HELD, CALLER_JOIN_HELD];
    let mut notes = Vec::new();
    for run in &report.accounts {
        for label in labels {
            let requests = held_back(&run.denials, label);
            let frames = run
                .frame_denials
                .iter()
                .filter(|denial| denial.label.as_deref() == Some(label))
                .count();
            if requests + frames > 0 {
                notes.push(format!(
                    "{}: {label}: {requests} request(s), {frames} frame(s)",
                    run.alias
                ));
            }
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
