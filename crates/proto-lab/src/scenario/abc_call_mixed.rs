//! The `abc-call-mixed` scenario (M3 Task 19): a group call of the native client and the web client together. Account
//! a's native client (`teams-headless`, spawned from `TEAMS_HEADLESS_BIN` as `ab-call-native` does) calls the lab's
//! a–b–c group chat (the one `abc-chat` recorded in `chats.toml`) with video, and b's and c's web clients in the armed
//! browser answer it; every write goes through its own fence, the pages' through the browser fence (members read live),
//! the native client's through its engine's outbox, all into the lab's `fence.log`.
//!
//! Each page plays Chrome's fake microphone (the tone in bursts) and camera; the native client plays the same tone in
//! bursts and its camera (the file backend's test pattern) on
//! [`NATIVE_CAMERA_AFTER`](super::ab_call_native::NATIVE_CAMERA_AFTER) after its call connected. Three phases, in
//! order:
//! 1. **a calls, b and c answer**: each page must show the incoming call and clicks answer only on the run's own call
//!    (`ab-call`'s ring check: every invitation the page received is a's call to it in the run's chat), which the
//!    fence permits only on the page's own offered invitation. Each page must hear the tone (its audio tap) and its audio go both ways, then turns its camera on and must decode
//!    video ([`PAGE_FRAMES`] within [`VIDEO_WAIT`]); a's native client must show both pages in its call, each on a
//!    receiving line of its own with its camera on (its `call_participants` lines).
//! 2. **c leaves**: c's page hangs up; a's native client must show b alone.
//! 3. **c joins late**: c's page opens the chat and clicks the header's join of the running call (which the fence judges
//!    by the chat it names) only on the run's call (its invitation
//!    in its capture, the page showing the run's chat with one join); it must show the call screen, decode video
//!    again, and a's native client must show both pages again, c's line bringing at least [`NATIVE_FRAMES`] frames
//!    after the join click (the counts `teams-headless` prints while the call runs, its `video_lines` lines, measured
//!    from the first of them printed after a's client showed c gone, so no frame c sent before it left counts).
//!
//! Then a's native client hangs up at the end of its hold; it must have exited 0, heard the tone, and shown video on
//! two lines (each [`NATIVE_FRAMES`] at least). The native client's output goes into b's capture dir
//! (`native-a-call-abc.txt`, lab data like the capture). Judged by each page's fence: no deny but the reviewed list's and
//! the run's forced ones ([`FORCED_B`], [`FORCED_C`]), each page receiving the run's invitation and its attach let
//! through, and c's join let through.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use teams_fence::lab::LabChats;

use crate::browser::Media;
use crate::capture::EVENTS_FILE;
use crate::classify::Classifier;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::session::{LabSession, SessionOptions};

use super::ab_call::{
    ACCEPT_AUDIO_CSS, CAMERA, CHAT_JOIN, CHAT_JOIN_CSS, CONNECT_WAIT, CalleeRole, EarlierRings, HANG_UP_CSS,
    INCOMING_CALL, RING_WAIT, RingCheck, RunCall, TONE_WAIT, VIDEO_WAIT, chat_shown, chat_shown_script, check_ring,
    hang_up_after_failure, hang_up_page, invitations, judge_callee, mri, open_join, policy, ring_check,
    stand_up_calling, switch_microphone, video_frames, wait_calling_socket, watch_controls,
};
use super::ab_call_native::{
    ANSWER, NATIVE_CONNECT, NATIVE_EXIT, NATIVE_FRAMES, Native, PAGE_FRAMES, TONE_HZ, capture_len, headless_binary,
    judge_native, now_ms, read_from, spawn_native, stop_native, tone_args, video_args,
};
use super::ab_chat::{EARLIER_RUN_PROBE, load_at, probe_rules, save_screenshot, setup};
use super::abc_chat::{chat_link_by_id, lab_source, messaging_blocked};
use super::dsl::{Selector, js_string};
use super::echo_call::{
    AudioSummary, HANG_UP, ToneWait, USER_AGENT, linger, size_page, turn_off_noise_suppression, wait_for_tone_in_from,
    write_burst_tone,
};
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The forced denies b's page expects: an earlier run's probe.
pub const FORCED_B: &[&str] = &[EARLIER_RUN_PROBE];
/// The forced denies c's page expects: an earlier run's probe, as b's.
pub const FORCED_C: &[&str] = &[EARLIER_RUN_PROBE];
/// How long a's native client holds its call once the media connected: the three phases fit in it.
pub const NATIVE_HOLD: Duration = Duration::from_secs(200);
/// How long a's native client gets to show what a phase changed in its call.
pub const NATIVE_SHOWS: Duration = Duration::from_secs(45);
/// What a's native client's output is named after.
const CALLER: &str = "a-call-abc";

