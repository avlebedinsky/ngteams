//! The `ab-trouter` scenario (M1 Task 14, `docs/protocol/trouter.md`): account b's push channel (Trouter) while a
//! sends, edits and reacts in the a–b chat, recorded for the native client's Trouter notes.
//!
//! Both accounts run under the lab rule with members read live, as `ab-chat`: the Trouter session's own frames
//! (authenticate, keep-alives, the client's own `user.activity`, the loss acknowledgement), the responses to what the
//! service delivers (`crate::fence_ws::classify_frame`) and the endpoints' registrations reach nobody and pass.
//! `ab-trouter-quiet` ([`Activity::HeldBack`]) also holds back every `user.activity` ([`hold_activity`]), to see
//! whether the service treats an endpoint that never reports its activity differently.
//!
//! The steps:
//! 1. Before anything loads, a's fence force-denies every write carrying the probe marker, as in `ab-chat`
//!    ([`super::ab_chat`]).
//! 2. b opens the web client and must register its endpoint, then opens the chat with a (a new document, which
//!    registers again) and must read it: b now shows the a–b chat, with its push channel up. Each document must get a
//!    messaging-stack registration answered with a 2xx ([`wait_registered`]): the earlier document's never counts.
//! 3. a opens the web client and the chat with b, and proves enforcement live with this run's probe (`ab-chat`'s
//!    step 4).
//! 4. a sends the message; b, already showing the chat, must show it within [`DELIVERY`] (it can only come by push or
//!    by a read b makes by itself) and never the probe. A chat with unread messages opens at its "last read" mark,
//!    the newest ones not rendered, so b jumps to the newest while it waits ([`wait_for_newest`]): a jump that reads
//!    the chat shows in the capture. The time from a's Enter to b showing it is noted.
//! 5. a edits the message and reacts to it with a like ([`edit`], [`react`]); b must show the edit, and the reaction
//!    must be answered with a 2xx. These steps failing fails the run but doesn't end it: the observation still runs.
//! 6. Both pages stay open, nothing driven, for [`observation`] (when the activity is reported, longer than the
//!    lifetime a chat delivery carries): the capture shows what the service delivers to an idle endpoint and whether
//!    it closes the socket (the runs made while the delivery responses were denied showed it delivering again what
//!    went unanswered).
//! 7. The notes count each account's endpoint registrations, answered and denied ([`registrations_note`]). Judged as
//!    `ab-chat` ([`trouter_policy`]): no deny but the probes' forced ones (and, quiet, the held-back activity) and the
//!    reviewed expected-deny list (it holds no Trouter frame); nothing denied on the a–b chat.
//!
//! Nothing in the run answers a call.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::json;
use tokio::time::Instant;

use crate::browser::Media;
use crate::cdp::SessionId;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_http::without_query;
use crate::fence_ws::ForcedFrameDeny;
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{
    CHAT_OPEN, COMPOSE, DELIVERY, EARLIER_RUN_PROBE, EXPECTED_FORCED, THIS_RUN_PROBE, chat_link, chat_requests,
    chat_url, deny_probes, enforce, load, message_text, nonce, one_to_one_id, probe_text, save_screenshot, send,
    wait_for_newest,
};
use super::dsl::{Driver, RequestPattern, SeenRequest, Selector, StepError, js_string};
use super::expected::Review;
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// Whether the client's own `user.activity` goes out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    /// It goes out: it reaches nobody (`ab-trouter`).
    Reported,
    /// Every `user.activity` is force-denied ([`hold_activity`], `ab-trouter-quiet`).
    HeldBack,
}

/// The label of the forced deny that holds back the client's own `user.activity` ([`hold_activity`]).
pub const HELD_ACTIVITY: &str = "held-back activity";

/// The forced denies a quiet run expects: `ab-chat`'s two probe rules' and the held-back activity.
pub const QUIET_FORCED: [&str; 3] = [THIS_RUN_PROBE, EARLIER_RUN_PROBE, HELD_ACTIVITY];

/// A socket of the Trouter service, by its URL.
pub const TROUTER_SOCKET: &str = r"^wss://[a-z0-9-]+\.trouter\.teams\.microsoft\.com/";

/// What every `user.activity` event's text holds, as the web client writes it.
pub const ACTIVITY_EVENT: &str = r#""name":"user.activity""#;

