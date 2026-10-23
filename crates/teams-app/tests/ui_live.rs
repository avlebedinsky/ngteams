//! Live, against the company tenant with the lab accounts: M2's acceptance run of the app (M2 Task 25).
//!
//! This test is an orchestrator only. It opens no fence, holds no token and sends nothing: it starts a UI sandbox per
//! lab account (a nested niri on a headless weston, a private bus with its fake notification server and tray watcher,
//! the no-op opener; `ui-sandbox`), spawns in each the app built by `cargo build -p teams-app --target-dir target/lab`
//! (its path in `TEAMS_APP_BIN`) for a, b and c, one app process each, drives them by keyboard through the sandbox's
//! virtual keyboard (the chat switcher, the compose box, the message list's keys, the search; never a screen
//! coordinate), and reads their view traces (ids and kinds, never text) and `fence.log`. The binary this test build
//! makes has `test-support` unified in and refuses to run, so a wrong path fails the test rather than writes.
//!
//! **Credentials.** For each account it starts that binary's credential broker (`teams-app secrets --lab <alias>`)
//! outside the sandbox, with this process's session bus (the user's, where the lab's tokens are), and hands the other
//! end of the broker's socket pair to the app it spawns in the sandbox (`--secrets-fd`). It never reads or writes the
//! socket itself: both ends are closed here once the two processes hold them. The app's environment is the sandbox's
//! (checked from `/proc`: its bus and its compositor, no `DISPLAY`). Every broker has exited when the run ends.
//!
//! **The steps** (each app in a sandbox of its own, so each window is focused, as a user's would be):
//! - a and b open their 1:1 chat, c the a–b–c group chat (`chats.toml`'s `abc`), by the switcher (Ctrl+K, Down to the
//!   chat's row as the chat list's trace orders them, Enter).
//! - a types and sends [`MESSAGES`] messages to b; each is shown in b's chat view; send (a's Enter, pressed once the
//!   app has taken in what was typed: its `compose changed` lines have gone quiet) → shown (b's `chat-view shown`
//!   naming it) has a p95 under [`P95_LIMIT_MS`] (p50, p95 and max reported, with Enter → a's `compose sent` and
//!   `compose sent` → shown apart).
//! - a edits its last message (Up in the empty box), replies to it (Escape to the list, Down to the newest, `r`) and
//!   reacts to the reply (`a`, `1`): b shows the edit, the reply and the reaction.
//! - a sends to the group chat: c shows it; b, viewing the 1:1 chat, gets a notification on its sandbox's bus, whose
//!   activation opens the group chat, where b shows the message.
//! - names and presence: b's header for the 1:1 chat names a from people's names with a's picture, and shows a
//!   available; a sets Busy from the switcher's "Set status: Busy", b's header shows busy; a sets Automatic again, b's
//!   header shows available.
//! - b's search (Ctrl+Shift+F) finds one of a's messages; a channel opens read-only from the switcher ("General").
//! - b's window closed into the tray: a sends, b notifies once, and the window opened again from the tray shows it.
//! - b's app quit from the tray (its broker exits with it): a sends [`GAP_MESSAGES`], b's app started again shows every
//!   message of the run in the 1:1 chat exactly once.
//! - Images and files only as far as M2 Tasks 21–23 got: none is sent (sending stays deferred).
//! - `fence.log` over the run: whole lines; allows only on the a–b chat and the recorded group chat (their messages,
//!   edits, reactions, read receipts, typing) and the lab accounts' own availability; no deny but the fence refusing
//!   the profile lookup (`PostRead`) of someone outside the lab, a channel's member, before anything went; own-state
//!   requests, `PostRead` and frames only from the reviewed lists.
//!
//! Screenshots of each step go to `<data_dir>/screenshots/<time>/` (`0700`), and the traces, the processes' output and
//! `summary.json` to `<data_dir>/ui-live/<time>/`, never the repo. No output holds a token.
//!
//! Built only with `--features live`, and even then it skips unless `PROTO_LAB_LIVE=1`, so
//! `cargo test --all-features` stays offline. Run it under the lab's live lock (`flock
//! ~/.local/share/teams-lab/live.lock`) with an outer `timeout --signal=INT`, so no other lab run shares the accounts
//! or `fence.log`.

// test code throughout (its helpers unwrap and panic like the tests do)
#![cfg(test)]

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, DirBuilder};
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use teams_fence::lab::{Alias, LabChats, LabConfig, LabLocations};
use teams_fence::one_to_one_id;
use teams_proto::{Authenticate, Disconnect, EndpointRegistration, MessageLossAck, OwnFrame, OwnState, Ping};
use teams_ui::tray::{ITEM_PATH, MENU_PATH, MENU_QUIT};
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, Tools, wait_until};

/// How many messages a sends to b while b shows their chat.
const MESSAGES: usize = 20;
/// How many messages a sends while b's app is closed.
const GAP_MESSAGES: usize = 3;
/// The acceptance target for a's Enter → b's chat view showing the message, at the 95th percentile.
const P95_LIMIT_MS: i64 = 2000;
/// From an app's start to its window, its push channel and its chat list.
const STARTUP: Duration = Duration::from_secs(180);
/// How long a message, an edit, a reaction or a notification may take to show on the other side.
const ARRIVAL: Duration = Duration::from_secs(60);
/// How long a view change the keyboard asked for may take.
const STEP: Duration = Duration::from_secs(30);
/// How long a presence change may take to show (the engine reads presence once a minute).
const PRESENCE: Duration = Duration::from_secs(180);
/// How long an app and its broker get to exit once asked.
const EXIT: Duration = Duration::from_secs(60);
/// How long the run waits after a notification for a second one of the same message, which mustn't come.
const QUIET: Duration = Duration::from_secs(5);
/// How long the run lets the switcher take in a query before it presses Enter (the switcher traces no keystroke): the
/// debug build takes in the virtual keyboard's keystrokes more slowly than it types them.
const SETTLE: Duration = Duration::from_secs(4);
/// The compose box has taken in what was typed once no `compose changed` came for this long: Enter is pressed only
/// then, so it doesn't wait behind keystrokes the debug build hasn't handled yet.
const TYPED: Duration = Duration::from_millis(1500);
/// The sandboxes' refresh rate, mHz: three compositor pairs repainting at 60 Hz take CPU the apps need, and the trace
/// tells what a view shows when its model changes, not when a frame is presented, so a slower output doesn't move it.
const REFRESH_MHZ: u32 = 20_000;
/// The switcher lists at most this many chats (`teams_ui::switcher::MAX_MATCHES`).
const SWITCHER_CHATS: usize = 50;

