//! The `echo-call` scenario: account a places the web client's test call to Microsoft's echo service with Chrome's fake
//! microphone playing a tone, waits for the echo to play the tone back, and hangs up. The run is recorded
//! (`captures/echo-call/<ts>/`): the call's HTTP and WebSocket signaling, the fence's verdicts and the WebRTC hooks'
//! reports (SDP, ICE, `getStats`, the audio tap's readings of the received audio) are the material of
//! `docs/protocol/calling-echo.md`.
//!
//! The echo service is the one participant outside the lab the user approved (2026-10-19), by its MRI in `lab.toml`
//! (`[[service_participants]] name = "echo"`). The run uses the lab rule, under which what sets up the account's own
//! endpoint reaches nobody and passes, with the echo service's MRI ([`Classifier::with_test_call_callee`]): the call
//! start is a write the fence judges as a new call of a and the echo service. With no echo MRI in `lab.toml` the call
//! start is unknown and denied. A permitted start registers the call in the fence's call registry, and the call's
//! follow-ups (the echo's `add`, the endpoint metadata, the channel parameters, the acceptance's acknowledgement over
//! Trouter, `leave`) pass only on it.
//!
//! The browser plays nothing aloud: it is muted with no audio device ([`crate::browser::BASE_ARGS`]), and the received
//! audio is read in the page by the hooks' audio tap (`assets/rtc_hooks.js`).
//!
//! The steps:
//! 1. before the page loads, a's fence force-denies every write carrying `ab-chat`'s probe marker
//!    ([`forced_rules`]): the web client keeps a send that failed and sends it again on every later load, an earlier
//!    `ab-chat` run's probe or `abc-chat`'s first message among them, and the members' live read (on here for the call)
//!    would let the resend out to b, or b and c. a opens the web client and must load;
//! 2. a opens the Calls app, which starts the client's calling stack (the client defers it until then), then Settings →
//!    Devices from the account menu, in the same document ([`stand_up_calling`]);
//! 3. a clicks "Make a test call" ([`TEST_CALL_BUTTON`]); the call screen must show its hang-up button ([`HANG_UP`])
//!    within [`CALL_START`];
//! 4. a turns the call's noise suppression off ([`turn_off_noise_suppression`]): the client takes the steady tone for
//!    noise and would send silence;
//! 5. the run waits until the audio tap has read the tone coming back ([`AudioSummary::tone_back`]), for up to
//!    [`ECHO_WAIT`] or until the browser connection goes ([`wait_for_tone_in`]), stays [`AFTER_TONE`] more, hangs up
//!    (if the call is still up) and records [`AFTER_HANG_UP`] more;
//! 6. after the run, the fence must have allowed the call start ([`call_starts`]), the `add` and the `leave` and
//!    acknowledged the acceptance ([`CallFollowUps`]), the WebRTC statistics must show audio both ways and the tap the
//!    tone back ([`AudioSummary`]), and no deny but the reviewed expected-deny list's and step 1's ([`policy`]).
//!
//! The scenario never answers a call: nothing in it accepts one, and the fence keeps an answer unmapped (unknown,
//! denied). A failed run leaves the page's screenshot as `failure.png` in its capture.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use teams_fence::Mri;

use crate::browser::Media;
use crate::capture::{EVENTS_FILE, TAP_TONE_HZ};
use crate::classify::Classifier;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_http::ForcedDeny;
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{EARLIER_RUN_PROBE, load, probe_rules, save_screenshot};
use super::dsl::{Driver, Selector, StepError, js_string};
use super::expected::Review;
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

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

/// The call screen's audio options button, and the noise suppression switch in them.
pub const AUDIO_OPTIONS: Selector<'static> = Selector::Css("#audio-button-configure");
pub const NOISE_SUPPRESSION: Selector<'static> = Selector::Css(NOISE_SUPPRESSION_CSS);
const NOISE_SUPPRESSION_CSS: &str = r#"[data-tid="background-suppression-switch"]"#;

