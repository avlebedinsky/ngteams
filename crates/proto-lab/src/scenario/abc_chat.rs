//! Creates or messages the lab group chat. Creation holds only this run's first message to discover and persist the newly created chat id; messaging sends directly and checks both readers.

use crate::config::{ChatRecord, LabChats, record_abc_chat};
use crate::protocol::is_group_chat_id;
use regex::Regex;
use url::Url;

use crate::browser::Media;
use crate::capture_hold::Hold;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::protocol::chat_service_path;
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{
    CHAT_OPEN, DELIVERY, HOLD_WAIT, HeldPost, chat_requests, failed_in_page, load_at, message_text, nonce,
    save_screenshot, send, wait_for_newest,
};
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// What the group chat's first message carries: [`super::ab_chat::HELD_MARKER`] and more,
/// so every session keeps its queued retries unsent.
pub const FIRST_MESSAGE_MARKER: &str = "lab held capture abc-chat first message ";

/// The label of the capture hold of this run's first message ([`first_message_text`] of the run's nonce).
pub const THIS_RUN_FIRST_MESSAGE: &str = "this run's first message of the a-b-c chat";

/// A URL naming any group chat (`19:<id>@thread.v2`, plain or percent-encoded): where the first message may go.
pub const ANY_GROUP_CHAT: &str = r"(?i)19(:|%3A)[A-Za-z0-9_-]+(@|%40)thread\.v2";

/// The web client's deep link that starts a new chat with the accounts `mris`: a group chat for two or more. By MRI,
/// as [`super::ab_chat::chat_link`]: a UPN would require a profile lookup.
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

/// The group chat's first message ([`FIRST_MESSAGE_MARKER`] and this run's nonce, ended so that no other nonce's text
/// holds it).
#[must_use]
pub fn first_message_text(nonce: &str) -> String {
    format!("{FIRST_MESSAGE_MARKER}{nonce}.")
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
/// When the run can't start: no lab config, an unreadable `chats.toml`, no Chrome, a browser that can't be
/// armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let chats = LabChats::load(paths).map_err(|error| setup(error.to_string()))?;
    match chats.abc {
        None => create(paths, &config, options).await,
        Some(chat_id) => message(paths, &chat_id, options).await,
    }
}

