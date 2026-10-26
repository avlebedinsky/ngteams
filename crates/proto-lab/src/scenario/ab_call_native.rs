//! The `ab-call-native` scenario: account b's web client in the armed browser and account a's native client
//! (`teams-headless`, a normal lab build the scenario spawns, its path in `TEAMS_HEADLESS_BIN`) call each other in
//! their 1:1 chat, both ways, and then a call to b is answered on b's page while b's native client rings too. Every
//! write of either goes through its own fence: the page's through the browser fence (members read live), the native
//! client's through its engine's outbox and `Fence::open_lab_account`, both into the lab's `fence.log`.
//!
//! b's page loads at the a–b chat, starts its calling stack (the Calls app), and plays Chrome's fake microphone (the
//! tone in bursts, [`write_burst_tone`]); every native client plays the same tone in bursts (`--tone 440 --bursts`), so
//! each side can hear the other's: the page by its audio tap ([`wait_for_tone_in_from`]), the native client by the
//! tone's presence in its recording (`tone hz=440 heard=true`). In the first two phases the call carries video both
//! ways too (M3 Task 18): the native client takes video on Cisco's OpenH264 (`--video`, the lab's copy) and turns its
//! camera on (the file backend's test pattern) [`NATIVE_CAMERA_AFTER`] after its call connected; b's page turns its
//! camera on (Chrome's fake camera) once it heard the native client's tone; the page must decode the native client's
//! video ([`video_frames`] from the phase's capture on, at least [`PAGE_FRAMES`] within [`VIDEO_WAIT`]) and the native
//! client must show the page's (its `video received=` line, at least [`NATIVE_FRAMES`]). Three phases, in order:
//! 1. **a's native client calls b** (`teams-headless a call b`): b's page must show the incoming call; it clicks
//!    answer only on the run's own call (`ab-call`'s ring check, [`EarlierRings::OwnLeftOut`]: every invitation the
//!    page received is a's call to b in no group chat, an earlier phase's ring of a's call to b aside, and the page
//!    shows one incoming call); the page must hear a's tone and its audio go both ways, and a's
//!    native client must hear the page's tone, hold the call [`NATIVE_HOLD`], hang up and exit 0.
//! 2. **b's page calls a** (`teams-headless a watch --answer-lab-calls`): once a's calling stack registered, b's page
//!    calls from the chat's header (its start decided by the fence) and a's native client answers it (the engine
//!    offers only a lab caller's invitation naming only lab accounts); the page must hear a's tone, its audio go both
//!    ways, and then hang up; a's native client must hear the page's tone and exit 0 on its interrupt.
//! 3. **Answered elsewhere**: b's native client up too (`teams-headless b watch --calls`, which answers nothing), a's
//!    native client calls b, both of b's endpoints ring, b's page answers; b's native client's ring must end on the
//!    answered-elsewhere delivery (its `ring_ended` naming [`ANSWERED_ELSEWHERE`], while a's native client still holds
//!    the call), with no answer of its own.
//!
//! The native clients' output goes into b's capture dir (`native-<phase>.txt`, lab data like the capture); their
//! recordings into the lab's `recordings/`. Judged by b's fence: no deny but the reviewed list's and the run's forced
//! ones ([`FORCED`]), b's page receiving the run's invitations and its attach let through ([`judge_callee`]), and its
//! own start let through.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::browser::Media;
use crate::capture::{EVENTS_FILE, TAP_TONE_HZ};
use crate::classify::Classifier;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::session::{LabSession, SessionOptions};

