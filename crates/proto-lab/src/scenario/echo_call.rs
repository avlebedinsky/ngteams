//! The `echo-call` scenario: account a places the web client's test call to Microsoft's echo service with Chrome's fake
//! microphone playing a tone, stays in the call a while, and hangs up. The run is recorded (`captures/echo-call/<ts>/`):
//! the call's HTTP and WebSocket signaling, the fence's verdicts and the WebRTC hooks' reports (SDP, ICE, `getStats`)
//! are the material of `docs/protocol/calling-echo.md`.
//!
//! The echo service is the one participant outside the lab the user approved (2026-10-19), by its MRI in `lab.toml`
//! (`[[service_participants]] name = "echo"`). The run uses the calling classifiers ([`Classifier::calling`],
//! [`FrameClassifier::calling`]): the write mapping, the own-state the calling stack needs for the account's own
//! endpoint, and the call start, which the fence judges as a new call of a and the echo service
//! ([`Classifier::with_test_call_callee`]). With no echo MRI in `lab.toml` the call start is unknown and denied.
//!
//! The steps:
//! 1. a opens the web client and must load;
//! 2. a opens the Calls app, which starts the client's calling stack (the client defers it until then), then Settings →
//!    Devices from the account menu, in the same document ([`stand_up_calling`]);
//! 3. a clicks "Make a test call" ([`TEST_CALL_BUTTON`]); the call screen must show its hang-up button ([`HANG_UP`])
//!    within [`CALL_START`], and the fence must have allowed the call start ([`call_starts`]);
//! 4. the run stays in the call for [`IN_CALL`], hangs up (if the call is still up) and records [`AFTER_HANG_UP`] more;
//! 5. after the run, the capture's WebRTC statistics must show audio sent and received ([`AudioSummary`]).
//!
//! Live on 2026-10-20 steps 1–4 pass; step 5 doesn't: the call's later requests (the echo service's `add` to the
//! conversation, the call's acknowledgements, `leave`) land on the call, whose members the fence can't read yet, so they
//! stay unknown and are denied; the leg reaches Teams' media controller, the echo service never joins
//! (`docs/protocol/calling-echo.md`).
//!
//! The scenario never answers a call: nothing in it accepts one, and the fence keeps an answer unmapped (`Unknown`,
//! denied). A failed run leaves the page's screenshot as `failure.png` in its capture.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use teams_fence::Mri;

use crate::browser::Media;
use crate::capture::EVENTS_FILE;
use crate::classify::Classifier;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_ws::FrameClassifier;
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{load, save_screenshot};
use super::dsl::{Driver, Selector, StepError};
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport, TELEMETRY_HOSTS};

/// The account that places the call.
pub const ACCOUNT: Alias = Alias::A;

/// The name of the echo service in `lab.toml`'s `[[service_participants]]`.
pub const ECHO_SERVICE: &str = "echo";

/// The Calls app's button in the app bar (its `data-tid` is the Calls app's id).
pub const CALLS_APP: Selector<'static> = Selector::Tid("20c3440d-c67e-4420-9f80-0e50c39693df");

/// The "Settings and more" button in the title bar, and its Settings item.
pub const SETTINGS_MENU: Selector<'static> = Selector::Tid("more-options-header");
pub const SETTINGS_ITEM: Selector<'static> = Selector::Tid("settings-button-menu");

/// The "Make a test call" button on the Devices page.
pub const TEST_CALL_BUTTON: Selector<'static> = Selector::Css(TEST_CALL_BUTTON_CSS);
const TEST_CALL_BUTTON_CSS: &str = r#"[data-testid="start-echo-bot-call-component"] button"#;
const TEST_CALL_BUTTON_QUERY: &str =
    r#"document.querySelector('[data-testid="start-echo-bot-call-component"] button')"#;

/// The call screen's hang-up button.
pub const HANG_UP: Selector<'static> = Selector::Css(HANG_UP_CSS);
const HANG_UP_CSS: &str = r#"#hangup-button, [data-tid="hangup-main-btn"]"#;

/// The call start's URL, as the fence's verdicts name it (`crate::calling::call_start`).
pub const CALL_START_URL: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/epconv";

/// How long the app bar, and later the Devices page, get to show up.
pub const PAGE_OPEN: Duration = Duration::from_secs(75);
/// How long the app bar has to stay put before the run uses it: the client may load once more on its way to the origin
/// it assigns the account.
pub const PAGE_STEADY: Duration = Duration::from_secs(8);
/// How long the Calls app gets to start the calling stack.
pub const CALLS_APP_WARM: Duration = Duration::from_secs(15);
/// How long a control named by its text gets to show up.
pub const CONTROL_WAIT: Duration = Duration::from_secs(20);
/// How long the call gets to show its call screen.
pub const CALL_START: Duration = Duration::from_secs(60);
/// How long the run stays in the call before it hangs up.
pub const IN_CALL: Duration = Duration::from_secs(20);
/// How long the run keeps recording after it hung up: the call's end signaling.
pub const AFTER_HANG_UP: Duration = Duration::from_secs(5);

