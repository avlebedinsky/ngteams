//! Sends a message between the two lab accounts and records both browser sessions. The receiving page must show the message from the selected chat.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::browser::Media;
use crate::capture_hold::{HeldRequest, without_query};
use crate::commands::status::account_status;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::session::{LabSession, SessionOptions, SessionState, TEAMS_URL};

use regex::Regex;

use super::dsl::{Driver, RequestPattern, Selector, StepError, js_string};
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

// The deterministic 1:1 chat id used by browser scenarios and read probes.
pub use crate::protocol::one_to_one_id;

/// The message compose box's selector: the web client's rich-text editor (seen on 2026-10-20).
pub const COMPOSE_CSS: &str = r#"[contenteditable="true"][role="textbox"]"#;

/// The message compose box.
pub const COMPOSE: Selector<'static> = Selector::Css(COMPOSE_CSS);

/// Text marking held message captures; every session keeps marked retries unsent.
pub use crate::capture_hold::HELD_MARKER;

/// How long the failure's diagnosis (the compose box's state, a screenshot) may wait for a page.
const DIAGNOSIS_WAIT: Duration = Duration::from_secs(10);

/// How many times [`send`] presses Enter before it gives up, and how long it waits after each press: on a loaded
/// machine (load average 50, 2026-10-20) the client took longer than three 5 s waits to let a sent text go.
const SEND_TRIES: u32 = 3;
const SEND_WAIT: Duration = Duration::from_secs(15);

/// A message post to the chat service: `POST …/v1/users/ME/conversations/<chat>/messages`.
pub const MESSAGE_POST: &str = r"/v1/users/ME/conversations/[^/?]+/messages(\?|$)";

/// How long the chat gets to open.
pub const CHAT_OPEN: Duration = Duration::from_secs(60);
/// How long a deliberately held message post gets to fail.
pub const HOLD_FAILURE_WAIT: Duration = Duration::from_secs(30);
/// How long this run's capture post gets to be held once sent. The web client posts a new message to a chat only after
/// the unsent ones it still holds (earlier held captures, retried on every load) went through their retries, each
/// about 15 s (seen 2026-11-03: b's page with two of them posted nothing of this run's within 30 s).
pub const HOLD_WAIT: Duration = Duration::from_secs(90);
/// How long the real message gets to reach b.
pub const DELIVERY: Duration = Duration::from_secs(60);

/// The web client's deep link that starts, or opens, the 1:1 chat with the account `mri`. By MRI, not by UPN: the
/// client resolves a UPN with an additional profile lookup posted to the middle tier. Nor by the chat's id (`l/chat/<id>/0`): a page that loaded before the chat existed says
/// it is a chat the user isn't in.
#[must_use]
pub fn chat_link(mri: &str) -> String {
    let users: String = url::form_urlencoded::byte_serialize(mri.as_bytes()).collect();
    format!("https://teams.microsoft.com/l/chat/0/0?users={users}")
}

/// A URL that names the chat `chat_id`, plain or percent-encoded (any case).
///
/// # Errors
/// Never for an id from [`one_to_one_id`]; when the pattern doesn't compile.
pub fn chat_url(chat_id: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!("(?i){}", chat_in_url(chat_id)))
}

/// The regular expression of the chat id `chat_id` in a URL, plain or percent-encoded, as one group (without a case
/// flag): what [`chat_url`] and the patterns of the writes on the chat are made of.
#[must_use]
pub fn chat_in_url(chat_id: &str) -> String {
    let encoded = chat_id.replace(':', "%3A").replace('@', "%40");
    format!("({}|{})", regex::escape(chat_id), regex::escape(&encoded))
}

/// A `GET` whose URL names the chat `chat_id` ([`chat_url`]): the evidence that a page opened that chat.
///
/// # Errors
/// Never for an id from [`one_to_one_id`]; when the pattern doesn't compile.
pub fn chat_requests(chat_id: &str) -> Result<RequestPattern, regex::Error> {
    RequestPattern::new(Some("GET"), chat_url(chat_id)?.as_str())
}