use super::ab_call::{
    ACCEPT_AUDIO_CSS, CAMERA, CONNECT_WAIT, CalleeAction, CalleeRole, EarlierRings, HANG_UP_CSS, INCOMING_CALL,
    Modality, RING_LOOK, RING_WAIT, RunCall, TONE_WAIT, VIDEO_WAIT, back_to_chat, check_ring, hang_up_page,
    judge_callee, mri, native_call_conflict, place_call, policy, stand_up_calling, switch_microphone, video_frames,
    wait_calling_socket, watch_controls,
};
use super::ab_chat::{EARLIER_RUN_PROBE, chat_link, load_at, probe_rules, save_screenshot, setup};
use super::dsl::{Selector, js_string};
use super::echo_call::{
    AudioSummary, HANG_UP, ToneWait, USER_AGENT, call_starts, linger, size_page, turn_off_noise_suppression,
    wait_for_tone_in_from, write_burst_tone,
};
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The forced denies b's run expects: an earlier run's probe.
pub const FORCED: &[&str] = &[EARLIER_RUN_PROBE];

/// The environment variable naming the `teams-headless` binary the scenario spawns: a normal lab build
/// (`cargo build --release -p teams-headless --target-dir target/lab`), which refuses to run with `test-support` in.
pub const HEADLESS_BIN: &str = "TEAMS_HEADLESS_BIN";

/// The tone both sides play and listen for: the one the page's audio tap measures.
pub const TONE_HZ: u32 = TAP_TONE_HZ;

/// How long a's native client holds its call once the media connected.
pub const NATIVE_HOLD: Duration = Duration::from_secs(40);
/// How long a native client may take to connect its call, from its placing.
pub(crate) const NATIVE_CONNECT: Duration = Duration::from_secs(90);
/// How long a native client gets to register its calling stack.
pub const NATIVE_REGISTER: Duration = Duration::from_secs(90);
/// How long b's page holds the call it placed once it heard a's tone (or the wait for it ended).
pub const PAGE_HOLD: Duration = Duration::from_secs(15);
/// How long a native client gets to exit once its call is over or it was interrupted.
pub(crate) const NATIVE_EXIT: Duration = Duration::from_secs(45);
/// How long b's native client's ring gets to end once b's page answered.
pub const ELSEWHERE_WAIT: Duration = Duration::from_secs(30);
/// How long a's native client holds the call b's page answered while b's native client rang: past
/// [`ELSEWHERE_WAIT`], so the ring can't end on a's hang-up (a cancel) before the answered-elsewhere check is done.
pub const ELSEWHERE_HOLD: Duration = Duration::from_secs(ELSEWHERE_WAIT.as_secs() + 30);
/// The phrase of the call end a callee's other endpoint gets when the call was answered on another one (seen
/// 2026-10-25, `ab-call-native`: `code 487`, "Call cancelled as it was accepted by another fork.").
pub const ANSWERED_ELSEWHERE: &str = "accepted by another fork";
/// How long after its call connected a native client turns its camera on.
pub const NATIVE_CAMERA_AFTER: Duration = Duration::from_secs(5);
/// The native client's video frames b's page must have decoded (about 2 s of its camera).
pub const PAGE_FRAMES: u64 = 30;
/// The page's video frames a native client must have shown.
pub const NATIVE_FRAMES: u64 = 30;

/// What b's page clicks on its incoming call: answer.
pub(crate) const ANSWER: CalleeAction = CalleeAction {
    button: ACCEPT_AUDIO_CSS,
    what: "answer",
};
/// How long the page's call screen gets to go once the far end hung up.
const SCREEN_GONE: Duration = Duration::from_secs(30);

