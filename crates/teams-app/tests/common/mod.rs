//! What the live acceptance runs (`ui_live`, `call_live`) share: a UI sandbox per app with the normally built app in it
//! (one lab account's, [`App::new`], or several accounts' in one window, [`App::with_accounts`]), each account's
//! credential broker outside, the view trace on a pipe; the run's dirs, its numbers and the check that
//! nothing it kept holds a token. Each test loads this module and leaves out what it doesn't use.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder};
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd, AsRawFd as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{Running, Sandbox, SandboxOptions, Tools, wait_until};

/// From an app's start to its window and the trace lines its [`Launch::ready`] names.
pub const STARTUP: Duration = Duration::from_secs(180);
/// How long a view change the keyboard asked for may take.
pub const STEP: Duration = Duration::from_secs(30);
/// How long an app and its broker get to exit once asked.
const EXIT: Duration = Duration::from_secs(60);
/// How long an app asked to quit on a failed run gets before it is killed.
const QUIT_ON_FAILURE: Duration = Duration::from_secs(15);
/// The switcher lists at most this many chats (`teams_ui::switcher::MAX_MATCHES`).
const SWITCHER_CHATS: usize = 50;

// --- the run ---

/// The UTC time now, for the run's dirs.
pub fn stamp() -> String {
    let out = Command::new("date").args(["-u", "+%Y%m%dT%H%M%SZ"]).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// `dir`, made `0700` with its parents.
pub fn private_dir(dir: &Path) -> PathBuf {
    DirBuilder::new().recursive(true).mode(0o700).create(dir).unwrap();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    dir.to_owned()
}

pub fn now_ms() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(since.as_millis()).unwrap()
}

/// The nearest-rank percentile of sorted values.
pub fn percentile<T: Copy>(sorted: &[T], percent: usize) -> T {
    let rank = (sorted.len() * percent).div_ceil(100).max(1);
    sorted[rank - 1]
}

/// `values`' count, min, p50, p95 and max; `null` for none.
pub fn stats(mut values: Vec<i64>) -> Value {
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

/// What marks a token in text: a JWT (`eyJ…`), a bearer or skypetoken header.
const TOKEN_MARKERS: [&str; 3] = ["eyJ", "Bearer ", "skypetoken="];

/// The first token marker `text` holds.
pub fn token_marker(text: &str) -> Option<&'static str> {
    TOKEN_MARKERS.into_iter().find(|marker| text.contains(marker))
}

/// No text file the run kept in `out` holds a token: the apps', the brokers' and any other process's output, the
/// traces and `summary.json` (which holds every step the run printed); recordings (`.wav`) aren't text.
pub fn no_token_kept(out: &Path) {
    for entry in fs::read_dir(out).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "wav") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        if let Some(marker) = token_marker(&text) {
            panic!("{} holds {marker}", path.display());
        }
    }
}

// --- the view trace ---