/// The call start's URL, as the fence's verdicts name it: the one `teams_fence::contacting` maps.
pub use teams_fence::CALL_START_URL;

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
/// How long the run waits in the call for the echo to play the tone back before it hangs up anyway: the echo service
/// greets, records a few seconds and plays them back.
pub const ECHO_WAIT: Duration = Duration::from_secs(90);
/// How often the run looks for the tone in the capture while it waits.
pub const TONE_POLL: Duration = Duration::from_secs(1);
/// How long the run stays in the call once the tone came back.
pub const AFTER_TONE: Duration = Duration::from_secs(3);
/// How long the run keeps recording after it hung up: the call's end signaling.
pub const AFTER_HANG_UP: Duration = Duration::from_secs(5);

/// The tone the fake microphone plays: its frequency (the one the hooks' audio tap measures), length (Chrome loops the
/// file) and sample rate.
pub const TONE_HZ: u32 = TAP_TONE_HZ;
pub const TONE_SECONDS: u32 = 10;
pub const TONE_RATE: u32 = 48_000;

/// An audio tap reading is the tone when at least this share of its power is at [`TONE_HZ`]...
pub const TONE_SHARE: f64 = 0.5;
/// ...and its RMS level is at least this (about -46 dBFS: not the residue of silence).
pub const MIN_TONE_RMS: f64 = 0.005;
/// The tone came back once this many tap readings of one received track in a row are the tone (one second at the
/// tap's interval).
pub const TONE_READINGS: usize = 4;

/// The call's follow-ups a run must see permitted on its registered call (the last segment of the link each goes
/// to): the echo service's `add` and the `leave` on hanging up. The acceptance is acknowledged too, by one of two
/// ways ([`CallFollowUps::acknowledged`]).
pub const REQUIRED_FOLLOW_UPS: [&str; 2] = ["add", "leave"];

/// The browser's `User-Agent`: Chrome on Linux as a person runs it, instead of headless Chrome's own `HeadlessChrome`,
/// so the client sees the browser it supports. (Set while the calling stack wouldn't start; the cause turned out to be
/// the deferred start of step 2, and whether `HeadlessChrome` alone stops calls wasn't isolated.)
pub const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36";

/// The forced denies a run of this scenario expects ([`forced_rules`]): it has no probe of its own, so only an earlier
/// run's probe, `abc-chat`'s first message among them.
pub const EXPECTED_FORCED: [&str; 1] = [EARLIER_RUN_PROBE];

/// The rules a run forces on a's fence before its page loads: `ab-chat`'s rules for an earlier run's probe
/// ([`probe_rules`] without a probe of this run's), which deny `abc-chat`'s first message too, labelled
/// [`EARLIER_RUN_PROBE`].
///
/// # Errors
/// Never: the patterns are fixed.
pub fn forced_rules() -> Result<Vec<ForcedDeny>, regex::Error> {
    probe_rules(None)
}

/// The denies a run of this scenario expects: the reviewed list ([`super::expected`]) and the forced denies of
/// [`EXPECTED_FORCED`]. A deny on the call (its follow-ups, the echo service's `add`, `leave`) is unexpected: the call
/// needs them, and they pass on its registered call.
#[must_use]
pub fn policy() -> DenyPolicy {
    DenyPolicy::Reviewed(Review::new().expecting_forced(&EXPECTED_FORCED))
}

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

/// What a capture's WebRTC reports say about the call's audio: the largest counters any connection's statistics
/// reported, and what the audio tap read on the received tracks.
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
    /// The audio tap's readings of received tracks (`audio`).
    pub tap_readings: usize,
    /// The loudest received RMS level the tap read.
    pub max_tap_rms: f64,
    /// The largest share of a reading's power at [`TONE_HZ`], among readings at least [`MIN_TONE_RMS`] loud.
    pub max_tone_share: f64,
    /// The most readings of one received track in a row that were the tone ([`TONE_SHARE`], [`MIN_TONE_RMS`]).
    pub longest_tone: usize,
}

