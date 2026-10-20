//! The `ab-trouter` scenario (M1 Task 14, `docs/protocol/trouter.md`): account b's push channel (Trouter) while a
//! sends, edits and reacts in the a–b chat, recorded for the native client's Trouter notes.
//!
//! Both accounts run with the **messaging set**: the write mapping with members read live, as `ab-chat`, plus the
//! Trouter session's own frames ([`FrameClassifier::messaging`]: authenticate, keep-alives, the client's own
//! `user.activity`, the loss acknowledgement) and the registration of the messaging stack's endpoint only
//! ([`Classifier::messaging`]: `TeamsCDLWebWorker`, never the calling stack's `SkypeSpacesWeb`). The responses to what
//! the service delivers stay unknown and denied, so the run shows what an endpoint that never answers a delivery gets.
//! `ab-trouter-quiet` ([`Activity::HeldBack`]) also holds back every `user.activity`, to see whether the service treats
//! an endpoint that never reports its activity differently.
//!
//! The steps:
//! 1. Before anything loads, a's fence force-denies every write carrying the probe marker, as in `ab-chat`
//!    ([`super::ab_chat`]).
//! 2. b opens the web client and must register its messaging endpoint, then opens the chat with a (a new document,
//!    which registers again) and must read it: b now shows the a–b chat, with its push channel up.
//! 3. a opens the web client and the chat with b, and proves enforcement live with this run's probe (`ab-chat`'s
//!    step 4).
//! 4. a sends the message; b, already showing the chat, must show it within [`DELIVERY`] (it can only come by push or
//!    by a read b makes by itself) and never the probe. The time from a's Enter to b showing it is noted.
//! 5. a edits the message and reacts to it from the message's own actions ([`edit`], [`react`]); b must show the
//!    edit. These steps failing fails the run but doesn't end it: the observation still runs.
//! 6. Both pages stay open, nothing driven, for [`observation`] (when the activity is reported, longer than the
//!    lifetime a chat delivery carries): the capture shows whether the service delivers again what went unanswered,
//!    how often and for how long, and whether it closes the socket.
//! 7. b's messaging registration must have succeeded ([`registered`]). Judged as `ab-chat`, plus what comes up only
//!    with the push channel ([`trouter_policy`]): no deny but the probes' forced ones, the reviewed expected-deny list
//!    (the denied delivery responses are Trouter frames, on it) and [`TROUTER_RUN_REQUESTS`] (the calling stack's
//!    registration, presence, the activity feed's read state, agent invocations); nothing denied on the a–b chat.
//!
//! Nothing in the run answers a call, and nothing registers the calling stack's endpoint.

use std::time::Duration;

use serde_json::json;
use tokio::time::Instant;

use crate::browser::Media;
use crate::classify::Classifier;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_ws::FrameClassifier;
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{
    CHAT_OPEN, COMPOSE, DELIVERY, EXPECTED_FORCED, chat_link, chat_requests, chat_url, deny_probes, enforce, load,
    message_text, nonce, one_to_one_id, probe_text, save_screenshot, send,
};
use super::dsl::{Driver, RequestPattern, Selector};
use super::expected::{ExpectedRequest, Review, TEAMS};
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// Whether the client's own `user.activity` goes out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    /// It goes out, as the messaging set allows (`ab-trouter`).
    Reported,
    /// Every `user.activity` is denied (`ab-trouter-quiet`).
    HeldBack,
}

/// The registration of an endpoint with the registrar (either stack's; the fence lets only the messaging stack's out).
pub const REGISTRATION: &str = r"/registrar/prod/[Vv]2/registrations$";

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

/// The first quick reaction of a message's actions or of its "more options" menu.
const QUICK_REACTION: Selector<'static> = Selector::Css(r#"[data-tid^="default-reaction-"]"#);

/// How long the hover alone gets to show quick reactions before [`react`] opens the menu.
const QUICK_LOOK: Duration = Duration::from_secs(5);

/// The attribute [`edit`] marks the message's editor with, so the typing goes there and not to the compose box.
const EDITOR_MARK: Selector<'static> = Selector::Css(r#"[data-lab-editor="1"]"#);

/// The page's size: the client lays out a message's actions in full.
const VIEWPORT: (u32, u32) = (1440, 900);