/// A text no earlier run used.
#[must_use]
pub fn nonce() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{:x}-{:x}", now.as_millis(), std::process::id())
}

/// The message that must reach b.
#[must_use]
pub fn message_text(nonce: &str) -> String {
    format!("lab message {nonce}")
}

/// Runs `ab-chat` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, no Chrome, a browser whose capture setup fails.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let session_options = SessionOptions {
        capture: Some(ScenarioName::AbChat.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::System,
        user_agent: None,
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
    };
    Ok(run_sessions(&steps, a, b).await)
}

/// Drives and settles the opened pair with their capture setup already installed.
async fn run_sessions(steps: &Steps<'_>, mut a: LabSession, mut b: LabSession) -> ScenarioReport {
    let mut report = ScenarioReport::new(ScenarioName::AbChat);
    let outcome = steps.run(&mut a, &mut b, &mut report).await;
    if let Err(error) = outcome {
        report.failures.push(error.to_string());
        for (alias, session) in [(Alias::A, &a), (Alias::B, &b)] {
            if let Some(note) = save_screenshot(alias, session).await {
                report.notes.push(note);
            }
        }
    }
    // both browsers go down and both captures are finished, whatever happened to either
    for (alias, session) in [(Alias::A, a), (Alias::B, b)] {
        report.close_account(alias, session).await;
    }
    report
}

struct Steps<'a> {
    /// a's and b's MRIs, from the lab config.
    mri_a: &'a str,
    mri_b: &'a str,
    /// The a–b chat's id, from the lab config's MRIs; `None` when they aren't recorded.
    chat_id: Option<&'a str>,
    /// This run's [`nonce`], in its probe and its message.
    nonce: &'a str,
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
        let chat_id = self
            .chat_id
            .ok_or_else(|| setup("lab.toml has no MRI for a or b: run `proto-lab login` for them".to_owned()))?;
        let opened = chat_requests(chat_id).map_err(|error| setup(error.to_string()))?;

        // a starts the chat with b (or opens it, once it exists): a's compose box is checked to belong to it by where
        // the probe goes
        let mut drive_a = a.driver();
        drive_a
            .navigate("a opens the chat with b", &chat_link(self.mri_b))
            .await?;
        drive_a.wait_for("a's compose box", COMPOSE, CHAT_OPEN).await?;

        let message = message_text(self.nonce);
        send(&mut drive_a, "a (the message)", &message).await?;
        // b opens the chat once a has written to it, so b reads the chat's history from the service: the real message
        // showing there without the probe is the evidence that the probe never reached the service
        let mut drive_b = b.driver();
        drive_b
            .navigate("b opens the chat with a", &chat_link(self.mri_a))
            .await?;
        // what b shows next is the a–b chat only once b's page has read it
        drive_b
            .wait_for_request("b reads the a–b chat", &opened, CHAT_OPEN)
            .await?;
        wait_for_newest(&mut drive_b, "b receives the message", &message, DELIVERY).await?;
        report.notes.push("a's message reached b".to_owned());

        Ok(())
    }
}

/// The button the web client shows over a chat it opened at its "last read" mark, with unread messages below (the lab
/// accounts' web client is in English).
pub const JUMP_TO_NEWEST: &str = "Jump to newest";

/// The pill the web client shows instead of [`JUMP_TO_NEWEST`] over the same chat when a new message arrived while it
/// was open (seen live on 2026-10-21, b's client after a native run left about 65 unread messages).
pub const NEW_MESSAGES: &str = "New messages";

