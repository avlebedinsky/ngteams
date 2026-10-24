//! The app's window in the UI sandbox, on the mock (`harness = false`: GPUI runs on the process's main thread).
//!
//! The test runs itself again inside the sandbox ([`Sandbox::spawn_with_fds`]: the sandbox's compositor, bus and
//! opener, nothing of the desktop's, only the view trace's pipe passed through), in one of two modes:
//!
//! - `window`: lab account a's engine on the mock chat service and its Trouter (`teams-mock`, through the
//!   `test-support` mock origins, a test roster and log), handed to the app as `teams-app` hands its own
//!   ([`teams_app::app::launch`]), its settings in the test's own directory. The parent waits for the window, checks
//!   its rem size is the theme's font size, reads the view trace (the window opened, the push channel connected, the
//!   chat list filled from the store once the catch-up brought a's chats: the chats, not the channel or the
//!   notifications stream), screenshots the window (the rail, the list and the conversation pane drawn; the shot is
//!   kept under `target/ui-shots/`), drives the chat switcher with the sandbox's virtual keyboard (Ctrl+K opens it,
//!   Escape closes it, Ctrl+K and Enter open the chat at its top), checks the chat view shows that chat's messages
//!   from the store (the a–b chat's with their kinds: its event, a deleted and an edited message), and screenshots it
//!   again. Then c posts on the mock to another of a's chats: the app's notification reaches the sandbox's fake
//!   notification server (from the connection its tray item is on), and activating it opens that chat. The tray item
//!   is registered with the fake watcher under the app's name; the window closed through niri leaves the app running.
//!   The watcher stopped (a panel quitting) brings the window back, as no tray shows the app any more; started again,
//!   it has the item registered with it again, and the window closed leaves the app running again. A click on the
//!   item opens the window again, and once it is closed again the menu's "Quit" quits the app, whose engine's thread
//!   was joined (the account's engine lock is free).
//! - `http`: a window with an `img()` of an `https` URL on a loopback listener the child holds. GPUI's HTTP client is
//!   the null one: the image loads nothing, a request through `cx.http_client()` is refused, and the listener never
//!   sees a connection.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, future, io, thread};

use gpui::{
    App, AppContext as _, AsyncApp, Bounds, Context, IntoElement, ParentElement as _, Render, Styled as _, Window,
    WindowBounds, WindowOptions, WindowingModes, div, img, px, rgb, size,
};
use teams_app::app::{self, Launch};
use teams_app::trace::adopt_trace;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::lab::display_name;
use teams_fence::lab::{Alias, LabConfig, record_abc_chat};
use teams_fence::{Fence, FenceLog, LiveMembershipSource, Mri, Roster};
use teams_mock::{
    AB_CHAT, ABC_CHAT, AC_CHAT, CHANNEL, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, NOTIFICATIONS, TrouterMock,
    lab_toml,
};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::tray::{ITEM_PATH, MENU_PATH, MENU_QUIT};
use teams_ui::view::BACKGROUND;
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{
    LineError, NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until,
};

