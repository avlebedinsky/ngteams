//! Live, against the company tenant with the lab accounts: M3's acceptance run of calls in the app (M3 Task 21).
//!
//! This test is an orchestrator only, as `ui_live` is. It opens no fence, holds no token and sends nothing: it starts a
//! UI sandbox per lab account, each with its private PipeWire (its own microphone, its speaker recorded,
//! its fake camera), spawns in each the app built by `cargo build -p teams-app --release --target-dir target/lab` (its
//! path in `TEAMS_APP_BIN`) for a, b and c, each with its credential broker outside the sandbox, drives them by
//! keyboard through the sandbox's virtual keyboard (the call keys, the switcher) and b's call notifications through its
//! sandbox's bus, and reads their view traces, their recordings and `fence.log`. Where a case needs b's web client it
//! spawns a normally built `proto-lab run ab-call-app` (its path in `PROTO_LAB_BIN`), which arms its own browser fence,
//! and keeps in step with it by its progress lines and a's trace. The binaries this test build makes have
//! `test-support` unified in and refuse to run, so a wrong path fails the test rather than writes.
//!
//! **The cases** (`CALL_LIVE_PHASES`, comma-separated, picks some; all by default, in this order; `web` is
//! `web-answer` and `web-place`; `CALL_LIVE_APP_LOG` sets the apps' diagnostic log level, `TEAMS_APP_LOG`):
//! - `test-call`: a headset plugged into a's sandbox plays a keyed tone (990 Hz on and off in 200 ms slots of a shift
//!   register's pattern); a's test call ("Test call" in the switcher) is held until the echo service played back what
//!   it recorded, and the tone must come back in a's recording: each 2 s of it lined up with what the headset played,
//!   on the one clock, gives a delay, and the one at least three blocks agree on is the echo's round trip.
//! - `web`: b's web client and a's app call each other, both ways, with audio and video: a calls (Ctrl+Shift+C) and
//!   b's page answers (`--call answer`); then b's page calls (`--call place`) and a answers (Ctrl+Shift+S). Each time
//!   a turns its camera on (Ctrl+Shift+O), a's window presents b's video, and b's page says it heard and saw a; a
//!   hears b's page's tone whenever the page's own microphone played it (`ab-call-app`'s `mic`: Chrome's fake
//!   microphone stays silent in some of the page's calls, which is noted); a hangs up the first, b's page the second.
//!   a talks with its sandbox's own microphone here, its tone steady (the page's audio tap reads a steady tone).
//! - Before the app-to-app cases each one plugs in its voice: a microphone above its sandbox's own playing its tone
//!   keyed ([`plug_voice`]; the call's noise suppression lowers a steady tone within seconds).
//! - `one-to-one`: a calls b; b's banner, ringtone (in b's recording) and notification come; b answers from the
//!   notification; each one's voice is in the other's recording. b's speaker is looped back into b's sandbox's own
//!   microphone (100 ms, 6 dB, [`B_ECHO`]): with b silent (its voice unplugged) and a talking (a speech-like signal), b's
//!   microphone must hear the loopback and a must not hear itself back beyond [`ECHO_MARGIN_DB`] under the echo at b's
//!   microphone. a's microphone is switched mid-call (the keyed headset plugged into a's sandbox, which a's app follows
//!   as it follows the system), b hears the headset alone, and the keyed tone lined up in b's recording gives a → b's
//!   latency; the headset unplugged, b hears a's voice alone again. Both cameras on: each window presents the other's
//!   video; CPU per process and the frames per second presented are read over [`MEASURE_FOR`]; a hangs up and b's call
//!   ends with it.
//! - `decline`: a calls b again, b declines from the notification, and a is told: its in-call bar says the call went
//!   to voicemail (as the service took a declined call when it was captured), and a hangs up; or its call ends, the
//!   service saying everyone left (as it did in this test's runs).
//! - `group`: a calls the a–b–c group chat; b answers from its notification, c by Ctrl+Shift+S; each hears the other
//!   two, every camera on, each window presents the other two on two lines; c leaves (Ctrl+Shift+H), a no longer
//!   lists c; c joins again from the chat (Ctrl+Shift+J), hears both again, turns its camera on, sees both again and a
//!   sees c's video again; a hangs up; whoever is still in the call then hangs up too.
//!
//! Then every app quits from its tray (its broker exits with it), and `fence.log` over the run is judged
//! (`call_live/fence.rs`): no breach, no deny of a write, call writes only as the lab's call rule lets them and in
//! the lab's chats. Setup times, the echo's round trip, a → b latency, frames per second and CPU are in
//! `summary.json` with the load average beside them. The traces, the apps' and brokers' and b's page's output, the
//! recordings and `summary.json` go to `<data_dir>/call-live/<time>/` (`0700`), screenshots to
//! `<data_dir>/screenshots/<time>/`, never the repo; no text file kept there holds a token.
//!
//! Built only with `--features live`, and even then it skips unless `PROTO_LAB_LIVE=1`, so
//! `cargo test --all-features` stays offline. Run it under the lab's live lock (`flock
//! ~/.local/share/teams-lab/live.lock`) with an outer `timeout --signal=INT`, so no other lab run shares the accounts,
//! the browser profiles or `fence.log`.

// test code throughout (its helpers unwrap and panic like the tests do)
#![cfg(test)]

#[path = "call_live/apps.rs"]
mod apps;
#[path = "call_live/audio.rs"]
mod audio;
#[expect(
    dead_code,
    reason = "the live harness `ui_live` shares: the compose box's helpers aren't the calls'"
)]
mod common;
#[path = "call_live/fence.rs"]
mod fence;
#[path = "call_live/numbers.rs"]
mod numbers;
#[path = "call_live/web.rs"]
mod web;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teams_fence::lab::{Alias, LabChats, LabConfig, LabLocations};
use teams_fence::one_to_one_id;
use teams_media::tone::Tone;
use ui_sandbox::pipewire::nodes::{AddedNode, NodeSpec};
use ui_sandbox::pipewire::timeline::now_us;
use ui_sandbox::pipewire::{DEVICE_PRIORITY, Echo};
use ui_sandbox::{Sandbox, Tools, wait_until};

use apps::{App, at_ms, in_call_with, is, strings};
use audio::{AGREEING_BLOCKS, Recording, agreeing, db_under, keyed, lags, loudest};
use common::{no_token_kept, now_ms, private_dir, stamp};
use numbers::{CpuSampler, stats, stats_f, tile_fps};
use web::WebRun;

