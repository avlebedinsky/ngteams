//! The `abc-chat` scenario: the group chat of accounts a, b and c (M1 Task 8).
//!
//! Every account runs with the write mapping ([`Classifier::mapped`]) and members read live (`LiveMembershipSource`),
//! as in [`super::ab_chat`]. The group chat's id isn't derived from MRIs as a 1:1 chat's is: the scenario creates the
//! chat once and keeps its id in the lab's `chats.toml` ([`LabChats`]), never in the repo. What a run does depends on
//! whether that id is there yet.
//!
//! **Creation** (no id yet, account a only). a opens the web client straight at a new chat with b and c
//! ([`new_chat_link`]: a deep link reloads the whole client, so the run loads it there rather than loading it twice).
//! The web client creates a group chat when its first message is sent: a `POST v1/threads` naming a, b and c (each
//! `Admin`, `threadType` `chat`), which the fence judges as a [`teams_fence::WriteKind::ChatCreate`] by the members in
//! its body, then the message post. That first message carries [`FIRST_MESSAGE_MARKER`] and is force-denied under
//! [`FIRST_MESSAGE`]: the web client keeps a send that failed and sends it again on every later load, so the marker's
//! forced deny stays in every later run of this scenario. The marker begins with
//! [`PROBE_MARKER`](super::ab_chat::PROBE_MARKER), so `ab-chat`'s forced deny of an earlier run's probe covers it too
//! when the web client resends it there. The run's goal: the first message post to a group chat was denied under
//! [`FIRST_MESSAGE`] and failed in a's page, and the chat's id (from that post's URL) is recorded in `chats.toml`.
//!
//! **Messaging** (the id recorded; accounts a, b and c). It starts only once the fence can find a group chat's members
//! complete ([`messaging_blocked`]); until then a message would be denied and resent on every later load, so the run
//! fails before it opens anything. Group chats stay `Partial(Unsupported)` since M1 Task 8
//! (`docs/backlog/group-chat-member-read-completeness.md`).
//! 1. a's fence force-denies this run's probe ([`THIS_RUN_PROBE`]), the first message ([`FIRST_MESSAGE`]) and any
//!    earlier run's probe ([`EARLIER_RUN_PROBE`]), in that order (a deny names the first rule that matched it), from
//!    before the page loads.
//! 2. a opens the web client straight at the group chat ([`chat_link_by_id`]).
//! 3. **Live enforcement test**: a sends this run's probe; the fence must force-deny, under [`THIS_RUN_PROBE`], a
//!    message post to the group chat, and that very post must fail in a's page with `net::ERR_BLOCKED_BY_CLIENT`.
//! 4. a sends the real message; then b and c, one after the other, open the web client at the group chat, so each
//!    reads its history from the service (the fence denies the push channel's frames), and must show the message within
//!    [`DELIVERY`](super::ab_chat::DELIVERY) and not the probe.
//! 5. Judged by [`policy`]: no deny but the three forced ones and the reviewed expected-deny list
//!    ([`super::expected`]), and nothing denied on the group chat but those forced ones. The message and the members'
//!    typing and read receipts pass only when the fence's live read finds the group chat's membership complete and all
//!    lab accounts (`docs/protocol/fence-targets.md`).
//!
//! Every wait for the web client to load takes the run's load timeout (`proto-lab run --timeout`); on a loaded machine
//! the client can take minutes.

use std::time::Duration;

use regex::Regex;
use teams_fence::lab::{ChatRecord, LabChats, record_abc_chat};
use teams_fence::{LiveMembershipSource, Scope, is_group_chat_id};
use url::Url;

use crate::browser::Media;
use crate::classify::{Classifier, chat_service_path};
use crate::commands::status::account_status;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_http::{Denial, DenyCause, ForcedDeny, without_query};
use crate::fence_ws::FrameClassifier;
use crate::session::{LabSession, SessionOptions, SessionState};