impl AudioSummary {
    /// Reads the `rtc` entries of a capture's `events.jsonl` text. Lines that aren't JSON or aren't `rtc` are skipped.
    #[must_use]
    pub fn from_events(text: &str) -> Self {
        let mut tally = AudioTally::default();
        text.lines().for_each(|line| tally.add_line(line));
        tally.summary()
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

    /// Whether the tone came back: [`TONE_READINGS`] tap readings of one received track in a row were the tone.
    #[must_use]
    pub fn tone_back(&self) -> bool {
        self.longest_tone >= TONE_READINGS
    }
}

/// A received track as the tap names it: its context, session, connection and id.
type TrackKey = (String, String, u64, String);

/// An [`AudioSummary`] read one line at a time, as a capture grows.
#[derive(Debug, Default)]
pub struct AudioTally {
    summary: AudioSummary,
    connections: std::collections::BTreeSet<(String, String, u64)>,
    /// Per received track: the tone readings in a row so far.
    runs: std::collections::BTreeMap<TrackKey, usize>,
}

impl AudioTally {
    /// Takes one line of `events.jsonl`; one that isn't JSON or isn't `rtc` is skipped.
    pub fn add_line(&mut self, line: &str) {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if entry["type"] != "rtc" {
            return;
        }
        let payload = &entry["payload"];
        let (context, session) = (entry["context"].to_string(), entry["session"].to_string());
        let pc = payload["pc"].as_u64();
        if let Some(pc) = pc {
            self.connections.insert((context.clone(), session.clone(), pc));
        }
        match payload["event"].as_str() {
            Some("connectionstatechange") if payload["data"] == "connected" => self.summary.connected = true,
            Some("stats") => self.summary.add_stats(&payload["data"]),
            Some("audio") => {
                let data = &payload["data"];
                let track = (context, session, pc.unwrap_or_default(), data["track"].to_string());
                let rms = data["rms"].as_f64().unwrap_or(0.0);
                self.add_reading(track, rms, data["tone"].as_f64().unwrap_or(0.0));
            }
            _ => {}
        }
    }

    fn add_reading(&mut self, track: TrackKey, rms: f64, share: f64) {
        let summary = &mut self.summary;
        summary.tap_readings += 1;
        summary.max_tap_rms = summary.max_tap_rms.max(rms);
        let loud = rms >= MIN_TONE_RMS;
        if loud {
            summary.max_tone_share = summary.max_tone_share.max(share);
        }
        let run = self.runs.entry(track).or_default();
        *run = if loud && share >= TONE_SHARE { *run + 1 } else { 0 };
        summary.longest_tone = summary.longest_tone.max(*run);
    }

    /// The summary of the lines taken so far.
    #[must_use]
    pub fn summary(&self) -> AudioSummary {
        AudioSummary {
            connections: self.connections.len(),
            ..self.summary.clone()
        }
    }
}

/// The call's follow-ups in a capture's `events.jsonl` text: the fence's `http` verdicts on writes to the calling
/// services' links (`…/api/v2/ep/<service host>/…`, by the link's last segment: `add`, `acknowledge`, `leave`, …; a
/// read such as the broker's long poll isn't one), one per request, and the Trouter delivery responses (`3:::`) it let
/// through.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallFollowUps {
    /// The requests the fence allowed, by operation.
    pub allowed: std::collections::BTreeMap<String, usize>,
    /// The requests the fence denied, by operation.
    pub denied: std::collections::BTreeMap<String, usize>,
    /// The delivery responses the fence let through.
    pub delivery_responses: usize,
}

