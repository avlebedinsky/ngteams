//! Calls in the app's window (M3 Task 17), in the UI sandbox, on the mock (`harness = false`: GPUI runs on the
//! process's main thread).
//!
//! The parent runs the mock chat service with its calling service, its Trouter and its media (a loopback TURN server,
//! `teams-mock`'s `media`), and two UI sandboxes, each with its private PipeWire playing a tone of its own into its
//! microphone (a's 440 Hz, b's 660 Hz) and recording its speaker. In each it runs this test again as lab account a's
//! or b's app (`teams_app::app::launch` over an engine on the mock with its calling: its calling socket on the mock's
//! Trouter, its calls' media on the sandbox's PipeWire relayed through the mock's TURN server, unprocessed so a steady
//! tone crosses as it is; its fence on a test roster writing a `fence.log` of its own), with only the view trace's pipe
//! passed through. Then, driven by each sandbox's keyboard and b's notification's actions and read by the traces, the
//! recordings and the fence logs:
//!
//! - a calls b (Ctrl+Shift+C in the a–b chat a's settings open); b's banner, ringtone and notification come, and
//!   the ringtone is in b's recording; b answers from the notification's Answer; both connect, and each one's tone is
//!   in the other's recording; a turns its camera on (Ctrl+Shift+O: the sandbox's fake camera, H.264 the bundled
//!   source's), and b's window presents a's picture through GPUI's external surface and a's its self view (each
//!   counted by the trace's `video-tile presented`); a hangs up (Ctrl+Shift+H) and b's call ends with it;
//! - a calls b again; b declines from the notification's Decline; a's call is taken by b's voicemail (as captured) and
//!   a's in-call bar says so; a hangs up;
//! - both quit; every write in either `fence.log` was permitted.
//!
//! Without weston 15, niri, `dbus-daemon`, PipeWire or WirePlumber it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, future, io, thread};

use gpui::{App, AsyncApp, WindowingModes};
use teams_app::app::{self, Launch};
use teams_app::calling::AppCalls;
use teams_app::trace::adopt_trace;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::lab::{display_name, web_client_info};
use teams_core::{CallMedia, CallParts, Identity, Outbox, Relay};
use teams_fence::lab::Alias;
use teams_fence::{Fence, FenceLog, LiveMembershipSource, MembershipSource, Mri, Roster};
use teams_media::devices::pipewire::PipeWireDevices;
use teams_media::devices::{Devices, SAMPLE_RATE, wav};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::{RelayOptions, RelayServer};
use teams_media::session::SessionConfig;
use teams_media::tone::{Tone, amplitude_at};
use teams_media::turn::Transport as RelayTransport;
use teams_media::video::codec::H264Library;
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, TENANT};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::ringtone::{DeviceRinger, RINGTONE};
use teams_ui::settings::Settings;
use teams_ui::{Bridge, Ringer, SettingsFile, VideoFeed};
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput};
use ui_sandbox::pipewire::PipeWireOptions;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

/// Which account the child is (`a` or `b`); unset in the parent.
const CHILD_ENV: &str = "TEAMS_APP_CALL_TEST";
const TRACE_FD_ENV: &str = "TEAMS_APP_CALL_TRACE_FD";
/// The mock's origin, its Trouter's, and its TURN server's UDP address.
const ORIGIN_ENV: &str = "TEAMS_APP_CALL_ORIGIN";
const TROUTER_ENV: &str = "TEAMS_APP_CALL_TROUTER";
const RELAY_ENV: &str = "TEAMS_APP_CALL_RELAY";
/// The account's mock tokens (the mock's made-up ones).
const IC3_ENV: &str = "TEAMS_APP_CALL_IC3";
const SKYPE_ENV: &str = "TEAMS_APP_CALL_SKYPE";
/// The child's own directory: its store, settings and `fence.log`.
const DIR_ENV: &str = "TEAMS_APP_CALL_DIR";
/// The file in the sandbox dir the parent writes to have the child quit.
const QUIT_TRIGGER: &str = "quit";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(120);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(60);
/// How long the call is held once both connected, before what each heard is read.
const TALK: Duration = Duration::from_secs(4);
/// How long the ringtone is let ring before b answers.
const RING: Duration = Duration::from_secs(3);
/// Each sandbox's microphone tone.
const A_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};
/// A tone heard at least this loud in a recording, in its loudest 400 ms.
const HEARD: f32 = 0.1;
/// The frames a tile must have presented (traced at the first and every 30th).
const PRESENTED: u64 = 30;

fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("a") => child(Alias::A),
        Ok("b") => child(Alias::B),
        Ok(other) => Err(format!("unknown account {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("call window test: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent ---

fn sandbox_options(tone: Tone) -> SandboxOptions {
    SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        pipewire: Some(PipeWireOptions {
            tone,
            ..PipeWireOptions::default()
        }),
        ..SandboxOptions::default()
    }
}

fn parent() -> SandboxResult {
    let Some(a_box) = sandbox_or_skip("teams-app calls", &sandbox_options(A_TONE))? else {
        return Ok(());
    };
    let Some(b_box) = sandbox_or_skip("teams-app calls", &sandbox_options(B_TONE))? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let trouter = mock.start_trouter()?;
    let dir = tempfile::tempdir()?;
    let relay = mock.start_media(
        dir.path(),
        Tone {
            hz: 880.0,
            amplitude: 0.5,
        },
    )?;
    let lab = MockLab {
        origin: mock.origin(),
        trouter: trouter.origin().to_owned(),
        relay: relay.to_string(),
        dir: dir.path().to_owned(),
    };
    let mut a = AppUnderTest::start(&a_box, &lab, &mock, Alias::A)?;
    let mut b = AppUnderTest::start(&b_box, &lab, &mock, Alias::B)?;
    let outcome = calls(&mut a, &mut b, &a_box, &b_box);
    // a failure kills both apps first (their stderr is read to its end), then says what each wrote
    let outcome = outcome.map_err(|error| {
        format!(
            "{error}\n--- a: {}\n--- b: {}\n--- a's sandbox{}\n--- b's sandbox{}",
            a.running.fail(&"killed"),
            b.running.fail(&"killed"),
            a_box.logs(),
            b_box.logs()
        )
    });
    outcome?;
    // a's starts, follow-ups and leaves; b's answer and decline: each permitted, nothing denied
    a.quit(&a_box, &["call", "call_follow_up"])?;
    b.quit(&b_box, &["call_answer", "call_decline", "call_ringing"])?;
    drop(trouter);
    say("teams-app call window: ok");
    Ok(())
}

/// Where the children reach the mock.
struct MockLab {
    origin: String,
    trouter: String,
    relay: String,
    dir: PathBuf,
}

/// A child app in its sandbox: its process, its trace, its own dir.
struct AppUnderTest {
    alias: Alias,
    running: Running,
    trace: TraceReader,
    dir: PathBuf,
}

impl AppUnderTest {
    fn start(sandbox: &Sandbox, lab: &MockLab, mock: &ChatServiceMock, alias: Alias) -> SandboxResult<Self> {
        let mri = mri_of(alias);
        let dir = lab.dir.join(alias.to_string());
        fs::create_dir_all(&dir)?;
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let ic3 = mock.token(mri).ok_or("the mock's token")?;
        let skype = mock.skypetoken(mri).ok_or("the mock's skypetoken")?;
        let account = alias.to_string();
        let own = dir.to_string_lossy().into_owned();
        let child = sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, account.as_str()),
                (TRACE_FD_ENV, &fd),
                (ORIGIN_ENV, &lab.origin),
                (TROUTER_ENV, &lab.trouter),
                (RELAY_ENV, &lab.relay),
                (IC3_ENV, &ic3),
                (SKYPE_ENV, &skype),
                (DIR_ENV, &own),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let running = Running::new(child)?;
        let mut app = Self {
            alias,
            running,
            trace: TraceReader::start(reader),
            dir,
        };
        app.running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| app.running.fail(&format!("{alias}: no ready line: {error:?}")))?;
        app.trace.wait_for("status", "connected")?;
        app.trace.wait_for("calling", "registered")?;
        Ok(app)
    }

    /// Waits for an `in-call shown` line whose kinds hold every one of `kinds`; the line.
    fn in_call(&mut self, kinds: &[&str]) -> SandboxResult<serde_json::Value> {
        let what = format!("{}: in-call shown {kinds:?}", self.alias);
        self.trace.next_where(&what, |line| {
            line["view"] == "in-call"
                && line["change"] == "shown"
                && kinds
                    .iter()
                    .all(|kind| strings(line, "kinds").iter().any(|shown| shown == kind))
        })
    }

    /// Asks the app to quit and waits for it to exit cleanly; its `fence.log` permitted every write, the write kinds
    /// `kinds` among them.
    fn quit(mut self, sandbox: &Sandbox, kinds: &[&str]) -> SandboxResult {
        fs::write(sandbox.path(QUIT_TRIGGER), "")?;
        self.running
            .line_where(WAIT, |line| line == "quit")
            .map_err(|error| self.running.fail(&format!("{}: didn't quit: {error:?}", self.alias)))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("{}: exited {status}: {stderr}", self.alias).into());
        }
        let log = fs::read_to_string(self.dir.join("fence.log"))?;
        let denied: Vec<&str> = log.lines().filter(|line| line.contains("\"deny\"")).collect();
        if !denied.is_empty() {
            return Err(format!("{}: the fence denied {denied:#?}", self.alias).into());
        }
        for kind in kinds {
            if !log.contains(&format!("\"kind\":\"{kind}\"")) {
                return Err(format!("{}: no {kind} write in its fence.log: {log}", self.alias).into());
            }
        }
        if let Some(odd) = self.trace.seen.iter().find(|line| !line.is_object()) {
            return Err(format!("{}: a trace line isn't JSON: {odd}", self.alias).into());
        }
        Ok(())
    }
}