use super::ab_chat::{
    CHAT_OPEN, DELIVERY, EARLIER_RUN_PROBE, MESSAGE_POST, PROBE_FAIL, PROBE_MARKER, THIS_RUN_PROBE, chat_requests,
    chat_url, message_text, nonce, probe_text, save_screenshot, send,
};
use super::dsl::Driver;
use super::expected::Review;
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// What the group chat's first message carries: [`super::ab_chat::PROBE_MARKER`] and more, so every scenario that
/// force-denies an earlier run's probe denies it too.
pub const FIRST_MESSAGE_MARKER: &str = "lab fence probe abc-chat first message ";

/// The label of the forced deny of the group chat's first message ([`FIRST_MESSAGE_MARKER`]).
pub const FIRST_MESSAGE: &str = "the a-b-c chat's first message";

/// The forced denies a run of this scenario expects, in the order its rules are set ([`deny_markers`]).
pub const EXPECTED_FORCED: [&str; 3] = [THIS_RUN_PROBE, FIRST_MESSAGE, EARLIER_RUN_PROBE];

/// A URL naming any group chat (`19:<id>@thread.v2`, plain or percent-encoded): where the first message may go.
const ANY_GROUP_CHAT: &str = r"(?i)19(:|%3A)[A-Za-z0-9_-]+(@|%40)thread\.v2";

/// The denies a run of this scenario expects: the reviewed list ([`super::expected`]) and the forced denies of
/// [`EXPECTED_FORCED`]; with the group chat's id, any other deny of a URL naming it ([`chat_url`]) is unexpected, the
/// list notwithstanding.
///
/// # Errors
/// When the chat's pattern doesn't compile (never for a group chat id).
pub fn policy(chat_id: Option<&str>) -> Result<DenyPolicy, regex::Error> {
    let mut review = Review::new().expecting_forced(&EXPECTED_FORCED);
    if let Some(chat_id) = chat_id {
        review = review.with_mapped_target(chat_url(chat_id)?);
    }
    Ok(DenyPolicy::Reviewed(review))
}

/// The web client's deep link that starts a new chat with the accounts `mris`: a group chat for two or more. By MRI,
/// as [`super::ab_chat::chat_link`]: a UPN needs a profile lookup the fence denies.
#[must_use]
pub fn new_chat_link(mris: &[&str]) -> String {
    let users: Vec<String> = mris
        .iter()
        .map(|mri| url::form_urlencoded::byte_serialize(mri.as_bytes()).collect())
        .collect();
    format!("https://teams.microsoft.com/l/chat/0/0?users={}", users.join(","))
}

/// The web client's deep link to the existing chat `chat_id`.
#[must_use]
pub fn chat_link_by_id(chat_id: &str) -> String {
    let id: String = url::form_urlencoded::byte_serialize(chat_id.as_bytes()).collect();
    format!("https://teams.microsoft.com/l/chat/{id}/0")
}

/// The group chat's first message ([`FIRST_MESSAGE_MARKER`] and this run's nonce).
#[must_use]
pub fn first_message_text(nonce: &str) -> String {
    format!("{FIRST_MESSAGE_MARKER}{nonce}")
}

/// The group chat a message post goes to: the conversation id of a chat service
/// `POST …/v1/users/ME/conversations/<id>/messages` URL, when it is a group chat's ([`is_group_chat_id`]).
#[must_use]
pub fn group_chat_of_message_post(url: &str) -> Option<String> {
    let path = chat_service_path(&Url::parse(url).ok()?)?;
    match path.as_slice() {
        [users, me, conversations, id, messages]
            if users == "users" && me == "ME" && conversations == "conversations" && messages == "messages" =>
        {
            is_group_chat_id(id).then(|| id.clone())
        }
        _ => None,
    }
}

/// Runs `abc-chat` (see the module docs): the creation run until `chats.toml` records the group chat, the messaging
/// run after.
///
/// # Errors
/// When the run can't start: no lab config, an unreadable `chats.toml`, no Chrome, no fence, a browser that can't be
/// armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let chats = LabChats::load(paths).map_err(|error| setup(error.to_string()))?;
    match chats.abc {
        None => create(paths, &config, options).await,
        Some(chat_id) => match messaging_blocked(&chat_id) {
            Some(reason) => {
                let mut report = ScenarioReport::new(ScenarioName::AbcChat);
                report.failures.push(reason);
                Ok(report)
            }
            None => message(paths, &chat_id, options).await,
        },
    }
}

