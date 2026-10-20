//! The `ab-chat-full` scenario: the a–b 1:1 chat beyond a plain message, recorded for the protocol notes
//! (`docs/protocol/chat.md`). It runs under `ab-chat`'s fence: the write mapping ([`Classifier::mapped`]), members read
//! live, `ab-chat`'s forced-deny probes and its deny policy ([`ab_chat::policy`]: the reviewed expected-deny list, and
//! nothing denied on the a–b chat but the probes).
//!
//! 1. a's probes are force-denied from before its page loads (`ab_chat::deny_probes`); a and b load the web client.
//! 2. a opens the chat with b, and the live enforcement test comes first: this run's probe must be denied by the fence
//!    and fail in a's page (`ab_chat::enforce`).
//! 3. a, each step a write on the a–b chat whose answer the step waits for ([`Writes`]): sends a message, edits it,
//!    reacts to it and takes the reaction back, replies to the first message with a quote, reacts to the reply and
//!    leaves the reaction there (so b's read of the chat shows a reaction with its user), and sends a last message and
//!    deletes it ([`Texts`]).
//! 4. b opens the chat (it reads the chat's history: message pages and their sync state), shows a's edited message,
//!    goes to its compose box (whether the client then sends a read receipt is noted, not required), and reacts to a's
//!    first message.
//! 5. The run is judged as `ab-chat` is.
//!
//! No mention: under the fence the web client's mention picker can't offer b. The chat partner shows as "Unknown User"
//! because the profile lookups behind the picker are middle-tier `POST`s the fence denies, and the picker's only
//! suggestion is a message extension (live, 2026-10-20). The mention's wire form is
//! described from the web bundle in `docs/protocol/chat.md`, marked as not captured
//! (`docs/backlog/mention-not-captured-under-the-fence.md`).
//!
//! A step that fails after the enforcement test (the web client's controls moved, a write denied) is reported, with a
//! screenshot named after it in the capture dir and the page's visible `data-tid`s in the notes, and the run goes on
//! with the next step: each step is a capture of its own. Loading and the enforcement test end the run when they fail.
//!
//! The web client's controls, by their `data-tid` (from the web bundle, checked live): a message is
//! `chat-pane-message`; pointing at it shows its hover bar, with the quick reactions (`message-actions-like`, …), some
//! actions (`message-hover-action-edit`, `message-hover-action-quoted-reply`) and the more-options menu
//! (`message-actions-more`) holding the rest (`message-actions-edit`, `message-actions-delete`,
//! `message-actions-quoted-reply`).

use std::time::Duration;

use serde_json::Value;

use crate::browser::Media;
use crate::classify::Classifier;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_http::without_query;
use crate::fence_ws::FrameClassifier;
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{
    self, COMPOSE, DELIVERY, EXPECTED_FORCED, chat_in_url, chat_link, chat_requests, chat_url, deny_probes, enforce,
    load, nonce, one_to_one_id, probe_text, save_screenshot_as, setup,
};
use super::dsl::{Driver, Key, RequestPattern, SeenRequest, Selector, StepError};
use super::expected::Review;
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// How long a step waits for a control to show, or for its write to be answered.
pub const STEP: Duration = Duration::from_secs(30);

/// How long a chat gets to open from its deep link: longer than `ab-chat`'s, since this scenario's pages are busier
/// and, on a loaded machine (load average 50), the client still showed its splash after a minute (2026-10-20).
pub const CHAT_OPEN: Duration = Duration::from_secs(180);

/// How long one call to a page may go unanswered: the web client loading a chat on that machine kept its renderer
/// from answering an evaluation for over 30 s ([`super::dsl::CALL_TIMEOUT`]).
pub const PAGE_CALL: Duration = Duration::from_secs(90);

/// How long a step looks for an action on the hover bar before it tries the more-options menu.
const HOVER_BAR: Duration = Duration::from_secs(3);

/// The attribute the scenario marks the message it acts on with ([`find_message`]).
const MARK: &str = "data-lab-mark";

/// The message [`find_message`] marked.
pub const MARKED: Selector<'static> = Selector::Css("[data-lab-mark]");