/// Runs `abc-call-mixed` (see the module docs).
///
/// # Errors
/// When the run can't start: no `TEAMS_HEADLESS_BIN`, no `chats.toml` entry for the group chat, no lab config or no MRI
/// for a, b or c, no Chrome, no fence, a browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let binary = headless_binary()?;
    let chats = LabChats::load(paths).map_err(|error| setup(error.to_string()))?;
    let chat = chats
        .abc
        .ok_or_else(|| setup("chats.toml records no a-b-c chat: run `proto-lab run abc-chat` first".to_owned()))?;
    let mut report = ScenarioReport::new(ScenarioName::AbcCallMixed);
    if let Some(reason) = messaging_blocked(&lab_source()?, &chat) {
        report.failures.push(reason);
        return Ok(report);
    }
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let mris = [Alias::A, Alias::B, Alias::C].map(|alias| mri(&config, alias));
    let [Ok(mri_a), Ok(mri_b), Ok(mri_c)] = mris else {
        return Err(setup("lab.toml names no MRI for a, b or c".to_owned()));
    };
    let mut pages = open_pages(paths, options, [(Alias::B, mri_b), (Alias::C, mri_c)]).await?;
    let dir = pages[0].session.capture_dir().map(Path::to_path_buf);
    let mut run = Mixed {
        binary,
        dir: dir.clone().unwrap_or_else(|| paths.data_dir().join("headless")),
        chat: chat.clone(),
        call: RunCall {
            caller: mri_a,
            group_chat: Some(chat),
        },
        native: None,
        placed: None,
        options,
    };
    if let Err(error) = run.drive(&mut pages, &mut report).await {
        report.failures.push(error.to_string());
        for page in &pages {
            if let Some(note) = save_screenshot(page.alias, &page.session).await {
                report.notes.push(note);
            }
        }
        // b and c leave the group call too, so none is left running in the chat once a's native client stopped
        for page in &mut pages {
            hang_up_after_failure(page.alias, &mut page.session, &mut report).await;
        }
    }
    if let (Some(native), Some(page)) = (run.native.take(), pages.first_mut()) {
        let finished = stop_native(native, &mut page.session, &mut report).await;
        report.notes.push(format!(
            "{}: stopped at the run's end ({:?})",
            finished.what, finished.status
        ));
    }
    for page in pages {
        close_page(page, &run, &mut report).await;
    }
    Ok(report)
}