/// Why the messaging run must not start, if it mustn't: the fence can't find the group chat's members complete
/// ([`LiveMembershipSource::reads_in_full`]), so its message would be denied, and the web client would send it again on
/// every later load of a's profile, where every scenario would see it denied. Checked before any browser opens.
#[must_use]
pub fn messaging_blocked(chat_id: &str) -> Option<String> {
    let scope = Scope::Conversation { id: chat_id.to_owned() };
    (!LiveMembershipSource::reads_in_full(&scope)).then(|| {
        "the fence can't read the a-b-c chat's members in full (group chats stay Partial(Unsupported): \
         docs/backlog/group-chat-member-read-completeness.md), so a message to it would be denied and then sent again \
         on every later load of a's profile; nothing was opened"
            .to_owned()
    })
}

fn session_options(options: RunOptions) -> SessionOptions {
    SessionOptions {
        classifier: Classifier::mapped(),
        frames: FrameClassifier::seeded(),
        live_members: true,
        capture: Some(ScenarioName::AbcChat.as_str().to_owned()),
        display: options.display,
        media: Media::System,
        user_agent: None,
    }
}

fn mri(config: &LabConfig, alias: Alias) -> Result<&str, ScenarioError> {
    config.account(alias).mri.as_deref().ok_or_else(|| {
        setup(format!(
            "lab.toml has no MRI for {alias}: run `proto-lab login {alias}`"
        ))
    })
}

/// The creation run (see the module docs).
async fn create(paths: &LabPaths, config: &LabConfig, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let others = [mri(config, Alias::B)?, mri(config, Alias::C)?];
    let mut a = LabSession::open_with(paths, Alias::A, session_options(options)).await?;
    let mut report = ScenarioReport::new(ScenarioName::AbcChat);
    let nonce = nonce();
    deny_markers(&a, &probe_text(&nonce));
    let chat_id = match create_steps(&mut a, &others, &nonce, options, &mut report).await {
        Ok(chat_id) => Some(chat_id),
        Err(error) => {
            report.failures.push(error.to_string());
            if let Some(note) = save_screenshot(Alias::A, &a).await {
                report.notes.push(note);
            }
            None
        }
    };
    if let Some(chat_id) = &chat_id {
        match record_abc_chat(paths, chat_id) {
            Ok(ChatRecord::Recorded) => report.notes.push(format!(
                "the a-b-c chat's id is recorded in {}; run abc-chat again to message it",
                paths.chats_file().display()
            )),
            Ok(ChatRecord::AlreadyRecorded) => report.notes.push("the a-b-c chat's id was recorded already".to_owned()),
            Err(error) => report.failures.push(format!("can't record the a-b-c chat: {error}")),
        }
    }
    let policy = scenario_policy(chat_id.as_deref(), &mut report);
    report.close_account(Alias::A, a, &policy).await;
    Ok(report)
}

/// a loads, starts a chat with b and c, and sends its first message, which the fence must force-deny under
/// [`FIRST_MESSAGE`] as a post to a group chat and which must fail in a's page; the chat's id, from that post.
async fn create_steps(
    a: &mut LabSession,
    others: &[&str],
    nonce: &str,
    options: RunOptions,
    report: &mut ScenarioReport,
) -> Result<String, ScenarioError> {
    let mut drive_a = a.driver();
    load_at(Alias::A, a, &new_chat_link(others), options.load_timeout).await?;
    report
        .notes
        .push("a: the web client loaded at a new chat with b and c".to_owned());
    drive_a
        .wait_for("a's compose box", super::ab_chat::COMPOSE, options.load_timeout)
        .await?;
    let any_group = Regex::new(ANY_GROUP_CHAT).map_err(|error| setup(error.to_string()))?;
    send(&mut drive_a, "a (the first message)", &first_message_text(nonce)).await?;
    let denial = denied_in_page(a, &mut drive_a, FIRST_MESSAGE, &any_group).await?;
    let chat_id = group_chat_of_message_post(&denial.url)
        .ok_or_else(|| setup(format!("the first message went to {}, not to a group chat", denial.url)))?;
    report.notes.push(
        "a created a group chat with b and c; its first message was denied by the fence and failed in a's page"
            .to_owned(),
    );
    Ok(chat_id)
}