/// Each one's voice in the app-to-app cases: its tone, keyed ([`audio::keyed`]), played by a microphone plugged into
/// its sandbox ([`plug_voice`]): a steady tone is noise to the call's noise suppression, which lowers it within seconds.
/// a's is the tone b's page listens for (`proto_lab::capture::TAP_TONE_HZ`); with b's page, a talks through its
/// sandbox's own microphone, that tone steady ([`A_SANDBOX`]).
const A_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};
const C_TONE: Tone = Tone {
    hz: 550.0,
    amplitude: 0.5,
};
/// Each sandbox's own microphone (the default): a's plays its tone steady; b's and c's are silent, so b's carries only
/// its speaker looped back ([`B_ECHO`]) whenever b's voice is unplugged.
const A_SANDBOX: Tone = A_TONE;
const SILENT: Tone = Tone {
    hz: 440.0,
    amplitude: 0.0,
};
/// A voice's microphone: above the sandbox's own, below a headset plugged in.
const VOICE_PRIORITY: u32 = DEVICE_PRIORITY + 500;
/// The headset plugged into a's sandbox: this tone, keyed ([`audio::keyed`]).
const HEADSET_TONE: Tone = Tone {
    hz: 990.0,
    amplitude: 0.5,
};
/// b's speaker looped back into b's microphone, as a laptop's speaker reaches its microphone.
const B_ECHO: Echo = Echo {
    delay: Duration::from_millis(100),
    attenuation_db: 6.0,
};
/// The ringtone's first tone (`teams_ui::ringtone::RINGTONE`).
const RINGTONE_HZ: f32 = 784.0;
/// A far end's tone heard at least this loud in a recording (the call's processing lowers a steady tone).
const HEARD: f32 = 0.03;
/// A tone quieter than this in a recording isn't there.
const QUIET: f32 = 0.01;
/// The ringtone, played by the app itself, at least this loud.
const RINGTONE_HEARD: f32 = 0.05;
/// How far under the echo at b's microphone (what b played, less the loopback's attenuation) a's own speech must stay
/// in a's recording: Task 10's converged echo suppression, measured as its test measures it.
const ECHO_MARGIN_DB: f32 = 25.0;
/// The smallest speech-band level the echo check takes as heard ([`audio::speech_level`] of a 16-bit recording):
/// a margin is never claimed past it.
const MEASURABLE: f32 = 1e-5;
/// The frames a tile must have presented (traced at the first and every 30th).
const PRESENTED: u64 = 30;

/// From an ask to the call's media connected (or a ring to come).
const CONNECT: Duration = Duration::from_secs(90);
/// How long a tone may take to be heard once the call connected.
const TONE_WAIT: Duration = Duration::from_secs(30);
/// How long video may take to be presented once the camera is on.
const VIDEO_WAIT: Duration = Duration::from_secs(90);
/// How long the test call is held once connected: the echo service plays back what it recorded about 41 s in.
const ECHO_HOLD: Duration = Duration::from_secs(48);
/// How long the echo canceller gets to converge before the echo is measured, and over how long it is measured.
const ECHO_SETTLE: Duration = Duration::from_secs(8);
const ECHO_FOR: Duration = Duration::from_secs(6);
/// How long the keyed headset is heard for a → b's latency.
const LATENCY_FOR: Duration = Duration::from_secs(20);
/// How long CPU and frames per second are read in the 1:1 video call.
const MEASURE_FOR: Duration = Duration::from_secs(20);
/// How long the ringtone is let ring before b answers.
const RING_FOR: Duration = Duration::from_secs(3);
/// How long b's page may take to load and stand its calling up; how long it holds or waits around a's steps.
const WEB_READY: Duration = Duration::from_secs(420);
const WEB_STEP: Duration = Duration::from_secs(150);
const WEB_EXIT: Duration = Duration::from_secs(180);
/// How long the others' calls may take to stop listing one who left a group call.
const LEFT_SEEN: Duration = Duration::from_secs(180);
/// How long the run waits after a hangs up a group call for the others' calls to end by themselves.
const AFTER_CALLER: Duration = Duration::from_secs(15);

/// The lab as the run needs it.
pub struct Lab {
    pub app: PathBuf,
    pub proto_lab: PathBuf,
    pub ab_chat: String,
    pub group_chat: String,
    /// a's, b's and c's MRIs.
    pub mris: [String; 3],
    pub fence_log: PathBuf,
    pub out: PathBuf,
    pub shots: PathBuf,
    pub tools: Option<Tools>,
}

/// The lab, or `None` (after a note) when live tests aren't asked for. Fails without `TEAMS_APP_BIN`, `PROTO_LAB_BIN`
/// or the sandbox's tools.
fn lab() -> Option<Lab> {
    if std::env::var("PROTO_LAB_LIVE").as_deref() != Ok("1") {
        eprintln!("skipped: a live lab test; set PROTO_LAB_LIVE=1 to run it");
        return None;
    }
    let binary = |variable: &str, build: &str| {
        let path = std::env::var_os(variable).map_or_else(
            || panic!("{variable} is unset: build with `cargo build -p {build} --release --target-dir target/lab`"),
            PathBuf::from,
        );
        assert!(path.is_file(), "{variable} {} is not a file", path.display());
        path
    };
    let tools = Tools::find().unwrap_or_else(|missing| panic!("the UI sandbox's tools: {missing}"));
    let config = LabConfig::load(&LabLocations::from_env().unwrap()).unwrap();
    let mri = |alias| config.account(alias).mri.clone().unwrap();
    let stamp = stamp();
    Some(Lab {
        app: binary("TEAMS_APP_BIN", "teams-app"),
        proto_lab: binary("PROTO_LAB_BIN", "proto-lab"),
        ab_chat: one_to_one_id(&mri(Alias::A), &mri(Alias::B)).unwrap(),
        group_chat: LabChats::load(config.paths()).unwrap().abc.unwrap(),
        mris: [Alias::A, Alias::B, Alias::C].map(mri),
        fence_log: config.paths().fence_log(),
        out: private_dir(&config.paths().data_dir().join("call-live").join(&stamp)),
        shots: private_dir(&config.paths().data_dir().join("screenshots").join(&stamp)),
        tools: Some(tools),
    })
}

/// What the run found, case by case, for `summary.json`; and every call's setup.
#[derive(Default)]
struct Found {
    steps: BTreeMap<String, Value>,
    /// From the ask that brings a call up (a start, an answer, a join) to the asker's media connected, ms.
    setups: Vec<(String, i64)>,
    /// When b's page ran (Unix ms).
    pages: Vec<fence::Window>,
}

impl Found {
    fn step(&mut self, name: &str, value: Value) {
        eprintln!("call_live: {name}: {value}");
        self.steps.insert(name.to_owned(), value);
    }

    fn setup(&mut self, what: &str, asked_ms: i64, connected: &Value) -> i64 {
        let setup = at_ms(connected) - asked_ms;
        eprintln!("call_live: setup {what}: {setup} ms");
        self.setups.push((what.to_owned(), setup));
        setup
    }
}

/// The cases `CALL_LIVE_PHASES` asks for (all by default).
fn phases() -> Vec<String> {
    let all = "test-call,web,one-to-one,decline,group";
    std::env::var("CALL_LIVE_PHASES")
        .unwrap_or_else(|_| all.to_owned())
        .split(',')
        .map(|phase| phase.trim().to_owned())
        .filter(|phase| !phase.is_empty())
        .collect()
}