/// Which mode the child runs in.
const CHILD_ENV: &str = "TEAMS_APP_WINDOW_TEST";
/// The child's view trace descriptor.
const TRACE_FD_ENV: &str = "TEAMS_APP_WINDOW_TRACE_FD";
/// From the child's start to its `ready` (the mock started, the window drawn).
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(30);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 3;
/// The file in the sandbox dir the parent writes a chat's id to, for c to post there.
const POST_TRIGGER: &str = "post-as-c";
/// What c posts.
const C_SAYS: &str = "hello from c";
/// How long the app is watched staying up after its window closed.
const STAYS: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("window") => child_window(),
        Ok("http") => child_http(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("window test: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Prints `line` to stdout at once.
fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent: the sandbox, and the checks from outside ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("teams-app window", &options)? else {
        return Ok(());
    };
    an_empty_window_opens_on_the_mock(&sandbox).map_err(|error| format!("{error}{}", sandbox.logs()))?;
    gpui_s_http_client_is_the_null_one(&sandbox)?;
    say("teams-app window: ok");
    Ok(())
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
                let Ok(value) = serde_json::from_str(&line) else {
                    // a line that isn't JSON is a failure the parent reports: kept as a string
                    let _ = sender.send(serde_json::Value::String(line));
                    continue;
                };
                let _ = sender.send(value);
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    /// Waits for a line of `view` with `change`.
    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult {
        self.wait_where(&format!("{view} {change}"), |line| {
            line["view"] == view && line["change"] == change
        })
        .map(|_| ())
    }

    /// Waits for a line `found` takes (one seen already included), named `what` in the error.
    fn wait_where(
        &mut self,
        what: &str,
        found: impl Fn(&serde_json::Value) -> bool,
    ) -> SandboxResult<serde_json::Value> {
        if let Some(line) = self.seen.iter().find(|line| found(line)) {
            return Ok(line.clone());
        }
        self.next_where(what, found)
    }

    /// Waits for a line `found` takes among those not seen yet.
    fn next_where(
        &mut self,
        what: &str,
        found: impl Fn(&serde_json::Value) -> bool,
    ) -> SandboxResult<serde_json::Value> {
        loop {
            let line = self
                .lines
                .recv_timeout(WAIT)
                .map_err(|_| format!("no trace line {what}; seen {:?}", self.seen))?;
            let hit = found(&line);
            self.seen.push(line.clone());
            if hit {
                return Ok(line);
            }
        }
    }
}

/// The share of the screenshot's pixels within 8 of `color` (0xRRGGBB) on every channel.
fn colour_share(png: &Path, color: u32) -> SandboxResult<f64> {
    let wanted = color.to_be_bytes();
    let shot = image::open(png)?.to_rgb8();
    let near = shot
        .pixels()
        .filter(|pixel| {
            pixel
                .0
                .iter()
                .zip(&wanted[1..])
                .all(|(&got, &want)| got.abs_diff(want) <= 8)
        })
        .count();
    let total = u64::from(shot.width()) * u64::from(shot.height());
    Ok(f64::from(u32::try_from(near)?) / f64::from(u32::try_from(total)?))
}

/// Where the offline runs' screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn an_empty_window_opens_on_the_mock(sandbox: &Sandbox) -> SandboxResult {
    let (reader, writer) = io::pipe()?;
    let fd = writer.as_raw_fd().to_string();
    let child = sandbox.spawn_with_fds(
        &std::env::current_exe()?,
        &[],
        &[(CHILD_ENV, "window"), (TRACE_FD_ENV, &fd)],
        &[writer.as_fd()],
    )?;
    // the child holds the only writer from here, so the trace ends when it exits
    drop(writer);
    let mut running = Running::new(child)?;
    let mut trace = TraceReader::start(reader);
    let rem = running
        .line_where(STARTUP, |line| line.starts_with("rem-size "))
        .map_err(|error| running.fail(&format!("no rem-size line: {error:?}")))?;
    // the theme's compact font size reached the window drawn on the real platform (GPUI's own default is 16 px)
    let compact = format!("rem-size {}", teams_ui::theme::FONT_SIZE);
    if rem != compact {
        return Err(running.fail(&format!(
            "the window's rem size isn't the theme's: {rem}, not {compact}"
        )));
    }
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    trace.wait_for("window", "opened")?;
    trace
        .wait_for("status", "connected")
        .map_err(|error| running.fail(&error))?;
    let listed = the_chat_list_shows_a_s_chats(&mut trace).map_err(|error| running.fail(&error))?;

    let windows = sandbox.niri_msg(&["windows"])?;
    if !windows.contains(teams_ui::window::LAB_APP_ID) {
        return Err(running.fail(&format!("no window with the app id: {windows}")));
    }
    let png = sandbox.screenshot("teams-app-window.png", true)?;
    fs::copy(&png, shots_dir()?.join("teams-app-window.png"))?;
    let palette = teams_ui::theme::LIGHT_PALETTE;
    for (region, color, least) in [
        ("the rail", palette.rail, 0.02),
        ("the list", palette.surface, 0.1),
        ("the conversation pane", BACKGROUND, 0.2),
    ] {
        let share = colour_share(&png, color)?;
        if share < least {
            return Err(running.fail(&format!("{region}'s colour fills {share:.2} of the window")));
        }
    }
    let chat = the_switcher_opens_a_chat(sandbox, &mut trace, &listed).map_err(|error| running.fail(&error))?;
    the_chat_view_shows_its_messages(&mut trace, &chat).map_err(|error| running.fail(&error))?;
    // the frames after the trace's line
    thread::sleep(Duration::from_secs(1));
    let png = sandbox.screenshot("teams-app-chat.png", true)?;
    fs::copy(&png, shots_dir()?.join("teams-app-chat.png"))?;

    a_message_notifies_and_opens_its_chat(sandbox, &mut trace, &running, &chat)
        .map_err(|error| running.fail(&error))?;
    the_tray_keeps_the_app_and_quits_it(sandbox, &mut trace, &running).map_err(|error| running.fail(&error))?;
    running
        .line_where(WAIT, |line| line == "engine-released")
        .map_err(|error| running.fail(&format!("the engine wasn't released: {error:?}")))?;
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the app exited {status}: {stderr}").into());
    }
    if let Some(odd) = trace.seen.iter().find(|line| !line.is_object()) {
        return Err(format!("a trace line isn't JSON: {odd}").into());
    }
    Ok(())
}