/// The rule that holds back every `user.activity` on the Trouter sockets, labelled [`HELD_ACTIVITY`]: what
/// `ab-trouter-quiet` and `own-presence-endpoint` force on each account's fence before its page loads.
///
/// # Errors
/// Never: the pattern is fixed.
pub fn hold_activity() -> Result<ForcedFrameDeny, regex::Error> {
    Ok(ForcedFrameDeny::new(TROUTER_SOCKET)?
        .with_text_containing(ACTIVITY_EVENT)
        .labelled(HELD_ACTIVITY))
}

/// The forced denies a run with `activity` expects.
#[must_use]
pub fn expected_forced(activity: Activity) -> &'static [&'static str] {
    match activity {
        Activity::Reported => &EXPECTED_FORCED,
        Activity::HeldBack => &QUIET_FORCED,
    }
}

/// The registration of an endpoint with the registrar (either stack's; under the lab rule both pass).
pub const REGISTRATION: &str = r"/registrar/prod/[Vv]2/registrations$";

/// What the messaging stack's registration body holds and the calling stack's (`SkypeSpacesWeb`) doesn't: its
/// `clientDescription.appId`, as the web client writes it (compact JSON).
pub const MESSAGING_REGISTRATION: &str = r#""appId":"TeamsCDLWebWorker""#;

/// How long b's endpoint gets to register after a load.
pub const REGISTER: Duration = Duration::from_secs(90);

/// How long `ab-trouter` stays open after a's last action, nothing driven: longer than the lifetime a chat delivery
/// carries (`X-Trouter-Delivery-Control: …; ttl=900`, seen live 2026-10-20), so a redelivery of an unanswered one
/// would show.
pub const OBSERVE: Duration = Duration::from_mins(16);

/// How long `ab-trouter-quiet` stays open after a's last action: several keep-alives, and time for the service to
/// act on an endpoint that never reported its activity.
pub const OBSERVE_QUIET: Duration = Duration::from_mins(5);

/// How long a's edit and reaction requests get to go out.
pub const ACTION: Duration = Duration::from_secs(20);

/// A message edit: `PUT …/conversations/<chat>/messages/<id>`.
pub const MESSAGE_EDIT: &str = r"/v1/users/ME/conversations/[^/?]+/messages/[^/?]+(\?|$)";

/// A reaction: `PUT …/conversations/<chat>/messages/<id>/properties?name=emotions`.
pub const REACTION: &str = r"/v1/users/ME/conversations/[^/?]+/messages/[^/?]+/properties\?name=emotions";

/// The "more options" button of a message's actions (seen in the web bundle `26260.1717.5229.7049`).
const MORE: Selector<'static> = Selector::Tid("message-actions-more");

/// The "Edit" entry of a message's "more options" menu.
const EDIT: Selector<'static> = Selector::Tid("message-actions-edit");

/// The like on a message's hover bar: the quick reaction `ab-chat-full` (Task 7) reacted with live (2026-10-20). The
/// bundle's `default-reaction-*` showed neither on the hover bar nor in the "more options" menu in this build.
const LIKE: &str = r#"[data-tid="message-actions-like"]"#;

/// A reaction a click on the message, or its context menu, may offer: the like or one of the bundle's defaults.
const ANY_REACTION: &str = r#"[data-tid="message-actions-like"], [data-tid^="default-reaction-"]"#;

/// How long each way to a reaction gets to show one before [`react`] tries the next.
const QUICK_LOOK: Duration = Duration::from_secs(5);

/// The attribute [`edit`] marks the message's editor with, so the typing goes there and not to the compose box.
const EDITOR_MARK: Selector<'static> = Selector::Css(r#"[data-lab-editor="1"]"#);

/// The page's size: the client lays out a message's actions in full.
const VIEWPORT: (u32, u32) = (1440, 900);

/// How a run with `activity` judges its denies: `ab-chat`'s policy ([`super::ab_chat::policy`]), with the held-back
/// activity's forced deny for a quiet run ([`expected_forced`]).
///
/// # Errors
/// Never for an id from [`one_to_one_id`]; when the chat's pattern doesn't compile.
pub fn trouter_policy(activity: Activity, chat_id: Option<&str>) -> Result<DenyPolicy, regex::Error> {
    let mut review = Review::new().expecting_forced(expected_forced(activity));
    if let Some(chat_id) = chat_id {
        review = review.with_mapped_target(chat_url(chat_id)?);
    }
    Ok(DenyPolicy::Reviewed(review))
}