/// The live acceptance run of calls in the app, in its cases; see the module's docs.
#[test]
fn the_app_calls_through_the_lab_and_the_fence_holds() {
    let Some(lab) = lab() else {
        return;
    };
    let phases = phases();
    let wants = |phase: &str| phases.iter().any(|one| one == phase);
    let log_start = fs::metadata(&lab.fence_log).map_or(0, |meta| meta.len());
    let started_ms = now_ms();
    let mut found = Found::default();
    let mut a = App::with_pipewire(&lab, "a", A_SANDBOX, None);
    if wants("test-call") {
        test_call(&mut a, &mut found);
    }
    if wants("web") || wants("web-answer") {
        web_a_calls_b(&lab, &mut a, &mut found);
    }
    if wants("web") || wants("web-place") {
        web_b_calls_a(&lab, &mut a, &mut found);
    }
    let mut others = Vec::new();
    if wants("one-to-one") || wants("decline") || wants("group") {
        let mut b = App::with_pipewire(&lab, "b", SILENT, Some(B_ECHO));
        let mut c = App::with_pipewire(&lab, "c", SILENT, None);
        let mut voices = [
            Some(plug_voice(&a, A_TONE)),
            Some(plug_voice(&b, B_TONE)),
            Some(plug_voice(&c, C_TONE)),
        ];
        if wants("one-to-one") {
            one_to_one(&lab, [&mut a, &mut b], &mut voices[1], &mut found);
        }
        if wants("decline") {
            declined(&lab, &mut a, &mut b, &mut found);
        }
        if wants("group") {
            group(&lab, [&mut a, &mut b, &mut c], &mut found);
        }
        others.extend([b, c]);
    }
    for app in std::iter::once(&mut a).chain(others.iter_mut()) {
        app.shot("end");
        app.keep_recording();
        app.quit();
    }
    drop(others);
    drop(a);
    let (fence, problems) = fence::judge(&lab, log_start, &found.pages);
    let setups: Vec<i64> = found.setups.iter().map(|(_, setup)| *setup).collect();
    let summary = json!({
        "started_ms": started_ms,
        "ended_ms": now_ms(),
        "phases": phases,
        "steps": found.steps,
        "setup_ms": {"each": found.setups.iter().map(|(what, ms)| json!([what, ms])).collect::<Vec<_>>(), "all": stats(setups)},
        "web_windows": found.pages,
        "fence_log": fence,
        "fence_problems": problems,
        "screenshots": lab.shots,
    });
    fs::write(lab.out.join("summary.json"), format!("{summary:#}\n")).unwrap();
    no_token_kept(&lab.out);
    eprintln!("{summary:#}");
    assert!(problems.is_empty(), "fence.log: {problems:#?}");
}

// --- the cases ---

/// A device plugged into `app`'s sandbox above the system's: the headset playing the keyed tone.
fn plug_headset(app: &App) -> AddedNode {
    let spec = NodeSpec {
        name: "sandbox-headset-mic".to_owned(),
        description: "Headset".to_owned(),
        priority: DEVICE_PRIORITY + 1_000,
    };
    app.pipewire().add_looped_microphone(spec, keyed(HEADSET_TONE)).unwrap()
}

/// A microphone plugged into `app`'s sandbox above its own: its voice, `tone` keyed.
fn plug_voice(app: &App, tone: Tone) -> AddedNode {
    let spec = NodeSpec {
        name: "sandbox-voice-mic".to_owned(),
        description: "Voice".to_owned(),
        priority: VOICE_PRIORITY,
    };
    app.pipewire().add_looped_microphone(spec, keyed(tone)).unwrap()
}

/// `app`'s recording, read as it grows.
fn recording(app: &App) -> Recording {
    let (path, timeline) = app.recording();
    Recording { path, timeline }
}

/// Waits until `heard`'s last 2 s hold `hz` at [`HEARD`] or louder; its level then, or the panic naming `what`.
fn wait_heard(heard: &Recording, hz: f32, what: &str) -> f32 {
    try_heard(heard, hz)
        .unwrap_or_else(|level| panic!("{what}: {hz} Hz not heard within {TONE_WAIT:?} (at {level:.4})"))
}

/// [`wait_heard`] (a keyed voice or a steady tone: its loudest 200 ms over the last 2 s); the level at the end when it
/// wasn't heard.
fn try_heard(heard: &Recording, hz: f32) -> Result<f32, f32> {
    let level = || loudest(&heard.tail(Duration::from_secs(2)), hz, Duration::from_millis(200));
    wait_until(TONE_WAIT, || Some(level()).filter(|level| *level >= HEARD)).ok_or_else(level)
}

/// Waits until `heard` holds `wanted` and not `other` (keyed or not: each one's loudest 200 ms over the last 1.2 s);
/// when (Unix ms), or the panic naming `what`.
fn wait_alone(heard: &Recording, wanted: f32, other: f32, what: &str) -> i64 {
    let level = |hz| {
        loudest(
            &heard.tail(Duration::from_millis(1_200)),
            hz,
            Duration::from_millis(200),
        )
    };
    wait_until(TONE_WAIT, || {
        (level(wanted) >= HEARD && level(other) < QUIET).then(now_ms)
    })
    .unwrap_or_else(|| {
        let (level, left) = (level(wanted), level(other));
        panic!("{what}: {wanted} Hz not heard alone (at {level:.4}, {other} Hz at {left:.4})")
    })
}

/// Turns `app`'s camera on (Ctrl+Shift+O) and waits for its in-call bar to say so.
fn camera_on(app: &mut App) {
    let from = app.trace().mark();
    app.ctrl_shift('o');
    app.in_call(from, apps::STEP, &["camera-on"]);
}

/// Whether `line` is a `video-tile presented` of `call` that isn't the self view, with at least `frames` frames.
fn presented(line: &Value, call: &Value, frames: u64) -> bool {
    is(line, "video-tile", "presented")
        && line["call"] == *call
        && strings(line, "kinds") != ["self"]
        && line["frames"].as_u64().is_some_and(|shown| shown >= frames)
}

/// The tiles (other than the self view) of `call` that presented at least [`PRESENTED`] frames from `from` on.
fn tiles_presented(app: &mut App, from: usize, call: &Value) -> Vec<String> {
    let mut tiles: Vec<String> = app
        .trace()
        .all_from(from, |line| presented(line, call, PRESENTED))
        .iter()
        .flat_map(|line| strings(line, "kinds"))
        .collect();
    tiles.sort();
    tiles.dedup();
    tiles
}

/// Waits until `app` presented `count` tiles of `call` (other than its self view) [`PRESENTED`] frames each.
fn wait_tiles(app: &mut App, from: usize, call: &Value, count: usize) -> Vec<String> {
    let deadline = Instant::now() + VIDEO_WAIT;
    loop {
        let tiles = tiles_presented(app, from, call);
        if tiles.len() >= count {
            return tiles;
        }
        assert!(
            Instant::now() < deadline,
            "{}: {count} tile(s) of call {call} not presented within {VIDEO_WAIT:?}: {tiles:?}",
            app.alias
        );
        thread::sleep(Duration::from_millis(500));
    }
}