/// Runs `ab-call-native` (see the module docs).
///
/// # Errors
/// When the run can't start: no `TEAMS_HEADLESS_BIN`, no lab config or no MRI for a or b, no Chrome, no fence, a
/// browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let binary = headless_binary()?;
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let (mri_a, mri_b) = (mri(&config, Alias::A)?, mri(&config, Alias::B)?);
    let mut report = ScenarioReport::new(ScenarioName::AbCallNative);
    let tone = write_burst_tone(paths).map_err(|error| setup(format!("can't write the tone: {error}")))?;
    let session_options = SessionOptions {
        classifier: Classifier::new(),
        live_members: true,
        capture: Some(ScenarioName::AbCallNative.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::Fake { audio: Some(tone) },
        user_agent: Some(USER_AGENT.to_owned()),
    };
    let mut session = LabSession::open_with(paths, Alias::B, session_options).await?;
    for rule in probe_rules(None).map_err(|error| setup(error.to_string()))? {
        session.force_deny(rule);
    }
    let dir = session.capture_dir().map(Path::to_path_buf);
    let mut run = Run {
        binary,
        dir: dir.clone().unwrap_or_else(|| paths.data_dir().join("headless")),
        call: RunCall {
            caller: mri_a.clone(),
            group_chat: None,
        },
        mri_a,
        mri_b: mri_b.clone(),
        options,
        natives: Vec::new(),
        first_ring: None,
    };
    if let Err(error) = run.drive(&mut session, &mut report).await {
        report.failures.push(error.to_string());
        if let Some(note) = save_screenshot(Alias::B, &session).await {
            report.notes.push(note);
        }
    }
    run.stop_natives(&mut session, &mut report).await;
    report.close_account(Alias::B, session, &policy(FORCED)).await;
    if let Some(dir) = dir {
        judge_page(&dir, &run, &mri_b, &mut report);
        report.notes.push(format!("b: recorded into {}", dir.display()));
    }
    Ok(report)
}

/// The `teams-headless` binary from [`HEADLESS_BIN`].
pub(crate) fn headless_binary() -> Result<PathBuf, ScenarioError> {
    let binary = std::env::var_os(HEADLESS_BIN).map(PathBuf::from).ok_or_else(|| {
        setup(format!(
            "{HEADLESS_BIN} is unset: build `cargo build --release -p teams-headless --target-dir target/lab` and \
             name it"
        ))
    })?;
    if !binary.is_file() {
        return Err(setup(format!("{HEADLESS_BIN} {} is not a file", binary.display())));
    }
    Ok(binary)
}

/// b's capture after the run: the run's invitations reached the page and its attach went (a callee's judgement in
/// `ab-call`), and its own start was let through.
fn judge_page(dir: &Path, run: &Run, mri_b: &str, report: &mut ScenarioReport) {
    judge_callee(
        (Alias::B, mri_b, CalleeRole::Other),
        &run.call,
        run.first_ring,
        dir,
        report,
    );
    let text = fs::read_to_string(dir.join(EVENTS_FILE)).unwrap_or_default();
    let (allowed, denied) = call_starts(&text);
    report.notes.push(format!(
        "b: call start {allowed} allowed and {denied} denied by the fence"
    ));
    if allowed == 0 {
        report.failures.push("b's fence allowed no call start".to_owned());
    }
}

/// The run's state: the binary, where the native clients' output goes, the call a's invitations must name, the native
/// clients running.
struct Run {
    binary: PathBuf,
    dir: PathBuf,
    call: RunCall,
    mri_a: String,
    mri_b: String,
    options: RunOptions,
    natives: Vec<Native>,
    /// When phase 1's call was placed (Unix ms): the run's first invitation comes after it.
    first_ring: Option<u64>,
}

impl Run {
    async fn drive(&mut self, session: &mut LabSession, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        load_at(Alias::B, session, &chat_link(&self.mri_a), self.options.load_timeout).await?;
        size_page(session).await?;
        report.notes.push("b: the web client loaded at the a-b chat".to_owned());
        stand_up_calling(Alias::B, &mut session.driver(), report).await?;
        back_to_chat(Alias::B, &mut session.driver(), report).await?;
        wait_calling_socket(Alias::B, session, report).await?;
        watch_controls(Alias::B, session).await?;
        self.native_calls_page(session, report).await?;
        self.page_calls_native(session, report).await?;
        self.answered_elsewhere(session, report).await
    }

    /// Phase 1 (see the module docs).
    async fn native_calls_page(
        &mut self,
        session: &mut LabSession,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        const CALLER: &str = "a-call-b";
        report
            .notes
            .push("phase 1: a's native client calls b's page".to_owned());
        let since = now_ms();
        self.first_ring = Some(since);
        self.spawn(Alias::A, CALLER, &call_args(NATIVE_HOLD, true))?;
        self.answer_on_page(session, (CALLER, since), report).await?;
        let from = capture_len(session);
        hear_on_page(session, from, report).await?;
        see_on_page(session, from, report).await?;
        let caller = self.finish(CALLER, NATIVE_HOLD, session, report).await?;
        judge_native(&caller, report);
        judge_native_video(&caller, report);
        wait_screen_gone(session, report).await;
        Ok(())
    }