/// Waits up to `timeout` for `text` in the page, clicking [`JUMP_TO_NEWEST`] or [`NEW_MESSAGES`] (a `button` or a
/// `role="button"` element) whenever one shows: a chat with unread messages opens at its "last read" mark, and the
/// messages below it aren't rendered, so their text isn't in the page (a native client's run in the same chat leaves
/// dozens of them). A click only scrolls the chat.
///
/// # Errors
/// [`StepError::Timeout`] when the text doesn't show in time.
pub async fn wait_for_newest(drive: &mut Driver, step: &str, text: &str, timeout: Duration) -> Result<(), StepError> {
    let expression = format!(
        "(() => {{ if ((document.body?.innerText ?? '').includes({text})) return true; \
         const labels = {labels}; \
         const jump = [...document.querySelectorAll('button, [role=\"button\"]')] \
           .find(b => labels.includes((b.innerText ?? '').trim())); \
         if (jump) jump.click(); return false; }})()",
        text = js_string(text),
        labels = serde_json::json!([JUMP_TO_NEWEST, NEW_MESSAGES]),
    );
    drive
        .wait_until_true(step, &expression, &format!("the text {text:?}"), timeout)
        .await
}

/// Types `text` into the compose box ([`COMPOSE`]) and sends it with Enter, and waits until the compose box no longer
/// holds it: a client still settling the chat can drop an Enter, so it is pressed again, up to [`SEND_TRIES`] times in
/// all. Each wait is bounded, the page's answers included: a page that stops answering fails the send.
///
/// # Errors
/// When the compose box isn't there, the page refuses the input, or the box still holds `text` after the last try.
pub async fn send(driver: &mut Driver, who: &str, text: &str) -> Result<(), ScenarioError> {
    driver.type_into(&format!("{who} types"), COMPOSE, text).await?;
    press_until_sent(driver, who, text).await
}