/// a's test call (see the module docs).
fn test_call(a: &mut App, found: &mut Found) {
    let headset = plug_headset(a);
    // the session manager makes it the default before the call opens the microphone
    thread::sleep(Duration::from_secs(2));
    let heard = recording(a);
    let from = a.trace().mark();
    a.command("Test call");
    // from the app's own ask (the switcher's typing and the wait before its Enter left out)
    let asked = a.trace().wait_from(from, apps::STEP, "the test call asked", |line| {
        is(line, "call", "asked") && strings(line, "kinds").contains(&"test-call".to_owned())
    });
    let connected = a.in_call(from, CONNECT, &["test-call", "connected"]);
    let setup = found.setup("test call: asked → connected", at_ms(&asked), &connected);
    a.shot("test-call");
    thread::sleep(ECHO_HOLD);
    let held_until = now_us();
    a.ctrl_shift('h');
    a.in_call(from, apps::STEP, &["test-call", "ended", "hung-up"]);
    let blocks = lags(
        &heard,
        headset.timeline().unwrap(),
        HEADSET_TONE.hz,
        (at_ms(&connected) * 1_000, held_until),
        (5_000, 30_000),
        Duration::from_secs(2),
    );
    headset.remove().unwrap();
    // the round trip is the delay several blocks agree on; a block lined up elsewhere isn't counted
    let round_trips = agreeing(&blocks);
    found.step(
        "test_call",
        json!({
            "setup_ms": setup, "round_trip_ms": round_trips.clone().map(stats), "blocks_agreeing": round_trips.as_ref().map(Vec::len),
            "blocks_lined_up": blocks,
        }),
    );
    assert!(
        round_trips.is_some(),
        "a's test call didn't bring the headset's tone back: {AGREEING_BLOCKS} blocks or more must agree on its \
         delay, the blocks lined up at {blocks:?} ms"
    );
}

/// Waits up to [`CONNECT`] for `app`'s Call to be offered in `chat`, the chat it shows (its calling registered, no
/// call up or ringing): its last `call-button shown` line (traced when the button changes) names `chat` and `call`.
/// An earlier line, of another chat or from before a call ended, is no longer the last one.
fn wait_call_button(app: &mut App, chat: &str) {
    let deadline = Instant::now() + CONNECT;
    loop {
        let last = app.trace().last(|line| is(line, "call-button", "shown"));
        if last
            .as_ref()
            .is_some_and(|line| line["conversation"] == chat && strings(line, "kinds") == ["call"])
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{}: Call not offered in {chat} within {CONNECT:?}; the call button last shown: {last:?}",
            app.alias
        );
        thread::sleep(Duration::from_millis(200));
    }
}

/// a's app calls b's web client, which answers (see the module docs).
fn web_a_calls_b(lab: &Lab, a: &mut App, found: &mut Found) {
    a.open_chat(&lab.ab_chat);
    let heard = recording(a);
    // a calls, b's page answers
    let mut web = WebRun::start(lab, "answer", "web-answer");
    web.wait_step("ready", WEB_READY);
    let from = a.trace().mark();
    wait_call_button(a, &lab.ab_chat);
    let asked = now_ms();
    a.ctrl_shift('c');
    let connected = a.in_call(from, CONNECT, &["chat", "connected"]);
    let setup_out = found.setup("a calls b's page: asked → connected", asked, &connected);
    web.wait_step("in-call", WEB_STEP);
    camera_on(a);
    let call = connected["call"].clone();
    wait_tiles(a, from, &call, 1);
    // b's page says it saw a once it tried its own microphone, `mic` when that plays its tone into the call: the page's
    // fake microphone may stay silent (`ab-call-app`'s notes), and a can be checked hearing it only when it said so
    let saw = web.try_wait_step("saw", WEB_STEP);
    let mic = web.try_wait_step("mic", Duration::ZERO);
    let a_heard = mic.map(|_| try_heard(&heard, A_TONE.hz));
    a.shot("web-answered");
    a.ctrl_shift('h');
    a.in_call(from, apps::STEP, &["chat", "ended", "hung-up"]);
    let started = web.started_ms;
    let (status, said) = web.finish(WEB_EXIT);
    found.pages.push((started, now_ms()));
    let out = json!({
        "setup_ms": setup_out, "page_mic": mic.is_some(), "a_heard": format!("{a_heard:?}"), "page_heard_and_saw": saw.is_some(),
        "exit": status.map(|status| status.code()), "page_said": said.iter().filter(|line| line.starts_with("ab-call-app")).collect::<Vec<_>>(),
    });
    found.step("web_a_calls_b", out);
    assert!(
        status.is_some_and(|status| status.success()),
        "ab-call-app --call answer failed: {said:#?}"
    );
    assert!(saw.is_some(), "b's page didn't hear and see a");
    if let Some(Err(level)) = a_heard {
        panic!(
            "a hearing b's page: {} Hz not heard within {TONE_WAIT:?} (at {level:.4})",
            A_TONE.hz
        );
    }
}

/// b's web client calls a's app, which answers (see the module docs).
fn web_b_calls_a(lab: &Lab, a: &mut App, found: &mut Found) {
    let heard = recording(a);
    let mut web = WebRun::start(lab, "place", "web-place");
    web.wait_step("ready", WEB_READY);
    let from = a.trace().mark();
    let banner = a.trace().wait_from(from, CONNECT, "call-banner shown", |line| {
        is(line, "call-banner", "shown")
    });
    assert_eq!(
        strings(&banner, "people"),
        [lab.mris[1].clone()],
        "a's banner names another caller"
    );
    a.trace().wait_from(from, apps::STEP, "ringtone started", |line| {
        is(line, "ringtone", "started")
    });
    // a person takes a moment to answer: the ringtone is heard first
    thread::sleep(RING_FOR);
    let ringtone = loudest(&heard.tail(RING_FOR), RINGTONE_HZ, Duration::from_millis(400));
    let answered = now_ms();
    a.ctrl_shift('s');
    let connected = a.in_call(from, CONNECT, &["incoming", "connected"]);
    let setup_in = found.setup("b's page calls a: answered → connected", answered, &connected);
    camera_on(a);
    let call = connected["call"].clone();
    wait_tiles(a, from, &call, 1);
    // as above; a page that placed the call may keep its microphone silent (`ab-call-app`'s notes): a can be checked
    // hearing it only when it said `mic`
    let saw = web.try_wait_step("saw", WEB_STEP);
    let mic = web.try_wait_step("mic", Duration::ZERO);
    let a_heard = mic.map(|_| try_heard(&heard, A_TONE.hz));
    a.shot("web-placed");
    a.in_call(from, WEB_STEP, &["incoming", "ended", "remote"]);
    let started = web.started_ms;
    let (status, said) = web.finish(WEB_EXIT);
    found.pages.push((started, now_ms()));
    found.step(
        "web_b_calls_a",
        json!({
            "setup_ms": setup_in, "ringtone": ringtone, "page_mic": mic.is_some(), "a_heard": format!("{a_heard:?}"), "page_heard_and_saw": saw.is_some(),
            "exit": status.map(|status| status.code()), "page_said": said.iter().filter(|line| line.starts_with("ab-call-app")).collect::<Vec<_>>(),
        }),
    );
    assert!(
        status.is_some_and(|status| status.success()),
        "ab-call-app --call place failed: {said:#?}"
    );
    assert!(saw.is_some(), "b's page didn't hear and see a");
    if let Some(Err(level)) = a_heard {
        panic!(
            "a hearing b's page: {} Hz not heard within {TONE_WAIT:?} (at {level:.4})",
            A_TONE.hz
        );
    }
}

