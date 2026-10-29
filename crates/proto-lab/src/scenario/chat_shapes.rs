//! The `chat-shapes` scenario (M2 Task 10): the chat shapes the native UI still needs, recorded for
//! `docs/protocol/chat.md`. M1 captured one reaction key (the like), typing as one post per message and never its end,
//! and no message with a line break or a link; the mention was captured in the 1:1 chat only.
//!
//! Both accounts run under the lab rule with members read live, as `ab-trouter` does: b's open chat shows a's typing
//! only once b's push channel is up. Every contacting write is on the a–b chat or the lab's recorded group chat, mapped
//! and judged by the fence with members read live.
//!
//! 1. Before anything loads, a's fence force-denies this run's probe and any earlier run's probe, and the group chat's
//!    first message of any `abc-chat` run ([`super::abc_chat::forced_denies`]): a's profile holds both kinds unsent
//!    and sends them again once it loads.
//! 2. b loads, registers its messaging endpoint, opens the a–b chat and registers again (as `ab-trouter`'s step 2);
//!    b's page then records when its typing indicator (`data-tid="typing-indicator"`) shows and goes
//!    ([`TYPING_RECORDER`]).
//! 3. a loads, opens the a–b chat and proves enforcement live with this run's probe (`ab-chat`'s step 4); a sends the
//!    first message ([`Texts::first`]), and b must show it and not the probe.
//! 4. **Typing** ([`TypingRun`]): after [`QUIET_BEFORE_TYPING`], a presses a key in its compose box every
//!    [`TYPE_EVERY`] for [`TYPE_FOR`], stops with the text still in the box for [`PAUSE`], empties the box and waits
//!    [`AFTER_EMPTYING`]. The notes give every typing post a's page sent (`Control/Typing`, `Control/ClearTyping`) by
//!    when it went, and when b's indicator showed and went.
//! 5. **Lines and a link**: a sends a message of three lines ([`Texts::lines`], broken with Shift+Enter), the last
//!    holding a link ([`Texts::link`], followed by a space pressed as a key); b must show it, and the notes give b's
//!    typing indicator around it.
//! 6. **Reactions**: a points at its first message and reads which quick reactions the hover bar offers
//!    ([`HOVER_REACTIONS`]); it sets each in turn (each must be answered with a 2xx), then takes each back. The notes
//!    give, per reaction, every reaction request it caused (method, key, status).
//! 7. **A mention in the group chat**: with the lab's group chat recorded and readable in full (as `abc-chat` checks
//!    before it opens anything, [`super::abc_chat::messaging_blocked`]), a opens it and mentions b, picked from the
//!    mention picker only once it is b ([`super::ab_chat_full::mention_b`]); b opens the group chat and must show it.
//!    Without a recorded, readable chat the step fails before anything is sent there.
//!
//! A step that fails after the enforcement test is reported (with a screenshot in the capture dir and the page's
//! `data-tid`s) and the run goes on with the next one. Judged as `ab-trouter` is ([`policy`]): the forced denies of
//! step 1 and the reviewed expected-deny list; nothing denied on the a–b chat or the group chat but those forced
//! denies. Nothing answers a call.

use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;

use crate::browser::Media;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{
    CHAT_OPEN, COMPOSE, COMPOSE_CSS, DELIVERY, chat_link, chat_requests, chat_url, enforce, load, nonce, not_received,
    one_to_one_id, press_until_sent, probe_text, save_screenshot_as, setup, wait_for_newest,
};
use super::ab_chat_full::{MARKED, VISIBLE_TIDS, Writes, answered, click_visible, find_message, mention_b, send};
use super::ab_trouter::{size, wait_registered};
use super::abc_chat::{self, chat_link_by_id, lab_source, messaging_blocked};
use super::dsl::{Driver, Key, RequestPattern, SeenRequest, js_string};
use super::expected::Review;
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// How long a step waits for a control to show, or for its write to be answered.
pub const STEP: Duration = Duration::from_secs(30);