/// The quick reaction a and b use.
const LIKE: &str = r#"[data-tid="message-actions-like"]"#;

/// What a's messages say; each holds the run's nonce, and none another's text but the edit its original.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texts {
    /// The message a edits, reacts to and replies to.
    pub first: String,
    /// What a edits it to.
    pub edited: String,
    /// a's reply quoting the first message; a reacts to it and leaves the reaction.
    pub reply: String,
    /// The message a deletes.
    pub doomed: String,
}

impl Texts {
    #[must_use]
    pub fn new(nonce: &str) -> Self {
        Self {
            first: format!("lab full {nonce} first"),
            edited: format!("lab full {nonce} first, edited"),
            reply: format!("lab full {nonce} reply"),
            doomed: format!("lab full {nonce} to be deleted"),
        }
    }
}

/// The writes on the a–b chat the steps wait for, by method and URL (the chat's id plain or percent-encoded).
#[derive(Clone, Debug)]
pub struct Writes {
    /// `POST …/conversations/<chat>/messages`: a message (and typing, which a step tells apart by its body).
    pub post: RequestPattern,
    /// `PUT …/conversations/<chat>/messages/<id>`: an edit.
    pub edit: RequestPattern,
    /// `DELETE …/conversations/<chat>/messages/<id>`: a delete.
    pub delete: RequestPattern,
    /// Any method on `…/conversations/<chat>/messages/<id>/properties?name=emotions`: a reaction, added or taken back.
    pub reaction: RequestPattern,
    /// `PUT …/conversations/<chat>/properties?name=consumptionhorizon`: a read receipt.
    pub read_receipt: RequestPattern,
}

impl Writes {
    /// The writes on the chat `chat_id`.
    ///
    /// # Errors
    /// Never for an id from [`one_to_one_id`]; when a pattern doesn't compile.
    pub fn on(chat_id: &str) -> Result<Self, regex::Error> {
        let conversation = format!("(?i)/v1/users/ME/conversations/{}", chat_in_url(chat_id));
        let message = format!("{conversation}/messages/[^/?]+");
        Ok(Self {
            post: RequestPattern::new(Some("POST"), &format!(r"{conversation}/messages(\?|$)"))?,
            edit: RequestPattern::new(Some("PUT"), &format!(r"{message}(\?|$)"))?,
            delete: RequestPattern::new(Some("DELETE"), &format!(r"{message}(\?|$)"))?,
            reaction: RequestPattern::new(None, &format!(r"{message}/properties\?(.*&)?name=emotions(&|$)"))?,
            read_receipt: RequestPattern::new(
                Some("PUT"),
                &format!(r"{conversation}/properties\?(.*&)?name=consumptionhorizon(&|$)"),
            )?,
        })
    }
}

/// Runs `ab-chat-full` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, no Chrome, no fence, a browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let session_options = SessionOptions {
        classifier: Classifier::mapped(),
        frames: FrameClassifier::seeded(),
        live_members: true,
        capture: Some(ScenarioName::AbChatFull.as_str().to_owned()),
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
    let mut report = ScenarioReport::new(ScenarioName::AbChatFull);
    let nonce = nonce();
    // before a's page loads: the client sends an earlier run's unsent probe again as soon as it can
    if let Err(error) = deny_probes(&a, &probe_text(&nonce)) {
        report.failures.push(error.to_string());
    }
    let mri = |alias| config.account(alias).mri.clone().unwrap_or_default();
    let chat_id = one_to_one_id(&mri(Alias::A), &mri(Alias::B));
    let policy = ab_chat::policy(chat_id.as_deref()).unwrap_or_else(|error| {
        report
            .failures
            .push(format!("can't tell the a–b chat in a URL: {error}"));
        DenyPolicy::Reviewed(Review::new().expecting_forced(&EXPECTED_FORCED))
    });
    let full = Full {
        mri_a: mri(Alias::A),
        mri_b: mri(Alias::B),
        nonce: nonce.clone(),
        texts: Texts::new(&nonce),
    };
    if let Err(error) = full.run(chat_id.as_deref(), &mut a, &mut b, &mut report, options).await {
        report.failures.push(error.to_string());
        for (alias, session) in [(Alias::A, &a), (Alias::B, &b)] {
            if let Some(note) = save_screenshot_as(alias, session, "failure.png").await {
                report.notes.push(note);
            }
        }
    }
    // both browsers go down and both captures are finished, whatever happened to either
    for (alias, session) in [(Alias::A, a), (Alias::B, b)] {
        report.close_account(alias, session, &policy).await;
    }
    Ok(report)
}