/// The denied classes a run of this scenario expects besides the reviewed list: what comes up only once the push
/// channel is (seen live 2026-10-20), none of it needed for chat.
pub const TROUTER_RUN_REQUESTS: &[ExpectedRequest] = &[
    ExpectedRequest {
        class: "the calling stack's endpoint registration",
        hosts: TEAMS,
        method: "POST",
        path: "/registrar/prod/V2/registrations",
        reason: "the calling stack registers its own endpoint (`SkypeSpacesWeb`) once the push channel is up; the \
                 messaging set lets only the messaging stack's registration out (b's is checked to have gone out), \
                 and the account must not become reachable for calls",
    },
    ExpectedRequest {
        class: "presence endpoint",
        hosts: TEAMS,
        method: "PUT",
        path: "/ups/*/v1/me/endpoints/",
        reason: "creates the client's presence endpoint (availability and activity reported over the push channel): \
                 the scenario needs no presence, and setting it is out of M1's scope",
    },
    ExpectedRequest {
        class: "presence subscriptions",
        hosts: TEAMS,
        method: "POST",
        path: "/ups/*/v1/pubsub/subscriptions/*",
        reason: "subscribes the account's push endpoint to other people's presence: the scenario shows no presence",
    },
    ExpectedRequest {
        class: "activity feed read state",
        hosts: TEAMS,
        method: "PUT",
        path: "/api/chatsvc/*/v1/users/ME/conversations/48%3Anotifications/messages/*/properties",
        reason: "marks an item of the account's own activity feed (a pseudo-conversation) as read, retried while it is \
                 denied: the scenario reads no feed, and whether it becomes reviewed own-state is for the feed's own \
                 work, not this capture",
    },
    ExpectedRequest {
        class: "agent invocation",
        hosts: TEAMS,
        method: "POST",
        path: "/api/chatsvc/*/v1/agents/*/invoke",
        reason: "the client invoking a bot or agent (a `28:` participant, outside the lab roster) by itself: it \
                 reaches a service no one approved and stays denied; the scenario needs none",
    },
];

/// How a run of this scenario judges its denies: `ab-chat`'s policy ([`super::ab_chat::policy`]) plus
/// [`TROUTER_RUN_REQUESTS`].
///
/// # Errors
/// Never for an id from [`one_to_one_id`]; when the chat's pattern doesn't compile.
pub fn trouter_policy(chat_id: Option<&str>) -> Result<DenyPolicy, regex::Error> {
    let mut review = Review::new()
        .expecting_forced(&EXPECTED_FORCED)
        .expecting_requests(TROUTER_RUN_REQUESTS);
    if let Some(chat_id) = chat_id {
        review = review.with_mapped_target(chat_url(chat_id)?);
    }
    Ok(DenyPolicy::Reviewed(review))
}