/// How long one call to a page may go unanswered (as `ab-chat-full`: the web client loading a chat on a loaded machine
/// kept its renderer from answering for over 30 s).
pub const PAGE_CALL: Duration = Duration::from_secs(90);

/// How often a types a character in the typing step.
pub const TYPE_EVERY: Duration = Duration::from_millis(500);

/// How long the typing step waits, nothing typed, before a types: the first run (2026-10-21) showed a's first typing
/// post about 16 s after its last message, whenever its typing began, so the step starts past that.
pub const QUIET_BEFORE_TYPING: Duration = Duration::from_secs(20);

/// How long a types in the typing step: long enough for several typing posts at the rate the bundle's throttle allows
/// (one per 6 s). The recorded runs sent none while a typed, keys pressed or text inserted, and why is not known
/// (`docs/protocol/chat.md`, "Typing (chat-shapes)").
pub const TYPE_FOR: Duration = Duration::from_secs(45);

/// How long a leaves its typed text in the compose box, typing nothing, before it empties the box: longer than b's
/// indicator stayed after a's last typing post in the first run (about 25 s), so its going isn't the emptying's doing.
pub const PAUSE: Duration = Duration::from_secs(35);

/// How long the typing step goes on after a emptied its compose box.
pub const AFTER_EMPTYING: Duration = Duration::from_secs(15);

/// The key a presses again and again in the typing step (the text it makes is never sent).
const TYPED_KEY: Key = Key {
    key: "a",
    code: "KeyA",
    virtual_key: 65,
    text: Some("a"),
    modifiers: 0,
};

/// How long b's typing indicator is watched after b shows the lines message.
const LINES_SETTLE: Duration = Duration::from_secs(5);

/// How long the reaction requests a click caused get to come in after the first one's answer: a client that swaps a
/// reaction (takes another one back) sends a second request.
const REACTION_SETTLE: Duration = Duration::from_secs(2);

/// The page expression of the quick reactions the hover bar shows, by `data-tid` (`message-actions-<key>`, the like
/// `message-actions-like`; the web bundle `26260.1717.5229.7049` marks them `data-inp="message-hover-reactions"`, and
/// the ones in a message's menu `message-reaction`): the visible ones, each once, in page order.
pub const HOVER_REACTIONS: &str = r#"(() => {
  const visible = (e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
  const found = [...document.querySelectorAll('[data-tid^="message-actions-"]:is([data-inp="message-hover-reactions"], [data-inp="message-reaction"])')]
    .filter(visible).map((e) => e.dataset.tid);
  return [...new Set(found)];
})()"#;

/// The page script that records when b's typing indicator shows and goes: `window.__labTyping`, a list of
/// `[ms since the epoch, shown]` at each change (at most 400), from a `MutationObserver` and a 100 ms look. The
/// indicator is `data-tid="typing-indicator"` (web bundle `26260.1717.5229.7049`), shown while it isn't
/// `aria-hidden="true"` and has a box. `true` once installed (again: it stays as it is).
pub const TYPING_RECORDER: &str = r#"(() => {
  if (window.__labTyping) return true;
  const log = [];
  window.__labTyping = log;
  let shown = false;
  const visible = (e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
  const check = () => {
    const now = [...document.querySelectorAll('[data-tid="typing-indicator"]')]
      .some((e) => e.getAttribute('aria-hidden') !== 'true' && visible(e));
    if (now !== shown && log.length < 400) { shown = now; log.push([Date.now(), now]); }
  };
  new MutationObserver(check).observe(document.documentElement,
    { subtree: true, childList: true, attributes: true, attributeFilter: ['aria-hidden', 'class', 'style'] });
  setInterval(check, 100);
  check();
  return true;
})()"#;

/// What [`TYPING_RECORDER`] has recorded so far.
const TYPING_LOG: &str = "window.__labTyping ?? null";