    /// Phase 2 (see the module docs).
    async fn page_calls_native(
        &mut self,
        session: &mut LabSession,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        const CALLEE: &str = "a-watch";
        report
            .notes
            .push("phase 2: b's page calls a's native client".to_owned());
        self.spawn(Alias::A, CALLEE, &watch_args("--answer-lab-calls", true))?;
        self.wait_output(CALLEE, session, "calling_registration registered=true", NATIVE_REGISTER)
            .await?;
        session.clear_forced();
        for rule in probe_rules(None).map_err(|error| setup(error.to_string()))? {
            session.force_deny(rule);
        }
        back_to_chat(Alias::B, &mut session.driver(), report).await?;
        let from = capture_len(session);
        place_call((Alias::B, session), Modality::Audio, report).await?;
        self.wait_output(CALLEE, session, "call_answer call=", RING_WAIT)
            .await?;
        self.wait_output(CALLEE, session, "status=\"connected\"", CONNECT_WAIT)
            .await?;
        session
            .driver()
            .wait_for("b's call screen", HANG_UP, CONNECT_WAIT)
            .await?;
        report
            .notes
            .push(format!("b: in the call a's native client answered, at {}", now_ms()));
        hear_on_page(session, from, report).await?;
        see_on_page(session, from, report).await?;
        linger(session, PAGE_HOLD).await;
        if !hang_up_page(Alias::B, session, report).await? {
            report
                .failures
                .push("b's call screen was gone before b hung up".to_owned());
        }
        self.wait_output(CALLEE, session, "status=\"ended\"", SCREEN_GONE)
            .await?;
        let callee = self.stop(CALLEE, session, report).await?;
        judge_native(&callee, report);
        judge_native_video(&callee, report);
        Ok(())
    }

