//! The `ab-chat` scenario: account a sends a message in the a–b chat and account b receives it.
//!
//! Both accounts run with the write mapping ([`Classifier::mapped`]) and members read live (`LiveMembershipSource`,
//! fed each account's chat service credential from its own page's requests): a write passes only when the chat's
//! members, read fresh, are all lab accounts. The steps:
//! 1. Before anything loads, a's fence force-denies every write carrying [`PROBE_MARKER`] (`deny_probes`), for the
//!    whole run: the web client keeps a send that failed and sends it again on a later load, so an earlier run's probe
//!    would otherwise go out now. This run's probe ([`probe_text`] of a run-unique [`nonce`]) has a forced deny of its
//!    own, labelled [`THIS_RUN_PROBE`], so its deny can't be mistaken for an earlier run's probe sent again (labelled
//!    [`EARLIER_RUN_PROBE`]).
//! 2. a and b open the web client; both must load.
//! 3. a opens the chat with b ([`chat_link`] by b's MRI), which the client starts with a `POST v1/threads` (a chat
//!    create the fence judges by the members in its body) the first time.
//! 4. **Live enforcement test**: a sends this run's probe. The fence must force-deny, by this run's rule, a message post
//!    to the a–b chat (its id in the URL: the compose box belongs to that chat), and that very post (by its `Network`
//!    id) must fail in a's page with `net::ERR_BLOCKED_BY_CLIENT`.
//! 5. a sends the real message. b then opens the chat with a and must show it within [`DELIVERY`], and must not show
//!    the probe: b reads the chat's history from the service, so the probe missing there means it never got there.
//! 6. After the run, judged by [`policy`]: no deny but the two probes' forced ones and the reviewed expected-deny list
//!    ([`super::expected`]), and nothing denied on the a–b chat but the probes.
//!
//! Run live on 2026-10-20 (`docs/protocol/fence-targets.md`): steps 2–5 pass. Step 6 failed then on the web client's
//! hundreds of unmapped requests (middle-tier lookups posted as `POST`, presence, the user's own properties, the push
//! channel's frames, …), which the fence denies, as in `load`; those classes are now on the reviewed list, and stay
//! denied. A failed run leaves each page's screenshot as `failure.png` in its capture.
//!
//! Every call to a page is bounded ([`super::dsl::CALL_TIMEOUT`], shorter for the failure's diagnosis): a page that
//! stops answering fails the step, and the run still closes both browsers and finishes both captures.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::browser::Media;
use crate::classify::Classifier;
use crate::commands::status::account_status;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_http::{Denial, DenyCause, ForcedDeny, without_query};
use crate::fence_ws::FrameClassifier;
use crate::session::{LabSession, SessionOptions, SessionState, TEAMS_URL};

use regex::Regex;

use super::dsl::{Driver, RequestPattern, Selector, StepError};
use super::expected::Review;
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

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

/// The label of the forced deny of any other probe ([`PROBE_MARKER`]): an earlier run's, which the web client still
/// holds as unsent and sends again once it loads.
pub const EARLIER_RUN_PROBE: &str = "an earlier run's probe";

/// The forced denies a run of this scenario expects: its two probe rules' ([`deny_probes`]).
pub const EXPECTED_FORCED: [&str; 2] = [THIS_RUN_PROBE, EARLIER_RUN_PROBE];

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
/// How long a's probe post gets to fail.
pub const PROBE_FAIL: Duration = Duration::from_secs(30);
/// How long the real message gets to reach b.
pub const DELIVERY: Duration = Duration::from_secs(60);

/// The denies a run of this scenario expects: the reviewed list ([`super::expected`]) and the two probe rules' forced
/// denies ([`EXPECTED_FORCED`]); with the a–b chat's id, any other deny of a URL naming that chat ([`chat_url`]) is
/// unexpected, the list notwithstanding.
///
/// # Errors
/// Never for an id from [`one_to_one_id`]; when the chat's pattern doesn't compile.
pub fn policy(chat_id: Option<&str>) -> Result<DenyPolicy, regex::Error> {
    let mut review = Review::new().expecting_forced(&EXPECTED_FORCED);
    if let Some(chat_id) = chat_id {
        review = review.with_mapped_target(chat_url(chat_id)?);
    }
    Ok(DenyPolicy::Reviewed(review))
}

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
        frames: FrameClassifier::seeded(),
        live_members: true,
        capture: Some(ScenarioName::AbChat.as_str().to_owned()),
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
    let mut report = ScenarioReport::new(ScenarioName::AbChat);
    let nonce = nonce();
    // before a's page loads: the client sends an earlier run's unsent probe again as soon as it can
    if let Err(error) = deny_probes(&a, &probe_text(&nonce)) {
        report.failures.push(error.to_string());
    }
    let mri = |alias| config.account(alias).mri.as_deref().unwrap_or_default();
    let chat_id = one_to_one_id(mri(Alias::A), mri(Alias::B));
    let policy = policy(chat_id.as_deref()).unwrap_or_else(|error| {
        // the run fails, and a's and b's denies are still listed
        report
            .failures
            .push(format!("can't tell the a–b chat in a URL: {error}"));
        DenyPolicy::Reviewed(Review::new().expecting_forced(&EXPECTED_FORCED))
    });
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
        report.close_account(alias, session, &policy).await;
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
        not_received(&mut drive_b, &probe).await?;
        report
            .notes
            .push("enforcement: b's copy of the chat has the message and not the probe".to_owned());
        Ok(())
    }
}