/// The pattern of the messaging stack's endpoint registration: a [`REGISTRATION`] whose body holds
/// [`MESSAGING_REGISTRATION`]. Its URL alone ([`RequestPattern::matches`]) takes either stack's.
///
/// # Errors
/// Never: the pattern is fixed.
pub fn registration() -> Result<RequestPattern, regex::Error> {
    Ok(RequestPattern::new(Some("POST"), REGISTRATION)?.with_body_containing(MESSAGING_REGISTRATION))
}

/// Whether `request` is the messaging stack's registration (`pattern`, [`registration`], body included) answered with a
/// 2xx. The calling stack's, one whose body the events left out, and one that failed in the page never are.
#[must_use]
pub fn registered(pattern: &RequestPattern, request: &SeenRequest) -> bool {
    pattern.matches_request(request) && answered_2xx(request)
}

fn answered_2xx(request: &SeenRequest) -> bool {
    request.status.is_some_and(|status| (200..300).contains(&status))
}

/// Waits up to [`REGISTER`] for the messaging stack's registration sent as the `since`th request seen by `driver` or
/// later (since a document began: [`Driver::seen_so_far`]) to be answered with a 2xx ([`registered`]), and gives it.
/// Each registration answered with a 2xx has its body read ([`Driver::read_body`]: the event leaves it out) to tell the
/// stacks apart. The calling stack's registration, one whose body can't be read, a denied one, or one an earlier
/// document made never meets the wait, and the other request waits keep their place.
///
/// # Errors
/// [`StepError::Timeout`] when none is answered so in time (saying how many other stacks' or unreadable ones were),
/// [`StepError::Closed`] when the connection goes.
pub async fn wait_registered(driver: &mut Driver, step: &str, since: usize) -> Result<SeenRequest, StepError> {
    wait_registered_with(driver, step, since, REGISTER).await
}

async fn wait_registered_with(
    driver: &mut Driver,
    step: &str,
    since: usize,
    timeout: Duration,
) -> Result<SeenRequest, StepError> {
    let pattern = registration().map_err(|error| StepError::Failed {
        step: step.to_owned(),
        reason: error.to_string(),
    })?;
    let waiting_for = format!("{pattern} answered with a 2xx");
    let deadline = Instant::now() + timeout;
    let mut passed_over = Vec::<(Option<SessionId>, String)>::new();
    let (mut other_stack, mut unreadable) = (0, 0);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let answered = driver
            .wait_for_seen(step, &waiting_for, since, left, |seen| {
                pattern.matches(&seen.method, &seen.url)
                    && answered_2xx(seen)
                    && !passed_over.contains(&(seen.session.clone(), seen.request_id.clone()))
            })
            .await
            .map_err(|error| match error {
                StepError::Timeout { step, .. } => StepError::Timeout {
                    step,
                    waited: timeout,
                    waiting_for: format!(
                        "{waiting_for} ({other_stack} other stack's and {unreadable} unreadable registration(s) \
                         answered with a 2xx passed over)"
                    ),
                },
                other => other,
            })?;
        let read = driver.read_body(step, &answered).await?;
        if registered(&pattern, &read) {
            return Ok(read);
        }
        if read.body.is_some() {
            other_stack += 1;
        } else {
            unreadable += 1;
        }
        passed_over.push((read.session, read.request_id));
    }
}