fn mri_of(alias: Alias) -> &'static str {
    match alias {
        Alias::A => LAB_A,
        Alias::B => LAB_B,
        Alias::C => LAB_C,
    }
}

/// The child's view trace, line by line as it comes.
struct TraceReader {
    lines: mpsc::Receiver<serde_json::Value>,
    seen: Vec<serde_json::Value>,
}

impl TraceReader {
    fn start(reader: io::PipeReader) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let value = serde_json::from_str(&line).unwrap_or(serde_json::Value::String(line));
                let _ = sender.send(value);
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult<serde_json::Value> {
        let what = format!("{view} {change}");
        if let Some(line) = self
            .seen
            .iter()
            .find(|line| line["view"] == view && line["change"] == change)
        {
            return Ok(line.clone());
        }
        self.next_where(&what, |line| line["view"] == view && line["change"] == change)
    }

    fn next_where(
        &mut self,
        what: &str,
        found: impl Fn(&serde_json::Value) -> bool,
    ) -> SandboxResult<serde_json::Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line {what}; seen {:?}", self.seen))?;
            let hit = found(&line);
            self.seen.push(line.clone());
            if hit {
                return Ok(line);
            }
        }
    }
}

fn strings(line: &serde_json::Value, key: &str) -> Vec<String> {
    line[key]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Ctrl+Shift+`key` on the sandbox's keyboard.
fn ctrl_shift(input: &mut VirtualInput, key: char) -> SandboxResult {
    let (code, _) = input.keymap().key_for(key).ok_or("not on the US keymap")?;
    input.key_with(&[Modifier::Ctrl, Modifier::Shift], code)
}

/// The loudest `hz` gets in any 400 ms of the `stretch` before the end of the recording at `path`.
fn loudest(path: &Path, hz: f32, stretch: Duration) -> SandboxResult<f32> {
    let samples = wav::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let per_ms = usize::try_from(SAMPLE_RATE / 1_000)?;
    let window = 400 * per_ms;
    let wanted = usize::try_from(stretch.as_millis())? * per_ms;
    let tail = &samples[samples.len().saturating_sub(wanted)..];
    if tail.len() < window {
        return Err(format!("{} holds {} samples", path.display(), samples.len()).into());
    }
    Ok((0..=tail.len() - window)
        .step_by(window / 4)
        .map(|at| amplitude_at(&tail[at..at + window], hz))
        .fold(0.0, f32::max))
}

fn recording(sandbox: &Sandbox) -> SandboxResult<PathBuf> {
    Ok(sandbox.pipewire().ok_or("no PipeWire")?.recording())
}

/// Where the screenshots are kept: `target/ui-shots/`, never committed.
fn shot(sandbox: &Sandbox, name: &str) -> SandboxResult {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    // the frames after the trace's line
    thread::sleep(Duration::from_millis(500));
    let png = sandbox.screenshot(name, true)?;
    fs::copy(&png, dir.join(name))?;
    Ok(())
}

/// The id of the notification of an incoming call on `sandbox`'s bus that wasn't there before (`seen` of them).
fn call_notification(sandbox: &Sandbox, seen: usize) -> SandboxResult<u32> {
    wait_until(WAIT, || {
        sandbox
            .bus()
            .notifications()
            .into_iter()
            .filter(|notification| notification.actions.iter().any(|action| action == "answer"))
            .nth(seen)
    })
    .map(|notification| notification.id)
    .ok_or_else(|| format!("no incoming call's notification: {:?}", sandbox.bus().notifications()).into())
}

fn calls(a: &mut AppUnderTest, b: &mut AppUnderTest, a_box: &Sandbox, b_box: &Sandbox) -> SandboxResult {
    let mut a_keys = a_box.input()?;

    // a calls b from the a–b chat its settings opened
    a.trace.wait_for("call-button", "shown")?;
    ctrl_shift(&mut a_keys, 'c')?;
    a.in_call(&["chat", "ringing"])?;
    let banner = b.trace.wait_for("call-banner", "shown")?;
    if banner["people"] != serde_json::json!([LAB_A]) {
        return Err(format!("b's banner names another caller: {banner}").into());
    }
    b.trace.wait_for("ringtone", "started")?;
    b.trace.wait_for("call-notification", "shown")?;
    let ringing = call_notification(b_box, 0)?;
    shot(b_box, "teams-app-call-incoming.png")?;
    thread::sleep(RING);
    let ringtone = loudest(&recording(b_box)?, RINGTONE[0].hz, RING)?;
    say(&format!("b's ringtone at {ringtone:.3}"));
    if ringtone < HEARD {
        return Err(format!("b's ringtone isn't in b's recording ({ringtone:.3})").into());
    }

    // b answers from the notification: both connect, each hears the other
    b_box.bus().invoke_action(ringing, "answer")?;
    b.trace.wait_for("call-notification", "answered")?;
    b.trace.wait_for("ringtone", "stopped")?;
    b.in_call(&["incoming", "connected"])?;
    a.in_call(&["chat", "connected"])?;
    shot(a_box, "teams-app-call-connected.png")?;
    thread::sleep(TALK);
    let a_heard = loudest(&recording(a_box)?, B_TONE.hz, Duration::from_secs(2))?;
    let b_heard = loudest(&recording(b_box)?, A_TONE.hz, Duration::from_secs(2))?;
    say(&format!("a heard b at {a_heard:.3}, b heard a at {b_heard:.3}"));
    if a_heard < HEARD || b_heard < HEARD {
        return Err(format!("the tones didn't cross: a heard b at {a_heard:.3}, b heard a at {b_heard:.3}").into());
    }

    // a turns its camera on (off when the call started): b's window presents a's picture, a's its self view
    ctrl_shift(&mut a_keys, 'o')?;
    a.in_call(&["connected", "camera-on"])?;
    let presented = |tile: &'static str| {
        move |line: &serde_json::Value| {
            line["view"] == "video-tile"
                && line["change"] == "presented"
                && strings(line, "kinds") == [tile]
                && line["frames"].as_u64().is_some_and(|frames| frames >= PRESENTED)
        }
    };
    let remote = b
        .trace
        .next_where("b: the remote tile presented", presented("remote"))?;
    let own = a.trace.next_where("a: the self view presented", presented("self"))?;
    say(&format!(
        "b presented {} of a's frames, a {} of its own",
        remote["frames"], own["frames"]
    ));
    shot(b_box, "teams-app-call-video.png")?;
    shot(a_box, "teams-app-call-self-view.png")?;

    // a hangs up: b's call ends with it
    ctrl_shift(&mut a_keys, 'h')?;
    a.in_call(&["ended", "hung-up"])?;
    b.in_call(&["ended", "remote"])?;

    // a calls again; b declines from the notification; b's voicemail takes a's call, and a sees it
    a.trace.next_where("a: call-button shown call", |line| {
        line["view"] == "call-button" && strings(line, "kinds") == ["call"]
    })?;
    ctrl_shift(&mut a_keys, 'c')?;
    a.in_call(&["chat", "ringing"])?;
    b.trace.next_where("b: call-notification shown again", |line| {
        line["view"] == "call-notification" && line["change"] == "shown"
    })?;
    let second = call_notification(b_box, 1)?;
    b_box.bus().invoke_action(second, "decline")?;
    b.trace.next_where("b: call-notification declined", |line| {
        line["view"] == "call-notification" && line["change"] == "declined"
    })?;
    b.in_call(&["incoming", "ended", "declined"])?;
    a.in_call(&["voicemail"])?;
    shot(a_box, "teams-app-call-declined.png")?;
    ctrl_shift(&mut a_keys, 'h')?;
    a.in_call(&["ended", "hung-up"])?;
    Ok(())
}