/// What a's messages say; each holds the run's nonce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texts {
    /// The message a reacts to.
    pub first: String,
    /// The three lines of the message with line breaks; the last holds [`Texts::link`].
    pub lines: [String; 3],
    /// The link in the last line: on `example.com`, a domain reserved for examples.
    pub link: String,
    /// What a's mention of b in the group chat says after the mention.
    pub group_mention: String,
}

impl Texts {
    #[must_use]
    pub fn new(nonce: &str) -> Self {
        let link = format!("https://example.com/lab-shapes/{nonce}");
        Self {
            first: format!("lab shapes {nonce} first"),
            lines: [
                format!("lab shapes {nonce} line one"),
                "line two".to_owned(),
                format!("a link {link} on line three"),
            ],
            link,
            group_mention: format!("lab shapes {nonce} group mention"),
        }
    }
}

/// The denies a run of this scenario expects: the reviewed list, the
/// forced denies of [`abc_chat::EXPECTED_FORCED`] (this run's probe, earlier runs' probes, the group chat's first
/// messages), and nothing else on the a–b chat (`ab_chat_id`) or the group chat (`group_chat_id`).
///
/// # Errors
/// When a chat's pattern doesn't compile (never for a 1:1 chat id or a group chat id).
pub fn policy(ab_chat_id: Option<&str>, group_chat_id: Option<&str>) -> Result<DenyPolicy, regex::Error> {
    let mut review = Review::new().expecting_forced(&abc_chat::EXPECTED_FORCED);
    for chat_id in [ab_chat_id, group_chat_id].into_iter().flatten() {
        review = review.with_mapped_target(chat_url(chat_id)?);
    }
    Ok(DenyPolicy::Reviewed(review))
}

/// The `messagetype` of a typing post's body (`Control/Typing`, `Control/ClearTyping`, or any other `Control/…`);
/// `None` for anything else, a message among them.
#[must_use]
pub fn control_type(body: Option<&str>) -> Option<String> {
    let body: Value = serde_json::from_str(body?).ok()?;
    let kind = body.get("messagetype")?.as_str()?;
    kind.starts_with("Control/").then(|| kind.to_owned())
}

/// The typing posts among `requests` (`post`: the chat's message posts): when each was sent (ms since the epoch) and
/// its `messagetype` ([`control_type`]), in the order sent. One whose send time the events didn't give is left out.
#[must_use]
pub fn typing_posts(requests: &[SeenRequest], post: &RequestPattern) -> Vec<(u64, String)> {
    let mut posts: Vec<(u64, String)> = requests
        .iter()
        .filter(|request| post.matches(&request.method, &request.url))
        .filter_map(|request| Some((request.sent_ms?, control_type(request.body.as_deref())?)))
        .collect();
    posts.sort_by_key(|(sent, _)| *sent);
    posts
}

/// [`TYPING_RECORDER`]'s log as read from the page: each change's time (ms since the epoch) and whether the indicator
/// shows from then on. An entry of another form is left out.
#[must_use]
pub fn indicator_changes(log: &Value) -> Vec<(u64, bool)> {
    log.as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| Some((entry.get(0)?.as_u64()?, entry.get(1)?.as_bool()?)))
        .collect()
}

/// The reaction key a reaction request's body names (`{"emotions": {"key": …}}`), if it names one.
#[must_use]
pub fn reaction_key(body: Option<&str>) -> Option<String> {
    let body: Value = serde_json::from_str(body?).ok()?;
    body.get("emotions")?.get("key")?.as_str().map(str::to_owned)
}

/// What the typing step saw (times in ms since the epoch).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypingRun {
    /// When a typed its first character.
    pub began: u64,
    /// When a typed its last one, leaving the text in the box.
    pub stopped: u64,
    /// When a emptied the box.
    pub emptied: u64,
    /// When the step ended.
    pub ended: u64,
    /// a's typing posts ([`typing_posts`]).
    pub posts: Vec<(u64, String)>,
    /// b's indicator ([`indicator_changes`]).
    pub indicator: Vec<(u64, bool)>,
}

