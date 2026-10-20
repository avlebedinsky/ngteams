//! The `ab-chat` scenario: account a sends a message in the a–b chat and account b receives it.
//!
//! Both accounts run with the write mapping ([`Classifier::mapped`]) and members read live (`LiveMembershipSource`,
//! fed each account's chat service credential from its own page's requests): a write passes only when the chat's
//! members, read fresh, are all lab accounts. The steps:
//! 1. Before anything loads, a's fence force-denies every write carrying [`PROBE_MARKER`] (`deny_probes`), for the
//!    whole run: the web client keeps a send that failed and sends it again on a later load, so an earlier run's probe
//!    would otherwise go out now. This run's probe ([`probe_text`] of a run-unique [`nonce`]) has a forced deny of its
//!    own, labelled [`THIS_RUN_PROBE`], so its deny can't be mistaken for an earlier run's probe sent again.
//! 2. a and b open the web client; both must load.
//! 3. a opens the chat with b ([`chat_link`] by b's MRI), which the client starts with a `POST v1/threads` (a chat
//!    create the fence judges by the members in its body) the first time.
//! 4. **Live enforcement test**: a sends this run's probe. The fence must force-deny, by this run's rule, a message post
//!    to the a–b chat (its id in the URL: the compose box belongs to that chat), and that very post (by its `Network`
//!    id) must fail in a's page with `net::ERR_BLOCKED_BY_CLIENT`.
//! 5. a sends the real message. b then opens the chat with a and must show it within [`DELIVERY`], and must not show
//!    the probe: b reads the chat's history from the service, so the probe missing there means it never got there.
//! 6. After the run: no deny but the forced ones and telemetry ([`DenyPolicy::ForcedAnd`] with [`TELEMETRY_HOSTS`]).
//!
//! Run live on 2026-10-20 (`docs/protocol/fence-targets.md`): steps 2–5 pass. Step 6 doesn't: the web client makes
//! hundreds of requests the fence doesn't map (middle-tier lookups posted as `POST`, presence, the user's own
//! properties, the push channel's frames, …) and denies, as in `load`; until they are reviewed they count as
//! unexpected and the run exits non-zero. A failed run leaves each page's screenshot as `failure.png` in its capture.
//!
//! Every call to a page is bounded ([`super::dsl::CALL_TIMEOUT`], shorter for the failure's diagnosis): a page that
//! stops answering fails the step, and the run still closes both browsers and finishes both captures.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::classify::Classifier;
use crate::commands::status::account_status;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_http::{Denial, DenyCause, ForcedDeny, without_query};
use crate::session::{LabSession, SessionOptions, SessionState, TEAMS_URL};

use regex::Regex;

use super::dsl::{Driver, RequestPattern, Selector, StepError};
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport, TELEMETRY_HOSTS};

/// The message compose box's selector: the web client's rich-text editor (seen on 2026-10-20).
const COMPOSE_CSS: &str = r#"[contenteditable="true"][role="textbox"]"#;

/// The message compose box.
pub const COMPOSE: Selector<'static> = Selector::Css(COMPOSE_CSS);

/// How many characters the compose box holds, for a failed send's report (no text of the page).
const COMPOSE_STATE: &str =
    r#"document.querySelector('[contenteditable="true"][role="textbox"]')?.innerText.length ?? null"#;

/// What every probe text starts with ([`probe_text`]); `deny_probes` denies any write that carries it.
pub const PROBE_MARKER: &str = "lab fence probe ";

/// The label of the forced deny of this run's probe ([`crate::fence_http::Denial::label`]).
pub const THIS_RUN_PROBE: &str = "this run's probe";

/// How long the failure's diagnosis (the compose box's state, a screenshot) may wait for a page.
const DIAGNOSIS_WAIT: Duration = Duration::from_secs(10);

/// How many times [`send`] presses Enter before it gives up, and how long it waits after each press.
const SEND_TRIES: u32 = 3;
const SEND_WAIT: Duration = Duration::from_secs(5);

/// A message post to the chat service: `POST …/v1/users/ME/conversations/<chat>/messages`.
pub const MESSAGE_POST: &str = r"/v1/users/ME/conversations/[^/?]+/messages(\?|$)";

/// How long the chat gets to open.
pub const CHAT_OPEN: Duration = Duration::from_secs(60);
/// How long a's probe post gets to fail.
pub const PROBE_FAIL: Duration = Duration::from_secs(30);
/// How long the real message gets to reach b.
pub const DELIVERY: Duration = Duration::from_secs(60);