/// b's and c's pages opened in the armed browser, each under the earlier run's probe rules.
async fn open_pages(
    paths: &LabPaths,
    options: RunOptions,
    accounts: [(Alias, String); 2],
) -> Result<Vec<Page>, ScenarioError> {
    let tone = write_burst_tone(paths).map_err(|error| setup(format!("can't write the tone: {error}")))?;
    let session_options = SessionOptions {
        classifier: Classifier::new(),
        live_members: true,
        capture: Some(ScenarioName::AbcCallMixed.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::Fake { audio: Some(tone) },
        user_agent: Some(USER_AGENT.to_owned()),
    };
    let mut pages: Vec<Page> = Vec::new();
    for (alias, mri) in accounts {
        let session = match LabSession::open_with(paths, alias, session_options.clone()).await {
            Ok(session) => session,
            Err(error) => {
                for page in pages {
                    let _ = page.session.close().await;
                }
                return Err(error.into());
            }
        };
        for rule in probe_rules(None).map_err(|error| setup(error.to_string()))? {
            session.force_deny(rule);
        }
        pages.push(Page { alias, mri, session });
    }
    Ok(pages)
}

/// Closes `page` under its policy and judges its capture: the run's invitation received and its attach let through,
/// and c's join let through.
async fn close_page(page: Page, run: &Mixed, report: &mut ScenarioReport) {
    let (forced, role) = if page.alias == Alias::C {
        (FORCED_C, CalleeRole::Joins)
    } else {
        (FORCED_B, CalleeRole::Other)
    };
    let capture = page.session.capture_dir().map(Path::to_path_buf);
    report.close_account(page.alias, page.session, &policy(forced)).await;
    if let Some(capture) = capture {
        judge_callee((page.alias, &page.mri, role), &run.call, run.placed, &capture, report);
        report
            .notes
            .push(format!("{}: recorded into {}", page.alias, capture.display()));
    }
}

/// One page of the run: b's or c's.
struct Page {
    alias: Alias,
    mri: String,
    session: LabSession,
}

/// The run's state: the native client's binary, where its output goes, the chat and the call the invitations name, the
/// native client running, when it placed its call.
struct Mixed {
    binary: PathBuf,
    dir: PathBuf,
    chat: String,
    call: RunCall,
    native: Option<Native>,
    placed: Option<u64>,
    options: RunOptions,
}

impl Mixed {
    async fn drive(&mut self, pages: &mut [Page], report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        let link = chat_link_by_id(&self.chat);
        for page in pages.iter_mut() {
            load_at(page.alias, &mut page.session, &link, self.options.load_timeout).await?;
            size_page(&page.session).await?;
            report
                .notes
                .push(format!("{}: the web client loaded at the group chat", page.alias));
            stand_up_calling(page.alias, &mut page.session.driver(), report).await?;
            wait_calling_socket(page.alias, &mut page.session, report).await?;
            watch_controls(page.alias, &page.session).await?;
        }
        self.everyone_in(pages, report).await?;
        let absent = self.c_leaves(pages, report).await?;
        self.c_joins_late(pages, absent, report).await?;
        self.finish(pages, report).await
    }

    /// Phase 1 (see the module docs).
    async fn everyone_in(&mut self, pages: &mut [Page], report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        report
            .notes
            .push("phase 1: a's native client calls the group chat, b and c answer".to_owned());
        let placed = now_ms();
        self.placed = Some(placed);
        let mut args = vec!["call".to_owned(), "abc".to_owned()];
        args.extend(tone_args());
        args.extend(video_args());
        args.extend([
            "--seconds".to_owned(),
            NATIVE_HOLD.as_secs().to_string(),
            "--wait".to_owned(),
            NATIVE_CONNECT.as_secs().to_string(),
        ]);
        self.native = Some(spawn_native((&self.binary, &self.dir), Alias::A, CALLER, &args)?);
        for page in pages.iter_mut() {
            answer(page, &self.call, placed, report).await?;
        }
        for page in pages.iter_mut() {
            let from = capture_len(&page.session);
            hear(page, from, report).await?;
            see(page, from, report).await?;
        }
        let first = pages.first_mut().ok_or_else(|| setup("no page".to_owned()))?;
        self.wait_shown(&mut first.session, &["b", "c"], report).await
    }

    /// Phase 2 (see the module docs). Answers when (Unix ms) a's native client was seen showing c gone.
    async fn c_leaves(&mut self, pages: &mut [Page], report: &mut ScenarioReport) -> Result<u64, ScenarioError> {
        report.notes.push("phase 2: c's page leaves the call".to_owned());
        let [first, last] = pages else {
            return Err(setup("the run's pages are b's and c's".to_owned()));
        };
        hang_up_page(last.alias, &last.session, report).await?;
        self.wait_shown(&mut first.session, &["b"], report).await?;
        Ok(now_ms())
    }

    /// Phase 3 (see the module docs); `absent` is when (Unix ms) a's native client was seen showing c gone.
    async fn c_joins_late(
        &mut self,
        pages: &mut [Page],
        absent: u64,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        report.notes.push("phase 3: c's page joins the running call".to_owned());
        let [first, last] = pages else {
            return Err(setup("the run's pages are b's and c's".to_owned()));
        };
        let alias = last.alias;
        open_join(alias, &last.session, report).await?;
        self.check_join(last, report).await?;
        // what each of the native client's lines brought before the join, counted after c was seen gone (a count
        // printed before that may grow by frames c sent before it left): c's video after the join counts on top
        let before = self.wait_counts_after(&mut first.session, absent, report).await?;
        let from = capture_len(&last.session);
        let mut driver = last.session.driver();
        driver
            .click(&format!("{alias} joins the running call"), CHAT_JOIN)
            .await?;
        report.notes.push(format!("{alias}: clicked join at {}", now_ms()));
        driver
            .wait_for(&format!("{alias}'s call screen after the join"), HANG_UP, CONNECT_WAIT)
            .await?;
        report.notes.push(format!("{alias}: in the call at {}", now_ms()));
        see(last, from, report).await?;
        self.wait_shown(&mut first.session, &["b", "c"], report).await?;
        self.wait_frames_since(&mut first.session, "c", &before, report).await
    }

    /// Waits up to [`NATIVE_SHOWS`] until a's native client prints its video counts after `after` (Unix ms), the page's
    /// events flowing, and answers them ([`video_lines_after`]).
    async fn wait_counts_after(
        &self,
        session: &mut LabSession,
        after: u64,
        report: &mut ScenarioReport,
    ) -> Result<Vec<(u64, u64)>, ScenarioError> {
        let native = self
            .native
            .as_ref()
            .ok_or_else(|| setup("no native client runs".to_owned()))?;
        let deadline = tokio::time::Instant::now() + NATIVE_SHOWS;
        loop {
            if let Some(counts) = video_lines_after(&native.output(), after) {
                report
                    .notes
                    .push(format!("a: video by line before the join {counts:?} by {}", now_ms()));
                return Ok(counts);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(setup(format!(
                    "a's native client printed no video counts after c was seen gone ({after}) within {NATIVE_SHOWS:?}"
                )));
            }
            linger(session, Duration::from_millis(500)).await;
        }
    }

    /// Waits up to [`NATIVE_SHOWS`] until the line a's native client shows `who` on brought at least [`NATIVE_FRAMES`]
    /// frames more than `before` (each line's counts then) said, the page's events flowing.
    async fn wait_frames_since(
        &self,
        session: &mut LabSession,
        who: &str,
        before: &[(u64, u64)],
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let native = self
            .native
            .as_ref()
            .ok_or_else(|| setup("no native client runs".to_owned()))?;
        let deadline = tokio::time::Instant::now() + NATIVE_SHOWS;
        loop {
            let output = native.output();
            let line = latest_participants(&output)
                .and_then(|shown| shown.into_iter().find(|participant| participant.who == who)?.line);
            let since = line.map(|line| {
                let frames = |lines: &[(u64, u64)]| frames_on(lines, line);
                let now = latest_video_lines(&output).unwrap_or_default();
                (line, frames(&now).saturating_sub(frames(before)))
            });
            if let Some((line, frames)) = since.filter(|(_, frames)| *frames >= NATIVE_FRAMES) {
                report.notes.push(format!(
                    "a: {who}'s line {line} brought {frames} frames since the join by {}",
                    now_ms()
                ));
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(setup(format!(
                    "a's native client didn't show {NATIVE_FRAMES} of {who}'s frames since the join within \
                     {NATIVE_SHOWS:?} (line and frames since: {since:?})"
                )));
            }
            linger(session, Duration::from_millis(500)).await;
        }
    }

    /// Whether c's page may click the chat's join: its capture must hold the run's call's invitation ([`ring_check`])
    /// and its page must show the run's group chat with one join ([`chat_shown`]). Anything else fails the step.
    async fn check_join(&self, page: &Page, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        let alias = page.alias;
        let refuse = |reason: String| setup(format!("{alias} doesn't click join: {reason}"));
        let placed = self.placed.ok_or_else(|| refuse("the run placed no call".to_owned()))?;
        let text = page
            .session
            .capture_dir()
            .map(|dir| fs::read_to_string(dir.join(EVENTS_FILE)))
            .ok_or_else(|| refuse("the run records no capture to bind the running call to".to_owned()))?
            .map_err(|error| refuse(format!("can't read the capture: {error}")))?;
        match ring_check(&invitations(&text), &self.call, &page.mri, placed) {
            RingCheck::Ready => {}
            RingCheck::Refused(reason) => return Err(refuse(reason)),
            RingCheck::Pending => return Err(refuse("no invitation of the run's call reached the page".to_owned())),
        }
        let reading = page
            .session
            .driver()
            .evaluate(
                &format!("{alias}'s chat"),
                &chat_shown_script(&self.chat, CHAT_JOIN_CSS),
            )
            .await?;
        chat_shown(reading.as_str().unwrap_or_default()).map_err(refuse)?;
        report
            .notes
            .push(format!("{alias}: the page shows the run's chat and one join"));
        Ok(())
    }

    /// Waits up to [`NATIVE_SHOWS`] until a's native client shows exactly `who` (lab names) in its call, each on a
    /// receiving line of its own with its camera on, the page's events flowing.
    async fn wait_shown(
        &self,
        session: &mut LabSession,
        who: &[&str],
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let native = self
            .native
            .as_ref()
            .ok_or_else(|| setup("no native client runs".to_owned()))?;
        let deadline = tokio::time::Instant::now() + NATIVE_SHOWS;
        loop {
            let shown = latest_participants(&native.output());
            if shows(shown.as_deref(), who) {
                report
                    .notes
                    .push(format!("a: shows {who:?} on lines of their own by {}", now_ms()));
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(setup(format!(
                    "a's native client didn't show {who:?} within {NATIVE_SHOWS:?} (it shows {shown:?})"
                )));
            }
            linger(session, Duration::from_millis(500)).await;
        }
    }

    /// The end: a's native client hangs up at the end of its hold; it must have exited 0, heard the tone and shown
    /// video on two lines; b's page's call screen goes.
    async fn finish(&mut self, pages: &mut [Page], report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        let first = pages.first_mut().ok_or_else(|| setup("no page".to_owned()))?;
        let mut native = self
            .native
            .take()
            .ok_or_else(|| setup("no native client runs".to_owned()))?;
        let status = native
            .wait(&mut first.session, NATIVE_HOLD + NATIVE_CONNECT + NATIVE_EXIT)
            .await;
        let finished = native.finished(status, report);
        judge_native(&finished, report);
        let lines = native_lines(&finished.output);
        let shown = lines.iter().filter(|(_, frames)| *frames >= NATIVE_FRAMES).count();
        report.notes.push(format!("a: video by line {lines:?}"));
        if shown < 2 {
            report.failures.push(format!(
                "a's native client showed {NATIVE_FRAMES} frames on {shown} line(s), not 2: {lines:?}"
            ));
        }
        for page in pages.iter_mut() {
            let gone = page
                .session
                .driver()
                .wait_until_true(
                    &format!("{}'s call screen goes", page.alias),
                    &format!("document.querySelector({}) === null", js_string(HANG_UP_CSS)),
                    "the call screen to go",
                    CONNECT_WAIT,
                )
                .await;
            if gone.is_err() {
                let _ = hang_up_page(page.alias, &page.session, report).await;
            }
        }
        Ok(())
    }
}

/// `page` shows the incoming call within [`RING_WAIT`], clicks answer only on the run's own call, and is in the call
/// within [`CONNECT_WAIT`].
async fn answer(
    page: &mut Page,
    call: &RunCall,
    placed: u64,
    report: &mut ScenarioReport,
) -> Result<(), ScenarioError> {
    let alias = page.alias;
    let session = &mut page.session;
    session
        .driver()
        .wait_for(&format!("{alias} shows the incoming call"), INCOMING_CALL, RING_WAIT)
        .await?;
    check_ring(
        (alias, &mut *session),
        ANSWER,
        (call, &page.mri),
        placed,
        EarlierRings::Judged,
    )
    .await?;
    session
        .driver()
        .click(&format!("{alias} clicks answer"), Selector::Css(ACCEPT_AUDIO_CSS))
        .await?;
    report.notes.push(format!("{alias}: clicked answer at {}", now_ms()));
    session
        .driver()
        .wait_for(&format!("{alias}'s call screen"), HANG_UP, CONNECT_WAIT)
        .await?;
    report.notes.push(format!("{alias}: in the call at {}", now_ms()));
    Ok(())
}

/// `page` in the call opens its microphone (noise suppression off, another microphone), must hear the tone within
/// [`TONE_WAIT`], and its call audio must go both ways (the capture from byte `from` on).
async fn hear(page: &mut Page, from: u64, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    let alias = page.alias;
    turn_off_noise_suppression(alias, &mut page.session.driver(), report).await?;
    switch_microphone(alias, &mut page.session.driver(), report).await?;
    let Some(file) = page.session.capture_dir().map(|dir| dir.join(EVENTS_FILE)) else {
        return Err(setup("the run records no capture to hear the tone in".to_owned()));
    };
    let session = &mut page.session;
    let heard = wait_for_tone_in_from(&file, from, TONE_WAIT, async |poll| linger(session, poll).await).await;
    match heard {
        ToneWait::Back(after) => report.notes.push(format!(
            "{alias}: heard the tone ({TONE_HZ} Hz) {after:?} into the wait"
        )),
        other => report
            .failures
            .push(format!("{alias} didn't hear the tone ({other:?})")),
    }
    let audio = AudioSummary::from_events(&read_from(&file, from));
    if !audio.both_ways() {
        report
            .failures
            .push(format!("{alias}'s call audio didn't go both ways (WebRTC statistics)"));
    }
    Ok(())
}

/// `page` turns its camera on and must decode at least [`PAGE_FRAMES`] of video within [`VIDEO_WAIT`] (the capture
/// from byte `from` on).
async fn see(page: &mut Page, from: u64, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    let alias = page.alias;
    page.session
        .driver()
        .click(&format!("{alias} turns its camera on"), CAMERA)
        .await?;
    let Some(file) = page.session.capture_dir().map(|dir| dir.join(EVENTS_FILE)) else {
        return Err(setup("the run records no capture to read the video in".to_owned()));
    };
    let deadline = tokio::time::Instant::now() + VIDEO_WAIT;
    loop {
        let frames = video_frames(&read_from(&file, from));
        if frames >= PAGE_FRAMES {
            report
                .notes
                .push(format!("{alias}: decoded {frames} video frame(s) by {}", now_ms()));
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            report.failures.push(format!(
                "{alias} decoded {frames} video frames within {VIDEO_WAIT:?}, not {PAGE_FRAMES}"
            ));
            return Ok(());
        }
        linger(&mut page.session, Duration::from_secs(1)).await;
    }
}

/// One participant as a native client's `call_participants` line shows it: its lab name, its line, whether its camera
/// is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub who: String,
    pub line: Option<u64>,
    pub video_on: bool,
}