impl TypingRun {
    /// The step's findings, for the run's notes: the typing posts while a typed (when, and the gaps between them),
    /// those after it stopped and after it emptied the box (each with its type), and b's indicator against a's last
    /// typing post.
    #[must_use]
    pub fn notes(&self) -> Vec<String> {
        let while_typing: Vec<u64> = self.posts_between(self.began, self.stopped).map(|(at, _)| at).collect();
        let gaps: Vec<String> = while_typing
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).to_string())
            .collect();
        let mut notes = vec![format!(
            "typing: a typed for {} ms: {} typing post(s), at {} ms after its first character; gaps (ms): {}",
            self.stopped.saturating_sub(self.began),
            while_typing.len(),
            list(while_typing.iter().map(|at| since(*at, self.began))),
            list(gaps.into_iter()),
        )];
        for (what, from, to) in [
            ("stopped (text left in the box)", self.stopped, self.emptied),
            ("emptied the box", self.emptied, self.ended),
        ] {
            let posts: Vec<String> = self
                .posts_between(from, to)
                .map(|(at, kind)| format!("{kind} at {} ms", since(at, from)))
                .collect();
            notes.push(format!(
                "typing: after a {what}, for {} ms: {}",
                to.saturating_sub(from),
                if posts.is_empty() {
                    "no typing post".to_owned()
                } else {
                    posts.join(", ")
                }
            ));
        }
        notes.push(self.indicator_note());
        notes
    }

    /// The posts sent in `[from, to)`, as (time, type).
    fn posts_between(&self, from: u64, to: u64) -> impl Iterator<Item = (u64, &str)> {
        self.posts
            .iter()
            .filter(move |(at, _)| (from..to).contains(at))
            .map(|(at, kind)| (*at, kind.as_str()))
    }

    /// b's indicator: each change, after a's first character, and how long it stayed after a's last `Control/Typing`.
    fn indicator_note(&self) -> String {
        let changes = list(self.indicator.iter().map(|(at, shown)| {
            format!(
                "{} at {} ms",
                if *shown { "shown" } else { "gone" },
                since(*at, self.began)
            )
        }));
        let last_typing = self
            .posts
            .iter()
            .filter(|(_, kind)| kind == "Control/Typing")
            .map(|(at, _)| *at)
            .max();
        let gone_after = last_typing.and_then(|last| {
            self.indicator
                .iter()
                .find(|(at, shown)| !shown && *at >= last)
                .map(|(at, _)| at - last)
        });
        let stayed = match (last_typing, gone_after) {
            (None, _) => "a sent no Control/Typing".to_owned(),
            (Some(_), None) => "it never went after a's last Control/Typing".to_owned(),
            (Some(_), Some(ms)) => format!("it went {ms} ms after a's last Control/Typing was sent"),
        };
        format!("typing: b's indicator: {changes}; {stayed}")
    }
}

/// `at` against `base`, in ms, signed (`+1200`, `-300`).
fn since(at: u64, base: u64) -> String {
    match at.checked_sub(base) {
        Some(after) => format!("+{after}"),
        None => format!("-{}", base - at),
    }
}

/// The items, comma-separated; `none` for no item.
fn list(items: impl Iterator<Item = String>) -> String {
    let items: Vec<String> = items.collect();
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

/// Now, in ms since the epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_millis()).ok())
        .unwrap_or_default()
}