/// One run's accounts and texts.
struct Full {
    mri_a: String,
    mri_b: String,
    nonce: String,
    texts: Texts,
}

impl Full {
    /// Loading, the enforcement test, then a's and b's steps.
    async fn run(
        &self,
        chat_id: Option<&str>,
        a: &mut LabSession,
        b: &mut LabSession,
        report: &mut ScenarioReport,
        options: RunOptions,
    ) -> Result<(), ScenarioError> {
        for (alias, session) in [(Alias::A, &mut *a), (Alias::B, &mut *b)] {
            load(alias, session, options.load_timeout).await?;
            report.notes.push(format!("{alias}: the web client loaded"));
        }
        let chat_id = chat_id
            .ok_or_else(|| setup("lab.toml has no MRI for a or b: run `proto-lab login` for them".to_owned()))?;
        let writes = Writes::on(chat_id).map_err(|error| setup(error.to_string()))?;
        let names_chat = chat_url(chat_id).map_err(|error| setup(error.to_string()))?;
        let mut drive_a = a.driver();
        drive_a.set_call_timeout(PAGE_CALL);
        drive_a
            .navigate("a opens the chat with b", &chat_link(&self.mri_b))
            .await?;
        drive_a.wait_for("a's compose box", COMPOSE, CHAT_OPEN).await?;
        enforce(a, &mut drive_a, &probe_text(&self.nonce), &names_chat).await?;
        report
            .notes
            .push("enforcement: a's probe post was denied by the fence and failed in a's page".to_owned());
        let mut steps = Steps {
            writes: &writes,
            texts: &self.texts,
            report,
        };
        steps.a(a, &mut drive_a).await;
        let opened = chat_requests(chat_id).map_err(|error| setup(error.to_string()))?;
        steps.b(b, &self.mri_a, &opened).await;
        Ok(())
    }
}

/// The steps after the enforcement test, each reported on its own ([`Steps::settle`]).
struct Steps<'a> {
    writes: &'a Writes,
    texts: &'a Texts,
    report: &'a mut ScenarioReport,
}