/// The lab as the run needs it.
struct Lab {
    binary: PathBuf,
    ab_chat: String,
    group_chat: String,
    accounts: Vec<String>,
    fence_log: PathBuf,
    out: PathBuf,
    shots: PathBuf,
    tools: Tools,
}

/// The lab, or `None` (after a note) when live tests aren't asked for. Fails without `TEAMS_APP_BIN` or the sandbox's
/// tools.
fn lab() -> Option<Lab> {
    if std::env::var("PROTO_LAB_LIVE").as_deref() != Ok("1") {
        eprintln!("skipped: a live lab test; set PROTO_LAB_LIVE=1 to run it");
        return None;
    }
    let binary = std::env::var_os("TEAMS_APP_BIN").map_or_else(
        || panic!("TEAMS_APP_BIN is unset: build with `cargo build -p teams-app --target-dir target/lab`"),
        PathBuf::from,
    );
    assert!(binary.is_file(), "TEAMS_APP_BIN {} is not a file", binary.display());
    let tools = Tools::find().unwrap_or_else(|missing| panic!("the UI sandbox's tools: {missing}"));
    let config = LabConfig::load(&LabLocations::from_env().unwrap()).unwrap();
    let mri = |alias| config.account(alias).mri.clone().unwrap();
    let ab_chat = one_to_one_id(&mri(Alias::A), &mri(Alias::B)).unwrap();
    let group_chat = LabChats::load(config.paths()).unwrap().abc.unwrap();
    let stamp = stamp();
    Some(Lab {
        binary,
        ab_chat,
        group_chat,
        accounts: [Alias::A, Alias::B, Alias::C].map(mri).to_vec(),
        fence_log: config.paths().fence_log(),
        out: private_dir(&config.paths().data_dir().join("ui-live").join(&stamp)),
        shots: private_dir(&config.paths().data_dir().join("screenshots").join(&stamp)),
        tools,
    })
}

/// The UTC time now, for the run's dirs.
fn stamp() -> String {
    let out = Command::new("date").args(["-u", "+%Y%m%dT%H%M%SZ"]).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// `dir`, made `0700` with its parents.
fn private_dir(dir: &Path) -> PathBuf {
    DirBuilder::new().recursive(true).mode(0o700).create(dir).unwrap();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    dir.to_owned()
}

fn now_ms() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(since.as_millis()).unwrap()
}

// --- the view trace ---

/// An app's view trace, line by line as it comes; every line is also kept in a file of the run.
struct Trace {
    lines: mpsc::Receiver<Value>,
    seen: Vec<Value>,
    name: String,
}

impl Trace {
    fn start(reader: io::PipeReader, file: PathBuf, name: String) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            let mut kept = fs::File::create(file).unwrap();
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let _ = writeln!(kept, "{line}");
                let value = serde_json::from_str(&line).unwrap_or(Value::String(line));
                if sender.send(value).is_err() {
                    return;
                }
            }
        });
        Self {
            lines,
            seen: Vec::new(),
            name,
        }
    }

    /// The lines that came so far, without waiting.
    fn drain(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            assert!(line.is_object(), "{}: a trace line isn't JSON: {line}", self.name);
            self.seen.push(line);
        }
    }

    /// How many lines came so far: a mark to wait from.
    fn mark(&mut self) -> usize {
        self.drain();
        self.seen.len()
    }

    /// The first line from `from` on that `found` takes, waiting up to `limit` for it; named `what` in the failure.
    fn wait_from(&mut self, from: usize, limit: Duration, what: &str, found: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + limit;
        loop {
            self.drain();
            if let Some(line) = self.seen[from.min(self.seen.len())..].iter().find(|line| found(line)) {
                return line.clone();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "{}: no trace line {what} within {limit:?}", self.name);
            match self.lines.recv_timeout(left.min(Duration::from_millis(200))) {
                Ok(line) => {
                    assert!(line.is_object(), "{}: a trace line isn't JSON: {line}", self.name);
                    self.seen.push(line);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("{}: the trace ended (the app exited) before {what}", self.name)
                }
            }
        }
    }

    /// The last line `found` takes, among those that came.
    fn last(&mut self, found: impl Fn(&Value) -> bool) -> Option<Value> {
        self.drain();
        self.seen.iter().rev().find(|line| found(line)).cloned()
    }

    /// The lines from `from` on that `found` takes.
    fn count_from(&mut self, from: usize, found: impl Fn(&Value) -> bool) -> usize {
        self.drain();
        self.seen[from.min(self.seen.len())..]
            .iter()
            .filter(|line| found(line))
            .count()
    }
}

/// Whether `line` is `view` `change`.
fn is(line: &Value, view: &str, change: &str) -> bool {
    line["view"] == view && line["change"] == change
}