/// The id of the `seen`-th incoming call's notification (one with an Answer) on `sandbox`'s bus.
fn call_notification(sandbox: &Sandbox, seen: usize) -> u32 {
    wait_until(CONNECT, || {
        sandbox
            .bus()
            .notifications()
            .into_iter()
            .filter(|notification| notification.actions.iter().any(|action| action == "answer"))
            .nth(seen)
    })
    .map_or_else(
        || panic!("no incoming call's notification: {:?}", sandbox.bus().notifications()),
        |notification| notification.id,
    )
}

/// How many incoming calls' notifications `sandbox`'s bus has had.
fn call_notifications(sandbox: &Sandbox) -> usize {
    sandbox
        .bus()
        .notifications()
        .iter()
        .filter(|notification| notification.actions.iter().any(|action| action == "answer"))
        .count()
}

/// b's ring: its banner naming a, its ringtone in its recording, its notification; the notification's id.
fn b_rings(lab: &Lab, b: &mut App, from: usize, rang: usize) -> (u32, f32) {
    let banner = b.trace().wait_from(from, CONNECT, "call-banner shown", |line| {
        is(line, "call-banner", "shown")
    });
    assert_eq!(
        strings(&banner, "people"),
        [lab.mris[0].clone()],
        "b's banner names another caller"
    );
    b.trace().wait_from(from, apps::STEP, "ringtone started", |line| {
        is(line, "ringtone", "started")
    });
    b.trace()
        .wait_from(from, apps::STEP, "call-notification shown", |line| {
            is(line, "call-notification", "shown")
        });
    let id = call_notification(&b.sandbox, rang);
    thread::sleep(RING_FOR);
    let ringtone = loudest(&recording(b).tail(RING_FOR), RINGTONE_HZ, Duration::from_millis(400));
    assert!(
        ringtone >= RINGTONE_HEARD,
        "b's ringtone isn't in b's recording ({ringtone:.3})"
    );
    (id, ringtone)
}

/// a calls b, b answers from its notification (see the module docs).
fn one_to_one(lab: &Lab, [a, b]: [&mut App; 2], b_voice: &mut Option<AddedNode>, found: &mut Found) {
    a.open_chat(&lab.ab_chat);
    b.open_chat(&lab.ab_chat);
    let (a_heard, b_heard) = (recording(a), recording(b));
    let (a_from, b_from) = (a.trace().mark(), b.trace().mark());
    wait_call_button(a, &lab.ab_chat);
    let rang = call_notifications(&b.sandbox);
    a.ctrl_shift('c');
    let (notification, ringtone) = b_rings(lab, b, b_from, rang);
    b.shot("ringing");
    let answered = now_ms();
    b.sandbox.bus().invoke_action(notification, "answer").unwrap();
    b.trace()
        .wait_from(b_from, apps::STEP, "call-notification answered", |line| {
            is(line, "call-notification", "answered")
        });
    b.trace().wait_from(b_from, apps::STEP, "ringtone stopped", |line| {
        is(line, "ringtone", "stopped")
    });
    let b_connected = b.in_call(b_from, CONNECT, &["incoming", "connected"]);
    let a_connected = a.in_call(a_from, CONNECT, &["chat", "connected"]);
    let b_setup = found.setup("1:1: b answered → b connected", answered, &b_connected);
    let a_setup = found.setup("1:1: b answered → a connected", answered, &a_connected);
    let tones = json!({
        "a_hears_b": wait_heard(&a_heard, B_TONE.hz, "a hearing b"),
        "b_hears_a": wait_heard(&b_heard, A_TONE.hz, "b hearing a"),
    });
    let echo = echo_check(
        [&mut *a, &mut *b],
        (&b_connected["call"], b_voice),
        (&a_heard, &b_heard),
    );
    let switched = switch_microphone(a, &b_heard);
    // video both ways, then CPU and frames per second over a while
    let (a_video, b_video) = (a.trace().mark(), b.trace().mark());
    camera_on(a);
    camera_on(b);
    let (a_call, b_call) = (a_connected["call"].clone(), b_connected["call"].clone());
    wait_tiles(a, a_video, &a_call, 1);
    wait_tiles(b, b_video, &b_call, 1);
    let (a_measure, b_measure) = (a.trace().mark(), b.trace().mark());
    let sampler = CpuSampler::start(processes(&mut [a, b]));
    thread::sleep(MEASURE_FOR);
    let cpu = sampler.finish();
    let fps = json!({
        "a_remote": stats_f(tile_fps(&a.trace().all_from(a_measure, |_| true), &a_call, "remote")),
        "a_self": stats_f(tile_fps(&a.trace().all_from(a_measure, |_| true), &a_call, "self")),
        "b_remote": stats_f(tile_fps(&b.trace().all_from(b_measure, |_| true), &b_call, "remote")),
        "b_self": stats_f(tile_fps(&b.trace().all_from(b_measure, |_| true), &b_call, "self")),
    });
    a.shot("one-to-one");
    b.shot("one-to-one");
    a.ctrl_shift('h');
    a.in_call(a_from, apps::STEP, &["chat", "ended", "hung-up"]);
    b.in_call(b_from, apps::STEP, &["incoming", "ended", "remote"]);
    found.step(
        "one_to_one",
        json!({
            "ringtone": ringtone, "b_setup_ms": b_setup, "a_setup_ms": a_setup, "tones": tones, "echo": echo,
            "microphone_switch": switched, "video_fps": fps, "measured_for_s": MEASURE_FOR.as_secs(), "cpu": cpu,
        }),
    );
}

/// The processes whose CPU is read in a call: each app, its sandbox's compositors and PipeWire, and this process (the
/// sandboxes' cameras and recordings run on its threads).
fn processes(apps: &mut [&mut App]) -> Vec<(String, u32)> {
    let mut processes = vec![("orchestrator".to_owned(), std::process::id())];
    for app in apps.iter_mut() {
        let alias = app.alias;
        processes.push((format!("{alias}-app"), app.pid()));
        for (name, pid) in ["weston", "niri"].iter().zip(app.sandbox.helper_pids()) {
            processes.push((format!("{alias}-{name}"), pid));
        }
        for (name, pid) in ["pipewire", "wireplumber"].iter().zip(app.pipewire().pids()) {
            processes.push((format!("{alias}-{name}"), pid));
        }
    }
    processes
}