/// The messaging run (see the module docs).
async fn message(paths: &LabPaths, chat_id: &str, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let mut sessions = open_all(paths, options).await?;
    let mut report = ScenarioReport::new(ScenarioName::AbcChat);
    let nonce = nonce();
    if let Some((_, a)) = sessions.first() {
        deny_markers(a, &probe_text(&nonce));
    }
    let names_chat = chat_url(chat_id).map_err(|error| setup(error.to_string()))?;
    let steps = MessageSteps {
        chat_id,
        names_chat: &names_chat,
        nonce: &nonce,
        options,
    };
    if let Err(error) = steps.run(&mut sessions, &mut report).await {
        report.failures.push(error.to_string());
        for (alias, session) in &sessions {
            if let Some(note) = save_screenshot(*alias, session).await {
                report.notes.push(note);
            }
        }
    }
    let policy = scenario_policy(Some(chat_id), &mut report);
    for (alias, session) in sessions {
        report.close_account(alias, session, &policy).await;
    }
    Ok(report)
}

/// [`policy`], or, when the chat's pattern doesn't compile, a failure and the list alone (the denies are still
/// listed).
fn scenario_policy(chat_id: Option<&str>, report: &mut ScenarioReport) -> DenyPolicy {
    policy(chat_id).unwrap_or_else(|error| {
        report
            .failures
            .push(format!("can't tell the a-b-c chat in a URL: {error}"));
        DenyPolicy::Reviewed(Review::new().expecting_forced(&EXPECTED_FORCED))
    })
}

/// Opens a's, b's and c's sessions, in that order; if one can't be opened, the ones already open go down.
async fn open_all(paths: &LabPaths, options: RunOptions) -> Result<Vec<(Alias, LabSession)>, ScenarioError> {
    let mut sessions = Vec::with_capacity(Alias::ALL.len());
    for alias in Alias::ALL {
        match LabSession::open_with(paths, alias, session_options(options)).await {
            Ok(session) => sessions.push((alias, session)),
            Err(error) => {
                for (_, session) in sessions {
                    let _ = session.close().await;
                }
                return Err(error.into());
            }
        }
    }
    Ok(sessions)
}

struct MessageSteps<'a> {
    chat_id: &'a str,
    /// Finds the group chat's id in a URL ([`chat_url`]).
    names_chat: &'a Regex,
    nonce: &'a str,
    options: RunOptions,
}

impl MessageSteps<'_> {
    async fn run(
        &self,
        sessions: &mut [(Alias, LabSession)],
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let [(_, a), readers @ ..] = sessions else {
            return Err(setup("no session for a".to_owned()));
        };
        let mut drive_a = a.driver();
        load_at(Alias::A, a, &chat_link_by_id(self.chat_id), self.options.load_timeout).await?;
        report
            .notes
            .push("a: the web client loaded at the a-b-c chat".to_owned());
        drive_a
            .wait_for("a's compose box", super::ab_chat::COMPOSE, self.options.load_timeout)
            .await?;
        let probe = probe_text(self.nonce);
        send(&mut drive_a, "a (the probe)", &probe).await?;
        denied_in_page(a, &mut drive_a, THIS_RUN_PROBE, self.names_chat).await?;
        report
            .notes
            .push("enforcement: a's probe post was denied by the fence and failed in a's page".to_owned());
        let message = message_text(self.nonce);
        send(&mut drive_a, "a (the message)", &message).await?;
        for (alias, session) in readers.iter_mut() {
            self.receive(*alias, session, &message, &probe).await?;
            report
                .notes
                .push(format!("{alias} shows a's message and not the probe"));
        }
        Ok(())
    }

    /// `alias` opens the group chat, reads it from the service, and must show `message` and not `probe`.
    async fn receive(
        &self,
        alias: Alias,
        session: &mut LabSession,
        message: &str,
        probe: &str,
    ) -> Result<(), ScenarioError> {
        let opened = chat_requests(self.chat_id).map_err(|error| setup(error.to_string()))?;
        // b and c load only now, so each reads the chat's history from the service (the fence denies the push
        // channel's frames): the message there without the probe is the evidence the probe never got there
        let mut driver = session.driver();
        load_at(
            alias,
            session,
            &chat_link_by_id(self.chat_id),
            self.options.load_timeout,
        )
        .await?;
        driver
            .wait_for_request(&format!("{alias} reads the a-b-c chat"), &opened, CHAT_OPEN)
            .await?;
        driver
            .wait_for_text(&format!("{alias} receives the message"), message, DELIVERY)
            .await?;
        if driver
            .shows_text(&format!("{alias} must not have the probe"), probe)
            .await?
        {
            return Err(setup(format!("{alias} received the probe the fence denied")));
        }
        Ok(())
    }
}