/// The denies a run of this scenario expects.
pub const POLICY: DenyPolicy = DenyPolicy::ForcedAnd(&TELEMETRY_HOSTS);

/// The 1:1 chat id of two work accounts, as the web client builds it: `19:<oid>_<oid>@unq.gbl.spaces`, the two object
/// ids (lowercase) sorted. `None` unless both MRIs are `8:orgid:<oid>` of two different accounts.
#[must_use]
pub fn one_to_one_id(mri_a: &str, mri_b: &str) -> Option<String> {
    let oid = |mri: &str| {
        let oid = mri.strip_prefix("8:orgid:")?.to_ascii_lowercase();
        let shaped = oid.len() == 36 && oid.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-');
        shaped.then_some(oid)
    };
    let mut oids = [oid(mri_a)?, oid(mri_b)?];
    oids.sort();
    let [first, second] = oids;
    (first != second).then(|| format!("19:{first}_{second}@unq.gbl.spaces"))
}

/// The web client's deep link that starts, or opens, the 1:1 chat with the account `mri`. By MRI, not by UPN: the
/// client resolves a UPN with a profile lookup posted to the middle tier, which the fence denies (an unmapped `POST`),
/// and the chat never opens. Nor by the chat's id (`l/chat/<id>/0`): a page that loaded before the chat existed says
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
    let encoded = chat_id.replace(':', "%3A").replace('@', "%40");
    Regex::new(&format!("(?i)({}|{})", regex::escape(chat_id), regex::escape(&encoded)))
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

/// The probe a's forced deny stops: it must never reach b.
#[must_use]
pub fn probe_text(nonce: &str) -> String {
    format!("{PROBE_MARKER}{nonce}: must not arrive")
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
    let nonce = nonce();
    // before a's page loads: the client sends an earlier run's unsent probe again as soon as it can
    if let Err(error) = deny_probes(&a, &probe_text(&nonce)) {
        report.failures.push(error.to_string());
    }
    let mri = |alias| config.account(alias).mri.as_deref().unwrap_or_default();
    let chat_id = one_to_one_id(mri(Alias::A), mri(Alias::B));
    let steps = Steps {
        mri_a: mri(Alias::A),
        mri_b: mri(Alias::B),
        chat_id: chat_id.as_deref(),
        nonce: &nonce,
        options,
    };
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
        report.close_account(alias, session, POLICY).await;
    }
    Ok(report)
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
        let names_chat = chat_url(chat_id).map_err(|error| setup(error.to_string()))?;
        // a starts the chat with b (or opens it, once it exists): a's compose box is checked to belong to it by where
        // the probe goes
        let mut drive_a = a.driver();
        drive_a
            .navigate("a opens the chat with b", &chat_link(self.mri_b))
            .await?;
        drive_a.wait_for("a's compose box", COMPOSE, CHAT_OPEN).await?;

        let probe = probe_text(self.nonce);
        enforce(a, &mut drive_a, &probe, &names_chat).await?;
        report
            .notes
            .push("enforcement: a's probe post was denied by the fence and failed in a's page".to_owned());
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
        drive_b
            .wait_for_text("b receives the message", &message, DELIVERY)
            .await?;
        report.notes.push("a's message reached b".to_owned());
        if drive_b.shows_text("b must not have the probe", &probe).await? {
            return Err(setup("b received the probe the fence denied".to_owned()));
        }
        report
            .notes
            .push("enforcement: b's copy of the chat has the message and not the probe".to_owned());
        Ok(())
    }
}

/// Forces a deny ([`ForcedDeny`]) on every `POST` or `PUT` of `session` whose body holds [`PROBE_MARKER`], whatever
/// its URL: this run's probe (a client that put the first message into the chat's creation is stopped too), and any
/// earlier run's probe the client still holds as unsent and sends again once it loads. This run's `probe` gets its
/// rule first, labelled [`THIS_RUN_PROBE`]: a deny names the first rule that matched it, so only this run's probe is
/// denied under that label.
///
/// # Errors
/// Never: the patterns are fixed.
fn deny_probes(session: &LabSession, probe: &str) -> Result<(), ScenarioError> {
    let any_url = |method| ForcedDeny::new(method, ".").map_err(|error| setup(error.to_string()));
    for method in ["POST", "PUT"] {
        session.force_deny(any_url(method)?.with_body_containing(probe).labelled(THIS_RUN_PROBE));
    }
    for method in ["POST", "PUT"] {
        session.force_deny(any_url(method)?.with_body_containing(PROBE_MARKER));
    }
    Ok(())
}