/// b's speaker looped back into b's microphone: b falls silent (its voice unplugged: its sandbox's own microphone
/// carries only the loopback) and a talks (a microphone plugged into a's sandbox above its voice plays a speech-like
/// signal, [`audio::speech_like`]: a steady tone is noise to the processing, and a canceller can't tell one period of it
/// from the next). Once b's canceller had [`ECHO_SETTLE`], what a hears back over [`ECHO_FOR`] (a's recording's
/// speech-band level) must be [`ECHO_MARGIN_DB`] or more under the echo at b's microphone: what b played of a's speech
/// less the loopback's [`B_ECHO`] attenuation. b's microphone must have heard something (its trace's
/// `call-participant self` `mic-sound`, the level before its processing) at both ends of the window: the loopback
/// reached what b's app captures, so the margin is the canceller's, not a dead microphone's. Then b's voice is plugged
/// in again.
fn echo_check(
    [a, b]: [&mut App; 2],
    (b_call, b_voice): (&Value, &mut Option<AddedNode>),
    (a_heard, b_heard): (&Recording, &Recording),
) -> Value {
    if let Some(voice) = b_voice.take() {
        voice.remove().unwrap();
    }
    // b silent (its app moved off the voice): what a hears before it talks is the floor, noted
    thread::sleep(Duration::from_secs(5));
    let floor_to = now_us();
    let floor = audio::speech_level(&a_heard.between(floor_to - 2_000_000, floor_to));
    // a microphone of its own: the voice's (`sandbox-voice-mic`) stays plugged in a's sandbox
    let spec = NodeSpec {
        name: "sandbox-speech-mic".to_owned(),
        description: "Speech".to_owned(),
        priority: DEVICE_PRIORITY + 1_000,
    };
    let speech = a
        .pipewire()
        .add_looped_microphone(spec, audio::speech_like(10, 7, 0.5))
        .unwrap();
    thread::sleep(ECHO_SETTLE + ECHO_FOR);
    let to = now_us();
    let from = to - i64::try_from(ECHO_FOR.as_micros()).unwrap();
    let played = audio::speech_level(&b_heard.between(from, to));
    let heard_back = audio::speech_level(&a_heard.between(from, to));
    let b_trace = b.trace();
    b_trace.drain();
    let b_mic = [from, to].map(|at| mic_heard_at(&b_trace.seen, b_call, at / 1_000));
    speech.remove().unwrap();
    *b_voice = Some(plug_voice(b, B_TONE));
    // all a heard back counts as echo, whatever of it was there before (the floor, noted); nothing under what a 16-bit
    // recording's speech-band level resolves
    let at_mic = played * 10_f32.powf(-B_ECHO.attenuation_db / 20.0);
    let margin = db_under(at_mic, heard_back.max(MEASURABLE));
    eprintln!(
        "call_live: echo: b played a's speech at {played:.5} ({at_mic:.5} at its microphone, which heard something at \
         the window's ends: {b_mic:?}), a heard back {heard_back:.6} over a floor of {floor:.6}: {margin:.1} dB"
    );
    assert!(played >= 0.001, "b didn't play a's speech ({played:.5})");
    assert_eq!(
        b_mic,
        [Some(true); 2],
        "b's microphone didn't hear the loopback of its speaker through the window"
    );
    assert!(
        margin >= ECHO_MARGIN_DB,
        "a heard its own speech back {margin:.1} dB under the echo at b's microphone, not {ECHO_MARGIN_DB} dB"
    );
    json!({
        "b_played": played, "echo_at_b_mic": at_mic, "b_mic_heard": b_mic, "a_heard_back": heard_back, "floor": floor,
        "margin_db": (margin * 10.0).round() / 10.0, "at_floor": heard_back <= floor,
    })
}

/// Whether the account's microphone heard something (before its processing) in `call` at `when_ms` (Unix ms), by the
/// last `call-participant self` line traced then or before (`mic-sound` or `mic-silent`); `None` before any.
fn mic_heard_at(lines: &[Value], call: &Value, when_ms: i64) -> Option<bool> {
    lines
        .iter()
        .rev()
        .find(|line| is(line, "call-participant", "self") && line["call"] == *call && at_ms(line) <= when_ms)
        .map(|line| strings(line, "kinds").iter().any(|kind| kind == "mic-sound"))
}

/// a's microphone switched mid-call: the keyed headset plugged into a's sandbox, b hears it alone, its latency read
/// over [`LATENCY_FOR`]; unplugged, b hears a's voice alone again.
fn switch_microphone(a: &mut App, b_heard: &Recording) -> Value {
    let from = a.trace().mark();
    let headset = plug_headset(a);
    let plugged = now_ms();
    let switched = wait_alone(b_heard, HEADSET_TONE.hz, A_TONE.hz, "b hearing a's headset");
    let measured_from = now_us();
    thread::sleep(LATENCY_FOR);
    let latency = lags(
        b_heard,
        headset.timeline().unwrap(),
        HEADSET_TONE.hz,
        (measured_from, now_us()),
        (0, 2_000),
        Duration::from_secs(2),
    );
    headset.remove().unwrap();
    let unplugged = now_ms();
    let back = wait_alone(b_heard, A_TONE.hz, HEADSET_TONE.hz, "b hearing a's microphone again");
    if let Some(ended) = a.trace().all_from(from, |line| in_call_with(line, &["ended"])).first() {
        panic!("a's call ended while its microphone moved: {ended}");
    }
    assert!(
        latency.len() >= 3,
        "a → b's latency: the headset's tone lined up in {} block(s) only",
        latency.len()
    );
    json!({
        "to_headset_ms": switched - plugged,
        "back_ms": back - unplugged,
        "a_to_b_latency_ms": stats(latency.clone()),
        "latency_blocks": latency.len(),
    })
}