/// The tone the fake microphone plays: its frequency, length (Chrome loops the file) and sample rate.
pub const TONE_HZ: u32 = 440;
pub const TONE_SECONDS: u32 = 10;
pub const TONE_RATE: u32 = 48_000;

/// The browser's `User-Agent`: Chrome on Linux as a person runs it, instead of headless Chrome's own `HeadlessChrome`,
/// so the client sees the browser it supports. (Set while the calling stack wouldn't start; the cause turned out to be
/// the deferred start of step 2, and whether `HeadlessChrome` alone stops calls wasn't isolated.)
pub const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36";

/// The denies a run of this scenario expects: telemetry only, as in `ab-chat`.
pub const POLICY: DenyPolicy = DenyPolicy::ForcedAnd(&TELEMETRY_HOSTS);

/// A mono 16-bit PCM WAV file of a sine tone at `frequency` Hz, `seconds` long at `rate` samples per second, at half
/// of full scale.
#[must_use]
pub fn tone_wav(frequency: u32, seconds: u32, rate: u32) -> Vec<u8> {
    let samples = rate.saturating_mul(seconds);
    let data_len = samples.saturating_mul(2);
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&data_len.saturating_add(36).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1_u16.to_le_bytes()); // mono
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&rate.saturating_mul(2).to_le_bytes()); // bytes per second
    wav.extend_from_slice(&2_u16.to_le_bytes()); // bytes per frame
    wav.extend_from_slice(&16_u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    let step = std::f64::consts::TAU * f64::from(frequency) / f64::from(rate);
    for index in 0..samples {
        let value = (f64::from(index) * step).sin() * f64::from(i16::MAX) / 2.0;
        #[expect(clippy::cast_possible_truncation, reason = "the value is within ±i16::MAX / 2")]
        let sample = value.round() as i16;
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

/// The echo service's MRI in `config`, if `lab.toml` records one.
#[must_use]
pub fn echo_mri(config: &LabConfig) -> Option<&str> {
    config
        .service_participants()
        .iter()
        .find(|service| service.name == ECHO_SERVICE)
        .and_then(|service| service.mri.as_deref())
}

/// Writes the tone ([`tone_wav`]) as `media/tone-<Hz>.wav` (mode `0600`, its dir `0700`) under the lab's data dir, for
/// Chrome's fake microphone, and gives its path.
///
/// # Errors
/// When the file can't be written.
pub fn write_tone(paths: &LabPaths) -> std::io::Result<PathBuf> {
    let dir = paths.data_dir().join("media");
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let path = dir.join(format!("tone-{TONE_HZ}.wav"));
    let temporary = dir.join(format!("tone-{TONE_HZ}.wav.{}.new", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&tone_wav(TONE_HZ, TONE_SECONDS, TONE_RATE))?;
    file.sync_all()?;
    fs::rename(&temporary, &path)?;
    Ok(path)
}

/// What a capture's WebRTC statistics say about the call's audio: the largest counters any connection reported.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioSummary {
    /// The connections the hooks saw.
    pub connections: usize,
    /// Audio RTP packets sent (`outbound-rtp`, kind `audio`).
    pub packets_sent: u64,
    /// Audio RTP packets received (`inbound-rtp`, kind `audio`).
    pub packets_received: u64,
    /// The received audio's energy (`totalAudioEnergy`): above zero once anything but silence came back.
    pub received_energy: f64,
    /// The loudest received audio level seen (`audioLevel`, 0 to 1).
    pub max_received_level: f64,
    /// Whether any connection reported `connected`.
    pub connected: bool,
}

impl AudioSummary {
    /// Reads the `rtc` entries of a capture's `events.jsonl` text. Lines that aren't JSON or aren't `rtc` are skipped.
    #[must_use]
    pub fn from_events(text: &str) -> Self {
        let mut summary = Self::default();
        let mut connections = std::collections::BTreeSet::new();
        for line in text.lines() {
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if entry["type"] != "rtc" {
                continue;
            }
            let payload = &entry["payload"];
            if let Some(pc) = payload["pc"].as_u64() {
                connections.insert((entry["context"].to_string(), entry["session"].to_string(), pc));
            }
            match payload["event"].as_str() {
                Some("connectionstatechange") if payload["data"] == "connected" => summary.connected = true,
                Some("stats") => summary.add_stats(&payload["data"]),
                _ => {}
            }
        }
        summary.connections = connections.len();
        summary
    }

    fn add_stats(&mut self, stats: &Value) {
        for entry in stats.as_array().into_iter().flatten() {
            if entry["kind"] != "audio" {
                continue;
            }
            match entry["type"].as_str() {
                Some("outbound-rtp") => {
                    self.packets_sent = self.packets_sent.max(entry["packetsSent"].as_u64().unwrap_or(0));
                }
                Some("inbound-rtp") => {
                    self.packets_received = self
                        .packets_received
                        .max(entry["packetsReceived"].as_u64().unwrap_or(0));
                    self.received_energy = self
                        .received_energy
                        .max(entry["totalAudioEnergy"].as_f64().unwrap_or(0.0));
                    self.max_received_level = self.max_received_level.max(entry["audioLevel"].as_f64().unwrap_or(0.0));
                }
                _ => {}
            }
        }
    }

    /// Whether audio went both ways: packets sent, and packets with sound in them received.
    #[must_use]
    pub fn both_ways(&self) -> bool {
        self.packets_sent > 0 && self.packets_received > 0 && self.received_energy > 0.0
    }
}

/// How many call starts the fence allowed and denied in a capture's `events.jsonl` text: its `http` verdicts on `POST`
/// [`CALL_START_URL`], one per request (a request pauses on its target's session and the browser's, and both are
/// recorded; they share the `Network` id).
#[must_use]
pub fn call_starts(text: &str) -> (usize, usize) {
    let mut allowed = std::collections::BTreeSet::new();
    let mut denied = std::collections::BTreeSet::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry["type"] != "verdict"
            || entry["channel"] != "http"
            || entry["method"] != "POST"
            || entry["url"] != CALL_START_URL
        {
            continue;
        }
        let id = entry["network_id"].to_string();
        match entry["verdict"].as_str() {
            Some("allow") => allowed.insert(id),
            Some("deny") => denied.insert(id),
            _ => false,
        };
    }
    (allowed.len(), denied.len())
}

/// Runs `echo-call` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, no Chrome, no fence, a browser that can't be armed, no tone file.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let mut report = ScenarioReport::new(ScenarioName::EchoCall);
    let classifier = if let Some(echo) = echo_mri(&config) {
        Classifier::calling().with_test_call_callee(Mri::new(echo))
    } else {
        report.notes.push(format!(
            "lab.toml has no MRI for the service participant {ECHO_SERVICE:?}: the fence will deny the call"
        ));
        Classifier::calling()
    };
    let tone = write_tone(paths).map_err(|error| ScenarioError::Setup(format!("can't write the tone: {error}")))?;
    let session_options = SessionOptions {
        classifier,
        frames: FrameClassifier::calling(),
        live_members: true,
        capture: Some(ScenarioName::EchoCall.as_str().to_owned()),
        display: options.display,
        media: Media::Fake { audio: Some(tone) },
        user_agent: Some(USER_AGENT.to_owned()),
    };
    let mut session = LabSession::open_with(paths, ACCOUNT, session_options).await?;
    if let Err(error) = call(&mut session, options, &mut report).await {
        report.failures.push(error.to_string());
        if let Some(note) = save_screenshot(ACCOUNT, &session).await {
            report.notes.push(note);
        }
    }
    let capture = session.capture_dir().map(Path::to_path_buf);
    // the browser goes down and the capture is finished, whatever happened
    report.close_account(ACCOUNT, session, POLICY).await;
    if let Some(dir) = capture {
        judge_capture(&dir, &mut report);
        report.notes.push(format!("recorded into {}", dir.display()));
    }
    Ok(report)
}