// --- the children ---

/// One account's mock tokens: its `ic3` token and its skypetoken; they never run out in a test.
struct Tokens {
    ic3: String,
    skypetoken: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn bearer_rejected(&self, _audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        future::ready(Ok(Some(Secret::new(self.skypetoken.clone()))))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(AccessToken {
            token: Secret::new(self.ic3.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        self.ic3()
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

fn env(name: &str) -> SandboxResult<String> {
    Ok(std::env::var(name).map_err(|_| format!("{name} isn't set"))?)
}

/// The account's engine on the mock, as `teams_core::lab::start` builds one but for the mock origins, a test roster
/// and a `fence.log` of its own, with its calling: its calls' media on `devices`, relayed through the mock's TURN
/// server, unprocessed.
fn engine(
    alias: Alias,
    devices: Arc<dyn Devices + Send + Sync>,
    feed: VideoFeed,
    dir: &Path,
) -> SandboxResult<EngineHandle> {
    let mri = mri_of(alias);
    let tokens = Arc::new(Tokens {
        ic3: env(IC3_ENV)?,
        skypetoken: env(SKYPE_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let fence = Arc::new(
        Fence::new(
            Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), [Mri::new(ECHO)]),
            FenceLog::from_writer(fs::File::create(dir.join("fence.log"))?),
        )
        .with_own_account(Mri::new(mri)),
    );
    let origin = env(ORIGIN_ENV)?;
    let trouter_origin = env(TROUTER_ENV)?;
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &origin);
    let transport = Arc::new(http()?);
    let live = LiveMembershipSource::with_mock_origin(&origin)?;
    live.set_credential_source(transport.clone());
    let members: Arc<dyn MembershipSource + Send> = Arc::new(live);
    let options = Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
        backoff_jitter_percent: 0,
        ..Options::default()
    };
    let trouter = || -> SandboxResult<_> {
        Ok(
            Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), &trouter_origin)?
                .with_options(options),
        )
    };
    let calling = trouter()?.for_calls(CallStack::new(Arc::clone(&fence), Arc::clone(&members)))?;
    let name = display_name(&format!("bot-{alias}@lab.example.test")).to_owned();
    let identity = Identity {
        mri: Mri::new(mri),
        display_name: Some(name.clone()),
        endpoint_id: String::new(),
        language_id: Some("en-us".into()),
        tenant_id: TENANT.into(),
        client: web_client_info(),
        region: Some("amer".into()),
        partition: Some("amer01".into()),
    };
    let media = CallMedia {
        devices,
        video: Some(teams_core::lab::call_video(H264Library::Bundled, Arc::new(feed))),
        relay: Relay::Fixed(vec![RelayServer {
            addr: env(RELAY_ENV)?.parse()?,
            transport: RelayTransport::Udp,
        }]),
        relay_options: RelayOptions::default(),
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
            ..SessionConfig::default()
        },
    };
    let push = Push::new(trouter()?);
    let handle = engine::start(Parts {
        calls: Some(CallParts::new(calling, identity, media)),
        store: Store::open(&dir.join("store.sqlite"))?,
        client: Arc::new(Client::new(http()?)),
        outbox: Outbox::new(transport, fence, members, Author::new(Mri::new(mri), name)),
        push: Some(push),
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })?;
    Ok(handle)
}

fn child(alias: Alias) -> SandboxResult {
    teams_app::guard::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let devices: Arc<dyn Devices + Send + Sync> = Arc::new(PipeWireDevices::connect()?);
    let feed = VideoFeed::new();
    let engine = engine(alias, Arc::clone(&devices), feed.clone(), &dir)?;
    let settings = SettingsFile::new(dir.join("settings.toml"));
    if alias == Alias::A {
        let opened = Settings {
            selected_chat: Some(AB_CHAT.to_owned()),
            ..Settings::default()
        };
        settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    }
    let ringer: Arc<dyn Ringer> = Arc::new(DeviceRinger::new(devices));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                alias,
                engine,
                broker: None,
                signed_out: None,
                trace,
                settings,
                bus: teams_ui::bus::address_from_env(),
                diagnostics: None,
                calls: Some(AppCalls {
                    ringer: Some(ringer),
                    echo: Some(Mri::new(ECHO)),
                    video: Some(feed),
                }),
            },
        );
        match launched {
            Ok(launched) => {
                quit_when_asked(launched.bridge, cx);
                say("ready");
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    say("quit");
    Ok(())
}

/// Once the parent writes [`QUIT_TRIGGER`] in the sandbox dir: the engine shut down, then the app quits.
fn quit_when_asked(bridge: gpui::Entity<Bridge>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let trigger = PathBuf::from(dir).join(QUIT_TRIGGER);
    cx.spawn(async move |cx: &mut AsyncApp| {
        while !trigger.exists() {
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
        cx.update(|cx| {
            let stopped = bridge.update(cx, Bridge::shutdown);
            app::quit_after(stopped, cx);
        });
    })
    .detach();
}