impl Steps<'_> {
    /// a's steps, in order: each goes on from where the last one left the page.
    async fn a(&mut self, a: &LabSession, drive: &mut Driver) {
        let outcome = send(drive, &self.writes.post, "a (the first message)", &self.texts.first).await;
        self.settle(Alias::A, a, drive, "send", outcome).await;
        let outcome = self.edit(drive).await;
        self.settle(Alias::A, a, drive, "edit", outcome).await;
        let outcome = react(drive, &self.writes.reaction, "a", &self.texts.first).await;
        self.settle(Alias::A, a, drive, "react", outcome).await;
        let outcome = react(drive, &self.writes.reaction, "a (taking it back)", &self.texts.first).await;
        self.settle(Alias::A, a, drive, "unreact", outcome).await;
        let outcome = self.reply(drive).await;
        self.settle(Alias::A, a, drive, "reply", outcome).await;
        let outcome = react(drive, &self.writes.reaction, "a (on the reply)", &self.texts.reply).await;
        self.settle(Alias::A, a, drive, "react to the reply", outcome).await;
        let outcome = self.delete(drive).await;
        self.settle(Alias::A, a, drive, "delete", outcome).await;
    }

    /// b's steps: it opens the chat, shows a's edited message, goes to its compose box (its read receipt is noted if
    /// it sends one) and reacts to the message.
    async fn b(&mut self, b: &LabSession, mri_a: &str, opened: &RequestPattern) {
        let mut drive = b.driver();
        drive.set_call_timeout(PAGE_CALL);
        let outcome = self.b_reads(&mut drive, mri_a, opened).await;
        self.settle(Alias::B, b, &mut drive, "read", outcome).await;
        let outcome = react(&mut drive, &self.writes.reaction, "b", &self.texts.first).await;
        self.settle(Alias::B, b, &mut drive, "react", outcome).await;
    }

    /// Notes a step that went through; for one that failed, the failure, a screenshot (`failure-<alias>-<step>.png`
    /// in the capture dir), the `data-tid`s the page shows, and Escape twice to close what the step left open.
    async fn settle(
        &mut self,
        alias: Alias,
        session: &LabSession,
        drive: &mut Driver,
        step: &str,
        outcome: Result<(), ScenarioError>,
    ) {
        let error = match outcome {
            Ok(()) => {
                self.report.notes.push(format!("{alias}: {step}: done"));
                return;
            }
            Err(error) => error,
        };
        self.report.failures.push(format!("{alias}: {step}: {error}"));
        if let Some(note) = save_screenshot_as(alias, session, &format!("failure-{alias}-{step}.png")).await {
            self.report.notes.push(note);
        }
        let tids = drive
            .evaluate("the page's controls", VISIBLE_TIDS)
            .await
            .unwrap_or(Value::Null);
        self.report
            .notes
            .push(format!("{alias}: {step}: visible data-tids: {tids}"));
        for _ in 0..2 {
            let _ = drive.press("close what is open", Key::ESCAPE).await;
        }
    }

    /// a edits its first message to [`Texts::edited`] in the message's own editor, and waits for the edit's answer.
    async fn edit(&self, drive: &mut Driver) -> Result<(), ScenarioError> {
        find_message(drive, "a finds its first message", &self.texts.first).await?;
        message_action(drive, "a", "edit").await?;
        drive.skip_seen();
        if let Err(error) = rewrite_editor(drive, "a's edit box", &self.texts.first, &self.texts.edited).await {
            // an edit box left open sits ahead of the compose box: the next send would type into it
            cancel_edit(drive, &self.texts.first).await;
            return Err(error.into());
        }
        drive.press_enter("a saves the edit").await?;
        let edit = self.writes.edit.clone().with_body_containing(&self.texts.edited);
        let seen = drive.wait_for_answer("a's edit is put", &edit, STEP).await?;
        answered("a's edit", &seen)
    }

    /// a replies to its first message with a quote of it, from the message's actions.
    async fn reply(&self, drive: &mut Driver) -> Result<(), ScenarioError> {
        find_message(drive, "a finds its first message", &self.texts.first).await?;
        message_action(drive, "a", "quoted-reply").await?;
        send(drive, &self.writes.post, "a (the reply)", &self.texts.reply).await
    }

    /// a sends [`Texts::doomed`] and deletes it from the message's actions.
    async fn delete(&self, drive: &mut Driver) -> Result<(), ScenarioError> {
        send(
            drive,
            &self.writes.post,
            "a (the message to delete)",
            &self.texts.doomed,
        )
        .await?;
        find_message(drive, "a finds the message to delete", &self.texts.doomed).await?;
        drive.skip_seen();
        message_action(drive, "a", "delete").await?;
        let seen = drive.wait_for_answer("a's delete", &self.writes.delete, STEP).await?;
        answered("a's delete", &seen)
    }

    /// b opens the chat with a, shows a's edited message and goes to its compose box. Whether b then sends a read
    /// receipt within [`STEP`] is noted, not required: in the headless run of 2026-10-20 b's page sent none while a's
    /// did, after a typed in it. A receipt that is sent and refused fails the step.
    async fn b_reads(&mut self, drive: &mut Driver, mri_a: &str, opened: &RequestPattern) -> Result<(), ScenarioError> {
        drive.navigate("b opens the chat with a", &chat_link(mri_a)).await?;
        drive
            .wait_for_request("b reads the a–b chat", opened, CHAT_OPEN)
            .await?;
        drive
            .wait_for_text("b shows a's edited message", &self.texts.edited, DELIVERY)
            .await?;
        drive.type_into("b goes to its compose box", COMPOSE, "").await?;
        match drive
            .wait_for_answer("b's read receipt", &self.writes.read_receipt, STEP)
            .await
        {
            Ok(seen) => {
                answered("b's read receipt", &seen)?;
                self.report.notes.push("b: read receipt: sent and answered".to_owned());
            }
            Err(StepError::Timeout { .. }) => self
                .report
                .notes
                .push(format!("b: read receipt: none sent within {}s", STEP.as_secs())),
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}

/// The distinct `data-tid`s of the visible elements, for a failed step's diagnosis: the web client's names of its
/// controls, no text of the page.
const VISIBLE_TIDS: &str = r"(() => {
  const visible = (e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
  const tids = [...document.querySelectorAll('[data-tid]')].filter(visible).map((e) => e.dataset.tid);
  return [...new Set(tids)].slice(0, 200).join(' ');
})()";