/// Steps 1–4: load, start the calling stack and open Devices, place the test call, stay, hang up.
async fn call(session: &mut LabSession, options: RunOptions, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    load(ACCOUNT, session, options.load_timeout).await?;
    report.notes.push(format!("{ACCOUNT}: the web client loaded"));
    // a window the client lays out in full: the account menu and Settings' pages
    session
        .cdp()
        .call(
            Some(session.page()),
            "Emulation.setDeviceMetricsOverride",
            serde_json::json!({"width": 1440, "height": 900, "deviceScaleFactor": 1, "mobile": false}),
        )
        .await
        .map_err(|error| ScenarioError::Setup(format!("can't size the page: {error}")))?;
    let mut driver = session.driver();
    stand_up_calling(&mut driver, report).await?;
    driver.click("a makes a test call", TEST_CALL_BUTTON).await?;
    driver.wait_for("the call screen", HANG_UP, CALL_START).await?;
    report.notes.push(format!("{ACCOUNT}: the call screen is up"));
    linger(session, IN_CALL).await;
    let still_up = driver
        .evaluate(
            "the call screen",
            &format!("document.querySelector({}) !== null", js(HANG_UP_CSS)),
        )
        .await?;
    if still_up == Value::Bool(true) {
        driver.click("a hangs up", HANG_UP).await?;
        report.notes.push(format!("{ACCOUNT}: hung up after {IN_CALL:?}"));
    } else {
        report.notes.push(format!(
            "{ACCOUNT}: the call screen was gone before {IN_CALL:?}: nothing to hang up"
        ));
    }
    linger(session, AFTER_HANG_UP).await;
    Ok(())
}

