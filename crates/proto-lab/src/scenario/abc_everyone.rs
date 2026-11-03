//! Captures an everyone mention in the recorded group chat. One labelled shape capture is deliberately held, then the real message is sent and must appear on the other account.

use std::time::Duration;

use serde_json::Value;

use crate::browser::Media;
use crate::classify::strict_json;
use crate::config::{Alias, LabPaths};
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{
    CHAT_OPEN, COMPOSE, DELIVERY, HELD_MARKER, load_at, nonce, save_screenshot_as, setup, wait_for_newest,
};
use super::ab_chat_full::{Writes, answered, clear_compose};
use super::abc_chat::chat_link_by_id;
use super::dsl::{Driver, Key, js_string};
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// How long a step waits for a control to show, or for its write to be answered.
pub const STEP: Duration = Duration::from_secs(30);

/// How long one call to a page may go unanswered (as `chat-shapes`).
const PAGE_CALL: Duration = Duration::from_secs(90);

/// The mention picker's label of the suggestion that mentions everyone in the chat, in the web client's English
/// (`peoplePicker`'s `compose_mention_everyone_tag`, web bundle `26260.1717.5229.7049`).
pub const EVERYONE: &str = "Everyone";

/// How many times a moves the picker's highlight looking for [`EVERYONE`] before it gives up.
const MOST_STEPS: usize = 12;

/// The mention picker (the web client's `data-tid`).
const PICKER: &str = r#"[data-tid="AutocompletePopup-Mentions"]"#;

/// The page expression of the picker's visible suggestions.
const OPTIONS: &str = r#"[...document.querySelectorAll('[data-tid="AutocompletePopup-Mentions"] [role="option"]')]
    .filter((e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; })"#;

/// The page function of the label a suggestion shows: its first line of text, else its accessible name.
const SHOWN_NAME: &str = r"((e) => (e.innerText ?? '').split(/\n/).map((l) => l.trim()).find((l) => l)
    ?? (e.getAttribute('aria-label') ?? '').trim())";

/// The page expression of the index of the picker's highlighted suggestion (`aria-selected`, else the first).
const SELECTED: &str = r"(() => { const o = OPTIONS; const i = o.findIndex((e) => e.getAttribute('aria-selected') === 'true'); return o.length ? Math.max(i, 0) : -1; })()";

/// `ArrowDown`: moves the picker's highlight to the next suggestion.
const ARROW_DOWN: Key = Key {
    key: "ArrowDown",
    code: "ArrowDown",
    virtual_key: 40,
    text: None,
    modifiers: 0,
};

/// The held mention's text after the mention: the probe marker first, so every run's capture hold stops it.
#[must_use]
pub fn held_text(nonce: &str) -> String {
    format!("{HELD_MARKER}{nonce} everyone held")
}

/// The sent mention's text after the mention.
#[must_use]
pub fn sent_text(nonce: &str) -> String {
    format!("lab everyone {nonce}")
}

/// Runs `abc-everyone` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, an unreadable `chats.toml`, no Chrome, a browser that can't be
/// armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let chats = crate::config::LabChats::load(paths).map_err(|error| setup(error.to_string()))?;
    let mut report = ScenarioReport::new(ScenarioName::AbcEveryone);
    let Some(group) = chats.abc else {
        report
            .failures
            .push("no group chat recorded in chats.toml: run abc-chat first".to_owned());
        return Ok(report);
    };
    let session_options = SessionOptions {
        capture: Some(ScenarioName::AbcEveryone.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::System,
        user_agent: None,
    };
    let a = LabSession::open_with(paths, Alias::A, session_options.clone()).await?;
    let b = match LabSession::open_with(paths, Alias::B, session_options).await {
        Ok(b) => b,
        Err(error) => {
            let _ = a.close().await;
            return Err(error.into());
        }
    };
    let nonce = nonce();
    Ok(drive(&group, &nonce, options, a, b).await)
}

/// Runs and closes the already-open armed sessions; startup above remains responsible for eligibility and opening.
async fn drive(group: &str, nonce: &str, options: RunOptions, mut a: LabSession, mut b: LabSession) -> ScenarioReport {
    let mut report = ScenarioReport::new(ScenarioName::AbcEveryone);
    match crate::capture_hold::Hold::new("POST", super::ab_chat::MESSAGE_POST) {
        Ok(rule) => a.hold(
            rule.with_body_containing(&held_text(nonce))
                .labelled("held everyone mention"),
        ),
        Err(error) => report.failures.push(error.to_string()),
    }

    let steps = Steps { group, nonce, options };
    if let Err(error) = steps.run(&mut a, &mut b, &mut report).await {
        report.failures.push(error.to_string());
        for (alias, session) in [(Alias::A, &a), (Alias::B, &b)] {
            if let Some(note) = save_screenshot_as(alias, session, "failure.png").await {
                report.notes.push(note);
            }
        }
    }
    for (alias, session) in [(Alias::A, a), (Alias::B, b)] {
        report.close_account(alias, session).await;
    }
    report
}