    /// Phase 3 (see the module docs).
    async fn answered_elsewhere(
        &mut self,
        session: &mut LabSession,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        const OTHER: &str = "b-watch";
        const CALLER: &str = "a-call-b-elsewhere";
        report
            .notes
            .push("phase 3: a's native client calls b, answered on b's page while b's native client rings".to_owned());
        self.spawn(Alias::B, OTHER, &watch_args("--calls", false))?;
        self.wait_output(OTHER, session, "calling_registration registered=true", NATIVE_REGISTER)
            .await?;
        let since = now_ms();
        self.spawn(Alias::A, CALLER, &call_args(ELSEWHERE_HOLD, false))?;
        self.wait_output(OTHER, session, "incoming_call", RING_WAIT).await?;
        self.answer_on_page(session, (CALLER, since), report).await?;
        self.wait_output(OTHER, session, "ring_ended", ELSEWHERE_WAIT).await?;
        let rang = self.native(OTHER)?.output();
        if !ring_ended_elsewhere(&rang) {
            report.failures.push(format!(
                "b's native client's ring didn't end as answered elsewhere ({ANSWERED_ELSEWHERE:?}): {}",
                rang.lines()
                    .filter(|line| line.contains("ring_ended"))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
        if self.native(CALLER)?.output().contains("status=\"ended\"") {
            report
                .failures
                .push("a's native client's call ended before b's native client's ring did".to_owned());
        }
        let caller = self.finish(CALLER, ELSEWHERE_HOLD, session, report).await?;
        if caller.status.is_none_or(|status| !status.success()) {
            report.failures.push(format!(
                "a's native client ({}) didn't exit 0: {:?}",
                caller.what, caller.status
            ));
        }
        wait_screen_gone(session, report).await;
        let other = self.stop(OTHER, session, report).await?;
        if other.output.contains("call_answer") {
            report.failures.push("b's native client answered a call".to_owned());
        }
        if other.status.is_none_or(|status| !status.success()) {
            report
                .failures
                .push(format!("b's native client didn't exit 0: {:?}", other.status));
        }
        Ok(())
    }

    /// b's page shows the incoming call within [`RING_WAIT`], clicks answer on the run's own call only (`ab-call`'s
    /// ring check: every invitation the page received is the run's call, but an earlier phase's of the same call, and
    /// the page shows one incoming call), and is in the call within [`CONNECT_WAIT`]. Meanwhile the native caller's
    /// output (`caller`, the call placed at `since`) is read every [`RING_LOOK`]: a call the service ended at once
    /// because the chat already has a running call ([`native_call_conflict`]) fails the phase then.
    async fn answer_on_page(
        &self,
        session: &mut LabSession,
        (caller, since): (&str, u64),
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let started = tokio::time::Instant::now();
        loop {
            let left = RING_WAIT.saturating_sub(started.elapsed());
            let shown = session
                .driver()
                .wait_for("b shows the incoming call", INCOMING_CALL, left.min(RING_LOOK))
                .await;
            let Err(error) = shown else { break };
            if let Some(conversation) = native_call_conflict(&self.native(caller)?.output()) {
                return Err(setup(format!(
                    "chat has a running call {conversation}: the service ended {caller}'s call at once (409, 5704)"
                )));
            }
            if started.elapsed() >= RING_WAIT {
                return Err(error.into());
            }
        }
        report
            .notes
            .push(format!("b: the incoming call showed at {}", now_ms()));
        check_ring(
            (Alias::B, &mut *session),
            ANSWER,
            (&self.call, &self.mri_b),
            since,
            EarlierRings::OwnLeftOut,
        )
        .await?;
        session
            .driver()
            .click("b clicks answer", Selector::Css(ACCEPT_AUDIO_CSS))
            .await?;
        report.notes.push(format!("b: clicked answer at {}", now_ms()));
        session
            .driver()
            .wait_for("b's call screen", HANG_UP, CONNECT_WAIT)
            .await?;
        report.notes.push(format!("b: in the call at {}", now_ms()));
        Ok(())
    }

    /// `teams-headless <alias> <args>` started as `what`, its output into `native-<what>.txt` in the run's dir.
    fn spawn(&mut self, alias: Alias, what: &'static str, args: &[String]) -> Result<(), ScenarioError> {
        let native = spawn_native((&self.binary, &self.dir), alias, what, args)?;
        self.natives.push(native);
        Ok(())
    }

    fn native(&self, what: &str) -> Result<&Native, ScenarioError> {
        self.natives
            .iter()
            .find(|native| native.what == what)
            .ok_or_else(|| setup(format!("no native client {what} runs")))
    }

    fn take(&mut self, what: &str) -> Result<Native, ScenarioError> {
        let at = self
            .natives
            .iter()
            .position(|native| native.what == what)
            .ok_or_else(|| setup(format!("no native client {what} runs")))?;
        Ok(self.natives.remove(at))
    }

    /// Waits up to `limit` until native client `what`'s output holds `needle`, the page's events flowing.
    async fn wait_output(
        &self,
        what: &str,
        session: &mut LabSession,
        needle: &str,
        limit: Duration,
    ) -> Result<(), ScenarioError> {
        let native = self.native(what)?;
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if native.output().contains(needle) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(setup(format!("{what}: no {needle} within {limit:?}")));
            }
            linger(session, Duration::from_millis(250)).await;
        }
    }

    /// Waits for native client `what`, which holds its call `hold`, to exit by itself while the page's events flow
    /// (killing it when it doesn't).
    async fn finish(
        &mut self,
        what: &str,
        hold: Duration,
        session: &mut LabSession,
        report: &mut ScenarioReport,
    ) -> Result<Finished, ScenarioError> {
        let mut native = self.take(what)?;
        let status = native.wait(session, hold + NATIVE_CONNECT + NATIVE_EXIT).await;
        Ok(native.finished(status, report))
    }

    /// Interrupts native client `what` (SIGINT, as a person's Ctrl-C) and waits for it to exit.
    async fn stop(
        &mut self,
        what: &str,
        session: &mut LabSession,
        report: &mut ScenarioReport,
    ) -> Result<Finished, ScenarioError> {
        let native = self.take(what)?;
        Ok(stop_native(native, session, report).await)
    }

    /// Every native client still running, interrupted and waited for (none is left running).
    async fn stop_natives(&mut self, session: &mut LabSession, report: &mut ScenarioReport) {
        for native in std::mem::take(&mut self.natives) {
            let finished = stop_native(native, session, report).await;
            report.notes.push(format!(
                "{}: stopped at the run's end ({:?})",
                finished.what, finished.status
            ));
        }
    }
}

/// `teams-headless <alias> <args>` (`binary`) started as `what`, its output into `native-<what>.txt` in `dir`.
pub(crate) fn spawn_native(
    (binary, dir): (&Path, &Path),
    alias: Alias,
    what: &'static str,
    args: &[String],
) -> Result<Native, ScenarioError> {
    let out = dir.join(format!("native-{what}.txt"));
    let file = fs::File::create(&out).map_err(|error| setup(format!("can't create {}: {error}", out.display())))?;
    let errors = file
        .try_clone()
        .map_err(|error| setup(format!("can't share {}: {error}", out.display())))?;
    let child = tokio::process::Command::new(binary)
        .arg(alias.as_str())
        .args(args)
        .env_remove("TEAMS_HEADLESS_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(errors))
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| setup(format!("can't start teams-headless for {what}: {error}")))?;
    Ok(Native { what, child, out })
}

/// Interrupts `native` and waits up to [`NATIVE_EXIT`] for it to exit (killing it when it doesn't).
pub(crate) async fn stop_native(mut native: Native, session: &mut LabSession, report: &mut ScenarioReport) -> Finished {
    native.interrupt();
    let status = native.wait(session, NATIVE_EXIT).await;
    native.finished(status, report)
}

/// `call b` with the run's tone, held `hold` once connected; with `video`, its camera on as the run's are.
fn call_args(hold: Duration, video: bool) -> Vec<String> {
    let mut args = vec!["call".to_owned(), "b".to_owned()];
    args.extend(tone_args());
    if video {
        args.extend(video_args());
    }
    args.extend([
        "--seconds".to_owned(),
        hold.as_secs().to_string(),
        "--wait".to_owned(),
        NATIVE_CONNECT.as_secs().to_string(),
    ]);
    args
}

/// The tone arguments every native client takes: the page's tone, in bursts, and the same tone looked for.
pub(crate) fn tone_args() -> Vec<String> {
    let hz = TONE_HZ.to_string();
    vec![
        "--tone".to_owned(),
        hz.clone(),
        "--bursts".to_owned(),
        "--hear".to_owned(),
        hz,
    ]
}

/// `watch <flag>` with the run's tone; with `video`, the answered call's camera on as the run's are.
fn watch_args(flag: &str, video: bool) -> Vec<String> {
    let mut args = vec!["watch".to_owned(), flag.to_owned()];
    args.extend(tone_args());
    if video {
        args.extend(video_args());
    }
    args
}

/// The video arguments of a native client whose call carries video: Cisco's OpenH264, the camera on
/// [`NATIVE_CAMERA_AFTER`] after the call connected.
pub(crate) fn video_args() -> Vec<String> {
    vec![
        "--video".to_owned(),
        "--camera-after".to_owned(),
        NATIVE_CAMERA_AFTER.as_secs().to_string(),
    ]
}

/// One native client the run spawned.
pub(crate) struct Native {
    what: &'static str,
    child: tokio::process::Child,
    out: PathBuf,
}

/// A native client that exited (or was killed), with its output.
pub(crate) struct Finished {
    pub(crate) what: &'static str,
    pub(crate) status: Option<ExitStatus>,
    pub(crate) output: String,
}

impl Native {
    pub(crate) fn output(&self) -> String {
        fs::read_to_string(&self.out).unwrap_or_default()
    }