fn session_options(options: RunOptions) -> SessionOptions {
    SessionOptions {
        capture: Some(ScenarioName::AbcChat.as_str().to_owned()),
        capture_request_bodies: true,
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
    let a = LabSession::open_with(paths, Alias::A, session_options(options)).await?;
    let nonce = nonce();
    Ok(create_session(paths, a, &others, &nonce, options).await)
}

/// Creation after the browser was opened, including recording the id before any later evidence can fail.
async fn create_session(
    paths: &LabPaths,
    mut a: LabSession,
    others: &[&str],
    nonce: &str,
    options: RunOptions,
) -> ScenarioReport {
    let mut report = ScenarioReport::new(ScenarioName::AbcChat);
    hold_first_message(&a, nonce);
    let mut created = None;
    let outcome = create_steps(&mut a, others, nonce, options, &mut report, &mut created).await;
    if outcome.is_err()
        && let Some(note) = save_screenshot(Alias::A, &a).await
    {
        report.notes.push(note);
    }
    record_creation(paths, created, outcome, &mut report);

    report.close_account(Alias::A, a).await;
    report
}

/// Settles a creation run in `report`: the error its steps stopped at, if they did, is a failure; the chat's id, once
/// this run's first message's deny named it (`chat_id`), is recorded in `paths`' chats file whether or not the steps
/// went on to pass: the chat exists from that deny on, and a later run without its id would create another a-b-c chat.
/// The id, when there is one.
pub fn record_creation(
    paths: &LabPaths,
    chat_id: Option<String>,
    outcome: Result<(), ScenarioError>,
    report: &mut ScenarioReport,
) -> Option<String> {
    if let Err(error) = outcome {
        report.failures.push(error.to_string());
    }
    let chat_id = chat_id?;
    match record_abc_chat(paths, &chat_id) {
        Ok(ChatRecord::Recorded) => report.notes.push(format!(
            "the a-b-c chat's id is recorded in {}; run abc-chat again to message it",
            paths.chats_file().display()
        )),
        Ok(ChatRecord::AlreadyRecorded) => report.notes.push("the a-b-c chat's id was recorded already".to_owned()),
        Err(error) => report.failures.push(format!(
            "can't record the a-b-c chat: {error}; its id is in the URL of the capture hold {THIS_RUN_FIRST_MESSAGE:?} \
             in {}: record it as `abc` in {} before abc-chat runs again, or the run creates another chat",
            paths.captures_dir().display(),
            paths.chats_file().display()
        )),
    }
    Some(chat_id)
}

/// a loads, starts a chat with b and c, and sends this run's first message, which the explicit capture hold fails under
/// [`THIS_RUN_FIRST_MESSAGE`] as a post to a group chat and which must fail in a's page. The chat's id, from that post,
/// goes into `created` as soon as the deny is seen, before the checks in the page: the chat exists by then.
async fn create_steps(
    a: &mut LabSession,
    others: &[&str],
    nonce: &str,
    options: RunOptions,
    report: &mut ScenarioReport,
    created: &mut Option<String>,
) -> Result<(), ScenarioError> {
    let mut drive_a = a.driver();
    load_at(Alias::A, a, &new_chat_link(others), options.load_timeout).await?;
    report
        .notes
        .push("a: the web client loaded at a new chat with b and c".to_owned());
    drive_a
        .wait_for("a's compose box", super::ab_chat::COMPOSE, options.load_timeout)
        .await?;
    let any_group = Regex::new(ANY_GROUP_CHAT).map_err(|error| setup(error.to_string()))?;
    let this_run = HeldPost::new(THIS_RUN_FIRST_MESSAGE, &any_group).map_err(|error| setup(error.to_string()))?;
    send(&mut drive_a, "a (the first message)", &first_message_text(nonce)).await?;
    let Some(denial) = this_run.wait(a).await else {
        return Err(no_deny(a, THIS_RUN_FIRST_MESSAGE));
    };
    let chat_id = group_chat_of_message_post(&denial.url)
        .ok_or_else(|| setup(format!("the first message went to {}, not to a group chat", denial.url)))?;
    *created = Some(chat_id);
    failed_in_page(&mut drive_a, &denial, "a's first message post").await?;
    report.notes.push(
        "a created a group chat with b and c; its first message was denied in the capture and failed in a's page"
            .to_owned(),
    );
    Ok(())
}

/// The messaging run (see the module docs).
async fn message(paths: &LabPaths, chat_id: &str, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let sessions = open_all(paths, options).await?;
    let nonce = nonce();
    message_sessions(chat_id, options, &nonce, sessions).await
}

/// Message and close the already-open sessions; each caller supplies the same armed session type as startup.
async fn message_sessions(
    chat_id: &str,
    options: RunOptions,
    nonce: &str,
    mut sessions: Vec<(Alias, LabSession)>,
) -> Result<ScenarioReport, ScenarioError> {
    let mut report = ScenarioReport::new(ScenarioName::AbcChat);

    let steps = MessageSteps {
        chat_id,

        nonce,
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

    for (alias, session) in sessions {
        report.close_account(alias, session).await;
    }
    Ok(report)
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
        let message = message_text(self.nonce);
        send(&mut drive_a, "a (the message)", &message).await?;
        for (alias, session) in readers.iter_mut() {
            self.receive(*alias, session, &message).await?;
            report.notes.push(format!("{alias} shows a's message"));
        }
        Ok(())
    }

    /// `alias` opens the group chat, reads it from the service, and must show `message`.
    async fn receive(&self, alias: Alias, session: &mut LabSession, message: &str) -> Result<(), ScenarioError> {
        let opened = chat_requests(self.chat_id).map_err(|error| setup(error.to_string()))?;
        // Each reader loads its own view of the recorded chat's history from the service.
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
        // a chat with unread messages (each native run leaves some in the group chat) opens at its "last read" mark,
        // the newest not rendered
        wait_for_newest(&mut driver, &format!("{alias} receives the message"), message, DELIVERY).await?;

        Ok(())
    }
}

/// Holds this run's first message while discovering the newly created group chat.
#[must_use]
pub fn first_message_holds(nonce: &str) -> Vec<Hold> {
    let message = first_message_text(nonce);
    ["POST", "PUT"]
        .into_iter()
        .filter_map(|method| {
            Hold::new(method, ".")
                .ok()
                .map(|rule| rule.with_body_containing(&message).labelled(THIS_RUN_FIRST_MESSAGE))
        })
        .collect()
}

/// Forces [`first_message_holds`] of `nonce` on `session`, in their order.
fn hold_first_message(session: &LabSession, nonce: &str) {
    for rule in first_message_holds(nonce) {
        session.hold(rule);
    }
}

/// The error of a run whose message post wasn't held for capture under `label` within [`HOLD_WAIT`].
fn no_deny(session: &LabSession, label: &str) -> ScenarioError {
    let forced = session.held_requests().len();
    setup(format!(
        "no message post denied as {label:?} within {HOLD_WAIT:?} ({forced} other capture hold(s))"
    ))
}

fn setup(reason: String) -> ScenarioError {
    ScenarioError::Setup(reason)
}

#[cfg(test)]
#[path = "../../tests/support/scenario/abc_chat_driver_tests.rs"]
mod tests;