impl CallFollowUps {
    /// Reads the verdicts of a capture's `events.jsonl` text. Lines that aren't JSON or aren't verdicts are skipped.
    #[must_use]
    pub fn from_events(text: &str) -> Self {
        let mut seen = std::collections::BTreeSet::new();
        let mut follow_ups = Self::default();
        for line in text.lines() {
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if entry["type"] != "verdict" {
                continue;
            }
            let allowed = entry["verdict"] == "allow";
            if entry["channel"] == "web_socket" {
                let data = entry["frame"]["data"].as_str().unwrap_or_default();
                if allowed && entry["frame"]["kind"] == "text" && data.starts_with("3:::") {
                    follow_ups.delivery_responses += 1;
                }
                continue;
            }
            let Some(operation) = entry["url"].as_str().and_then(call_operation) else {
                continue;
            };
            let read = ["GET", "HEAD", "OPTIONS"]
                .iter()
                .any(|method| entry["method"] == *method);
            if entry["channel"] != "http" || read || !seen.insert(entry["network_id"].to_string()) {
                continue;
            }
            let tally = if allowed {
                &mut follow_ups.allowed
            } else {
                &mut follow_ups.denied
            };
            *tally.entry(operation.to_owned()).or_default() += 1;
        }
        follow_ups
    }

    /// Whether the call's acceptance was acknowledged: by the delivery response on the socket it came on, or, when
    /// that didn't go, by `POST …/acknowledge` (the client sends the request only then).
    #[must_use]
    pub fn acknowledged(&self) -> bool {
        self.delivery_responses > 0 || self.allowed.contains_key("acknowledge")
    }

    /// The operations of [`REQUIRED_FOLLOW_UPS`] the fence allowed none of.
    #[must_use]
    pub fn missing(&self) -> Vec<&'static str> {
        REQUIRED_FOLLOW_UPS
            .into_iter()
            .filter(|operation| !self.allowed.contains_key(*operation))
            .collect()
    }
}

/// Where the calling services' links start: the flight proxy's `ep/<service host>/` paths.
const CALL_SERVICES: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/";

/// The operation of a link on a calling service behind the flight proxy (`…/api/v2/ep/<service host>/…/<operation>`,
/// its query left out), or `None` for any other URL.
fn call_operation(url: &str) -> Option<&str> {
    let rest = url.strip_prefix(CALL_SERVICES)?;
    let path = rest.split(['?', '#']).next().unwrap_or_default().trim_end_matches('/');
    let (_, operation) = path.rsplit_once('/')?;
    (!operation.is_empty()).then_some(operation)
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
        Classifier::new().with_test_call_callee(Mri::new(echo))
    } else {
        report.notes.push(format!(
            "lab.toml has no MRI for the service participant {ECHO_SERVICE:?}: the fence will deny the call"
        ));
        Classifier::new()
    };
    let tone = write_tone(paths).map_err(|error| ScenarioError::Setup(format!("can't write the tone: {error}")))?;
    let session_options = SessionOptions {
        classifier,
        live_members: true,
        capture: Some(ScenarioName::EchoCall.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::Fake { audio: Some(tone) },
        user_agent: Some(USER_AGENT.to_owned()),
    };
    let mut session = LabSession::open_with(paths, ACCOUNT, session_options).await?;
    // before the page loads: the client sends an earlier run's unsent probe again as soon as it can
    match forced_rules() {
        Ok(rules) => rules.into_iter().for_each(|rule| session.force_deny(rule)),
        Err(error) => report.failures.push(format!("can't force the probe denies: {error}")),
    }
    if let Err(error) = call(&mut session, options, &mut report).await {
        report.failures.push(error.to_string());
        if let Some(note) = save_screenshot(ACCOUNT, &session).await {
            report.notes.push(note);
        }
    }
    let capture = session.capture_dir().map(Path::to_path_buf);
    // the browser goes down and the capture is finished, whatever happened
    report.close_account(ACCOUNT, session, &policy()).await;
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
    turn_off_noise_suppression(&mut driver, report).await?;
    let waited = wait_for_tone(session, ECHO_WAIT).await;
    report.notes.push(match waited {
        ToneWait::Back(after) => format!("{ACCOUNT}: the tone came back {after:?} after the call screen showed"),
        ToneWait::TimedOut => format!("{ACCOUNT}: no tone came back within {ECHO_WAIT:?}"),
        ToneWait::Lost => format!("{ACCOUNT}: the browser connection went before the tone came back"),
    });
    linger(session, AFTER_TONE).await;
    let still_up = driver
        .evaluate(
            "the call screen",
            &format!("document.querySelector({}) !== null", js_string(HANG_UP_CSS)),
        )
        .await?;
    if still_up == Value::Bool(true) {
        driver.click("a hangs up", HANG_UP).await?;
        report.notes.push(format!("{ACCOUNT}: hung up"));
    } else {
        report.notes.push(format!(
            "{ACCOUNT}: the call screen was gone before the run hung up: nothing to hang up"
        ));
    }
    linger(session, AFTER_HANG_UP).await;
    Ok(())
}