/// The participants a native client's output last showed in its call (its last `call_participants` line); `None`
/// without one.
#[must_use]
pub fn latest_participants(output: &str) -> Option<Vec<Shown>> {
    let line = output.lines().rev().find(|line| line.contains(" call_participants "))?;
    let json = line.split_once("participants=")?.1;
    let json = json.rsplit_once(" speaking=").map_or(json, |(json, _)| json);
    let participants: Vec<Value> = serde_json::from_str(json).ok()?;
    Some(
        participants
            .iter()
            .map(|participant| Shown {
                who: participant["who"].as_str().unwrap_or_default().to_owned(),
                line: participant["line"].as_u64(),
                video_on: participant["video_on"].as_bool().unwrap_or(false),
            })
            .collect(),
    )
}

/// Whether `shown` is exactly `who`, each on a receiving line of its own (1 and up) with its camera on.
#[must_use]
pub fn shows(shown: Option<&[Shown]>, who: &[&str]) -> bool {
    let Some(shown) = shown else { return false };
    let mut lines: Vec<u64> = shown.iter().filter_map(|participant| participant.line).collect();
    lines.sort_unstable();
    lines.dedup();
    shown.len() == who.len()
        && who
            .iter()
            .all(|name| shown.iter().any(|participant| participant.who == *name))
        && shown
            .iter()
            .all(|participant| participant.video_on && participant.line.is_some_and(|line| line >= 1))
        && lines.len() == shown.len()
}