    pub(crate) fn interrupt(&self) {
        let pid = self
            .child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw);
        if let Some(pid) = pid {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::INT);
        }
    }

    /// Waits up to `limit` for it to exit, the page's events flowing; kills it when it doesn't. `None` when it had to
    /// be killed.
    pub(crate) async fn wait(&mut self, session: &mut LabSession, limit: Duration) -> Option<ExitStatus> {
        let deadline = tokio::time::Instant::now() + limit;
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            linger(session, Duration::from_millis(500)).await;
        }
        let _ = self.child.kill().await;
        None
    }

    pub(crate) fn finished(self, status: Option<ExitStatus>, report: &mut ScenarioReport) -> Finished {
        let output = self.output();
        for line in output.lines().filter(|line| is_summary(line)) {
            report.notes.push(format!("{}: {line}", self.what));
        }
        report.notes.push(format!(
            "{}: exited {:?} (output in {})",
            self.what,
            status.map(|status| status.code()),
            self.out.display()
        ));
        Finished {
            what: self.what,
            status,
            output,
        }
    }
}

/// The lines of a native client's output a run notes: its call's summary, what its recording holds, its devices, its
/// answer and its ring's end.
#[must_use]
pub fn is_summary(line: &str) -> bool {
    [
        "call_summary",
        "tone hz=",
        "echo heard=",
        "devices ",
        "call_answer",
        "ring_ended",
        "camera_",
        "video_codec",
        "video received=",
    ]
    .iter()
    .any(|kind| line.contains(kind))
}