/// Runs `chat-shapes` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, an unreadable `chats.toml`, no Chrome, no fence, a browser that can't be
/// armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let chats = teams_fence::lab::LabChats::load(paths).map_err(|error| setup(error.to_string()))?;
    let mut report = ScenarioReport::new(ScenarioName::ChatShapes);
    // checked before any browser opens, as `abc-chat` does: a message the fence would deny is sent again on every
    // later load of a's profile
    let group = match chats.abc {
        None => Err("no group chat recorded in chats.toml: run abc-chat first".to_owned()),
        Some(chat_id) => match messaging_blocked(&lab_source()?, &chat_id) {
            Some(reason) => Err(reason),
            None => Ok(chat_id),
        },
    };
    let session_options = SessionOptions {
        live_members: true,
        capture: Some(ScenarioName::ChatShapes.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::System,
        user_agent: None,
        ..SessionOptions::fail_closed()
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
    let nonce = nonce();
    for rule in abc_chat::forced_denies(&nonce) {
        a.force_deny(rule);
    }
    let mri = |alias| config.account(alias).mri.clone().unwrap_or_default();
    let ab_chat_id = one_to_one_id(&mri(Alias::A), &mri(Alias::B));
    let policy = policy(ab_chat_id.as_deref(), group.as_deref().ok()).unwrap_or_else(|error| {
        report.failures.push(format!("can't tell a chat in a URL: {error}"));
        DenyPolicy::Reviewed(Review::new().expecting_forced(&abc_chat::EXPECTED_FORCED))
    });
    let shapes = Shapes {
        mri_a: mri(Alias::A),
        mri_b: mri(Alias::B),
        texts: Texts::new(&nonce),
        probe: probe_text(&nonce),
        options,
    };
    if let Err(error) = shapes
        .run(ab_chat_id.as_deref(), group, &mut a, &mut b, &mut report)
        .await
    {
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
struct Shapes {
    mri_a: String,
    mri_b: String,
    texts: Texts,
    probe: String,
    options: RunOptions,
}

/// The two pages, once both show the a–b chat.
struct Pages<'a> {
    a: &'a LabSession,
    b: &'a LabSession,
    drive_a: Driver,
    drive_b: Driver,
    /// The writes on the a–b chat.
    writes: Writes,
}

impl Shapes {
    /// Steps 2 and 3, then each step on its own ([`settle`]).
    async fn run(
        &self,
        ab_chat_id: Option<&str>,
        group: Result<String, String>,
        a: &mut LabSession,
        b: &mut LabSession,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let chat_id = ab_chat_id
            .ok_or_else(|| setup("lab.toml has no MRI for a or b: run `proto-lab login` for them".to_owned()))?;
        let drive_b = self.open_b(b, chat_id, report).await?;
        let drive_a = self.open_a(a, chat_id, report).await?;
        let writes = Writes::on(chat_id).map_err(|error| setup(error.to_string()))?;
        let mut pages = Pages {
            a,
            b,
            drive_a,
            drive_b,
            writes,
        };
        send(
            &mut pages.drive_a,
            &pages.writes.post,
            "a (the first message)",
            &self.texts.first,
        )
        .await?;
        wait_for_newest(
            &mut pages.drive_b,
            "b shows a's first message",
            &self.texts.first,
            DELIVERY,
        )
        .await?;
        not_received(&mut pages.drive_b, &self.probe).await?;
        report
            .notes
            .push("enforcement: b's copy of the chat has a's message and not the probe".to_owned());

        let outcome = self.typing(&mut pages, report).await;
        settle(&mut pages, "typing", outcome, report).await;
        let outcome = self.lines(&mut pages, report).await;
        settle(&mut pages, "lines and a link", outcome, report).await;
        let outcome = self.reactions(&mut pages, report).await;
        settle(&mut pages, "reactions", outcome, report).await;
        let outcome = match group {
            Ok(group) => self.group_mention(&mut pages, &group, report).await,
            Err(reason) => Err(setup(reason)),
        };
        settle(&mut pages, "group mention", outcome, report).await;
        Ok(())
    }

    /// Step 2: b loads, registers its messaging endpoint, opens the chat with a (registering again), reads it, and
    /// starts recording its typing indicator ([`TYPING_RECORDER`]).
    async fn open_b(
        &self,
        b: &mut LabSession,
        chat_id: &str,
        report: &mut ScenarioReport,
    ) -> Result<Driver, ScenarioError> {
        let opened = chat_requests(chat_id).map_err(|error| setup(error.to_string()))?;
        let mut drive_b = b.driver();
        drive_b.set_call_timeout(PAGE_CALL);
        load(Alias::B, b, self.options.load_timeout).await?;
        report.notes.push("b: the web client loaded".to_owned());
        wait_registered(&mut drive_b, "b registers its messaging endpoint", 0).await?;
        size(b).await?;
        let chat_document = drive_b.seen_so_far();
        drive_b
            .navigate("b opens the chat with a", &chat_link(&self.mri_a))
            .await?;
        drive_b
            .wait_for_request("b reads the a–b chat", &opened, CHAT_OPEN)
            .await?;
        wait_registered(&mut drive_b, "b registers its messaging endpoint again", chat_document).await?;
        drive_b
            .wait_for("b's compose box", COMPOSE, self.options.load_timeout.max(CHAT_OPEN))
            .await?;
        drive_b
            .wait_until_true(
                "b records its typing indicator",
                TYPING_RECORDER,
                "the recorder to be installed",
                STEP,
            )
            .await?;
        report.notes.push(
            "b: the a–b chat is open, the messaging endpoint registered, the typing indicator recorded".to_owned(),
        );
        Ok(drive_b)
    }

    /// Step 3: a loads, opens the chat with b and proves enforcement live.
    async fn open_a(
        &self,
        a: &mut LabSession,
        chat_id: &str,
        report: &mut ScenarioReport,
    ) -> Result<Driver, ScenarioError> {
        let mut drive_a = a.driver();
        drive_a.set_call_timeout(PAGE_CALL);
        load(Alias::A, a, self.options.load_timeout).await?;
        report.notes.push("a: the web client loaded".to_owned());
        size(a).await?;
        drive_a
            .navigate("a opens the chat with b", &chat_link(&self.mri_b))
            .await?;
        drive_a
            .wait_for("a's compose box", COMPOSE, self.options.load_timeout.max(CHAT_OPEN))
            .await?;
        let names_chat = chat_url(chat_id).map_err(|error| setup(error.to_string()))?;
        enforce(a, &mut drive_a, &self.probe, &names_chat).await?;
        report
            .notes
            .push("enforcement: a's probe post was denied by the fence and failed in a's page".to_owned());
        Ok(drive_a)
    }

    /// Step 4: a types, stops, empties its compose box; the typing posts and b's indicator go into the notes.
    async fn typing(&self, pages: &mut Pages<'_>, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        let drive = &mut pages.drive_a;
        drive.type_into("a goes to its compose box", COMPOSE, "").await?;
        tokio::time::sleep(QUIET_BEFORE_TYPING).await;
        let mark = drive.seen_so_far();
        let mut run = TypingRun {
            began: now_ms(),
            ..TypingRun::default()
        };
        let typing = Instant::now();
        // keys pressed, not text inserted: the run that inserted the text (2026-10-21) sent no typing post in 45 s of
        // it, so key presses were tried; they brought none while a typed either, and why is not known (chat.md)
        while typing.elapsed() < TYPE_FOR {
            drive.press("a types a character", TYPED_KEY).await?;
            tokio::time::sleep(TYPE_EVERY).await;
        }
        run.stopped = now_ms();
        tokio::time::sleep(PAUSE).await;
        run.emptied = now_ms();
        empty_compose(drive).await?;
        tokio::time::sleep(AFTER_EMPTYING).await;
        run.ended = now_ms();
        drive.seen_so_far();
        run.posts = typing_posts(&drive.requests()[mark..], &pages.writes.post);
        let log = pages.drive_b.evaluate("b's typing indicator log", TYPING_LOG).await?;
        run.indicator = indicator_changes(&log);
        report.notes.extend(run.notes());
        Ok(())
    }

    /// Step 5: a sends [`Texts::lines`] as one message, broken with Shift+Enter; its post must be answered and b must
    /// show it. The notes give b's typing indicator around it: each change since a began typing it, against the post.
    async fn lines(&self, pages: &mut Pages<'_>, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        let began = now_ms();
        let drive = &mut pages.drive_a;
        drive.skip_seen();
        let [first, rest @ ..] = &self.texts.lines;
        drive.type_into("a types the first line", COMPOSE, first).await?;
        for line in rest {
            drive.press("a breaks the line", Key::SHIFT_ENTER).await?;
            // the space after the link is a key pressed, as a person types it: the first run's link, typed in one
            // insertion, stayed plain text
            match line.split_once(&self.texts.link) {
                Some((before, after)) => {
                    drive
                        .insert_text("a types the link", &format!("{before}{}", self.texts.link))
                        .await?;
                    drive.press("a presses space", Key::SPACE).await?;
                    drive.insert_text("a types the rest", after.trim_start()).await?;
                }
                None => drive.insert_text("a types a line", line).await?,
            }
        }
        press_until_sent(drive, "a (the lines)", first).await?;
        let post = pages.writes.post.clone().with_body_containing(first);
        let seen = drive.wait_for_answer("a's lines are posted", &post, STEP).await?;
        answered("a's lines", &seen)?;
        wait_for_newest(&mut pages.drive_b, "b shows a's lines", first, DELIVERY).await?;
        tokio::time::sleep(LINES_SETTLE).await;
        let log = pages.drive_b.evaluate("b's typing indicator log", TYPING_LOG).await?;
        let posted = seen.sent_ms.unwrap_or(began);
        let changes = indicator_changes(&log)
            .into_iter()
            .filter(|(at, _)| *at >= began)
            .map(|(at, shown)| format!("{} at {} ms", if shown { "shown" } else { "gone" }, since(at, posted)));
        report.notes.push(format!(
            "lines: b's typing indicator against a's post of the lines: {}",
            list(changes)
        ));
        Ok(())
    }

    /// Step 6: a sets each quick reaction its hover bar offers on its first message, then takes each back.
    async fn reactions(&self, pages: &mut Pages<'_>, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        let drive = &mut pages.drive_a;
        find_message(drive, "a finds its first message", &self.texts.first).await?;
        drive.hover("a points at the message", MARKED).await?;
        let offered = format!("{HOVER_REACTIONS}.length > 0");
        drive
            .wait_until_true("a's hover bar", &offered, "the hover bar's quick reactions", STEP)
            .await?;
        let tids = drive
            .evaluate("the hover bar's quick reactions", HOVER_REACTIONS)
            .await?;
        let tids: Vec<String> = tids
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tid| tid.as_str().map(str::to_owned))
            .collect();
        report
            .notes
            .push(format!("a: the hover bar offers {}", tids.join(", ")));
        for (what, method) in [("sets", "PUT"), ("takes back", "DELETE")] {
            for tid in &tids {
                let outcome = react(drive, &pages.writes.reaction, &self.texts.first, tid, method).await?;
                report.notes.push(format!("a {what} {tid}: {outcome}"));
            }
        }
        Ok(())
    }

    /// Step 7: a opens the group chat and mentions b; b opens it and must show the mention.
    async fn group_mention(
        &self,
        pages: &mut Pages<'_>,
        group: &str,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let link = chat_link_by_id(group);
        let wait = self.options.load_timeout.max(CHAT_OPEN);
        pages.drive_a.navigate("a opens the a-b-c chat", &link).await?;
        pages
            .drive_a
            .wait_for("a's compose box in the a-b-c chat", COMPOSE, wait)
            .await?;
        let writes = Writes::on(group).map_err(|error| setup(error.to_string()))?;
        mention_b(
            &mut pages.drive_a,
            &writes.post,
            &self.mri_b,
            &self.texts.group_mention,
            &mut report.notes,
        )
        .await?;
        report
            .notes
            .push("a: group mention: b offered by name, picked and mentioned".to_owned());
        let opened = chat_requests(group).map_err(|error| setup(error.to_string()))?;
        pages.drive_b.navigate("b opens the a-b-c chat", &link).await?;
        pages
            .drive_b
            .wait_for_request("b reads the a-b-c chat", &opened, wait)
            .await?;
        wait_for_newest(
            &mut pages.drive_b,
            "b shows a's mention in the a-b-c chat",
            &self.texts.group_mention,
            DELIVERY,
        )
        .await?;
        report.notes.push("b: shows a's mention in the a-b-c chat".to_owned());
        Ok(())
    }
}

/// a clicks the quick reaction `tid` on the message holding `text` and waits for a reaction request of `method`
/// (`PUT` sets, `DELETE` takes back) to be answered with a 2xx; then, after [`REACTION_SETTLE`], says what every
/// reaction request since the click was (method, key, status), the waited one among them.
async fn react(
    drive: &mut Driver,
    reaction: &RequestPattern,
    text: &str,
    tid: &str,
    method: &str,
) -> Result<String, ScenarioError> {
    find_message(drive, "a finds its first message", text).await?;
    drive.hover("a points at the message", MARKED).await?;
    let mark = drive.seen_so_far();
    let css = format!(r"[data-tid={}]", js_string(tid));
    click_visible(drive, &format!("a clicks {tid}"), &css, STEP).await?;
    let step = format!("a's {method} of {tid}");
    let seen = drive
        .wait_for_seen(&step, &format!("a reaction {method} answered"), mark, STEP, |seen| {
            seen.method.eq_ignore_ascii_case(method)
                && reaction.matches(&seen.method, &seen.url)
                && (seen.status.is_some() || seen.failed.is_some())
        })
        .await?;
    answered(&step, &seen)?;
    tokio::time::sleep(REACTION_SETTLE).await;
    drive.seen_so_far();
    let caused: Vec<String> = drive.requests()[mark..]
        .iter()
        .filter(|request| reaction.matches(&request.method, &request.url))
        .map(|request| {
            format!(
                "{} key {:?} answered {:?}",
                request.method,
                reaction_key(request.body.as_deref()),
                request.status
            )
        })
        .collect();
    Ok(list(caused.into_iter()))
}

/// Empties the compose box ([`super::ab_chat_full::clear_compose`]), and the box must be empty.
async fn empty_compose(drive: &mut Driver) -> Result<(), ScenarioError> {
    super::ab_chat_full::clear_compose(drive).await?;
    let empty = format!(
        "(document.querySelector({})?.innerText ?? '').trim() === ''",
        js_string(COMPOSE_CSS)
    );
    drive
        .wait_until_true("a's compose box is empty", &empty, "an empty compose box", STEP)
        .await?;
    Ok(())
}

/// Notes a step that went through; for one that failed, the failure, a screenshot of each page
/// (`failure-<alias>-<step>.png` in its capture dir), the `data-tid`s a's page shows, and Escape twice on a's page to
/// close what the step left open.
async fn settle(pages: &mut Pages<'_>, step: &str, outcome: Result<(), ScenarioError>, report: &mut ScenarioReport) {
    let error = match outcome {
        Ok(()) => {
            report.notes.push(format!("{step}: done"));
            return;
        }
        Err(error) => error,
    };
    report.failures.push(format!("{step}: {error}"));
    let file = step.replace(' ', "-");
    for (alias, session) in [(Alias::A, pages.a), (Alias::B, pages.b)] {
        if let Some(note) = save_screenshot_as(alias, session, &format!("failure-{alias}-{file}.png")).await {
            report.notes.push(note);
        }
    }
    let tids = pages
        .drive_a
        .evaluate("the page's controls", VISIBLE_TIDS)
        .await
        .unwrap_or(Value::Null);
    report.notes.push(format!("{step}: a's visible data-tids: {tids}"));
    for _ in 0..2 {
        let _ = pages.drive_a.press("close what is open", Key::ESCAPE).await;
    }
}