struct Steps<'a> {
    group: &'a str,
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
        let link = chat_link_by_id(self.group);
        let wait = self.options.load_timeout.max(CHAT_OPEN);
        let mut drive_b = b.driver();
        drive_b.set_call_timeout(PAGE_CALL);
        load_at(Alias::B, b, &link, self.options.load_timeout).await?;
        drive_b.wait_for("b's compose box", COMPOSE, wait).await?;
        report
            .notes
            .push("b: the web client loaded at the a-b-c chat".to_owned());

        let mut drive_a = a.driver();
        drive_a.set_call_timeout(PAGE_CALL);
        load_at(Alias::A, a, &link, self.options.load_timeout).await?;
        drive_a.wait_for("a's compose box", COMPOSE, wait).await?;

        let post = Writes::on(self.group).map_err(|error| setup(error.to_string()))?.post;
        let held = held_text(self.nonce);
        self.mention_everyone(&mut drive_a, a, &held, true, report).await?;
        let seen = drive_a
            .wait_for_failed_request(
                "a's held mention of everyone fails",
                &post.clone().with_body_containing(&held),
                STEP,
            )
            .await?;
        let seen = drive_a.read_body("a's held mention's body", &seen).await?;
        let body = seen.body.clone().unwrap_or_default();
        report.notes.push(format!(
            "held: the post's mentions: {}",
            mentions_note(&body, self.group)
        ));
        if let Err(reason) = judged_as_the_chat(&seen.url, &body, self.group) {
            return Err(setup(format!("held: {reason}; nothing more is sent")));
        }
        report
            .notes
            .push("held: the captured Everyone mention refers to the selected group chat".to_owned());

        let sent = sent_text(self.nonce);
        self.mention_everyone(&mut drive_a, a, &sent, false, report).await?;
        let seen = drive_a
            .wait_for_answer(
                "a's mention of everyone is posted",
                &post.with_body_containing(&sent),
                STEP,
            )
            .await?;
        answered("a's mention of everyone", &seen)?;
        report
            .notes
            .push("sent: a's mention of everyone was answered".to_owned());
        wait_for_newest(&mut drive_b, "b shows a's mention of everyone", &sent, DELIVERY).await?;
        report.notes.push("b: shows a's mention of everyone".to_owned());
        tokio::time::sleep(Duration::from_secs(2)).await;
        if let Some(note) = save_screenshot_as(Alias::B, b, "everyone-received.png").await {
            report.notes.push(note);
        }
        Ok(())
    }

    /// a types `@`, waits for the picker to offer [`EVERYONE`], moves its highlight there, picks it and sends `text`
    /// after the mention. The first time (`look`), what the picker shows goes into the notes and a screenshot of it
    /// into a's capture dir.
    async fn mention_everyone(
        &self,
        drive: &mut Driver,
        a: &LabSession,
        text: &str,
        look: bool,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        drive.type_into("a starts a mention", COMPOSE, "@").await?;
        let offered = format!("{OPTIONS}.some((e) => {SHOWN_NAME}(e) === {})", js_string(EVERYONE));
        if let Err(error) = drive
            .wait_until_true(
                "a's picker offers everyone",
                &offered,
                "the picker to offer Everyone",
                STEP,
            )
            .await
        {
            let shown = drive
                .evaluate("what the picker shows", &picker_state())
                .await
                .unwrap_or(Value::Null);
            report.notes.push(format!("a: the picker showed {shown}"));
            for _ in 0..2 {
                let _ = drive.press("a closes the picker", Key::ESCAPE).await;
            }
            let _ = clear_compose(drive).await;
            return Err(error.into());
        }
        if look {
            let shown = drive.evaluate("what the picker shows", &picker_state()).await?;
            report.notes.push(format!("a: the picker shows {shown}"));
            if let Some(note) = save_screenshot_as(Alias::A, a, "everyone-picker.png").await {
                report.notes.push(note);
            }
        }
        highlight_everyone(drive).await?;
        drive.press_enter("a picks Everyone").await?;
        drive.skip_seen();
        super::ab_chat::send(drive, "a (the mention of everyone)", &format!(" {text}")).await
    }
}