/// What `requests` hold of endpoint registrations ([`REGISTRATION`], either stack's, by URL), for the run's notes: how
/// many were answered with a 2xx, how many failed in the page or were answered otherwise (each URL, without its query, with its count),
/// and how many got neither yet.
#[must_use]
pub fn registrations_note(who: &str, requests: &[SeenRequest]) -> String {
    let Ok(pattern) = registration() else {
        return format!("{who}: can't read the registrations");
    };
    let (mut answered, mut pending) = (0, 0);
    let mut denied = BTreeMap::<&str, usize>::new();
    for request in requests
        .iter()
        .filter(|request| pattern.matches(&request.method, &request.url))
    {
        if answered_2xx(request) {
            answered += 1;
        } else if request.failed.is_some() || request.status.is_some() {
            *denied.entry(without_query(&request.url)).or_default() += 1;
        } else {
            pending += 1;
        }
    }
    let denied = if denied.is_empty() {
        "none".to_owned()
    } else {
        denied
            .iter()
            .map(|(url, count)| format!("{count} × {url}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "{who}: endpoint registrations: {answered} answered with a 2xx; failed or answered otherwise: {denied}; \
         {pending} unanswered"
    )
}

/// The text of the edited message: the message's own text with a suffix and no new space (the editor turned the space
/// before a typed ` (edited)` into a no-break space, seen live 2026-10-20, so no page showed the text as typed).
#[must_use]
pub fn edited_text(nonce: &str) -> String {
    format!("{}-edited", message_text(nonce))
}

/// How long a run with `activity` stays open after a's last action.
#[must_use]
pub fn observation(activity: Activity) -> Duration {
    match activity {
        Activity::Reported => OBSERVE,
        Activity::HeldBack => OBSERVE_QUIET,
    }
}

/// The scenario's name for `activity`.
#[must_use]
pub fn scenario(activity: Activity) -> ScenarioName {
    match activity {
        Activity::Reported => ScenarioName::AbTrouter,
        Activity::HeldBack => ScenarioName::AbTrouterQuiet,
    }
}

/// Runs `ab-trouter` or `ab-trouter-quiet` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, no Chrome, no fence, a browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions, activity: Activity) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let name = scenario(activity);
    let session_options = SessionOptions {
        live_members: true,
        capture: Some(name.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::System,
        user_agent: None,
        ..SessionOptions::fail_closed()
    };
    let a = LabSession::open_with(paths, Alias::A, session_options.clone()).await?;
    let b = match LabSession::open_with(paths, Alias::B, session_options).await {
        Ok(b) => b,
        Err(error) => {
            // a's browser goes down whatever happened
            let _ = a.close().await;
            return Err(error.into());
        }
    };
    let nonce = nonce();
    let mri = |alias| config.account(alias).mri.as_deref().unwrap_or_default();
    let chat_id = one_to_one_id(mri(Alias::A), mri(Alias::B));
    let steps = Steps {
        mri_a: mri(Alias::A),
        mri_b: mri(Alias::B),
        chat_id: chat_id.as_deref(),
        nonce: &nonce,
        options,
        observe: observation(activity),
        reaction_waits: ReactionWaits::default(),
    };
    Ok(finish(a, b, activity, &steps).await)
}

/// Drives both admitted accounts and closes both even when a step, a screenshot or the first close fails.
async fn finish(mut a: LabSession, mut b: LabSession, activity: Activity, steps: &Steps<'_>) -> ScenarioReport {
    let mut report = ScenarioReport::new(scenario(activity));
    if let Err(error) = deny_probes(&a, Some(&probe_text(steps.nonce))) {
        report.failures.push(error.to_string());
    }
    if activity == Activity::HeldBack {
        match hold_activity() {
            Ok(rule) => {
                a.force_deny_frames(rule.clone());
                b.force_deny_frames(rule);
            }
            Err(error) => report.failures.push(format!("can't hold the activity back: {error}")),
        }
    }
    let policy = trouter_policy(activity, steps.chat_id).unwrap_or_else(|error| {
        report
            .failures
            .push(format!("can't tell the a–b chat in a URL: {error}"));
        DenyPolicy::Reviewed(Review::new().expecting_forced(expected_forced(activity)))
    });
    if let Err(error) = steps.run(&mut a, &mut b, &mut report).await {
        report.failures.push(error.to_string());
        for (alias, session) in [(Alias::A, &a), (Alias::B, &b)] {
            if let Some(note) = save_screenshot(alias, session).await {
                report.notes.push(note);
            }
        }
    }
    for (alias, session) in [(Alias::A, a), (Alias::B, b)] {
        report.close_account(alias, session, &policy).await;
    }
    report
}

struct Steps<'a> {
    mri_a: &'a str,
    mri_b: &'a str,
    chat_id: Option<&'a str>,
    nonce: &'a str,
    options: RunOptions,
    /// How long both pages stay open after a's last action ([`observation`]).
    observe: Duration,
    reaction_waits: ReactionWaits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReactionWaits {
    quick: Duration,
    action: Duration,
}

impl Default for ReactionWaits {
    fn default() -> Self {
        Self {
            quick: QUICK_LOOK,
            action: ACTION,
        }
    }
}

impl Steps<'_> {
    async fn run(
        &self,
        a: &mut LabSession,
        b: &mut LabSession,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let chat_id = self
            .chat_id
            .ok_or_else(|| setup("lab.toml has no MRI for a or b: run `proto-lab login` for them".to_owned()))?;
        let mut drive_b = self.open_b(b, chat_id, report).await?;
        let mut drive_a = a.driver();
        load(Alias::A, a, self.options.load_timeout).await?;
        report.notes.push("a: the web client loaded".to_owned());
        size(a).await?;
        drive_a
            .navigate("a opens the chat with b", &chat_link(self.mri_b))
            .await?;
        // the chat link loads the whole client again (on the other origin, at times), which under load outlasts
        // `CHAT_OPEN` (seen live 2026-10-20): the compose box gets a load's time
        drive_a
            .wait_for("a's compose box", COMPOSE, self.options.load_timeout.max(CHAT_OPEN))
            .await?;
        let names_chat = chat_url(chat_id).map_err(|error| setup(error.to_string()))?;
        let probe = probe_text(self.nonce);
        enforce(a, &mut drive_a, &probe, &names_chat).await?;
        report
            .notes
            .push("enforcement: a's probe post was denied by the fence and failed in a's page".to_owned());

        let message = message_text(self.nonce);
        send(&mut drive_a, "a (the message)", &message).await?;
        let sent = Instant::now();
        wait_for_newest(
            &mut drive_b,
            "b shows the message without reloading",
            &message,
            DELIVERY,
        )
        .await?;
        report.notes.push(format!(
            "a's message showed in b's open chat {} ms after a's Enter",
            sent.elapsed().as_millis()
        ));
        if drive_b.shows_text("b must not have the probe", &probe).await? {
            return Err(setup("b received the probe the fence denied".to_owned()));
        }
        self.edit_and_react(a, &mut drive_a, &mut drive_b, report).await;
        report
            .notes
            .push(format!("observing for {:?}, nothing driven", self.observe));
        tokio::time::sleep(self.observe).await;
        report.notes.push(format!(
            "b: {} WebSocket frame(s) denied so far",
            b.frame_denials().len()
        ));
        for (who, driver) in [("a", &mut drive_a), ("b", &mut drive_b)] {
            driver.seen_so_far();
            report.notes.push(registrations_note(who, driver.requests()));
        }
        Ok(())
    }

    /// Step 2: b loads, registers its messaging endpoint, opens the chat with a (registering again) and reads it.
    async fn open_b(
        &self,
        b: &mut LabSession,
        chat_id: &str,
        report: &mut ScenarioReport,
    ) -> Result<Driver, ScenarioError> {
        let opened = chat_requests(chat_id).map_err(|error| setup(error.to_string()))?;
        let mut drive_b = b.driver();
        load(Alias::B, b, self.options.load_timeout).await?;
        report.notes.push("b: the web client loaded".to_owned());
        wait_registered(&mut drive_b, "b registers its messaging endpoint", 0).await?;
        size(b).await?;
        // what the chat's document sends from here on: its registration, not the first document's
        let chat_document = drive_b.seen_so_far();
        drive_b
            .navigate("b opens the chat with a", &chat_link(self.mri_a))
            .await?;
        drive_b
            .wait_for_request("b reads the a–b chat", &opened, CHAT_OPEN)
            .await?;
        wait_registered(&mut drive_b, "b registers its messaging endpoint again", chat_document).await?;
        report
            .notes
            .push("b: the a–b chat is open and the messaging endpoint registered".to_owned());
        Ok(drive_b)
    }

    /// Step 5: a edits its message and reacts to it; b must show the edit. A failure is kept in the report.
    async fn edit_and_react(
        &self,
        a: &LabSession,
        drive_a: &mut Driver,
        drive_b: &mut Driver,
        report: &mut ScenarioReport,
    ) {
        let message = message_text(self.nonce);
        let edited = edited_text(self.nonce);
        let edit = async {
            edit(a, drive_a, &message, &edited).await?;
            let sent = Instant::now();
            wait_for_newest(drive_b, "b shows the edit", &edited, DELIVERY).await?;
            Ok::<_, ScenarioError>(sent.elapsed())
        };
        match edit.await {
            Ok(after) => report.notes.push(format!(
                "a's edit showed in b's open chat {} ms after it was sent",
                after.as_millis()
            )),
            Err(error) => {
                report.failures.push(format!("the edit: {error}"));
                report.notes.push(tids_note("a", drive_a).await);
            }
        }
        // the message as a's page shows it now: edited, or not when the edit failed
        let shown = match drive_a.shows_text("a's page shows the edit", &edited).await {
            Ok(true) => edited,
            _ => message,
        };
        let clicked = Instant::now();
        match react(a, drive_a, &shown, self.reaction_waits).await {
            Ok(way) => report.notes.push(format!(
                "a reacted to its message with a like from {way}, answered {} ms after it began",
                clicked.elapsed().as_millis()
            )),
            Err(error) => {
                report.failures.push(format!("the reaction: {error}"));
                report.notes.push(tids_note("a", drive_a).await);
            }
        }
    }
}

/// Sizes `session`'s page to [`VIEWPORT`].
pub(crate) async fn size(session: &LabSession) -> Result<(), ScenarioError> {
    let (width, height) = VIEWPORT;
    session
        .cdp()
        .call(
            Some(session.page()),
            "Emulation.setDeviceMetricsOverride",
            json!({"width": width, "height": height, "deviceScaleFactor": 1, "mobile": false}),
        )
        .await
        .map(drop)
        .map_err(|error| setup(format!("can't size the page: {error}")))
}

/// Moves the mouse over the last leaf element whose text holds `text` inside the chat's messages
/// (`[data-tid="chat-pane-message"]`; anywhere on `session`'s page when no message holds it), so the client shows that
/// message's actions. The same text also shows elsewhere (the chat list's preview, notices), which once took the
/// hover away from the message (seen live 2026-10-20). Gives the point the mouse rests on.
async fn hover(session: &LabSession, driver: &Driver, text: &str) -> Result<(f64, f64), ScenarioError> {
    let expression = format!(
        "(() => {{ const t = {}; const leaves = s => [...document.querySelectorAll(s)].filter(e => \
         e.childElementCount === 0 && (e.textContent || '').includes(t)); let all = \
         leaves('[data-tid=\"chat-pane-message\"] *'); if (!all.length) all = leaves('body *'); const e = \
         all[all.length - 1]; if (!e) return null; e.scrollIntoView({{block: 'center'}}); const r = \
         e.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; }})()",
        js_string(text)
    );
    let point = driver.evaluate("find the message", &expression).await?;
    let (Some(x), Some(y)) = (point[0].as_f64(), point[1].as_f64()) else {
        return Err(setup("no element shows the message".to_owned()));
    };
    for dx in [-40.0, 0.0] {
        mouse(session, "mouseMoved", (x + dx, y), "none").await?;
    }
    Ok((x, y))
}

/// Clicks `button` (`left` or `right`) at `point` of `session`'s page, as a mouse would: pressed, then released.
async fn click_at(session: &LabSession, point: (f64, f64), button: &str) -> Result<(), ScenarioError> {
    for kind in ["mousePressed", "mouseReleased"] {
        mouse(session, kind, point, button).await?;
    }
    Ok(())
}

/// One mouse event of `kind` at `(x, y)` of `session`'s page; `button` is `none`, `left` or `right`.
async fn mouse(session: &LabSession, kind: &str, (x, y): (f64, f64), button: &str) -> Result<(), ScenarioError> {
    let mut params = json!({"type": kind, "x": x, "y": y, "button": button});
    if kind != "mouseMoved" {
        params["clickCount"] = json!(1);
    }
    session
        .cdp()
        .call(Some(session.page()), "Input.dispatchMouseEvent", params)
        .await
        .map(drop)
        .map_err(|error| setup(format!("can't use the mouse: {error}")))
}

/// a edits `message` to `edited`: its actions' "more options", "Edit", the whole text replaced, Enter; the edit's
/// `PUT` must go out within [`ACTION`].
async fn edit(a: &LabSession, drive_a: &mut Driver, message: &str, edited: &str) -> Result<(), ScenarioError> {
    let put = RequestPattern::new(Some("PUT"), MESSAGE_EDIT).map_err(|error| setup(error.to_string()))?;
    hover(a, drive_a, message).await?;
    drive_a.wait_for("the message's actions", MORE, ACTION).await?;
    drive_a.click("a opens the message's options", MORE).await?;
    drive_a.wait_for("the Edit entry", EDIT, ACTION).await?;
    drive_a.click("a chooses Edit", EDIT).await?;
    let mark = format!(
        "(() => {{ const t = {}; const e = [...document.querySelectorAll('[contenteditable=\"true\"]')].find(e => \
         (e.innerText || '').includes(t)); if (!e) return false; e.setAttribute('data-lab-editor', '1'); e.focus(); \
         const s = window.getSelection(); s.removeAllRanges(); const r = document.createRange(); \
         r.selectNodeContents(e); s.addRange(r); return true; }})()",
        js_string(message)
    );
    drive_a
        .wait_until_true("a's message editor", &mark, "an editor holding the message", ACTION)
        .await?;
    drive_a.type_into("a types the edit", EDITOR_MARK, edited).await?;
    drive_a.press_enter("a saves the edit").await?;
    drive_a.wait_for_request("a's edit goes out", &put, ACTION).await?;
    Ok(())
}

/// a reacts to `message` with a like, tried in this order, each given [`QUICK_LOOK`]: the hover bar the mouse on the
/// message shows ([`LIKE`], as `ab-chat-full` reacts), then a click on the message, then its context menu (a right
/// click; [`ANY_REACTION`] for both, the menu given [`ACTION`]). The reaction's `PUT` must go out and be answered with a
/// 2xx within [`ACTION`]. Gives where the like was found.
async fn react(
    a: &LabSession,
    drive_a: &mut Driver,
    message: &str,
    waits: ReactionWaits,
) -> Result<&'static str, ScenarioError> {
    let put = RequestPattern::new(Some("PUT"), REACTION).map_err(|error| setup(error.to_string()))?;
    let point = hover(a, drive_a, message).await?;
    let way = if click_visible(drive_a, "a likes it on the hover bar", LIKE, waits.quick)
        .await
        .is_ok()
    {
        "its hover bar"
    } else {
        click_at(a, point, "left").await?;
        if click_visible(
            drive_a,
            "a likes it once it clicked the message",
            ANY_REACTION,
            waits.quick,
        )
        .await
        .is_ok()
        {
            "a click on the message"
        } else {
            click_at(a, point, "right").await?;
            click_visible(
                drive_a,
                "a likes it from the message's context menu",
                ANY_REACTION,
                waits.action,
            )
            .await?;
            "its context menu"
        }
    };
    let sent = drive_a
        .wait_for_request("a's reaction goes out", &put, waits.action)
        .await?;
    let same = |seen: &SeenRequest| seen.session == sent.session && seen.request_id == sent.request_id;
    let answer = drive_a
        .wait_for_seen(
            "a's reaction is answered",
            "the reaction's answer",
            0,
            waits.action,
            |seen| same(seen) && (seen.status.is_some() || seen.failed.is_some()),
        )
        .await?;
    match (answer.status, answer.failed) {
        (Some(200..=299), _) => Ok(way),
        (status, failed) => Err(setup(format!(
            "a's reaction ({way}) was answered {status:?}, failed {failed:?}"
        ))),
    }
}