/// Types `text` into the compose box, sends it ([`ab_chat::send`]) and waits for its post (`post`, holding the text)
/// to be answered.
///
/// # Errors
/// When the send fails, or its post isn't answered with a 2xx in time.
pub async fn send(drive: &mut Driver, post: &RequestPattern, who: &str, text: &str) -> Result<(), ScenarioError> {
    drive.skip_seen();
    ab_chat::send(drive, who, text).await?;
    let post = post.clone().with_body_containing(text.trim());
    let seen = drive.wait_for_answer(&format!("{who} is posted"), &post, STEP).await?;
    answered(&format!("{who}'s post"), &seen)
}

/// `who` reacts to the message holding `text` with a like from its hover bar, or takes the like back when it has
/// one, and waits for the reaction's answer.
async fn react(drive: &mut Driver, reaction: &RequestPattern, who: &str, text: &str) -> Result<(), ScenarioError> {
    find_message(drive, &format!("{who} finds the message to react to"), text).await?;
    drive.hover(&format!("{who} points at the message"), MARKED).await?;
    drive.skip_seen();
    click_visible(drive, &format!("{who} clicks like"), LIKE, STEP).await?;
    let seen = drive
        .wait_for_answer(&format!("{who}'s reaction"), reaction, STEP)
        .await?;
    answered(&format!("{who}'s reaction ({})", seen.method), &seen)
}

/// Whether a write's answer says it went through: a 2xx; a failure in the page (a fenced write fails as
/// `net::ERR_BLOCKED_BY_CLIENT`) or any other status is an error naming the request (without its query).
///
/// # Errors
/// As above.
pub fn answered(what: &str, seen: &SeenRequest) -> Result<(), ScenarioError> {
    let request = format!("{} {}", seen.method, without_query(&seen.url));
    match (seen.status, &seen.failed) {
        (Some(200..=299), _) => Ok(()),
        (_, Some(error)) => Err(setup(format!("{what}: {request} failed in the page: {error}"))),
        (status, None) => Err(setup(format!("{what}: {request} was answered {status:?}"))),
    }
}

/// Waits up to [`STEP`] for a message (`chat-pane-message`) whose text holds `text`, the oldest if several do (a
/// quote of it in a later message comes after it), and marks it as [`MARKED`] (the mark taken off any other).
///
/// # Errors
/// [`StepError::Timeout`] when there's no such message in time.
pub async fn find_message(drive: &mut Driver, step: &str, text: &str) -> Result<(), StepError> {
    let expression = format!(
        r#"(() => {{
  for (const e of document.querySelectorAll('[{MARK}]')) e.removeAttribute('{MARK}');
  const found = [...document.querySelectorAll('[data-tid="chat-pane-message"]')]
    .find((e) => (e.innerText ?? '').includes({text}));
  if (!found) return false;
  found.setAttribute('{MARK}', '');
  return true;
}})()"#,
        text = js_string(text)
    );
    drive
        .wait_until_true(step, &expression, &format!("a message holding {text:?}"), STEP)
        .await
}