/// The strings of a trace line's array `key`.
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

/// The chat list, once the catch-up filled it from the mock: a's chats with their kinds, never the team's channel or
/// a's notifications stream. Its chats in order.
fn the_chat_list_shows_a_s_chats(trace: &mut TraceReader) -> SandboxResult<Vec<String>> {
    let line = trace.wait_where("chat-list shown with a's chats", |line| {
        let ids = strings(line, "conversations");
        line["view"] == "chat-list"
            && [AB_CHAT, AC_CHAT, ABC_CHAT]
                .iter()
                .all(|chat| ids.iter().any(|id| id == chat))
    })?;
    let ids = strings(&line, "conversations");
    let kinds = strings(&line, "kinds");
    if ids.iter().any(|id| id == CHANNEL || id == NOTIFICATIONS) {
        return Err(format!("the list shows a channel or an own stream: {ids:?}").into());
    }
    let kind_of = |chat: &str| {
        ids.iter()
            .position(|id| id == chat)
            .and_then(|at| kinds.get(at).cloned())
    };
    if kind_of(AB_CHAT).as_deref() != Some("one-to-one") || kind_of(ABC_CHAT).as_deref() != Some("group") {
        return Err(format!("the chats' kinds: {ids:?} {kinds:?}").into());
    }
    Ok(ids)
}

/// Ctrl+K, the way into a chat with the keyboard alone.
fn ctrl_k(input: &mut VirtualInput) -> SandboxResult {
    let (key, _) = input.keymap().key_for('k').ok_or("k isn't on the US keymap")?;
    input.key_with(&[Modifier::Ctrl], key)
}

/// The chat view of the chat opened: a `chat-view shown` line for it; the a–b chat's from the store with its event,
/// its deleted and its edited message (the mock's `messages.json`).
fn the_chat_view_shows_its_messages(trace: &mut TraceReader, chat: &str) -> SandboxResult {
    let line = trace.wait_where("chat-view shown for the chat opened", |line| {
        line["view"] == "chat-view"
            && line["conversation"] == chat
            && (chat != AB_CHAT || !strings(line, "messages").is_empty())
    })?;
    if chat != AB_CHAT {
        return Ok(());
    }
    let kinds = strings(&line, "kinds");
    for kind in ["event", "deleted", "edited", "text"] {
        if !kinds.iter().any(|shown| shown == kind) {
            return Err(format!("the a–b chat shows no {kind} message: {kinds:?}").into());
        }
    }
    if strings(&line, "messages").len() != kinds.len() {
        return Err(format!("a kind per message: {line}").into());
    }
    Ok(())
}

/// The switcher with the sandbox's virtual keyboard: Ctrl+K opens it, Escape closes it; Ctrl+K and Enter open the chat
/// at its top, one the list shows. The chat opened.
fn the_switcher_opens_a_chat(sandbox: &Sandbox, trace: &mut TraceReader, listed: &[String]) -> SandboxResult<String> {
    let mut input = sandbox.input()?;
    ctrl_k(&mut input)?;
    trace.next_where("switcher opened", |line| {
        line["view"] == "switcher" && line["change"] == "opened"
    })?;
    input.key(keys::ESC)?;
    trace.next_where("switcher closed", |line| {
        line["view"] == "switcher" && line["change"] == "closed"
    })?;
    ctrl_k(&mut input)?;
    trace.next_where("switcher opened again", |line| {
        line["view"] == "switcher" && line["change"] == "opened"
    })?;
    input.key(keys::ENTER)?;
    let opened = trace.next_where("chat opened", |line| {
        line["view"] == "chat" && line["change"] == "opened"
    })?;
    let chat = opened["conversation"].as_str().unwrap_or_default().to_owned();
    if !listed.contains(&chat) {
        return Err(format!("the switcher opened {chat:?}, not a listed chat: {listed:?}").into());
    }
    trace.next_where("switcher closed after", |line| {
        line["view"] == "switcher" && line["change"] == "closed"
    })?;
    Ok(chat)
}