/// Whether a native client's output says it heard the tone at [`TONE_HZ`] in its recording.
#[must_use]
pub fn native_heard(output: &str) -> bool {
    let heard = format!("tone hz={TONE_HZ} heard=true");
    output.lines().any(|line| line.starts_with(&heard))
}

/// Whether a native client's output says its ring ended because the call was answered on another endpoint: a
/// `ring_ended` line naming [`ANSWERED_ELSEWHERE`] (a cancel or a timeout names another phrase).
#[must_use]
pub fn ring_ended_elsewhere(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.contains(" ring_ended ") && line.contains(ANSWERED_ELSEWHERE))
}

/// The frames a native client's output says it received of the far end's video (its `video received=<n>` line);
/// `None` without the line.
#[must_use]
pub fn native_video_frames(output: &str) -> Option<u64> {
    output
        .lines()
        .find_map(|line| line.strip_prefix("video received="))
        .and_then(|rest| rest.split(' ').next())
        .and_then(|frames| frames.parse().ok())
}

/// A native client whose call carried video must have turned its camera on and shown at least [`NATIVE_FRAMES`] of
/// the page's video.
fn judge_native_video(native: &Finished, report: &mut ScenarioReport) {
    if !native.output.contains("camera_on call=") {
        report
            .failures
            .push(format!("the native client ({}) didn't turn its camera on", native.what));
    }
    match native_video_frames(&native.output) {
        Some(frames) if frames >= NATIVE_FRAMES => {}
        frames => report.failures.push(format!(
            "the native client ({}) showed {frames:?} of the page's video frames, not {NATIVE_FRAMES}",
            native.what
        )),
    }
}

/// A native client must have exited 0 and heard the page's tone.
pub(crate) fn judge_native(native: &Finished, report: &mut ScenarioReport) {
    if native.status.is_none_or(|status| !status.success()) {
        report.failures.push(format!(
            "the native client ({}) didn't exit 0: {:?}",
            native.what, native.status
        ));
    }
    if !native_heard(&native.output) {
        report.failures.push(format!(
            "the native client ({}) didn't hear b's tone in its recording",
            native.what
        ));
    }
}