/// Starts the web client's calling stack, then opens Settings → Devices in the same document. The client starts its
/// calling stack only when the Calls app comes up (or on a pre-warm it may not get to in a lab run); a test call asked
/// for before that waits for it forever (`Deferring calling stack Initialization. [entryPoint:startEchoBotCall]` in the
/// client's own log, 2026-10-20). Settings' deep link reloads the page, which loses the stack again, so Settings is
/// opened from the account menu.
async fn stand_up_calling(driver: &mut Driver, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    // the client may load once more on its way to the origin it assigns the account: the app bar has to stay
    driver.wait_for("the Calls app", CALLS_APP, PAGE_OPEN).await?;
    tokio::time::sleep(PAGE_STEADY).await;
    driver.wait_for("the Calls app", CALLS_APP, PAGE_OPEN).await?;
    let document = driver.evaluate("the web client", "performance.timeOrigin").await?;
    driver.click("a opens the Calls app", CALLS_APP).await?;
    tokio::time::sleep(CALLS_APP_WARM).await;
    driver.click("a opens the account menu", SETTINGS_MENU).await?;
    driver
        .wait_for("the Settings item", SETTINGS_ITEM, CONTROL_WAIT)
        .await?;
    driver.click("a opens Settings", SETTINGS_ITEM).await?;
    click_text(driver, "a opens Devices", "Devices").await?;
    let same = format!("performance.timeOrigin === {document} && {TEST_CALL_BUTTON_QUERY} !== null");
    driver
        .wait_until_true(
            "the Devices page",
            &same,
            "the test call button in the same document",
            PAGE_OPEN,
        )
        .await?;
    report.notes.push(format!(
        "{ACCOUNT}: the Calls app started the calling stack; Settings → Devices is open"
    ));
    Ok(())
}

/// Clicks the first control (a menu item, tab, tree item, option, link or button) whose text's first line is exactly
/// `text`, once there is one.
async fn click_text(driver: &mut Driver, step: &str, text: &str) -> Result<(), ScenarioError> {
    let find = format!(
        "[...document.querySelectorAll('[role=menuitem],[role=tab],[role=treeitem],[role=option],[role=link],a,button')]\
         .find(e => (e.innerText ?? '').trim().split('\\n')[0].trim() === {})",
        js(text)
    );
    driver
        .wait_until_true(
            step,
            &format!("{find} !== undefined"),
            &format!("a control named {text:?}"),
            CONTROL_WAIT,
        )
        .await?;
    let clicked = driver
        .evaluate(
            step,
            &format!("(() => {{ const e = {find}; if (!e) return false; e.click(); return true; }})()"),
        )
        .await?;
    if clicked == Value::Bool(true) {
        Ok(())
    } else {
        Err(StepError::Failed {
            step: step.to_owned(),
            reason: format!("no control named {text:?} to click"),
        }
        .into())
    }
}

/// A JavaScript string literal of `text`.
fn js(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

/// Steps 3 and 5 from the capture: the fence must have allowed a call start, and the WebRTC statistics must show audio
/// both ways.
fn judge_capture(capture: &Path, report: &mut ScenarioReport) {
    let text = match fs::read_to_string(capture.join(EVENTS_FILE)) {
        Ok(text) => text,
        Err(error) => {
            report.failures.push(format!("can't read the capture: {error}"));
            return;
        }
    };
    let (allowed, denied) = call_starts(&text);
    report.notes.push(format!(
        "call start: {allowed} allowed and {denied} denied by the fence"
    ));
    if allowed == 0 {
        report.failures.push("the fence allowed no call start".to_owned());
    }
    let audio = AudioSummary::from_events(&text);
    report.notes.push(format!(
        "audio: {} connection(s), connected {}, {} packets sent, {} received, energy {:.4}, peak level {:.3}",
        audio.connections,
        audio.connected,
        audio.packets_sent,
        audio.packets_received,
        audio.received_energy,
        audio.max_received_level
    ));
    if !audio.both_ways() {
        report
            .failures
            .push("the call's audio didn't go both ways (WebRTC statistics in the capture)".to_owned());
    }
}

/// Keeps the session's events flowing (and recorded) for `duration`, or until the connection goes.
async fn linger(session: &mut LabSession, duration: Duration) {
    let end = tokio::time::Instant::now() + duration;
    while let Some(left) = end.checked_duration_since(tokio::time::Instant::now()) {
        if left.is_zero() || !session.pump(left.min(Duration::from_millis(500))).await {
            return;
        }
    }
}