/// c posts to a chat of a's that isn't the one shown: the app's notification reaches the sandbox's fake server (from
/// the app's own connection to the bus it was given, with the body's action), and activating it opens that chat.
fn a_message_notifies_and_opens_its_chat(
    sandbox: &Sandbox,
    trace: &mut TraceReader,
    running: &Running,
    shown: &str,
) -> SandboxResult {
    let target = if shown == AC_CHAT { ABC_CHAT } else { AC_CHAT };
    let staged = sandbox.path("post-as-c.staged");
    fs::write(&staged, target)?;
    fs::rename(&staged, sandbox.path(POST_TRIGGER))?;
    running
        .line_where(WAIT, |line| line.starts_with("posted "))
        .map_err(|error| format!("c didn't post: {error:?}"))?;
    trace.wait_where("notification shown for c's message", |line| {
        line["view"] == "notification" && line["change"] == "shown" && line["conversation"] == target
    })?;
    let notifications = sandbox.bus().notifications();
    let notification = notifications
        .iter()
        .find(|notification| notification.body == C_SAYS)
        .ok_or_else(|| format!("no notification of c's message: {notifications:?}"))?;
    if notification.app_name != app::title(Alias::A).as_ref()
        || notification.actions != ["default", "Open"]
        || notification.summary.is_empty()
    {
        return Err(format!("the notification: {notification:?}").into());
    }
    // the app's one connection: the tray item is on it too
    if !sandbox
        .bus()
        .tray_items()
        .iter()
        .any(|item| item.sender == notification.sender)
    {
        return Err(format!("the notification came from another connection: {notification:?}").into());
    }
    sandbox.bus().invoke_action(notification.id, "default")?;
    trace.wait_where("notification opened", |line| {
        line["view"] == "notification" && line["change"] == "opened" && line["conversation"] == target
    })?;
    trace.wait_where("its chat opened", |line| {
        line["view"] == "chat" && line["change"] == "opened" && line["conversation"] == target
    })?;
    Ok(())
}

/// The tray item: registered under the app's name, its title the app's; closing the window keeps the app, the watcher
/// gone brings the window back and a new one has the item again, a click on the item opens the window again, and the
/// menu's "Quit" quits.
fn the_tray_keeps_the_app_and_quits_it(sandbox: &Sandbox, trace: &mut TraceReader, running: &Running) -> SandboxResult {
    let name = format!("org.kde.StatusNotifierItem-{}-1", running.pid());
    let items = sandbox.bus().tray_items();
    if !items.iter().any(|item| item.service == name) {
        return Err(format!("no tray item {name}: {items:?}").into());
    }
    trace.wait_where("the tray's unread total", |line| {
        line["view"] == "tray" && line["change"] == "unread"
    })?;
    let host = sandbox.bus().connect()?;
    let item = zbus::blocking::Proxy::new(&host, name.as_str(), ITEM_PATH, "org.kde.StatusNotifierItem")?;
    let title: String = item.get_property("Title")?;
    if !title.starts_with(app::title(Alias::A).as_ref()) {
        return Err(format!("the item's title: {title}").into());
    }

    sandbox.niri_msg(&["action", "close-window"])?;
    trace.next_where("window closed", |line| {
        line["view"] == "window" && line["change"] == "closed"
    })?;
    stays(running)?;
    the_tray_follows_its_watcher(sandbox, trace, running, &name)?;
    item.call_method("Activate", &(0_i32, 0_i32))?;
    trace.next_where("tray open", |line| line["view"] == "tray" && line["change"] == "open")?;
    trace.next_where("window opened again", |line| {
        line["view"] == "window" && line["change"] == "opened"
    })?;
    wait_until(WAIT, || {
        sandbox
            .niri_msg(&["windows"])
            .ok()
            .filter(|windows| windows.contains(teams_ui::window::LAB_APP_ID))
    })
    .ok_or("the window didn't come back")?;
    sandbox.niri_msg(&["action", "close-window"])?;
    trace.next_where("window closed again", |line| {
        line["view"] == "window" && line["change"] == "closed"
    })?;

    let menu = zbus::blocking::Proxy::new(&host, name.as_str(), MENU_PATH, "com.canonical.dbusmenu")?;
    menu.call_method("Event", &(MENU_QUIT, "clicked", zbus::zvariant::Value::from(""), 0_u32))?;
    trace.next_where("tray quit", |line| line["view"] == "tray" && line["change"] == "quit")?;
    Ok(())
}