/// a calls b again and b declines from its notification (see the module docs).
fn declined(lab: &Lab, a: &mut App, b: &mut App, found: &mut Found) {
    a.open_chat(&lab.ab_chat);
    let (a_from, b_from) = (a.trace().mark(), b.trace().mark());
    wait_call_button(a, &lab.ab_chat);
    let rang = call_notifications(&b.sandbox);
    a.ctrl_shift('c');
    let (notification, _) = b_rings(lab, b, b_from, rang);
    let declined = now_ms();
    b.sandbox.bus().invoke_action(notification, "decline").unwrap();
    b.trace()
        .wait_from(b_from, apps::STEP, "call-notification declined", |line| {
            is(line, "call-notification", "declined")
        });
    let b_ended = b.in_call(b_from, apps::STEP, &["incoming", "ended", "declined"]);
    // a is told: its call taken by b's voicemail (as captured), or ended by the service with b gone from it (seen
    // live too: `0`/`5002`, "all participants left the audio-video modality")
    let told = a.trace().wait_from(a_from, CONNECT, "a told of the decline", |line| {
        in_call_with(line, &["voicemail"]) || in_call_with(line, &["chat", "ended", "remote"])
    });
    a.shot("declined");
    let how = if strings(&told, "kinds").iter().any(|kind| kind == "voicemail") {
        a.ctrl_shift('h');
        a.in_call(a_from, apps::STEP, &["chat", "ended", "hung-up"]);
        "voicemail"
    } else {
        "ended"
    };
    found.step(
        "declined",
        json!({"b_ended_ms": at_ms(&b_ended) - declined, "a_told_ms": at_ms(&told) - declined, "a_told": how}),
    );
}

/// The a–b–c group call (see the module docs).
fn group(lab: &Lab, [a, b, c]: [&mut App; 3], found: &mut Found) {
    let chat = lab.group_chat.clone();
    a.open_chat(&chat);
    c.open_chat(&chat);
    let heard = [recording(a), recording(b), recording(c)];
    let (a_from, b_from, c_from) = (a.trace().mark(), b.trace().mark(), c.trace().mark());
    wait_call_button(a, &chat);
    let (b_rang, c_rang) = (call_notifications(&b.sandbox), call_notifications(&c.sandbox));
    let asked = now_ms();
    a.ctrl_shift('c');
    let b_note = call_notification(&b.sandbox, b_rang);
    call_notification(&c.sandbox, c_rang);
    for (app, from) in [(&mut *b, b_from), (&mut *c, c_from)] {
        let banner = app.trace().wait_from(from, CONNECT, "a group call's banner", |line| {
            is(line, "call-banner", "shown") && line["conversation"] == chat.as_str()
        });
        assert_eq!(
            strings(&banner, "people"),
            [lab.mris[0].clone()],
            "{}'s banner names another caller",
            app.alias
        );
    }
    let b_answered = now_ms();
    b.sandbox.bus().invoke_action(b_note, "answer").unwrap();
    let c_answered = now_ms();
    c.ctrl_shift('s');
    let b_connected = b.in_call(b_from, CONNECT, &["incoming", "connected"]);
    let c_connected = c.in_call(c_from, CONNECT, &["incoming", "connected"]);
    let a_connected = a.in_call(a_from, CONNECT, &["chat", "connected"]);
    let setups = json!({
        "a": found.setup("group: a asked → a connected", asked, &a_connected),
        "b": found.setup("group: b answered → b connected", b_answered, &b_connected),
        "c": found.setup("group: c answered → c connected", c_answered, &c_connected),
    });
    let tones = json!({
        "a_hears_b": wait_heard(&heard[0], B_TONE.hz, "a hearing b"),
        "a_hears_c": wait_heard(&heard[0], C_TONE.hz, "a hearing c"),
        "b_hears_a": wait_heard(&heard[1], A_TONE.hz, "b hearing a"),
        "b_hears_c": wait_heard(&heard[1], C_TONE.hz, "b hearing c"),
        "c_hears_a": wait_heard(&heard[2], A_TONE.hz, "c hearing a"),
        "c_hears_b": wait_heard(&heard[2], B_TONE.hz, "c hearing b"),
    });
    let calls = [
        a_connected["call"].clone(),
        b_connected["call"].clone(),
        c_connected["call"].clone(),
    ];
    let video_from = [a.trace().mark(), b.trace().mark(), c.trace().mark()];
    for app in [&mut *a, &mut *b, &mut *c] {
        camera_on(app);
    }
    let mut tiles = BTreeMap::new();
    for ((app, from), call) in [&mut *a, &mut *b, &mut *c].into_iter().zip(video_from).zip(&calls) {
        tiles.insert(app.alias, wait_tiles(app, from, call, 2));
        app.shot("group");
    }
    let rejoined = leave_and_join(lab, [&mut *a, &mut *b, &mut *c], &calls[0], found);
    // a ends it: a hangs up, and whoever is still in the call after a while hangs up too
    let ended_from = [b.trace().mark(), c.trace().mark()];
    a.ctrl_shift('h');
    a.in_call(a_from, apps::STEP, &["ended", "hung-up"]);
    thread::sleep(AFTER_CALLER);
    let mut after_a = BTreeMap::new();
    for (app, from) in [&mut *b, &mut *c].into_iter().zip(ended_from) {
        let ended = app
            .trace()
            .all_from(from, |line| in_call_with(line, &["ended"]))
            .first()
            .cloned();
        after_a.insert(app.alias, ended.as_ref().map(|line| strings(line, "kinds")));
        if ended.is_none() {
            app.ctrl_shift('h');
            app.in_call(from, apps::STEP, &["ended", "hung-up"]);
        }
    }
    found.step(
        "group",
        json!({"setup_ms": setups, "tones": tones, "tiles": tiles, "rejoined": rejoined, "after_a_hung_up": after_a}),
    );
}

/// The participant grids of `call` that a trace's `lines` (from its first) hold, each that began at index `from` or
/// later: the MRIs of the `call-participant shown` lines the app traces for each participant, up to the
/// `call-participant self` line it closes the grid with, and that line's time (Unix ms). A grid is told by its closing
/// line, never by its lines' times: each line has its own, and a grid's lines can straddle a millisecond.
fn grids(lines: &[Value], call: &Value, from: usize) -> Vec<(i64, Vec<String>)> {
    let mut grids = Vec::new();
    let mut open: Option<(usize, Vec<String>)> = None;
    for (index, line) in lines.iter().enumerate() {
        if line["call"] != *call {
            continue;
        }
        if is(line, "call-participant", "shown") {
            open.get_or_insert_with(|| (index, Vec::new()))
                .1
                .extend(strings(line, "people"));
        } else if is(line, "call-participant", "self") {
            let (began, people) = open.take().unwrap_or((index, Vec::new()));
            if began >= from {
                grids.push((at_ms(line), people));
            }
        }
    }
    grids
}

/// Waits up to `limit` for a grid of `app`'s `call` ([`grids`]) from `from` on to hold what `wanted` asks of its
/// participants' MRIs; when (Unix ms), or the panic naming `what`.
fn wait_grid(
    app: &mut App,
    (call, from): (&Value, usize),
    limit: Duration,
    what: &str,
    wanted: impl Fn(&[String]) -> bool,
) -> i64 {
    let deadline = Instant::now() + limit;
    loop {
        let trace = app.trace();
        trace.drain();
        if let Some((at, _)) = grids(&trace.seen, call, from)
            .into_iter()
            .find(|(_, grid)| wanted(grid))
        {
            return at;
        }
        assert!(Instant::now() < deadline, "{}: {what} not within {limit:?}", app.alias);
        thread::sleep(Duration::from_millis(500));
    }
}