/// Opens the action `action` (`edit`, `quoted-reply`, `delete`) of the message [`find_message`] marked: from its
/// hover bar (`message-hover-action-<action>`) when the bar has it within [`HOVER_BAR`], else from its more-options
/// menu (`message-actions-more`, then `message-actions-<action>`).
///
/// # Errors
/// When there's no marked message, or the action is in neither place in time.
pub async fn message_action(drive: &mut Driver, who: &str, action: &str) -> Result<(), StepError> {
    drive.hover(&format!("{who} points at the message"), MARKED).await?;
    let on_bar = format!(
        r#"[data-tid="message-hover-action-{action}"]:is(button, [role="menuitem"]), [data-tid="message-hover-action-{action}"] :is(button, [role="menuitem"])"#
    );
    let step = format!("{who} picks {action} on the hover bar");
    if click_visible(drive, &step, &on_bar, HOVER_BAR).await.is_ok() {
        return Ok(());
    }
    let more = r#"[data-tid="message-actions-more"]"#;
    click_visible(drive, &format!("{who} opens the message's menu"), more, STEP).await?;
    let in_menu = format!(r#"[data-tid="message-actions-{action}"]"#);
    click_visible(drive, &format!("{who} picks {action} in the menu"), &in_menu, STEP).await
}

/// Waits up to `timeout` for a visible element matching `css` (with a box of its own) and clicks the first one.
///
/// # Errors
/// [`StepError::Timeout`] when none shows in time.
pub async fn click_visible(drive: &mut Driver, step: &str, css: &str, timeout: Duration) -> Result<(), StepError> {
    let expression = format!(
        "(() => {{ const e = {}; if (!e) return false; e.click(); return true; }})()",
        first_visible(css)
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

/// The page expression of the first visible element matching `css` (`undefined` when there's none).
fn first_visible(css: &str) -> String {
    format!(
        "[...document.querySelectorAll({})].find((e) => {{ const r = e.getBoundingClientRect(); return r.width > 0 \
         && r.height > 0; }})",
        js_string(css)
    )
}

/// Replaces the text of the editor (`contenteditable`) holding `old` with `new`: waits up to [`STEP`] for that editor
/// to have the focus (focusing it), selects everything in it (Ctrl+A) and, once the focused element is still in an
/// editor holding `old`, types `new` there. It types into the focus rather than into an element found beforehand: the
/// web client's editor rebuilds its element as it is focused (live, 2026-10-20).
///
/// # Errors
/// [`StepError::Timeout`] when no such editor has the focus in time; when the browser refuses the input.
pub async fn rewrite_editor(drive: &mut Driver, step: &str, old: &str, new: &str) -> Result<(), StepError> {
    let old_text = js_string(old);
    let focus = format!(
        r#"(() => {{
  const editor = [...document.querySelectorAll('[contenteditable="true"]')]
    .find((e) => (e.innerText ?? '').includes({old_text}));
  if (!editor) return false;
  if (!editor.contains(document.activeElement)) editor.focus();
  return editor.contains(document.activeElement);
}})()"#
    );
    let waiting_for = format!("an editor holding {old:?} to have the focus");
    drive.wait_until_true(step, &focus, &waiting_for, STEP).await?;
    drive.press(&format!("{step}: select all"), Key::SELECT_ALL).await?;
    let focused = format!(
        r#"(() => {{
  const editor = document.activeElement?.closest('[contenteditable="true"]');
  return !!editor && (editor.innerText ?? '').includes({old_text});
}})()"#
    );
    drive
        .wait_until_true(&format!("{step}: still focused"), &focused, &waiting_for, STEP)
        .await?;
    drive.insert_text(&format!("{step}: type"), new).await
}

/// Closes an edit box still holding `text` unsaved, as far as it can: focuses it and presses Escape, which cancels an
/// edit in the web client. What fails here is left to the step's report.
async fn cancel_edit(drive: &mut Driver, text: &str) {
    let focus = format!(
        r#"(() => {{
  const editor = [...document.querySelectorAll('[contenteditable="true"]')]
    .find((e) => (e.innerText ?? '').includes({}));
  editor?.focus();
  return !!editor;
}})()"#,
        js_string(text)
    );
    if drive.evaluate("a goes back to its edit box", &focus).await.ok() == Some(Value::Bool(true)) {
        let _ = drive.press("a cancels the edit", Key::ESCAPE).await;
    }
}

/// A JavaScript string literal of `text`.
fn js_string(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}