/// The app stays up (its engine isn't released) for [`STAYS`] after its window closed.
fn stays(running: &Running) -> SandboxResult {
    match running.line_where(STAYS, |line| line == "engine-released") {
        Err(LineError::Timeout) => Ok(()),
        other => Err(format!("the app didn't stay in the tray: {other:?}").into()),
    }
}

/// With the window closed into the tray: the watcher stopped brings the window back (nothing shows the app any more),
/// started again has the item `name` registered with it again, and the window closed leaves the app up again.
fn the_tray_follows_its_watcher(
    sandbox: &Sandbox,
    trace: &mut TraceReader,
    running: &Running,
    name: &str,
) -> SandboxResult {
    let registered = |sandbox: &Sandbox| {
        sandbox
            .bus()
            .tray_items()
            .iter()
            .filter(|item| item.service == name)
            .count()
    };
    let before = registered(sandbox);
    sandbox.bus().stop_watcher()?;
    trace.next_where("tray gone", |line| line["view"] == "tray" && line["change"] == "gone")?;
    trace.next_where("window back", |line| {
        line["view"] == "window" && line["change"] == "opened"
    })?;
    wait_until(WAIT, || {
        sandbox
            .niri_msg(&["windows"])
            .ok()
            .filter(|windows| windows.contains(teams_ui::window::LAB_APP_ID))
    })
    .ok_or("the window didn't come back when the watcher left")?;
    sandbox.bus().start_watcher()?;
    trace.next_where("tray registered again", |line| {
        line["view"] == "tray" && line["change"] == "registered"
    })?;
    if registered(sandbox) != before + 1 {
        return Err(format!(
            "the item didn't register with the new watcher: {:?}",
            sandbox.bus().tray_items()
        )
        .into());
    }
    sandbox.niri_msg(&["action", "close-window"])?;
    trace.next_where("window closed into the tray again", |line| {
        line["view"] == "window" && line["change"] == "closed"
    })?;
    stays(running)
}

fn gpui_s_http_client_is_the_null_one(sandbox: &Sandbox) -> SandboxResult {
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, "http")])?;
    let running = Running::new(child)?;
    let refused = running
        .line_where(STARTUP, |line| line.starts_with("client-refused "))
        .map_err(|error| format!("no client-refused line: {error:?}"))?;
    if !refused.contains("No HttpClient available") {
        return Err(format!("GPUI's HTTP client isn't the null one: {refused}").into());
    }
    let connections = running
        .line_where(WAIT, |line| line.starts_with("connections "))
        .map_err(|error| format!("no connections line: {error:?}"))?;
    if connections != "connections 0" {
        return Err(format!("a request left the process: {connections}").into());
    }
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the http child exited {status}: {stderr}").into());
    }
    Ok(())
}

// --- the children ---

/// Runs `done` once `frames` more frames of `window` have been presented.
fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// One account's mock token, for the transport and the push channel alike; it never runs out in a test.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: std::time::SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

/// The mock and a's engine on it, as `teams_core::lab::start` builds one but for the mock origins, a test roster and
/// a test log; its push channel on the Trouter mock.
struct MockLab {
    _runtime: tokio::runtime::Runtime,
    mock: Arc<ChatServiceMock>,
    _trouter: TrouterMock,
    dir: tempfile::TempDir,
}