/// Clicks the first visible element matching `css`, waiting up to `timeout` for one (as `ab-chat-full` does): a
/// control the page keeps, hidden, in more than one place shows only where the mouse is.
async fn click_visible(drive: &mut Driver, step: &str, css: &str, timeout: Duration) -> Result<(), StepError> {
    let expression = format!(
        "(() => {{ const e = [...document.querySelectorAll({})].find(e => {{ const r = e.getBoundingClientRect(); \
         return r.width > 0 && r.height > 0; }}); if (!e) return false; e.click(); return true; }})()",
        js_string(css)
    );
    drive
        .wait_until_true(
            step,
            &expression,
            &format!("a visible element matching {css:?}"),
            timeout,
        )
        .await
}

/// For a failed edit or reaction: the `data-tid`s the page shows that name messages, reactions or editing (the
/// client's control names, no page text).
async fn tids_note(who: &str, driver: &Driver) -> String {
    let expression = "[...new Set([...document.querySelectorAll('[data-tid]')].map(e => e.dataset.tid))].filter(t => \
                      /message|reaction|edit/i.test(t) && !/[0-9a-f]{8}-|19:|8:orgid/.test(t)).join(' ')";
    match driver.evaluate("the page's message controls", expression).await {
        Ok(value) => format!("{who}: message controls on the page: {}", value.as_str().unwrap_or("?")),
        Err(error) => format!("{who}: can't list the message controls: {error}"),
    }
}

fn setup(reason: String) -> ScenarioError {
    ScenarioError::Setup(reason)
}

#[cfg(test)]
#[path = "../../tests/support/scenario/ab_trouter_driver_tests.rs"]
mod tests;