/// Types `text` into the compose box ([`COMPOSE`]) and sends it with Enter, and waits until the compose box no longer
/// holds it: a client still settling the chat can drop an Enter, so it is pressed again, up to [`SEND_TRIES`] times in
/// all. Each wait is bounded, the page's answers included: a page that stops answering fails the send.
///
/// # Errors
/// When the compose box isn't there, the page refuses the input, or the box still holds `text` after the last try.
pub async fn send(driver: &mut Driver, who: &str, text: &str) -> Result<(), ScenarioError> {
    driver.type_into(&format!("{who} types"), COMPOSE, text).await?;
    let sent = format!(
        "!(document.querySelector({})?.innerText ?? '').includes({})",
        serde_json::Value::String(COMPOSE_CSS.to_owned()),
        serde_json::Value::String(text.to_owned())
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

/// The live enforcement test: with [`deny_probes`] in place, a sends `probe`, this run's. The fence must force-deny, by
/// this run's rule ([`THIS_RUN_PROBE`]), a message post to the a–b chat (`chat` finds its id in the URL: the evidence
/// that a's compose box belongs to that chat) after it was sent, and that very post, found by its `Network` id, must
/// fail in a's page as blocked by the fence.
async fn enforce(a: &LabSession, drive_a: &mut Driver, probe: &str, chat: &Regex) -> Result<(), ScenarioError> {
    let message_post = Regex::new(MESSAGE_POST).map_err(|error| setup(error.to_string()))?;
    let this_run = |denial: &Denial| {
        denial.cause == DenyCause::Forced
            && denial.label.as_deref() == Some(THIS_RUN_PROBE)
            && message_post.is_match(&denial.url)
            && chat.is_match(&denial.url)
    };
    send(drive_a, "a (the probe)", probe).await?;
    let deadline = tokio::time::Instant::now() + PROBE_FAIL;
    let denial = loop {
        if let Some(denial) = a.denials().into_iter().find(|denial| this_run(denial)) {
            break denial;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(no_probe_deny(a, drive_a).await);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let network_id = denial.network_id.as_deref().ok_or_else(|| {
        setup("the fence's deny of the probe names no Network request: can't tell it failed in a's page".to_owned())
    })?;
    let failed = drive_a
        .wait_for_failure_of("a's probe post fails in the page", network_id, PROBE_FAIL)
        .await?;
    if !failed.method.eq_ignore_ascii_case(&denial.method) || without_query(&failed.url) != denial.url {
        return Err(setup(format!(
            "the request {network_id} that failed in a's page is {} {}, not the denied {} {}",
            failed.method,
            without_query(&failed.url),
            denial.method,
            denial.url
        )));
    }
    let error = failed.failed.unwrap_or_default();
    if !error.contains("ERR_BLOCKED_BY_CLIENT") {
        return Err(setup(format!(
            "a's probe post failed with {error}, not as blocked by the fence"
        )));
    }
    Ok(())
}

/// The error of an enforcement test whose probe wasn't denied in time, with what a's run shows instead: its other
/// forced denies, and how much the compose box holds (asked for [`DIAGNOSIS_WAIT`] at most).
async fn no_probe_deny(a: &LabSession, drive_a: &Driver) -> ScenarioError {
    let elsewhere = a
        .denials()
        .iter()
        .filter(|denial| denial.cause == DenyCause::Forced)
        .count();
    let page = tokio::time::timeout(DIAGNOSIS_WAIT, drive_a.evaluate("the compose state", COMPOSE_STATE))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    setup(format!(
        "no message post of this run's probe to the a–b chat was denied within {PROBE_FAIL:?} ({elsewhere} other \
         forced deny(s); compose box holds {page} characters)"
    ))
}

/// Saves what `session`'s tab shows as `failure.png` (mode `0600`) in its capture dir, outside the repository, for the
/// diagnosis of a failed run; a note saying where, or why not. `None` for a run that isn't recorded. A page that doesn't
/// answer within [`DIAGNOSIS_WAIT`] gets no screenshot: the run goes on to close the browsers.
async fn save_screenshot(alias: Alias, session: &LabSession) -> Option<String> {
    let path = session.capture_dir()?.join("failure.png");
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