/// The other half of the live enforcement test, on b's page once it has read the chat's history from the service and
/// shows a's message there: the probe the fence denied in a's page ([`enforce`]) must not show. A probe there reached
/// the service, so the fence let it out.
///
/// # Errors
/// When b's page shows `probe`, or doesn't answer.
pub async fn not_received(drive_b: &mut Driver, probe: &str) -> Result<(), ScenarioError> {
    if drive_b.shows_text("b must not have the probe", probe).await? {
        return Err(setup("b received the probe the fence denied".to_owned()));
    }
    Ok(())
}

/// Forces a deny ([`ForcedDeny`]) on every `POST` or `PUT` of `session` whose body holds [`PROBE_MARKER`], whatever
/// its URL: this run's probe (a client that put the first message into the chat's creation is stopped too), and any
/// earlier run's probe the client still holds as unsent and sends again once it loads. This run's `probe` gets its
/// rule first, labelled [`THIS_RUN_PROBE`]: a deny names the first rule that matched it, so only this run's probe is
/// denied under that label, and any other probe under [`EARLIER_RUN_PROBE`].
///
/// # Errors
/// Never: the patterns are fixed.
pub(crate) fn deny_probes(session: &LabSession, probe: &str) -> Result<(), ScenarioError> {
    let any_url = |method| ForcedDeny::new(method, ".").map_err(|error| setup(error.to_string()));
    for method in ["POST", "PUT"] {
        session.force_deny(any_url(method)?.with_body_containing(probe).labelled(THIS_RUN_PROBE));
    }
    for method in ["POST", "PUT"] {
        session.force_deny(
            any_url(method)?
                .with_body_containing(PROBE_MARKER)
                .labelled(EARLIER_RUN_PROBE),
        );
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
pub(crate) async fn enforce(
    a: &LabSession,
    drive_a: &mut Driver,
    probe: &str,
    chat: &Regex,
) -> Result<(), ScenarioError> {
    let this_run = ForcedPost::new(THIS_RUN_PROBE, chat).map_err(|error| setup(error.to_string()))?;
    send(drive_a, "a (the probe)", probe).await?;
    let Some(denial) = this_run.wait(a).await else {
        return Err(no_probe_deny(a, drive_a).await);
    };
    failed_in_page(drive_a, &denial, "a's probe post").await
}

/// A forced deny, under one label ([`Denial::label`]), of a message post ([`MESSAGE_POST`]) whose URL names one chat:
/// what a live enforcement test waits for.
pub struct ForcedPost<'a> {
    label: &'a str,
    /// Finds the chat in a URL.
    chat: &'a Regex,
    message_post: Regex,
}

impl<'a> ForcedPost<'a> {
    /// A forced deny under `label` of a message post whose URL `chat` finds a match in.
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
    pub fn matches(&self, denial: &Denial) -> bool {
        denial.cause == DenyCause::Forced
            && denial.label.as_deref() == Some(self.label)
            && self.message_post.is_match(&denial.url)
            && self.chat.is_match(&denial.url)
    }

    /// Waits up to [`PROBE_FAIL`] for `session`'s fence to deny one; `None` when it didn't.
    pub(crate) async fn wait(&self, session: &LabSession) -> Option<Denial> {
        let deadline = tokio::time::Instant::now() + PROBE_FAIL;
        loop {
            if let Some(denial) = session.denials().into_iter().find(|denial| self.matches(denial)) {
                return Some(denial);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

/// Checks that the request the fence denied as `denial`, found by its `Network` id, failed in `driver`'s page as
/// blocked by the fence (`net::ERR_BLOCKED_BY_CLIENT`) within [`PROBE_FAIL`]. `what` names the request in the errors.
///
/// # Errors
/// When the deny names no `Network` request, the request doesn't fail in time, the one that failed isn't the denied
/// one, or it failed otherwise.
pub(crate) async fn failed_in_page(driver: &mut Driver, denial: &Denial, what: &str) -> Result<(), ScenarioError> {
    let network_id = denial.network_id.as_deref().ok_or_else(|| {
        setup(format!(
            "the fence's deny of {what} names no Network request: can't tell it failed in the page"
        ))
    })?;
    let failed = driver
        .wait_for_failure_of(&format!("{what} fails in the page"), network_id, PROBE_FAIL)
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
            "{what} failed with {error}, not as blocked by the fence"
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