/// b's page in a call opens its microphone (noise suppression off, another microphone), must hear the far end's tone
/// within [`TONE_WAIT`], and its call audio must go both ways (the capture from byte `from` on).
pub(crate) async fn hear_on_page(
    session: &mut LabSession,
    from: u64,
    report: &mut ScenarioReport,
) -> Result<(), ScenarioError> {
    turn_off_noise_suppression(Alias::B, &mut session.driver(), report).await?;
    switch_microphone(Alias::B, &mut session.driver(), report).await?;
    let Some(file) = session.capture_dir().map(|dir| dir.join(EVENTS_FILE)) else {
        return Err(setup("the run records no capture to hear the tone in".to_owned()));
    };
    let heard = wait_for_tone_in_from(&file, from, TONE_WAIT, async |poll| linger(session, poll).await).await;
    match heard {
        ToneWait::Back(after) => report
            .notes
            .push(format!("b: heard a's native client's tone {after:?} into the wait")),
        other => report
            .failures
            .push(format!("b didn't hear a's native client's tone ({other:?})")),
    }
    let audio = AudioSummary::from_events(&read_from(&file, from));
    report.notes.push(format!(
        "b: audio {} packet(s) sent, {} received, energy {:.4}",
        audio.packets_sent, audio.packets_received, audio.received_energy
    ));
    if !audio.both_ways() {
        report
            .failures
            .push("b's call audio didn't go both ways (WebRTC statistics)".to_owned());
    }
    Ok(())
}

/// b's page turns its camera on (Chrome's fake camera) and must decode at least [`PAGE_FRAMES`] of the native client's
/// video (its camera on [`NATIVE_CAMERA_AFTER`] after the call connected) within [`VIDEO_WAIT`] (the capture from byte
/// `from` on).
pub(crate) async fn see_on_page(
    session: &mut LabSession,
    from: u64,
    report: &mut ScenarioReport,
) -> Result<(), ScenarioError> {
    session.driver().click("b turns its camera on", CAMERA).await?;
    report.notes.push(format!("b: camera on at {}", now_ms()));
    let Some(file) = session.capture_dir().map(|dir| dir.join(EVENTS_FILE)) else {
        return Err(setup("the run records no capture to read the video in".to_owned()));
    };
    let deadline = tokio::time::Instant::now() + VIDEO_WAIT;
    loop {
        let frames = video_frames(&read_from(&file, from));
        if frames >= PAGE_FRAMES {
            report.notes.push(format!(
                "b: decoded {frames} video frame(s) of the native client's by {}",
                now_ms()
            ));
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            report.failures.push(format!(
                "b decoded {frames} of the native client's video frames within {VIDEO_WAIT:?}, not {PAGE_FRAMES}"
            ));
            return Ok(());
        }
        linger(session, Duration::from_secs(1)).await;
    }
}

/// Waits up to [`SCREEN_GONE`] for b's call screen to go (the far end hung up), noting when it went or that it
/// stayed (then hangs it up).
async fn wait_screen_gone(session: &mut LabSession, report: &mut ScenarioReport) {
    let gone = session
        .driver()
        .wait_until_true(
            "b's call screen goes",
            &format!("document.querySelector({}) === null", js_string(HANG_UP_CSS)),
            "the call screen to go",
            SCREEN_GONE,
        )
        .await;
    match gone {
        Ok(()) => report.notes.push(format!("b: the call screen went by {}", now_ms())),
        Err(error) => {
            report.notes.push(format!("b: the call screen stayed: {error}"));
            let _ = hang_up_page(Alias::B, session, report).await;
        }
    }
}

/// The capture's length so far, in bytes: where a phase's own events begin.
pub(crate) fn capture_len(session: &LabSession) -> u64 {
    session
        .capture_dir()
        .and_then(|dir| fs::metadata(dir.join(EVENTS_FILE)).ok())
        .map_or(0, |metadata| metadata.len())
}

/// `file` from byte `from` on, as text.
pub(crate) fn read_from(file: &Path, from: u64) -> String {
    let bytes = fs::read(file).unwrap_or_default();
    let from = usize::try_from(from).unwrap_or(usize::MAX).min(bytes.len());
    String::from_utf8_lossy(&bytes[from..]).into_owned()
}

/// The time now in Unix milliseconds, as a capture stamps its entries (`ts_ms`).
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_millis()).ok())
        .unwrap_or(u64::MAX)
}