/// An app's view trace, line by line as it comes; every line is also kept in a file of the run.
pub struct Trace {
    lines: mpsc::Receiver<Value>,
    pub seen: Vec<Value>,
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
    pub fn drain(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            assert!(line.is_object(), "{}: a trace line isn't JSON: {line}", self.name);
            self.seen.push(line);
        }
    }

    /// How many lines came so far: a mark to wait from.
    pub fn mark(&mut self) -> usize {
        self.drain();
        self.seen.len()
    }

    /// The first line from `from` on that `found` takes, waiting up to `limit` for it; named `what` in the failure.
    pub fn wait_from(&mut self, from: usize, limit: Duration, what: &str, found: impl Fn(&Value) -> bool) -> Value {
        self.try_wait_from(from, limit, found)
            .unwrap_or_else(|| panic!("{}: no trace line {what} within {limit:?}", self.name))
    }

    /// [`Self::wait_from`], `None` when no line came in time.
    pub fn try_wait_from(&mut self, from: usize, limit: Duration, found: impl Fn(&Value) -> bool) -> Option<Value> {
        let deadline = Instant::now() + limit;
        loop {
            self.drain();
            if let Some(line) = self.seen[from.min(self.seen.len())..].iter().find(|line| found(line)) {
                return Some(line.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            match self.lines.recv_timeout(left.min(Duration::from_millis(200))) {
                Ok(line) => {
                    assert!(line.is_object(), "{}: a trace line isn't JSON: {line}", self.name);
                    self.seen.push(line);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("{}: the trace ended (the app exited)", self.name)
                }
            }
        }
    }

    /// The last line `found` takes, among those that came.
    pub fn last(&mut self, found: impl Fn(&Value) -> bool) -> Option<Value> {
        self.drain();
        self.seen.iter().rev().find(|line| found(line)).cloned()
    }

    /// Every line from `from` on that `found` takes.
    pub fn all_from(&mut self, from: usize, found: impl Fn(&Value) -> bool) -> Vec<Value> {
        self.drain();
        self.seen[from.min(self.seen.len())..]
            .iter()
            .filter(|line| found(line))
            .cloned()
            .collect()
    }

    /// How many lines from `from` on `found` takes.
    pub fn count_from(&mut self, from: usize, found: impl Fn(&Value) -> bool) -> usize {
        self.drain();
        self.seen[from.min(self.seen.len())..]
            .iter()
            .filter(|line| found(line))
            .count()
    }
}

/// Whether `line` is `view` `change`.
pub fn is(line: &Value, view: &str, change: &str) -> bool {
    line["view"] == view && line["change"] == change
}

/// The strings of a trace line's array `key`.
pub fn strings(line: &Value, key: &str) -> Vec<String> {
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

/// A trace line's time (Unix ms).
pub fn at_ms(line: &Value) -> i64 {
    line["at_ms"]
        .as_i64()
        .unwrap_or_else(|| panic!("a trace line without at_ms: {line}"))
}

// --- the apps ---

/// A process killed (and reaped) when dropped.
pub struct Reaped(pub Option<Child>);

impl Reaped {
    /// Waits up to `limit` for the exit.
    pub fn exit_within(&mut self, limit: Duration) -> Option<ExitStatus> {
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

/// How the run starts an app: the normally built binary, where the run keeps the app's output and its screenshots, the
/// variables it adds to the app's environment, and the trace lines (view, change) the app must have written, after its
/// window opened, before it counts as started.
pub struct Launch {
    pub binary: PathBuf,
    pub out: PathBuf,
    pub shots: PathBuf,
    pub env: Vec<(&'static str, String)>,
    pub ready: Vec<(&'static str, &'static str)>,
}

/// One run of an app: the app in its sandbox, each account's broker outside, its trace.
struct Run {
    app: Option<Running>,
    brokers: Vec<Reaped>,
    trace: Trace,
    pid: u32,
}

/// An app of one or more lab accounts: its sandbox (kept across a restart), its keyboard, its current run.
pub struct App {
    /// The first account's alias.
    pub alias: &'static str,
    /// Every account's alias, the first one first.
    pub aliases: Vec<&'static str>,
    launch: Launch,
    pub sandbox: Sandbox,
    pub input: VirtualInput,
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
    /// The sandbox of `alias` (`options`), then its app started in it ([`Self::start`]).
    pub fn new(tools: &Tools, alias: &'static str, options: &SandboxOptions, launch: Launch) -> Self {
        Self::with_accounts(tools, &[alias], options, launch)
    }

    /// The sandbox (`options`), then one app of every account of `aliases` started in it (the first one first), each
    /// with its own broker ([`Self::start`]).
    pub fn with_accounts(tools: &Tools, aliases: &[&'static str], options: &SandboxOptions, launch: Launch) -> Self {
        let alias = *aliases.first().expect("an account");
        let label = aliases.concat();
        let sandbox = Sandbox::start(tools, options).unwrap_or_else(|error| panic!("{label}'s sandbox: {error}"));
        let input = sandbox.input().unwrap();
        let mut app = Self {
            alias,
            aliases: aliases.to_vec(),
            launch,
            sandbox,
            input,
            run: None,
            starts: 0,
            shots: 0,
        };
        app.start();
        app
    }

    /// What names the app in the run's files and messages: its aliases together (`a`, `ab`).
    pub fn label(&self) -> String {
        self.aliases.concat()
    }

    fn run(&mut self) -> &mut Run {
        let label = self.label();
        self.run
            .as_mut()
            .unwrap_or_else(|| panic!("{label}'s app isn't running"))
    }

    pub fn trace(&mut self) -> &mut Trace {
        &mut self.run().trace
    }

    /// The app's process id.
    pub fn pid(&mut self) -> u32 {
        self.run().pid
    }

    /// `alias`'s broker started outside the sandbox (`name` names its output in the run's dir), and the app's end of
    /// the socket pair between them.
    fn start_broker(&self, alias: &str, name: &str) -> (Reaped, UnixStream) {
        let (broker_end, app_end) = UnixStream::pair().unwrap();
        let file = |suffix: &str| fs::File::create(self.launch.out.join(format!("{name}-{suffix}"))).unwrap();
        let mut command = Command::new(&self.launch.binary);
        command.env_clear();
        for variable in BROKER_ENV {
            if let Some(value) = std::env::var_os(variable) {
                command.env(variable, value);
            }
        }
        command
            .args(["secrets", "--lab", alias, "--fd", &broker_end.as_raw_fd().to_string()])
            .stdin(Stdio::null())
            .stdout(file("broker.out"))
            .stderr(file("broker.err"));
        ui_sandbox::spawn::pass_only(&mut command, &[broker_end.as_fd()]);
        let broker = Reaped(Some(command.spawn().unwrap()));
        // the broker holds its end from here: this process keeps none
        drop(broker_end);
        (broker, app_end)
    }

    /// Starts each account's broker outside the sandbox and the app inside it, a socket pair between the app and each
    /// broker (the `--lab`/`--secrets-fd` pairs in the accounts' order), the trace on a pipe; then waits for the window
    /// and the launch's ready lines (the push channel, the first catch-up, so what the app missed while it wasn't
    /// running is in its store before a chat is opened and can't land in the middle of a step).
    pub fn start(&mut self) {
        self.starts += 1;
        let name = format!("{}-{}", self.label(), self.starts);
        let out = self.launch.out.clone();
        let (brokers, app_ends): (Vec<Reaped>, Vec<UnixStream>) = self
            .aliases
            .iter()
            .map(|alias| {
                let broker_name = if self.aliases.len() == 1 {
                    name.clone()
                } else {
                    format!("{name}-{alias}")
                };
                self.start_broker(alias, &broker_name)
            })
            .unzip();
        let (reader, writer) = io::pipe().unwrap();
        let secrets_fds: Vec<String> = app_ends.iter().map(|end| end.as_raw_fd().to_string()).collect();
        let trace_fd = writer.as_raw_fd().to_string();
        let mut args: Vec<&str> = Vec::new();
        for (alias, fd) in self.aliases.iter().zip(&secrets_fds) {
            args.extend(["--lab", alias, "--secrets-fd", fd.as_str()]);
        }
        args.extend(["--trace-fd", trace_fd.as_str()]);
        let env: Vec<(&str, &str)> = self
            .launch
            .env
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let mut fds: Vec<_> = app_ends.iter().map(AsFd::as_fd).collect();
        fds.push(writer.as_fd());
        let child = self
            .sandbox
            .spawn_with_fds(&self.launch.binary, &args, &env, &fds)
            .unwrap();
        // the app holds the sockets' ends and the trace's writer from here: this process keeps none
        drop((app_ends, writer));
        let pid = child.id();
        let app = Running::new(child).unwrap();
        let trace = Trace::start(reader, out.join(format!("{name}-trace.jsonl")), name);
        self.run = Some(Run {
            app: Some(app),
            brokers,
            trace,
            pid,
        });
        self.trace()
            .wait_from(0, STARTUP, "window opened", |line| is(line, "window", "opened"));
        // read once the app runs (its window is up): right after the fork, `/proc` still shows the parent's
        self.environment_is_the_sandbox_s(pid);
        for (view, change) in self.launch.ready.clone() {
            self.trace()
                .wait_from(0, STARTUP, &format!("{view} {change}"), |line| is(line, view, change));
        }
    }

    /// The app's environment (from `/proc`) names the sandbox's bus, compositor and dir (and so its PipeWire, when it
    /// runs one) and no display of the desktop's.
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
        let alias = self.label();
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
        assert_eq!(
            variables.get(ui_sandbox::SANDBOX_ENV).map(PathBuf::from).as_deref(),
            Some(self.sandbox.dir()),
            "{alias}'s app isn't given its sandbox's dir"
        );
        assert!(!variables.contains_key("DISPLAY"), "{alias}'s app has a DISPLAY");
        assert!(
            !variables.contains_key("NIRI_SOCKET"),
            "{alias}'s app has a NIRI_SOCKET"
        );
    }

    /// A screenshot of the window, kept in the run's screenshots dir as `<n>-<alias>-<what>.png` (`0600`).
    pub fn shot(&mut self, what: &str) {
        self.shots += 1;
        // the frames after the trace's line
        thread::sleep(Duration::from_millis(500));
        let name = format!("{}-{what}.png", self.label());
        let Ok(png) = self.sandbox.screenshot(&name, true) else {
            eprintln!("{}: no screenshot of {what}", self.label());
            return;
        };
        let kept = self.launch.shots.join(format!("{:02}-{name}", self.shots));
        fs::copy(&png, &kept).unwrap();
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o600)).unwrap();
        let _ = fs::remove_file(png);
    }

    pub fn ctrl(&mut self, letter: char) {
        let (key, _) = self.input.keymap().key_for(letter).unwrap();
        self.input.key_with(&[Modifier::Ctrl], key).unwrap();
    }

    pub fn key(&mut self, key: u32) {
        self.input.key(key).unwrap();
    }

    pub fn type_text(&mut self, text: &str) {
        self.input.type_text(text).unwrap();
    }

    /// Opens `chat` by the switcher: Ctrl+K, Down to its row (the chat list's order, as its trace last told it),
    /// Enter; the `chat opened` line must name it (tried again when the list moved meanwhile).
    pub fn open_chat(&mut self, chat: &str) {
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
    pub fn tray(&mut self) -> (zbus::blocking::Connection, String) {
        let name = format!("org.kde.StatusNotifierItem-{}-1", self.run().pid);
        (self.sandbox.bus().connect().unwrap(), name)
    }

    /// Keeps the current run's stdout and stderr in the run's dir (`<alias>-<n>-app.out`, `.err`), which the token
    /// check reads.
    fn keep_output(&self, stdout: &str, stderr: &str) {
        let name = format!("{}-{}-app", self.label(), self.starts);
        fs::write(self.launch.out.join(format!("{name}.out")), stdout).unwrap();
        fs::write(self.launch.out.join(format!("{name}.err")), stderr).unwrap();
    }

    /// Copies the sandbox's speaker recording, when it runs its PipeWire, into the run's dir (`<alias>-speaker.wav`,
    /// `0600`).
    pub fn keep_recording(&self) {
        let Some(pipewire) = self.sandbox.pipewire() else {
            return;
        };
        let kept = self.launch.out.join(format!("{}-speaker.wav", self.label()));
        if fs::copy(pipewire.recording(), &kept).is_ok() {
            let _ = fs::set_permissions(&kept, fs::Permissions::from_mode(0o600));
        }
    }

    /// Clicks the tray menu's "Quit" of the running app; whether the click went. No panic: it also runs while a failed
    /// run unwinds.
    fn ask_to_quit(&self) -> bool {
        let Some(pid) = self.run.as_ref().map(|run| run.pid) else {
            return false;
        };
        let name = format!("org.kde.StatusNotifierItem-{pid}-1");
        let Ok(host) = self.sandbox.bus().connect() else {
            return false;
        };
        zbus::blocking::Proxy::new(
            &host,
            name.as_str(),
            teams_ui::tray::MENU_PATH,
            "com.canonical.dbusmenu",
        )
        .and_then(|menu| {
            menu.call_method(
                "Event",
                &(
                    teams_ui::tray::MENU_QUIT,
                    "clicked",
                    zbus::zvariant::Value::from(""),
                    0_u32,
                ),
            )
        })
        .is_ok()
    }

    /// The tray menu's "Quit": the app quits and each account's broker exits once its socket closes. All exit cleanly;
    /// the app's stdout and stderr are kept in the run's dir.
    pub fn quit(&mut self) {
        let label = self.label();
        assert!(self.ask_to_quit(), "{label}'s tray menu didn't take Quit");
        let mut run = self.run.take().unwrap();
        let app = run.app.take().unwrap();
        let stdout = stdout_until_closed(&app, EXIT);
        let (status, stderr) = app.finish(EXIT).unwrap();
        self.keep_output(&stdout, &stderr);
        assert!(status.success(), "{label}'s app exited {status}");
        for (alias, broker) in self.aliases.iter().zip(&mut run.brokers) {
            let exited = broker.exit_within(EXIT);
            assert!(
                exited.is_some_and(|status| status.success()),
                "{alias}'s broker didn't exit cleanly once {label}'s app quit: {exited:?}"
            );
        }
    }
}

/// What `app` writes to stdout until it closes it (the app exited) or `limit` passed, a line each.
fn stdout_until_closed(app: &Running, limit: Duration) -> String {
    let deadline = Instant::now() + limit;
    let mut text = String::new();
    while let Ok(line) = app.line(deadline.saturating_duration_since(Instant::now())) {
        text.push_str(&line);
        text.push('\n');
    }
    text
}

impl Drop for App {
    /// An app still running when the run fails: asked to quit from its tray first (its engine hangs up a call it is in,
    /// so no call of the run is left running at the service to refuse the next run's start), killed with its broker
    /// when it doesn't within [`QUIT_ON_FAILURE`]; its stdout and stderr kept beside the trace (never printed: they may
    /// name what a token check would have to read first), and its recording.
    fn drop(&mut self) {
        let asked = self.ask_to_quit();
        if let Some(run) = self.run.as_mut()
            && let Some(mut app) = run.app.take()
        {
            let pid = run.pid;
            if asked {
                let _ = wait_until(QUIT_ON_FAILURE, || exited(pid).then_some(()));
            }
            // reaps it, and kills it first if it is still there
            let stderr = app.fail(&"the run ended with the app running").to_string();
            let stdout = stdout_until_closed(&app, Duration::from_secs(2));
            self.keep_output(&stdout, &stderr);
            self.keep_recording();
            eprintln!(
                "{}: the app was killed; its output is kept in the run's dir",
                self.label()
            );
        }
    }
}

/// Whether process `pid` (a child not reaped yet) has exited: gone, or a zombie.
fn exited(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
        stat.rfind(')')
            .and_then(|end| stat.get(end + 2..end + 3))
            .is_some_and(|state| state == "Z")
    })
}

// --- the shared helpers' own tests (offline: they run without PROTO_LAB_LIVE) ---

#[test]
fn each_token_marker_is_found_and_plain_output_passes() {
    assert_eq!(token_marker("app started, 3 chats"), None);
    assert_eq!(
        token_marker("{\"aud\":\"https://ic3.teams.office.com\",\"exp\":1}"),
        None
    );
    assert_eq!(token_marker("token eyJhbGciOi.synthetic"), Some("eyJ"));
    assert_eq!(token_marker("Authorization: Bearer synthetic"), Some("Bearer "));
    assert_eq!(token_marker("X-Skypetoken: skypetoken=synthetic"), Some("skypetoken="));
}

#[test]
fn stats_are_nearest_rank() {
    let values: Vec<i64> = (1..=20).collect();
    assert_eq!(
        stats(values),
        json!({"n": 20, "min": 1, "p50": 10, "p95": 19, "max": 20})
    );
    assert_eq!(stats(Vec::new()), Value::Null);
}