/// Sends what the compose box holds with Enter, and waits until it no longer holds `text` (a part of what it holds):
/// [`send`]'s presses, for a message typed in parts (`chat-shapes`' lines).
///
/// # Errors
/// As [`send`].
pub(crate) async fn press_until_sent(driver: &mut Driver, who: &str, text: &str) -> Result<(), ScenarioError> {
    let sent = format!(
        "!(document.querySelector({})?.innerText ?? '').includes({})",
        js_string(COMPOSE_CSS),
        js_string(text)
    );
    let step = format!("{who} sends");
    for _ in 0..SEND_TRIES {
        driver.press_enter(&format!("{who} presses Enter")).await?;
        match driver
            .wait_until_true(&step, &sent, "the compose box to let the text go", SEND_WAIT)
            .await
        {
            Ok(()) => return Ok(()),
            // a dropped Enter, or a page too busy to answer: press again
            Err(StepError::Timeout { .. }) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(setup(format!(
        "{who}'s compose box still holds the text after {SEND_TRIES} Enter presses"
    )))
}

/// A capture hold, under one label ([`HeldRequest::label`]), of a message post ([`MESSAGE_POST`]) whose URL names one chat:
/// what a held-message capture waits for.
pub struct HeldPost<'a> {
    label: &'a str,
    /// Finds the chat in a URL.
    chat: &'a Regex,
    message_post: Regex,
}

impl<'a> HeldPost<'a> {
    /// A capture hold under `label` of a message post whose URL `chat` finds a match in.
    ///
    /// # Errors
    /// Never: [`MESSAGE_POST`] is fixed.
    pub fn new(label: &'a str, chat: &'a Regex) -> Result<Self, regex::Error> {
        Ok(Self {
            label,
            chat,
            message_post: Regex::new(MESSAGE_POST)?,
        })
    }

    /// Whether `denial` is one.
    #[must_use]
    pub fn matches(&self, denial: &HeldRequest) -> bool {
        denial.label.as_deref() == Some(self.label)
            && self.message_post.is_match(&denial.url)
            && self.chat.is_match(&denial.url)
    }

    /// Waits up to [`HOLD_WAIT`] for `session`'s capture holds to deny one; `None` when it didn't.
    pub(crate) async fn wait(&self, session: &LabSession) -> Option<HeldRequest> {
        let deadline = tokio::time::Instant::now() + HOLD_WAIT;
        loop {
            if let Some(denial) = session.held_requests().into_iter().find(|denial| self.matches(denial)) {
                return Some(denial);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

/// Checks that the held request `denial`, found by its `Network` id, failed in `driver`'s page as
/// blocked by the capture hold (`net::ERR_BLOCKED_BY_CLIENT`) within [`HOLD_FAILURE_WAIT`]. `what` names the request in the errors.
///
/// # Errors
/// When the deny names no `Network` request, the request doesn't fail in time, the one that failed isn't the denied
/// one, or it failed otherwise.
pub(crate) async fn failed_in_page(driver: &mut Driver, denial: &HeldRequest, what: &str) -> Result<(), ScenarioError> {
    let network_id = denial.network_id.as_deref().ok_or_else(|| {
        setup(format!(
            "the capture hold of {what} names no Network request: can't tell it failed in the page"
        ))
    })?;
    let failed = driver
        .wait_for_failure_of(&format!("{what} fails in the page"), network_id, HOLD_FAILURE_WAIT)
        .await?;
    if !failed.method.eq_ignore_ascii_case(&denial.method) || without_query(&failed.url) != denial.url {
        return Err(setup(format!(
            "the request {network_id} that failed in the page is {} {}, not the denied {} {}",
            failed.method,
            without_query(&failed.url),
            denial.method,
            denial.url
        )));
    }
    let error = failed.failed.unwrap_or_default();
    if !error.contains("ERR_BLOCKED_BY_CLIENT") {
        return Err(setup(format!(
            "{what} failed with {error}, not as blocked by the capture hold"
        )));
    }
    Ok(())
}

/// Saves what `session`'s tab shows as `failure.png` (mode `0600`) in its capture dir, outside the repository, for the
/// diagnosis of a failed run; a note saying where, or why not. `None` for a run that isn't recorded. A page that doesn't
/// answer within [`DIAGNOSIS_WAIT`] gets no screenshot: the run goes on to close the browsers.
pub(crate) async fn save_screenshot(alias: Alias, session: &LabSession) -> Option<String> {
    save_screenshot_as(alias, session, "failure.png").await
}

/// [`save_screenshot`] under the file name `name` in the capture dir: one per failed step of a run that goes on after
/// a step failed. An existing file is never overwritten.
pub(crate) async fn save_screenshot_as(alias: Alias, session: &LabSession, name: &str) -> Option<String> {
    let path = session.capture_dir()?.join(name);
    let mut driver = session.driver();
    driver.set_call_timeout(DIAGNOSIS_WAIT);
    let saved = match driver.screenshot("a screenshot of the failure").await {
        Ok(png) => write_private(&path, &png).map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    Some(match saved {
        Ok(()) => format!("{alias}: the page at the failure: {}", path.display()),
        Err(error) => format!("{alias}: no screenshot of the failure: {error}"),
    })
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

/// Opens the web client in `session` and waits for it to load.
pub(crate) async fn load(alias: Alias, session: &mut LabSession, timeout: Duration) -> Result<(), ScenarioError> {
    load_at(alias, session, TEAMS_URL, timeout).await
}

/// Opens the web client in `session` at `url` (the client, or one of its deep links) and waits up to `timeout` for it
/// to load.
pub(crate) async fn load_at(
    alias: Alias,
    session: &mut LabSession,
    url: &str,
    timeout: Duration,
) -> Result<(), ScenarioError> {
    session.navigate(url).await?;
    let state = session.settle(timeout).await;
    if state == SessionState::Loaded {
        return Ok(());
    }
    let status = account_status(&state, timeout, session.watch().last_authz_status());
    Err(setup(format!("{alias}: the web client didn't load: {status}")))
}

pub(crate) fn setup(reason: String) -> ScenarioError {
    ScenarioError::Setup(reason)
}

#[cfg(test)]
#[path = "../../tests/support/scenario/ab_chat_driver_tests.rs"]
mod driver_tests;