/// Opens the web client in `session` straight at the deep link `url` and waits up to `timeout` for it to load. Not
/// the client first and the link after: a deep link reloads the whole client, which on a loaded machine takes minutes
/// more.
async fn load_at(alias: Alias, session: &mut LabSession, url: &str, timeout: Duration) -> Result<(), ScenarioError> {
    session.navigate(url).await?;
    let state = session.settle(timeout).await;
    if state == SessionState::Loaded {
        return Ok(());
    }
    let status = account_status(&state, timeout, session.watch().last_authz_status());
    Err(setup(format!("{alias}: the web client didn't load: {status}")))
}

/// Forces a deny on every `POST` or `PUT` of `session` whose body holds, in this order: this run's `probe`
/// ([`THIS_RUN_PROBE`]), the group chat's first message ([`FIRST_MESSAGE_MARKER`], [`FIRST_MESSAGE`]), any earlier
/// run's probe ([`PROBE_MARKER`], [`EARLIER_RUN_PROBE`]). A deny names the first rule that matched it.
fn deny_markers(session: &LabSession, probe: &str) {
    for (marker, label) in [
        (probe, THIS_RUN_PROBE),
        (FIRST_MESSAGE_MARKER, FIRST_MESSAGE),
        (PROBE_MARKER, EARLIER_RUN_PROBE),
    ] {
        for method in ["POST", "PUT"] {
            // "." always compiles
            if let Ok(rule) = ForcedDeny::new(method, ".") {
                session.force_deny(rule.with_body_containing(marker).labelled(label));
            }
        }
    }
}

/// Waits for the fence's forced deny, under `label`, of a message post whose URL `chat` matches, and for that very
/// post (by its `Network` id) to fail in the page as blocked by the fence; the deny.
async fn denied_in_page(
    session: &LabSession,
    driver: &mut Driver,
    label: &str,
    chat: &Regex,
) -> Result<Denial, ScenarioError> {
    let message_post = Regex::new(MESSAGE_POST).map_err(|error| setup(error.to_string()))?;
    let wanted = |denial: &Denial| {
        denial.cause == DenyCause::Forced
            && denial.label.as_deref() == Some(label)
            && message_post.is_match(&denial.url)
            && chat.is_match(&denial.url)
    };
    let deadline = tokio::time::Instant::now() + PROBE_FAIL;
    let denial = loop {
        if let Some(denial) = session.denials().into_iter().find(|denial| wanted(denial)) {
            break denial;
        }
        if tokio::time::Instant::now() >= deadline {
            let forced = session
                .denials()
                .iter()
                .filter(|denial| denial.cause == DenyCause::Forced)
                .count();
            return Err(setup(format!(
                "no message post denied as {label:?} within {PROBE_FAIL:?} ({forced} other forced deny(s))"
            )));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let network_id = denial
        .network_id
        .clone()
        .ok_or_else(|| setup(format!("the deny as {label:?} names no Network request")))?;
    let failed = driver
        .wait_for_failure_of(
            &format!("the post denied as {label:?} fails in the page"),
            &network_id,
            PROBE_FAIL,
        )
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
            "the post denied as {label:?} failed with {error}, not as blocked"
        )));
    }
    Ok(denial)
}

fn setup(reason: String) -> ScenarioError {
    ScenarioError::Setup(reason)
}