/// The strings of a trace line's array `key`.
fn strings(line: &Value, key: &str) -> Vec<String> {
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

/// Whether `line` is a `chat-view shown` of `chat`.
fn chat_view_of(line: &Value, chat: &str) -> bool {
    is(line, "chat-view", "shown") && line["conversation"] == chat
}

/// The kind a `chat-view shown` line gives message `id`.
fn kind_of(line: &Value, id: &str) -> Option<String> {
    let at = strings(line, "messages").iter().position(|shown| shown == id)?;
    strings(line, "kinds").get(at).cloned()
}

// --- the apps ---

/// A process killed (and reaped) when dropped.
struct Reaped(Option<Child>);

impl Reaped {
    /// Waits up to `limit` for the exit.
    fn exit_within(&mut self, limit: Duration) -> Option<ExitStatus> {
        let child = self.0.as_mut()?;
        let status = wait_until(limit, || child.try_wait().ok().flatten());
        if status.is_some() {
            self.0 = None;
        }
        status
    }
}

impl Drop for Reaped {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One run of an app: the app in its sandbox, its broker outside, its trace.
struct Run {
    app: Option<Running>,
    broker: Reaped,
    trace: Trace,
    pid: u32,
}

/// A lab account's app: its sandbox (kept across a restart), its keyboard, its current run.
struct App {
    alias: &'static str,
    sandbox: Sandbox,
    input: VirtualInput,
    run: Option<Run>,
    starts: usize,
    shots: usize,
}

/// The variables the broker takes from this process: its bus (the user's session bus, where the lab's tokens are)
/// and where the lab's config and data are. Nothing of a display.
const BROKER_ENV: [&str; 7] = [
    "PATH",
    "HOME",
    "LANG",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
];

impl App {
    fn new(lab: &Lab, alias: &'static str) -> Self {
        let options = SandboxOptions {
            niri_extra: NIRI_FULL_WINDOW,
            refresh_mhz: Some(REFRESH_MHZ),
            ..SandboxOptions::default()
        };
        let sandbox = Sandbox::start(&lab.tools, &options).unwrap_or_else(|error| panic!("{alias}'s sandbox: {error}"));
        let input = sandbox.input().unwrap();
        let mut app = Self {
            alias,
            sandbox,
            input,
            run: None,
            starts: 0,
            shots: 0,
        };
        app.start(lab);
        app
    }

    fn run(&mut self) -> &mut Run {
        self.run
            .as_mut()
            .unwrap_or_else(|| panic!("{}'s app isn't running", self.alias))
    }

    fn trace(&mut self) -> &mut Trace {
        &mut self.run().trace
    }

    /// Starts the broker outside the sandbox and the app inside it, the socket pair between them, the trace on a pipe;
    /// then waits for the window, the push channel and the chat list.
    fn start(&mut self, lab: &Lab) {
        self.starts += 1;
        let name = format!("{}-{}", self.alias, self.starts);
        let (broker_end, app_end) = UnixStream::pair().unwrap();
        let file = |suffix: &str| fs::File::create(lab.out.join(format!("{name}-{suffix}"))).unwrap();
        let mut command = Command::new(&lab.binary);
        command.env_clear();
        for variable in BROKER_ENV {
            if let Some(value) = std::env::var_os(variable) {
                command.env(variable, value);
            }
        }
        command
            .args([
                "secrets",
                "--lab",
                self.alias,
                "--fd",
                &broker_end.as_raw_fd().to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(file("broker.out"))
            .stderr(file("broker.err"));
        ui_sandbox::spawn::pass_only(&mut command, &[broker_end.as_fd()]);
        let broker = Reaped(Some(command.spawn().unwrap()));
        let (reader, writer) = io::pipe().unwrap();
        let (secrets_fd, trace_fd) = (app_end.as_raw_fd().to_string(), writer.as_raw_fd().to_string());
        let child = self
            .sandbox
            .spawn_with_fds(
                &lab.binary,
                &[
                    "--lab",
                    self.alias,
                    "--secrets-fd",
                    &secrets_fd,
                    "--trace-fd",
                    &trace_fd,
                ],
                &[],
                &[app_end.as_fd(), writer.as_fd()],
            )
            .unwrap();
        // the two processes hold the socket's ends and the trace's writer from here: this process keeps none
        drop((broker_end, app_end, writer));
        let pid = child.id();
        let app = Running::new(child).unwrap();
        let trace = Trace::start(reader, lab.out.join(format!("{name}-trace.jsonl")), name);
        self.run = Some(Run {
            app: Some(app),
            broker,
            trace,
            pid,
        });
        let trace = self.trace();
        trace.wait_from(0, STARTUP, "window opened", |line| is(line, "window", "opened"));
        // read once the app runs (its window is up): right after the fork, `/proc` still shows the parent's
        self.environment_is_the_sandbox_s(pid);
        self.trace()
            .wait_from(0, STARTUP, "status connected", |line| is(line, "status", "connected"));
    }

    /// The app's environment (from `/proc`) names the sandbox's bus and compositor and no display of the desktop's.
    fn environment_is_the_sandbox_s(&self, pid: u32) {
        let environ = fs::read(format!("/proc/{pid}/environ")).unwrap();
        let variables: BTreeMap<String, String> = environ
            .split(|&byte| byte == 0)
            .filter_map(|entry| {
                let entry = String::from_utf8_lossy(entry);
                entry
                    .split_once('=')
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
            })
            .collect();
        let alias = self.alias;
        assert_eq!(
            variables.get("DBUS_SESSION_BUS_ADDRESS").map(String::as_str),
            Some(self.sandbox.bus().address()),
            "{alias}'s app isn't on its sandbox's bus"
        );
        assert_eq!(
            variables.get("WAYLAND_DISPLAY").map(PathBuf::from).as_deref(),
            Some(self.sandbox.wayland_socket()),
            "{alias}'s app isn't on its sandbox's compositor"
        );
        assert!(!variables.contains_key("DISPLAY"), "{alias}'s app has a DISPLAY");
        assert!(
            !variables.contains_key("NIRI_SOCKET"),
            "{alias}'s app has a NIRI_SOCKET"
        );
    }

    /// A screenshot of the window, kept in the run's screenshots dir as `<n>-<alias>-<what>.png` (`0600`).
    fn shot(&mut self, lab: &Lab, what: &str) {
        self.shots += 1;
        // the frames after the trace's line
        thread::sleep(Duration::from_millis(500));
        let name = format!("{}-{what}.png", self.alias);
        let Ok(png) = self.sandbox.screenshot(&name, true) else {
            eprintln!("{}: no screenshot of {what}", self.alias);
            return;
        };
        let kept = lab.shots.join(format!("{:02}-{name}", self.shots));
        fs::copy(&png, &kept).unwrap();
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o600)).unwrap();
        let _ = fs::remove_file(png);
    }

    fn ctrl(&mut self, letter: char) {
        let (key, _) = self.input.keymap().key_for(letter).unwrap();
        self.input.key_with(&[Modifier::Ctrl], key).unwrap();
    }

    fn key(&mut self, key: u32) {
        self.input.key(key).unwrap();
    }

    fn type_text(&mut self, text: &str) {
        self.input.type_text(text).unwrap();
    }

    /// Types `text` into the compose box that has the focus, and waits until the app has taken it in: a `compose
    /// changed` came, and then none for [`TYPED`].
    fn type_into_compose(&mut self, text: &str) {
        let from = self.trace().mark();
        self.type_text(text);
        let changed = |line: &Value| is(line, "compose", "changed");
        self.trace().wait_from(from, STEP, "compose changed", changed);
        let deadline = Instant::now() + STEP;
        let mut seen = self.trace().count_from(from, changed);
        loop {
            thread::sleep(TYPED);
            let now = self.trace().count_from(from, changed);
            if now == seen {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{}: the compose box kept changing",
                self.alias
            );
            seen = now;
        }
    }

    /// Opens `chat` by the switcher: Ctrl+K, Down to its row (the chat list's order, as its trace last told it),
    /// Enter; the `chat opened` line must name it (tried again when the list moved meanwhile).
    fn open_chat(&mut self, chat: &str) {
        for _ in 0..3 {
            let listed = self
                .trace()
                .wait_from(0, STARTUP, "chat-list shown with the chat", |line| {
                    is(line, "chat-list", "shown") && strings(line, "conversations").iter().any(|id| id == chat)
                });
            let listed = self
                .trace()
                .last(|line| is(line, "chat-list", "shown"))
                .filter(|last| strings(last, "conversations").iter().any(|id| id == chat))
                .unwrap_or(listed);
            let at = strings(&listed, "conversations")
                .iter()
                .position(|id| id == chat)
                .unwrap();
            assert!(
                at < SWITCHER_CHATS,
                "{}: {chat} is row {at}, past the switcher's",
                self.alias
            );
            let from = self.trace().mark();
            self.ctrl('k');
            self.trace()
                .wait_from(from, STEP, "switcher opened", |line| is(line, "switcher", "opened"));
            for _ in 0..at {
                self.key(keys::DOWN);
            }
            self.key(keys::ENTER);
            let opened = self
                .trace()
                .wait_from(from, STEP, "chat opened", |line| is(line, "chat", "opened"));
            if opened["conversation"] == chat {
                return;
            }
        }
        panic!("{}: the switcher didn't open {chat}", self.alias);
    }

    /// The tray item's bus name (`org.kde.StatusNotifierItem-<pid>-1`) and a connection to the sandbox's bus.
    fn tray(&mut self) -> (zbus::blocking::Connection, String) {
        let name = format!("org.kde.StatusNotifierItem-{}-1", self.run().pid);
        (self.sandbox.bus().connect().unwrap(), name)
    }

    /// The tray menu's "Quit": the app quits and its broker exits once the socket closes. Both exit cleanly; the
    /// app's stderr is kept in the run's dir.
    fn quit(&mut self, lab: &Lab) {
        let (host, name) = self.tray();
        let menu = zbus::blocking::Proxy::new(&host, name.as_str(), MENU_PATH, "com.canonical.dbusmenu").unwrap();
        menu.call_method("Event", &(MENU_QUIT, "clicked", zbus::zvariant::Value::from(""), 0_u32))
            .unwrap();
        let alias = self.alias;
        let mut run = self.run.take().unwrap();
        let (status, stderr) = run.app.take().unwrap().finish(EXIT).unwrap();
        fs::write(lab.out.join(format!("{alias}-{}-app.err", self.starts)), &stderr).unwrap();
        assert!(status.success(), "{alias}'s app exited {status}: {stderr}");
        let broker = run.broker.exit_within(EXIT);
        assert!(
            broker.is_some_and(|status| status.success()),
            "{alias}'s broker didn't exit cleanly once its app quit: {broker:?}"
        );
    }
}

impl Drop for App {
    /// An app still running when the run fails: its stderr kept beside the trace, then killed with its broker.
    fn drop(&mut self) {
        if let Some(run) = self.run.as_mut()
            && let Some(mut app) = run.app.take()
        {
            let stderr = app.fail(&"the run ended with the app running");
            eprintln!("{}: {stderr}", self.alias);
        }
    }
}

// --- the run ---

/// The nearest-rank percentile of sorted values.
fn percentile(sorted: &[i64], percent: usize) -> i64 {
    let rank = (sorted.len() * percent).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn stats(mut values: Vec<i64>) -> Value {
    values.sort_unstable();
    if values.is_empty() {
        return Value::Null;
    }
    json!({
        "n": values.len(),
        "min": values[0],
        "p50": percentile(&values, 50),
        "p95": percentile(&values, 95),
        "max": values[values.len() - 1],
    })
}

/// What the run found, step by step, for `summary.json`.
#[derive(Default)]
struct Found {
    steps: BTreeMap<&'static str, Value>,
}

impl Found {
    fn step(&mut self, name: &'static str, value: Value) {
        eprintln!("ui_live: {name}: {value}");
        self.steps.insert(name, value);
    }
}

/// The run's three apps.
struct Apps {
    a: App,
    b: App,
    c: App,
}

/// The live acceptance run, in its steps; see the module's docs.
#[test]
fn the_app_reaches_b_and_c_through_the_ui_and_the_fence_holds() {
    let Some(lab) = lab() else {
        return;
    };
    let nonce = now_ms() % 10_000_000;
    let log_start = fs::metadata(&lab.fence_log).map_or(0, |meta| meta.len());
    let started_ms = now_ms();
    let mut found = Found::default();
    let mut apps = Apps {
        a: App::new(&lab, "a"),
        b: App::new(&lab, "b"),
        c: App::new(&lab, "c"),
    };
    open_the_chats(&lab, &mut apps);
    let mut sent = latency(&lab, &mut apps, nonce, &mut found);
    sent.extend(edit_reply_react(&lab, &mut apps, nonce, &mut found));
    group_message(&lab, &mut apps, nonce, &mut found);
    names_and_presence(&lab, &mut apps, &mut found);
    search_and_channel(&lab, &mut apps, nonce, &sent[6], &mut found);
    sent.push(closed_into_the_tray(&lab, &mut apps, nonce, &mut found));
    sent.extend(quit_and_reopened(&lab, &mut apps, nonce, &mut found));
    every_message_once(&mut apps.b, &lab.ab_chat, &sent, &mut found);
    for app in [&mut apps.a, &mut apps.b, &mut apps.c] {
        app.shot(&lab, "end");
        app.quit(&lab);
    }
    drop(apps);
    let fence = fence_log_holds(&lab, log_start);
    no_token_printed(&lab.out);
    let summary = json!({
        "started_ms": started_ms,
        "ended_ms": now_ms(),
        "messages": MESSAGES,
        "gap_messages": GAP_MESSAGES,
        "steps": found.steps,
        "fence_log": fence,
        "screenshots": lab.shots,
    });
    fs::write(lab.out.join("summary.json"), format!("{summary:#}\n")).unwrap();
    eprintln!("{summary:#}");
    let p95 = summary["steps"]["latency"]["send_to_shown_ms"]["p95"].as_i64().unwrap();
    assert!(
        p95 < P95_LIMIT_MS,
        "send → shown p95 {p95} ms, not under {P95_LIMIT_MS} ms"
    );
}

/// a and b open their 1:1 chat, c the group chat.
fn open_the_chats(lab: &Lab, apps: &mut Apps) {
    apps.a.open_chat(&lab.ab_chat);
    apps.b.open_chat(&lab.ab_chat);
    apps.c.open_chat(&lab.group_chat);
    for app in [&mut apps.a, &mut apps.b, &mut apps.c] {
        app.shot(lab, "opened");
    }
}

/// One message a typed and sent to a chat: its id as both sides know it, when a pressed Enter, and when a's compose
/// box took the send (its `compose sent`).
#[derive(Clone, Debug)]
struct Sent {
    id: String,
    enter_ms: i64,
    taken_ms: i64,
}

/// a types `text` in the chat it shows and presses Enter; `watcher` shows the message in `chat`. The message's id (a
/// new one in the watcher's `chat-view shown`, which a's view shows too), when Enter was pressed and when the watcher
/// showed it.
fn send_and_watch(a: &mut App, watcher: &mut App, chat: &str, text: &str) -> (Sent, i64) {
    let known: HashSet<String> = watcher
        .trace()
        .last(|line| chat_view_of(line, chat))
        .map(|line| strings(&line, "messages").into_iter().collect())
        .unwrap_or_default();
    let from = watcher.trace().mark();
    let a_from = a.trace().mark();
    a.type_into_compose(text);
    let enter_ms = now_ms();
    a.key(keys::ENTER);
    let sent = a
        .trace()
        .wait_from(a_from, STEP, "compose sent", |line| is(line, "compose", "sent"));
    let taken_ms = sent["at_ms"].as_i64().unwrap();
    let shown = watcher.trace().wait_from(from, ARRIVAL, "a's message shown", |line| {
        chat_view_of(line, chat) && strings(line, "messages").iter().any(|id| !known.contains(id))
    });
    let id = strings(&shown, "messages")
        .into_iter()
        .find(|id| !known.contains(id))
        .unwrap();
    let shown_ms = shown["at_ms"].as_i64().unwrap();
    // the message a sent, as a's own view shows it once it came back
    a.trace().wait_from(a_from, ARRIVAL, "a's own message shown", |line| {
        chat_view_of(line, chat) && strings(line, "messages").contains(&id)
    });
    (Sent { id, enter_ms, taken_ms }, shown_ms)
}

/// a sends [`MESSAGES`] messages to b, one at a time, each shown by b before the next: send → shown per message.
fn latency(lab: &Lab, apps: &mut Apps, nonce: i64, found: &mut Found) -> Vec<Sent> {
    let mut sent = Vec::new();
    let (mut times, mut taken, mut through) = (Vec::new(), Vec::new(), Vec::new());
    for n in 1..=MESSAGES {
        let text = format!("M2 live {nonce} m{n:02}");
        let (message, shown_ms) = send_and_watch(&mut apps.a, &mut apps.b, &lab.ab_chat, &text);
        times.push(shown_ms - message.enter_ms);
        taken.push(message.taken_ms - message.enter_ms);
        through.push(shown_ms - message.taken_ms);
        sent.push(message);
    }
    apps.a.shot(lab, "sent");
    apps.b.shot(lab, "shown");
    found.step(
        "latency",
        json!({
            "send_to_shown_ms": stats(times.clone()),
            "enter_to_compose_sent_ms": stats(taken),
            "compose_sent_to_shown_ms": stats(through),
            "each_ms": times,
        }),
    );
    sent
}

/// a edits its last message (Up in the empty box), replies to it and reacts to the reply (Escape to the list, Down), all by the keyboard; b
/// shows each. The reply, as sent.
fn edit_reply_react(lab: &Lab, apps: &mut Apps, nonce: i64, found: &mut Found) -> Vec<Sent> {
    let chat = lab.ab_chat.clone();
    let last = apps.b.trace().last(|line| chat_view_of(line, &chat)).unwrap();
    let edited = strings(&last, "messages").last().cloned().unwrap();
    // the edit: Up in the empty box loads a's last message
    let from = apps.a.trace().mark();
    apps.a.key(keys::UP);
    apps.a
        .trace()
        .wait_from(from, STEP, "compose editing", |line| is(line, "compose", "editing"));
    apps.a.type_into_compose(" ed");
    let b_from = apps.b.trace().mark();
    let start = now_ms();
    apps.a.key(keys::ENTER);
    let shown = apps.b.trace().wait_from(b_from, ARRIVAL, "the edit shown", |line| {
        chat_view_of(line, &chat) && kind_of(line, &edited).as_deref() == Some("edited")
    });
    let edit_ms = shown["at_ms"].as_i64().unwrap() - start;
    // the reply: Escape to the list, Down to the newest (it stops there), r
    apps.a.key(keys::ESC);
    for _ in 0..3 {
        apps.a.key(keys::DOWN);
    }
    let from = apps.a.trace().mark();
    let (r, _) = apps.a.input.keymap().key_for('r').unwrap();
    apps.a.key(r);
    apps.a
        .trace()
        .wait_from(from, STEP, "compose replying", |line| is(line, "compose", "replying"));
    let (reply, reply_shown) = send_and_watch(&mut apps.a, &mut apps.b, &chat, &format!("M2 live {nonce} reply"));
    let line = apps.b.trace().last(|line| chat_view_of(line, &chat)).unwrap();
    assert!(
        strings(&line, "replies").contains(&reply.id),
        "b shows no reply: {line}"
    );
    // the reaction: Escape to the list, Down to the newest (the reply: Down stops there, whatever the reply left
    // selected), a, 1 (like)
    apps.a.key(keys::ESC);
    for _ in 0..3 {
        apps.a.key(keys::DOWN);
    }
    let (a_key, _) = apps.a.input.keymap().key_for('a').unwrap();
    let (one, _) = apps.a.input.keymap().key_for('1').unwrap();
    apps.a.key(a_key);
    let b_from = apps.b.trace().mark();
    let start = now_ms();
    apps.a.key(one);
    let id = reply.id.clone();
    let shown = apps.b.trace().wait_from(b_from, ARRIVAL, "the reaction shown", |line| {
        chat_view_of(line, &chat) && strings(line, "reacted").contains(&id)
    });
    let reaction_ms = shown["at_ms"].as_i64().unwrap() - start;
    // back to the box: Escape clears the selection, Escape again moves the focus
    apps.a.key(keys::ESC);
    apps.a.key(keys::ESC);
    apps.a.shot(lab, "edited-replied-reacted");
    apps.b.shot(lab, "edit-reply-reaction");
    found.step(
        "edit_reply_reaction",
        json!({"edit_ms": edit_ms, "reply_ms": reply_shown - reply.enter_ms, "reaction_ms": reaction_ms}),
    );
    vec![reply]
}

/// a sends to the group chat: c shows it; b, showing the 1:1 chat, notifies on its sandbox's bus, and the
/// notification activated opens the group chat, where b shows it.
fn group_message(lab: &Lab, apps: &mut Apps, nonce: i64, found: &mut Found) {
    let group = lab.group_chat.clone();
    apps.a.open_chat(&group);
    let b_from = apps.b.trace().mark();
    let text = format!("M2 live {nonce} group");
    let (message, c_shown) = send_and_watch(&mut apps.a, &mut apps.c, &group, &text);
    let noticed = apps
        .b
        .trace()
        .wait_from(b_from, ARRIVAL, "b's notification of the group chat", |line| {
            is(line, "notification", "shown") && line["conversation"] == group.as_str()
        });
    let notification = apps
        .b
        .sandbox
        .bus()
        .notifications()
        .into_iter()
        .rfind(|notification| notification.body.contains(&text))
        .unwrap_or_else(|| panic!("no notification of the group message on b's bus"));
    assert_eq!(notification.app_name, "Teams (lab b)", "{notification:?}");
    apps.b.shot(lab, "notified");
    let from = apps.b.trace().mark();
    apps.b.sandbox.bus().invoke_action(notification.id, "default").unwrap();
    apps.b
        .trace()
        .wait_from(from, STEP, "the group chat opened from the notification", |line| {
            is(line, "chat", "opened") && line["conversation"] == group.as_str()
        });
    let id = message.id.clone();
    apps.b
        .trace()
        .wait_from(from, ARRIVAL, "the group message shown by b", |line| {
            chat_view_of(line, &group) && strings(line, "messages").contains(&id)
        });
    apps.b.shot(lab, "group");
    apps.c.shot(lab, "group");
    found.step(
        "group",
        json!({
            "c_shown_ms": c_shown - message.enter_ms,
            "b_notified_ms": noticed["at_ms"].as_i64().unwrap() - message.enter_ms,
            "notification_kind": noticed["kinds"],
        }),
    );
}

/// b's header for the 1:1 chat shown, from `from` on, with a's presence `presence`.
fn b_header_line(apps: &mut Apps, chat: &str, from: usize, presence: &str) -> Value {
    apps.b
        .trace()
        .wait_from(from, PRESENCE, &format!("b's header showing a {presence}"), |line| {
            is(line, "header", "shown")
                && line["conversation"] == chat
                && strings(line, "kinds").first().map(String::as_str) == Some(presence)
        })
}

/// [`b_header_line`]'s kinds.
fn b_header(apps: &mut Apps, chat: &str, from: usize, presence: &str) -> Value {
    b_header_line(apps, chat, from, presence)["kinds"].clone()
}

/// a runs the switcher's "Set status: …" command it finds by `query`, and its report (`reported`) goes out; when
/// Enter was pressed.
fn set_status(a: &mut App, query: &str, reported: &str) -> i64 {
    let from = a.trace().mark();
    a.ctrl('k');
    a.trace()
        .wait_from(from, STEP, "switcher opened", |line| is(line, "switcher", "opened"));
    a.type_text(query);
    // the palette lists the command once the query is in
    thread::sleep(SETTLE);
    let start = now_ms();
    a.key(keys::ENTER);
    a.trace()
        .wait_from(from, STEP, "status chosen", |line| is(line, "status-menu", "chosen"));
    a.trace()
        .wait_from(from, ARRIVAL, &format!("own presence {reported} sent"), |line| {
            is(line, "own-presence", "sent") && strings(line, "kinds").first().map(String::as_str) == Some(reported)
        });
    start
}

/// b's header for the 1:1 chat names a from people's names with a's picture and shows a available; a sets Busy,
/// b's header shows busy; a sets Automatic (the reset), b's header shows available again.
fn names_and_presence(lab: &Lab, apps: &mut Apps, found: &mut Found) {
    let chat = lab.ab_chat.clone();
    apps.b.open_chat(&chat);
    let available = b_header(apps, &chat, 0, "available");
    assert_eq!(available[1], "named", "b's header doesn't name a: {available}");
    assert_eq!(
        available[2], "picture",
        "b's header shows no picture of a (Graph has one): {available}"
    );
    let busy_from = apps.b.trace().mark();
    let busy_start = set_status(&mut apps.a, "set status busy", "busy");
    let busy = b_header_line(apps, &chat, busy_from, "busy");
    apps.b.shot(lab, "a-busy");
    let reset_from = apps.b.trace().mark();
    let reset_start = set_status(&mut apps.a, "set status automatic", "reset");
    let back = b_header_line(apps, &chat, reset_from, "available");
    apps.b.shot(lab, "a-available");
    found.step(
        "names_and_presence",
        json!({
            "header": available,
            "busy_shown_ms": busy["at_ms"].as_i64().unwrap() - busy_start,
            "available_again_ms": back["at_ms"].as_i64().unwrap() - reset_start,
        }),
    );
}

/// b's search (Ctrl+Shift+F) finds `target`, one of a's messages, by the words it was sent with; then a channel opens
/// read-only from the switcher ("General"), and b goes back to the 1:1 chat.
fn search_and_channel(lab: &Lab, apps: &mut Apps, nonce: i64, target: &Sent, found: &mut Found) {
    let b = &mut apps.b;
    let from = b.trace().mark();
    let (f, _) = b.input.keymap().key_for('f').unwrap();
    b.input.key_with(&[Modifier::Ctrl, Modifier::Shift], f).unwrap();
    b.trace()
        .wait_from(from, STEP, "search opened", |line| is(line, "search", "opened"));
    b.type_text(&format!("{nonce} m07"));
    let id = target.id.clone();
    let hits = b
        .trace()
        .wait_from(from, STEP, "the search finding a's message", |line| {
            is(line, "search", "shown") && strings(line, "messages").contains(&id)
        });
    b.shot(lab, "search");
    b.key(keys::ESC);
    b.trace()
        .wait_from(from, STEP, "search closed", |line| is(line, "search", "closed"));
    // a channel, read-only: the switcher's channels under the chats
    let from = b.trace().mark();
    b.ctrl('k');
    b.trace()
        .wait_from(from, STEP, "switcher opened", |line| is(line, "switcher", "opened"));
    b.type_text("General");
    thread::sleep(SETTLE);
    b.key(keys::ENTER);
    let opened = b
        .trace()
        .wait_from(from, STEP, "a channel opened", |line| is(line, "channel", "opened"));
    let channel = opened["conversation"].as_str().unwrap().to_owned();
    let shown = b.trace().wait_from(from, ARRIVAL, "the channel's view shown", |line| {
        is(line, "channel-view", "shown") && line["conversation"] == channel.as_str()
    });
    // nothing to write in it: no compose line came while it was shown
    assert_eq!(b.trace().count_from(from, |line| line["view"] == "compose"), 0);
    b.shot(lab, "channel");
    b.open_chat(&lab.ab_chat);
    found.step(
        "search_and_channel",
        json!({
            "search_hits": strings(&hits, "messages").len(),
            "channel_messages": strings(&shown, "messages").len(),
            "channel_kinds": shown["kinds"],
        }),
    );
}

/// a types `text` in `chat` (the chat it shows) and presses Enter: the message's id once a's own view shows it.
fn a_sends(a: &mut App, chat: &str, text: &str) -> Sent {
    let known: HashSet<String> = a
        .trace()
        .last(|line| chat_view_of(line, chat))
        .map(|line| strings(&line, "messages").into_iter().collect())
        .unwrap_or_default();
    let from = a.trace().mark();
    a.type_into_compose(text);
    let enter_ms = now_ms();
    a.key(keys::ENTER);
    let sent = a
        .trace()
        .wait_from(from, STEP, "compose sent", |line| is(line, "compose", "sent"));
    let taken_ms = sent["at_ms"].as_i64().unwrap();
    let shown = a.trace().wait_from(from, ARRIVAL, "a's own message shown", |line| {
        chat_view_of(line, chat) && strings(line, "messages").iter().any(|id| !known.contains(id))
    });
    let id = strings(&shown, "messages")
        .into_iter()
        .find(|id| !known.contains(id))
        .unwrap();
    Sent { id, enter_ms, taken_ms }
}

/// b's window closed into the tray: a sends to b, b notifies once, and the window opened again from the tray item
/// shows the message.
fn closed_into_the_tray(lab: &Lab, apps: &mut Apps, nonce: i64, found: &mut Found) -> Sent {
    let chat = lab.ab_chat.clone();
    apps.a.open_chat(&chat);
    let from = apps.b.trace().mark();
    apps.b.sandbox.niri_msg(&["action", "close-window"]).unwrap();
    apps.b
        .trace()
        .wait_from(from, STEP, "b's window closed", |line| is(line, "window", "closed"));
    let sent = a_sends(&mut apps.a, &chat, &format!("M2 live {nonce} tray"));
    let notice = |line: &Value| is(line, "notification", "shown") && line["conversation"] == chat.as_str();
    let noticed = apps
        .b
        .trace()
        .wait_from(from, ARRIVAL, "b's notification of the 1:1 chat", notice);
    thread::sleep(QUIET);
    let notifications = apps.b.trace().count_from(from, notice);
    assert_eq!(notifications, 1, "b notified {notifications} times");
    // the window again, from the tray item
    let (host, name) = apps.b.tray();
    let item = zbus::blocking::Proxy::new(&host, name.as_str(), ITEM_PATH, "org.kde.StatusNotifierItem").unwrap();
    let opened_from = apps.b.trace().mark();
    item.call_method("Activate", &(0_i32, 0_i32)).unwrap();
    apps.b
        .trace()
        .wait_from(opened_from, STEP, "b's window opened again", |line| {
            is(line, "window", "opened")
        });
    let id = sent.id.clone();
    apps.b.trace().wait_from(
        opened_from,
        ARRIVAL,
        "the tray message in the window opened again",
        |line| chat_view_of(line, &chat) && strings(line, "messages").contains(&id),
    );
    apps.b.shot(lab, "from-the-tray");
    found.step(
        "tray",
        json!({"notified_ms": noticed["at_ms"].as_i64().unwrap() - sent.enter_ms, "notifications": notifications}),
    );
    sent
}

/// b's app quit from the tray (its broker exits with it); a sends [`GAP_MESSAGES`]; b's app started again shows them.
fn quit_and_reopened(lab: &Lab, apps: &mut Apps, nonce: i64, found: &mut Found) -> Vec<Sent> {
    let chat = lab.ab_chat.clone();
    apps.b.quit(lab);
    let gap: Vec<Sent> = (1..=GAP_MESSAGES)
        .map(|n| a_sends(&mut apps.a, &chat, &format!("M2 live {nonce} gap{n}")))
        .collect();
    let restarted = now_ms();
    apps.b.start(lab);
    let ids: Vec<String> = gap.iter().map(|sent| sent.id.clone()).collect();
    let shown = apps
        .b
        .trace()
        .wait_from(0, STARTUP, "the gap messages shown after the restart", |line| {
            chat_view_of(line, &chat) && ids.iter().all(|id| strings(line, "messages").contains(id))
        });
    thread::sleep(QUIET);
    let notified = apps.b.trace().count_from(0, |line| is(line, "notification", "shown"));
    apps.b.shot(lab, "restarted");
    found.step(
        "restart",
        json!({
            "shown_after_start_ms": shown["at_ms"].as_i64().unwrap() - restarted,
            "notifications_after_start": notified,
        }),
    );
    gap
}

/// b's 1:1 chat view shows every message of the run sent there exactly once.
fn every_message_once(b: &mut App, chat: &str, sent: &[Sent], found: &mut Found) {
    let line = b.trace().last(|line| chat_view_of(line, chat)).unwrap();
    let shown = strings(&line, "messages");
    for message in sent {
        let times = shown.iter().filter(|id| **id == message.id).count();
        assert_eq!(times, 1, "{} shown {times} times in b's view", message.id);
    }
    found.step("every_message_once", json!({"messages": sent.len()}));
}

/// What `fence.log` holds for the run, counted by line kind.
#[derive(Debug, Default)]
struct FenceCounts {
    writes: BTreeMap<String, usize>,
    group_writes: BTreeMap<String, usize>,
    own_availability: usize,
    post_reads: BTreeMap<String, usize>,
    own_requests: BTreeMap<String, usize>,
    own_frames: BTreeMap<String, usize>,
    /// Profile lookups of people outside the lab (a channel's members) the fence refused before anything went.
    outsider_lookups_denied: usize,
}

/// `fence.log`'s lines since `start` (a byte offset): whole JSON lines; allows only on the a–b chat, the recorded
/// group chat and the lab accounts' own availability; no deny; `PostRead`, own-state requests and frames only of the
/// reviewed kinds.
fn fence_log_holds(lab: &Lab, start: u64) -> Value {
    let log = fs::read(&lab.fence_log).unwrap();
    let run = &log[usize::try_from(start).unwrap()..];
    assert!(run.is_empty() || run.ends_with(b"\n"), "fence.log ends in a half line");
    let mut counts = FenceCounts::default();
    let mut lines = 0;
    for line in std::str::from_utf8(run).unwrap().lines() {
        lines += 1;
        let entry: Value = serde_json::from_str(line).unwrap_or_else(|error| panic!("not a whole line: {error}"));
        if entry["verdict"] != "allow" {
            counts.deny(&entry);
            continue;
        }
        counts.add(lab, &entry);
    }
    assert_eq!(
        counts.writes.get("message"),
        Some(&(MESSAGES + 1 + 1 + GAP_MESSAGES)),
        "{counts:?}"
    );
    assert_eq!(counts.writes.get("edit"), Some(&1), "{counts:?}");
    assert_eq!(counts.writes.get("reaction"), Some(&1), "{counts:?}");
    assert_eq!(counts.group_writes.get("message"), Some(&1), "{counts:?}");
    assert!(counts.own_availability >= 3, "{counts:?}");
    json!({
        "lines": lines,
        "allowed_writes": counts.writes,
        "allowed_group_writes": counts.group_writes,
        "own_availability": counts.own_availability,
        "post_reads": counts.post_reads,
        "own_requests": counts.own_requests,
        "own_frames": counts.own_frames,
        "outsider_lookups_denied": counts.outsider_lookups_denied,
    })
}

impl FenceCounts {
    /// The only deny the run takes: the fence refusing the middle tier's profile lookup of someone outside the lab
    /// (the engine asks the fence for each name Graph didn't give, M2 Task 12; a channel's members are tenant users),
    /// so nothing was sent. Any other deny fails the run.
    fn deny(&mut self, entry: &Value) {
        let outsider = entry["event"] == "post_read_decision"
            && entry["post_read"] == "short_profile"
            && entry["reason"]["outsiders"]["count"]
                .as_u64()
                .is_some_and(|count| count > 0);
        assert!(outsider, "a line that is no allow: {entry}");
        self.outsider_lookups_denied += 1;
    }

    fn add(&mut self, lab: &Lab, entry: &Value) {
        if let Some(kind) = entry["kind"].as_str() {
            self.add_write(lab, kind, entry);
        } else if entry["event"] == "frame" {
            let own = entry["own"].as_str().unwrap_or_default().to_owned();
            let frames = [Authenticate::KIND, Ping::KIND, MessageLossAck::KIND, Disconnect::KIND];
            assert!(frames.contains(&own.as_str()), "a frame off the list: {entry}");
            *self.own_frames.entry(own).or_default() += 1;
        } else if let Some(post_read) = entry["post_read"].as_str() {
            assert!(
                ["short_profile", "presence"].contains(&post_read),
                "a PostRead off the list: {entry}"
            );
            if entry["event"] == "post_read_decision" {
                *self.post_reads.entry(format!("{post_read}_decision")).or_default() += 1;
            } else {
                assert_eq!(entry["channel"], "http", "{entry}");
                *self.post_reads.entry(post_read.to_owned()).or_default() += 1;
            }
        } else {
            assert_eq!(entry["channel"], "http", "a line of another kind: {entry}");
            let own = entry["own"].as_str().unwrap_or_default().to_owned();
            assert_eq!(own, EndpointRegistration::KIND, "a request off the list: {entry}");
            *self.own_requests.entry(own).or_default() += 1;
        }
    }

    fn add_write(&mut self, lab: &Lab, kind: &str, entry: &Value) {
        if let Some(mri) = entry["scope"]["account"]["mri"].as_str() {
            assert_eq!(kind, "own_availability", "a {kind} on an account: {entry}");
            assert!(
                lab.accounts.iter().any(|account| account == mri),
                "an availability off the lab: {entry}"
            );
            self.own_availability += 1;
            return;
        }
        let conversation = entry["scope"]["conversation"]["id"].as_str().unwrap_or_default();
        let writes = if conversation == lab.ab_chat {
            assert!(
                ["message", "edit", "reaction", "read_receipt", "typing"].contains(&kind),
                "a {kind} allowed on the a–b chat"
            );
            &mut self.writes
        } else {
            assert!(
                conversation == lab.group_chat,
                "a {kind} allowed off the a–b and the recorded group chat: {entry}"
            );
            assert!(
                ["message", "read_receipt", "typing"].contains(&kind),
                "a {kind} allowed on the group chat"
            );
            &mut self.group_writes
        };
        *writes.entry(kind.to_owned()).or_default() += 1;
    }
}

/// No output of the run holds a token: no JWT (`eyJ…`), no bearer or skypetoken header.
fn no_token_printed(out: &Path) {
    for entry in fs::read_dir(out).unwrap() {
        let path = entry.unwrap().path();
        let text = fs::read_to_string(&path).unwrap();
        for marker in ["eyJ", "Bearer ", "skypetoken="] {
            assert!(!text.contains(marker), "{} holds {marker}", path.display());
        }
    }
}