/// Whether `driver` saw a registration ([`REGISTRATION`]) answered with a success: the messaging stack's, the only one
/// the messaging set lets out.
#[must_use]
pub fn registered(driver: &Driver) -> bool {
    let Ok(pattern) = RequestPattern::new(Some("POST"), REGISTRATION) else {
        return false;
    };
    driver.requests().iter().any(|request| {
        pattern.matches(&request.method, &request.url)
            && request.status.is_some_and(|status| (200..300).contains(&status))
    })
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

/// The frame classifier of a run with `activity`.
#[must_use]
pub fn frames(activity: Activity) -> FrameClassifier {
    match activity {
        Activity::Reported => FrameClassifier::messaging(),
        Activity::HeldBack => FrameClassifier::messaging().without_activity(),
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
        classifier: Classifier::messaging(),
        frames: frames(activity),
        live_members: true,
        capture: Some(name.as_str().to_owned()),
        display: options.display,
        media: Media::System,
        user_agent: None,
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
    let mut report = ScenarioReport::new(name);
    let nonce = nonce();
    if let Err(error) = deny_probes(&a, &probe_text(&nonce)) {
        report.failures.push(error.to_string());
    }
    let mri = |alias| config.account(alias).mri.as_deref().unwrap_or_default();
    let chat_id = one_to_one_id(mri(Alias::A), mri(Alias::B));
    let policy = trouter_policy(chat_id.as_deref()).unwrap_or_else(|error| {
        report
            .failures
            .push(format!("can't tell the a–b chat in a URL: {error}"));
        DenyPolicy::Reviewed(
            Review::new()
                .expecting_forced(&EXPECTED_FORCED)
                .expecting_requests(TROUTER_RUN_REQUESTS),
        )
    });
    let steps = Steps {
        mri_a: mri(Alias::A),
        mri_b: mri(Alias::B),
        chat_id: chat_id.as_deref(),
        nonce: &nonce,
        options,
        observe: observation(activity),
    };
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
    Ok(report)
}

struct Steps<'a> {
    mri_a: &'a str,
    mri_b: &'a str,
    chat_id: Option<&'a str>,
    nonce: &'a str,
    options: RunOptions,
    /// How long both pages stay open after a's last action ([`observation`]).
    observe: Duration,
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
        drive_b
            .wait_for_text("b shows the message without reloading", &message, DELIVERY)
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
        if !registered(&drive_b) {
            return Err(setup("b's messaging endpoint registration never succeeded".to_owned()));
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
        let registration = RequestPattern::new(Some("POST"), REGISTRATION).map_err(|error| setup(error.to_string()))?;
        let opened = chat_requests(chat_id).map_err(|error| setup(error.to_string()))?;
        let mut drive_b = b.driver();
        load(Alias::B, b, self.options.load_timeout).await?;
        report.notes.push("b: the web client loaded".to_owned());
        drive_b
            .wait_for_request("b registers its messaging endpoint", &registration, REGISTER)
            .await?;
        size(b).await?;
        drive_b
            .navigate("b opens the chat with a", &chat_link(self.mri_a))
            .await?;
        drive_b
            .wait_for_request("b reads the a–b chat", &opened, CHAT_OPEN)
            .await?;
        drive_b
            .wait_for_request("b registers its messaging endpoint again", &registration, REGISTER)
            .await?;
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
            drive_b.wait_for_text("b shows the edit", &edited, DELIVERY).await?;
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
        match react(a, drive_a, &shown).await {
            Ok(()) => report.notes.push("a reacted to its message".to_owned()),
            Err(error) => {
                report.failures.push(format!("the reaction: {error}"));
                report.notes.push(tids_note("a", drive_a).await);
            }
        }
    }
}

/// Sizes `session`'s page to [`VIEWPORT`].
async fn size(session: &LabSession) -> Result<(), ScenarioError> {
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
/// hover away from the message (seen live 2026-10-20).
async fn hover(session: &LabSession, driver: &Driver, text: &str) -> Result<(), ScenarioError> {
    let expression = format!(
        "(() => {{ const t = {}; const leaves = s => [...document.querySelectorAll(s)].filter(e => \
         e.childElementCount === 0 && (e.textContent || '').includes(t)); let all = \
         leaves('[data-tid=\"chat-pane-message\"] *'); if (!all.length) all = leaves('body *'); const e = \
         all[all.length - 1]; if (!e) return null; e.scrollIntoView({{block: 'center'}}); const r = \
         e.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; }})()",
        serde_json::Value::String(text.to_owned())
    );
    let point = driver.evaluate("find the message", &expression).await?;
    let (Some(x), Some(y)) = (point[0].as_f64(), point[1].as_f64()) else {
        return Err(setup("no element shows the message".to_owned()));
    };
    for (dx, kind) in [(-40.0, "mouseMoved"), (0.0, "mouseMoved")] {
        session
            .cdp()
            .call(
                Some(session.page()),
                "Input.dispatchMouseEvent",
                json!({"type": kind, "x": x + dx, "y": y}),
            )
            .await
            .map_err(|error| setup(format!("can't move the mouse: {error}")))?;
    }
    Ok(())
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
        serde_json::Value::String(message.to_owned())
    );
    drive_a
        .wait_until_true("a's message editor", &mark, "an editor holding the message", ACTION)
        .await?;
    drive_a.type_into("a types the edit", EDITOR_MARK, edited).await?;
    drive_a.press_enter("a saves the edit").await?;
    drive_a.wait_for_request("a's edit goes out", &put, ACTION).await?;
    Ok(())
}

/// a reacts to `message` with the first quick reaction of its actions, or, when the hover shows none (seen live
/// 2026-10-20), of its "more options" menu (the bundle also puts reactions there, `message-reactions-on-menu`); the
/// reaction's `PUT` must go out within [`ACTION`].
async fn react(a: &LabSession, drive_a: &mut Driver, message: &str) -> Result<(), ScenarioError> {
    let put = RequestPattern::new(Some("PUT"), REACTION).map_err(|error| setup(error.to_string()))?;
    hover(a, drive_a, message).await?;
    if drive_a
        .wait_for("the message's quick reactions", QUICK_REACTION, QUICK_LOOK)
        .await
        .is_err()
    {
        drive_a.wait_for("the message's actions", MORE, ACTION).await?;
        drive_a.click("a opens the message's options", MORE).await?;
        drive_a
            .wait_for("the menu's quick reactions", QUICK_REACTION, ACTION)
            .await?;
    }
    drive_a.click("a reacts", QUICK_REACTION).await?;
    drive_a.wait_for_request("a's reaction goes out", &put, ACTION).await?;
    Ok(())
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