impl MockLab {
    fn start() -> SandboxResult<(Self, EngineHandle)> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let dir = tempfile::tempdir()?;
        let config = LabConfig::parse(&lab_toml(dir.path()), dir.path())?;
        record_abc_chat(config.paths(), ABC_CHAT)?;
        let tokens = Arc::new(Tokens {
            token: mock.token(LAB_A).ok_or("the mock's token for a")?,
            renewals: broadcast::channel(4).0,
        });
        let fence = Arc::new(Fence::new(
            Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
            FenceLog::from_writer(io::sink()),
        ));
        let origin = mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &origin);
        let transport = Arc::new(http()?);
        let membership = LiveMembershipSource::with_mock_origin(&origin)?;
        membership.set_credential_source(transport.clone());
        let trouter_client = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())?
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
        let engine = engine::start(Parts {
            store: Store::open(&config.paths().store_file(Alias::A))?,
            client: Arc::new(Client::new(http()?)),
            outbox: Outbox::new(transport, fence, Arc::new(membership), author),
            push: Some(Push::new(trouter_client)),
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })?;
        let lab = Self {
            _runtime: runtime,
            mock: Arc::new(mock),
            _trouter: trouter,
            dir,
        };
        Ok((lab, engine))
    }

    /// Whether the account's engine lock is free: the engine's thread has ended.
    fn engine_released(&self) -> SandboxResult<bool> {
        let config = LabConfig::parse(&lab_toml(self.dir.path()), self.dir.path())?;
        let store = Store::open(&config.paths().store_file(Alias::A))?;
        Ok(store.engine_lock().is_ok())
    }
}

/// The trace descriptor the parent passed.
fn passed_trace_fd() -> SandboxResult<RawFd> {
    Ok(std::env::var(TRACE_FD_ENV)?.parse()?)
}

fn child_window() -> SandboxResult {
    teams_app::guard::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(passed_trace_fd()?)?);
    let (lab, engine) = MockLab::start()?;
    let settings = teams_ui::SettingsFile::new(lab.dir.path().join("ui").join("a.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let mock = Arc::clone(&lab.mock);
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                alias: Alias::A,
                engine,
                broker: None,
                signed_out: None,
                trace,
                settings,
                // the sandbox's private bus, with its fake notification server and tray watcher
                bus: teams_ui::bus::address_from_env(),
                diagnostics: None,
            },
        );
        match launched {
            Ok(launched) => {
                post_when_asked(mock, cx);
                let _ = launched.window.update(cx, |_, window, _| {
                    after_frames(window, READY_FRAMES, |window, _| {
                        say(&format!("rem-size {}", window.rem_size().as_f32()));
                        say("ready");
                    });
                });
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
    if lab.engine_released()? {
        say("engine-released");
    }
    Ok(())
}

/// Once the parent writes a chat's id to [`POST_TRIGGER`] in the sandbox dir, c posts [`C_SAYS`] there on the mock,
/// which delivers it to a's push channel as any post of c's.
fn post_when_asked(mock: Arc<ChatServiceMock>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let trigger = PathBuf::from(dir).join(POST_TRIGGER);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(chat) = fs::read_to_string(&trigger) else {
                continue;
            };
            let _ = fs::remove_file(&trigger);
            match mock.post_as(LAB_C, chat.trim(), &format!("<p>{C_SAYS}</p>")) {
                Some(id) => say(&format!("posted {id}")),
                None => say("post refused"),
            }
            return;
        }
    })
    .detach();
}

/// A window with one image whose source is an `https` URL.
struct Picture {
    url: String,
}

impl Render for Picture {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(BACKGROUND))
            .child(img(self.url.clone()).size(px(64.0)))
    }
}

/// How many connections `listener` (non-blocking) has waiting.
fn accepted(listener: &TcpListener) -> usize {
    std::iter::from_fn(|| listener.accept().ok()).count()
}

fn child_http() -> SandboxResult {
    teams_app::guard::window_allowed_here()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let url = format!("https://127.0.0.1:{}/picture.png", listener.local_addr()?.port());
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(320.0), px(240.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        let picture_url = url.clone();
        let opened = cx.open_window(options, move |_, cx| cx.new(|_| Picture { url: picture_url }));
        let Ok(window) = opened else {
            say("no window");
            cx.quit();
            return;
        };
        let _ = window.update(cx, move |_, window, _| {
            after_frames(window, 10, move |_, cx| check_http(cx, url, listener));
        });
    });
    Ok(())
}

/// After the image had its chance to load: a request through GPUI's own client, then the listener's count.
fn check_http(cx: &mut App, url: String, listener: TcpListener) {
    let client = cx.http_client();
    cx.spawn(async move |cx: &mut AsyncApp| {
        cx.background_executor().timer(Duration::from_secs(1)).await;
        match client.get(&url, ().into(), true).await {
            Ok(response) => say(&format!("client-answered {}", response.status())),
            Err(error) => say(&format!("client-refused {error:#}")),
        }
        say(&format!("connections {}", accepted(&listener)));
        cx.update(|cx| cx.quit());
    })
    .detach();
}