/// The frames each video line brought so far as a native client's output last said while its call ran (its last
/// `video_lines received=<line>:<frames>,…` line); `None` without one.
#[must_use]
pub fn latest_video_lines(output: &str) -> Option<Vec<(u64, u64)>> {
    let line = output.lines().rev().find(|line| line.contains(" video_lines "))?;
    let counts = line.split_once(" video_lines received=")?.1;
    counts
        .split(',')
        .map(|each| {
            let (line, frames) = each.split_once(':')?;
            Some((line.parse().ok()?, frames.parse().ok()?))
        })
        .collect()
}

/// The frames each video line brought as a native client's output last said ([`latest_video_lines`]), only when that
/// last `video_lines` line was printed after `after` (Unix ms, its `<ms>` prefix); `None` otherwise.
#[must_use]
pub fn video_lines_after(output: &str, after: u64) -> Option<Vec<(u64, u64)>> {
    let line = output.lines().rev().find(|line| line.contains(" video_lines "))?;
    let at: u64 = line.split_once(' ')?.0.parse().ok()?;
    if at <= after {
        return None;
    }
    latest_video_lines(line)
}

/// The frames `line` brought in `lines` (as [`latest_video_lines`] reads them); 0 when it isn't there.
#[must_use]
pub fn frames_on(lines: &[(u64, u64)], line: u64) -> u64 {
    lines
        .iter()
        .find(|(each, _)| *each == line)
        .map_or(0, |(_, frames)| *frames)
}

/// The frames a native client's output says it received on each video line (its `video line=<n> received=<n>` lines).
#[must_use]
pub fn native_lines(output: &str) -> Vec<(u64, u64)> {
    output
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("video line=")?;
            let (line, rest) = rest.split_once(' ')?;
            let received = rest.strip_prefix("received=")?.split(' ').next()?;
            Some((line.parse().ok()?, received.parse().ok()?))
        })
        .collect()
}