/// Moves the picker's highlight down until it is on [`EVERYONE`], at most [`MOST_STEPS`] times.
async fn highlight_everyone(drive: &mut Driver) -> Result<(), ScenarioError> {
    let highlighted = format!(
        "(() => {{ const o = {OPTIONS}; const i = {}; return i >= 0 && {SHOWN_NAME}(o[i]) === {}; }})()",
        SELECTED.replace("OPTIONS", OPTIONS),
        js_string(EVERYONE)
    );
    // checked before each step and once after the last
    for step in 0..=MOST_STEPS {
        let on = drive
            .evaluate("whether Everyone is highlighted", &highlighted)
            .await?
            .as_bool()
            .unwrap_or(false);
        if on {
            return Ok(());
        }
        if step < MOST_STEPS {
            drive.press("a moves the picker's highlight", ARROW_DOWN).await?;
        }
    }
    Err(setup(format!(
        "the picker's highlight never reached Everyone in {MOST_STEPS} steps"
    )))
}

/// What the picker shows, for the notes, without a name or an address of anyone it offers (the notes are printed):
/// whether it is open, how many suggestions it has, which one is highlighted, which one is [`EVERYONE`] and the lines
/// that one shows (the web client's own words), and how many lines each suggestion shows.
fn picker_state() -> String {
    r"(() => {
  const options = OPTIONS;
  const lines = (e) => (e.innerText ?? '').split(/\n/).map((l) => l.trim()).filter((l) => l);
  return {
    open: !!document.querySelector(POPUP),
    count: options.length,
    selected: options.findIndex((e) => e.getAttribute('aria-selected') === 'true'),
    everyone: options.findIndex((e) => NAME(e) === LABEL),
    everyoneLines: options.filter((e) => NAME(e) === LABEL).map((e) => lines(e).slice(0, 3)),
    lineCounts: options.map((e) => lines(e).length),
  };
})()"
        .replace("OPTIONS", OPTIONS)
        .replace("POPUP", &js_string(PICKER))
        .replace("NAME", SHOWN_NAME)
        .replace("LABEL", &js_string(EVERYONE))
}

/// The entries of a post body's `properties.mentions`, for the notes: each one's keys and values, its `mri` written
/// `<the chat>` when it is the group chat's id `group`; or why they can't be read.
#[must_use]
pub fn mentions_note(body: &str, group: &str) -> String {
    let Some(entries) = mention_entries(body) else {
        return "unreadable".to_owned();
    };
    let entries: Vec<Value> = entries
        .into_iter()
        .map(|mut entry| {
            if entry.get("mri").and_then(Value::as_str) == Some(group) {
                entry["mri"] = Value::from("<the chat>");
            }
            entry
        })
        .collect();
    Value::Array(entries).to_string()
}

/// A post body's `properties.mentions` entries, both read strictly.
fn mention_entries(body: &str) -> Option<Vec<Value>> {
    let body = strict_json(body.as_bytes())?;
    let mentions = strict_json(body.get("properties")?.get("mentions")?.as_str()?.as_bytes())?;
    mentions.as_array().cloned()
}

/// Whether the held post (`url`, `body`) is a mention of everyone the lab rule lets through: one entry of
/// `mentionType` `everyone`, and the classifier's verdict a message to the group chat `group` adding nobody.
///
/// # Errors
/// What it is instead.
pub fn judged_as_the_chat(url: &str, body: &str, group: &str) -> Result<(), String> {
    let entries = mention_entries(body).ok_or("its mentions can't be read")?;
    let everyone = entries
        .iter()
        .filter(|entry| entry.get("mentionType").and_then(Value::as_str) == Some("everyone"))
        .count();
    if everyone != 1 {
        return Err(format!("{everyone} mention(s) of everyone in it, not one"));
    }
    if entries.iter().any(|entry| {
        entry.get("mentionType").and_then(Value::as_str) == Some("everyone")
            && entry.get("mri").and_then(Value::as_str) != Some(group)
    }) {
        return Err("the Everyone mention does not refer to the selected group chat".into());
    }
    let pattern = super::ab_chat::chat_url(group).map_err(|error| error.to_string())?;
    let message = regex::Regex::new(super::ab_chat::MESSAGE_POST).map_err(|error| error.to_string())?;
    if pattern.is_match(url) && message.is_match(url) {
        Ok(())
    } else {
        Err("the post URL doesn't name the selected group chat".into())
    }
}

#[cfg(test)]
#[path = "../../tests/support/scenario/abc_everyone_driver_tests.rs"]
mod tests;