/// Turns the call's noise suppression off from the call screen's audio options ([`AUDIO_OPTIONS`],
/// [`NOISE_SUPPRESSION`]), so the tone reaches the echo service: the client's own suppression takes a steady tone for
/// noise and sends silence (seen 2026-10-24: the sent track's level stayed at about -84 dBFS with it on, in the M0
/// runs too). A setting of the account's own client, reaching nobody.
async fn turn_off_noise_suppression(driver: &mut Driver, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    driver.click("a opens the audio options", AUDIO_OPTIONS).await?;
    driver
        .wait_for("the noise suppression switch", NOISE_SUPPRESSION, CONTROL_WAIT)
        .await?;
    let checked = format!(
        "document.querySelector({}).getAttribute('aria-checked')",
        js_string(NOISE_SUPPRESSION_CSS)
    );
    if driver.evaluate("the noise suppression switch", &checked).await? == Value::String("true".to_owned()) {
        driver.click("a turns noise suppression off", NOISE_SUPPRESSION).await?;
    }
    driver
        .wait_until_true(
            "the noise suppression switch",
            &format!("{checked} === 'false'"),
            "noise suppression off",
            CONTROL_WAIT,
        )
        .await?;
    driver.click("a closes the audio options", AUDIO_OPTIONS).await?;
    report
        .notes
        .push(format!("{ACCOUNT}: the call's noise suppression is off"));
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
        js_string(text)
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

/// Steps 3 and 5 from the capture: the fence must have allowed a call start and the call's follow-ups on it, the
/// WebRTC statistics must show audio both ways, and the audio tap must have read the tone coming back.
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
    judge_follow_ups(&CallFollowUps::from_events(&text), report);
    judge_audio(&AudioSummary::from_events(&text), report);
}

/// The follow-ups of [`REQUIRED_FOLLOW_UPS`] and the acceptance's acknowledgement, over Trouter or by request
/// ([`CallFollowUps::acknowledged`]), must have passed.
fn judge_follow_ups(follow_ups: &CallFollowUps, report: &mut ScenarioReport) {
    report.notes.push(format!(
        "follow-ups: allowed {:?}, denied {:?}, {} delivery response(s) let through",
        follow_ups.allowed, follow_ups.denied, follow_ups.delivery_responses
    ));
    let missing = follow_ups.missing();
    if !missing.is_empty() {
        report
            .failures
            .push(format!("the fence allowed none of the call's {missing:?}"));
    }
    if !follow_ups.acknowledged() {
        report
            .failures
            .push("the call's acceptance wasn't acknowledged, over Trouter or by request".to_owned());
    }
}