/// c leaves the group call and joins it again; what it took.
fn leave_and_join(lab: &Lab, [a, b, c]: [&mut App; 3], a_call: &Value, found: &mut Found) -> Value {
    let c_mri = lab.mris[2].clone();
    let (a_left, c_left) = (a.trace().mark(), c.trace().mark());
    let left = now_ms();
    c.ctrl_shift('h');
    c.in_call(c_left, apps::STEP, &["ended", "hung-up"]);
    // the service tells the others a native client left about a minute after its `leave` (a web client's within
    // seconds): seen live, the roster marks it inactive then
    let gone = wait_grid(a, (a_call, a_left), LEFT_SEEN, "a's grid without c", |grid| {
        !grid.is_empty() && !grid.contains(&c_mri)
    });
    // c joins again from the chat it shows
    c.trace().wait_from(c_left, CONNECT, "c's Join offered", |line| {
        is(line, "call-button", "shown") && strings(line, "kinds") == ["join"]
    });
    let (a_back, c_join) = (a.trace().mark(), c.trace().mark());
    let asked = now_ms();
    c.ctrl_shift('j');
    let joined = c.in_call(c_join, CONNECT, &["join", "connected"]);
    let setup = found.setup("group: c asked to join → c connected", asked, &joined);
    let back = wait_grid(a, (a_call, a_back), CONNECT, "a's grid with c again", |grid| {
        grid.contains(&c_mri)
    });
    let c_heard = recording(c);
    let tones = json!({
        "c_hears_a": wait_heard(&c_heard, A_TONE.hz, "c hearing a after its join"),
        "c_hears_b": wait_heard(&c_heard, B_TONE.hz, "c hearing b after its join"),
        "b_hears_c": wait_heard(&recording(b), C_TONE.hz, "b hearing c after its join"),
    });
    camera_on(c);
    let c_call = joined["call"].clone();
    let c_tiles = wait_tiles(c, c_join, &c_call, 2);
    // a sees c's video again: the line c's camera is on in a's grid brings PRESENTED more frames
    let line = a.trace().wait_from(a_back, VIDEO_WAIT, "c's line in a's grid", |line| {
        is(line, "call-participant", "shown")
            && line["call"] == *a_call
            && strings(line, "people") == [c_mri.clone()]
            && strings(line, "kinds").iter().any(|kind| kind == "camera-on")
            && strings(line, "kinds").iter().any(|kind| kind.starts_with("line-"))
    });
    let tile = strings(&line, "kinds")
        .into_iter()
        .find(|kind| kind.starts_with("line-"))
        .unwrap();
    let base = a
        .trace()
        .last(|line| {
            is(line, "video-tile", "presented") && line["call"] == *a_call && strings(line, "kinds") == [tile.clone()]
        })
        .and_then(|line| line["frames"].as_u64())
        .unwrap_or(0);
    a.trace().wait_from(a_back, VIDEO_WAIT, "c's video on a again", |line| {
        is(line, "video-tile", "presented")
            && line["call"] == *a_call
            && strings(line, "kinds") == [tile.clone()]
            && line["frames"].as_u64().is_some_and(|frames| frames >= base + PRESENTED)
    });
    c.shot("rejoined");
    json!({
        "a_saw_c_leave_ms": gone - left, "a_saw_c_back_ms": back - asked, "join_setup_ms": setup, "tones": tones, "c_tiles": c_tiles,
        "c_line_on_a": tile,
    })
}

#[test]
fn the_phases_default_to_every_case_in_order() {
    // SAFETY-free: read only when the variable is unset in this process
    if std::env::var_os("CALL_LIVE_PHASES").is_none() {
        assert_eq!(phases(), ["test-call", "web", "one-to-one", "decline", "group"]);
    }
}

#[test]
fn the_microphone_heard_what_its_last_self_line_then_says() {
    let own = |call: i64, kinds: [&str; 2], at: i64| json!({"view": "call-participant", "change": "self", "call": call, "kinds": kinds, "at_ms": at});
    let lines = vec![
        own(7, ["speaking", "mic-sound"], 1_000),
        own(7, ["quiet", "mic-silent"], 2_000),
        json!({"view": "call-participant", "change": "shown", "call": 7, "people": ["b"], "kinds": ["quiet"], "at_ms": 2_500}),
        own(8, ["quiet", "mic-sound"], 2_600),
        own(7, ["quiet", "mic-sound"], 3_000),
    ];
    let call = json!(7);
    assert_eq!(mic_heard_at(&lines, &call, 999), None);
    assert_eq!(mic_heard_at(&lines, &call, 1_000), Some(true));
    assert_eq!(mic_heard_at(&lines, &call, 2_000), Some(false));
    // another call's line and a participant's change nothing
    assert_eq!(mic_heard_at(&lines, &call, 2_999), Some(false));
    assert_eq!(mic_heard_at(&lines, &call, 9_000), Some(true));
}

#[test]
fn a_grid_is_its_lines_up_to_its_self_line_whatever_their_times() {
    let shown = |who: &str, at: i64| json!({"view": "call-participant", "change": "shown", "call": 7, "people": [who], "kinds": ["camera-on"], "at_ms": at});
    let closed =
        |at: i64| json!({"view": "call-participant", "change": "self", "call": 7, "kinds": ["quiet"], "at_ms": at});
    let lines = vec![
        // a grid traced before the mark: not counted
        shown("b", 1_000),
        // the mark (index 1) falls inside the next grid: its line before the mark makes it began earlier
        shown("c", 1_000),
        closed(1_000),
        // b and c, the grid straddling a millisecond
        shown("b", 2_000),
        json!({"view": "video-tile", "change": "presented", "call": 7, "kinds": ["line-1"], "at_ms": 2_000}),
        shown("c", 2_001),
        closed(2_001),
        // c gone, another call's line among the grid's
        shown("x", 3_000),
        json!({"view": "call-participant", "change": "shown", "call": 8, "people": ["d"], "at_ms": 3_000}),
        closed(3_001),
        // nobody left
        closed(4_000),
    ];
    let call = json!(7);
    assert_eq!(
        grids(&lines, &call, 1),
        [
            (2_001, vec!["b".to_owned(), "c".to_owned()]),
            (3_001, vec!["x".to_owned()]),
            (4_000, Vec::new()),
        ]
    );
    assert_eq!(
        grids(&lines, &call, 0)[0],
        (1_000, vec!["b".to_owned(), "c".to_owned()])
    );
    // c left only where a whole grid lacks it
    let c_gone = |grid: &[String]| !grid.is_empty() && !grid.contains(&"c".to_owned());
    let found = grids(&lines[..7], &call, 1);
    assert!(!found.iter().any(|(_, grid)| c_gone(grid)), "{found:?}");
    assert_eq!(
        grids(&lines, &call, 1)
            .into_iter()
            .find(|(_, grid)| c_gone(grid))
            .map(|(at, _)| at),
        Some(3_001)
    );
}