/// The statistics must show audio both ways, and the tap the tone back.
fn judge_audio(audio: &AudioSummary, report: &mut ScenarioReport) {
    report.notes.push(format!(
        "audio: {} connection(s), connected {}, {} packets sent, {} received, energy {:.4}, peak level {:.3}; \
         the tap: {} reading(s), loudest RMS {:.4}, largest {TONE_HZ} Hz share {:.3}, {} tone reading(s) in a row",
        audio.connections,
        audio.connected,
        audio.packets_sent,
        audio.packets_received,
        audio.received_energy,
        audio.max_received_level,
        audio.tap_readings,
        audio.max_tap_rms,
        audio.max_tone_share,
        audio.longest_tone
    ));
    if !audio.both_ways() {
        report
            .failures
            .push("the call's audio didn't go both ways (WebRTC statistics in the capture)".to_owned());
    }
    if !audio.tone_back() {
        report.failures.push(format!(
            "the tone didn't come back: no {TONE_READINGS} readings in a row of a received track were the {TONE_HZ} Hz tone"
        ));
    }
}

/// What came of waiting for the tone ([`wait_for_tone_in`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneWait {
    /// The audio tap read the tone coming back, this long into the wait.
    Back(Duration),
    /// The limit passed first.
    TimedOut,
    /// The browser connection went first: the tone can't come back any more.
    Lost,
}

/// [`wait_for_tone_in`] on the session's capture, keeping its events flowing ([`linger`]). [`ToneWait::TimedOut`] at
/// once when the run doesn't record.
async fn wait_for_tone(session: &mut LabSession, limit: Duration) -> ToneWait {
    let Some(dir) = session.capture_dir() else {
        return ToneWait::TimedOut;
    };
    let file = dir.join(EVENTS_FILE);
    wait_for_tone_in(&file, limit, async |poll| linger(session, poll).await).await
}

/// Waits until the audio tap has read the tone coming back ([`AudioSummary::tone_back`]) in the capture's events
/// `file`, looking at it as it grows after each `keep_up(`[`TONE_POLL`]`)`, for up to `limit`. `keep_up` keeps the
/// session's events flowing for that long and gives `false` once the connection is gone, which ends the wait
/// ([`ToneWait::Lost`]) instead of reading a capture nothing adds to any more.
pub async fn wait_for_tone_in(
    file: &Path,
    limit: Duration,
    mut keep_up: impl AsyncFnMut(Duration) -> bool,
) -> ToneWait {
    let started = tokio::time::Instant::now();
    let mut tally = AudioTally::default();
    let mut offset = 0;
    while started.elapsed() < limit {
        let alive = keep_up(TONE_POLL).await;
        offset = read_new_lines(file, offset, &mut tally);
        if tally.summary().tone_back() {
            return ToneWait::Back(started.elapsed());
        }
        if !alive {
            return ToneWait::Lost;
        }
    }
    ToneWait::TimedOut
}

/// Feeds the whole lines `file` gained since byte `offset` to `tally`, and gives the offset after the last of them. A
/// file that can't be read gives nothing new.
fn read_new_lines(file: &Path, offset: u64, tally: &mut AudioTally) -> u64 {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut opened) = fs::File::open(file) else {
        return offset;
    };
    let mut bytes = Vec::new();
    if opened.seek(SeekFrom::Start(offset)).is_err() || opened.read_to_end(&mut bytes).is_err() {
        return offset;
    }
    let Some(last) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return offset;
    };
    String::from_utf8_lossy(&bytes[..last])
        .lines()
        .for_each(|line| tally.add_line(line));
    offset + u64::try_from(last + 1).unwrap_or(0)
}

/// Keeps the session's events flowing (and recorded) for `duration`, or until the connection goes. `false` when the
/// connection is gone.
async fn linger(session: &mut LabSession, duration: Duration) -> bool {
    let end = tokio::time::Instant::now() + duration;
    while let Some(left) = end.checked_duration_since(tokio::time::Instant::now()) {
        if left.is_zero() {
            break;
        }
        if !session.pump(left.min(Duration::from_millis(500))).await {
            return false;
        }
    }
    true
}
