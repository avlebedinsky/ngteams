//! The UI kit's text area and our theme on the real platform, in the UI sandbox (`harness = false`: GPUI runs on the
//! process's main thread).
//!
//! The test runs itself again inside the sandbox (its compositor, its private bus, nothing of the desktop's): a window
//! with the kit's `Textarea` set up as the compose box will be (Enter is the send, Shift-Enter breaks the line, soft
//! wrap), which reports on stdout every value, row count and Enter it sees. The parent types into it with the
//! sandbox's virtual keyboard: text, a selection copied and pasted, everything cut and pasted back, undo and redo
//! (through the nested compositor's own clipboard, never the desktop's), Shift-Enter then Enter, and a line too long
//! for the box. Then it switches the theme live the way a desktop does: a fake settings portal on the sandbox's bus
//! (`org.freedesktop.portal.Settings`, the one GPUI and the kit ask) says the colour scheme changed, and the window's
//! theme follows to dark and back. Last, a message's link drawn by our rich text renderer is clicked with the virtual
//! pointer and goes to the sandbox's no-op opener.
//!
//! IME composition can't be driven here (the sandbox has no input method); `tests/kit.rs` drives it through GPUI's
//! input handler on the test context, with the clipboard's plain-text-only paste.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::cell::Cell;
use std::io::{self, Write as _};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Window, WindowBounds, WindowOptions, WindowingModes, div, px, rgb, size,
};
use gpui_component::input::{Enter, InputEvent, Textarea, TextareaState};
use gpui_component::theme::Theme;
use teams_ui::theme;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};
use zbus::blocking::{Connection, connection::Builder};
use zbus::interface;
use zbus::zvariant::{OwnedValue, Value};

/// Which mode the child runs in.
const CHILD_ENV: &str = "TEAMS_UI_KIT_WINDOW_TEST";
/// The child window's Wayland app id.
const APP_ID: &str = "ms-teams-kit-test";
/// The text area's width: narrow, so a long line wraps.
const WIDTH: f32 = 240.0;
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 3;

/// The settings portal's bus name, object path and interface.
const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
/// The colour scheme's namespace and key, and its values: 1 prefers dark, 2 prefers light.
const APPEARANCE: &str = "org.freedesktop.appearance";
const COLOR_SCHEME: &str = "color-scheme";
const PREFER_DARK: u32 = 1;
const PREFER_LIGHT: u32 = 2;

fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("textarea") => child(),
        Ok("link") => link_child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kit window test: {error}");
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

// --- the parent: the sandbox, the fake portal, the keyboard ---

/// A fake settings portal: the colour scheme it holds, and nothing else.
struct Settings {
    scheme: Arc<AtomicU32>,
}

#[interface(name = "org.freedesktop.portal.Settings")]
impl Settings {
    #[zbus(property, name = "version")]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn version(&self) -> u32 {
        1
    }

    fn read(&self, namespace: &str, key: &str) -> zbus::fdo::Result<OwnedValue> {
        if namespace == APPEARANCE && key == COLOR_SCHEME {
            Ok(OwnedValue::from(self.scheme.load(Ordering::SeqCst)))
        } else {
            Err(zbus::fdo::Error::Failed(format!("no setting {namespace} {key}")))
        }
    }
}

/// The fake portal served on the sandbox's bus, and the way to change its colour scheme.
struct Portal {
    connection: Connection,
    scheme: Arc<AtomicU32>,
}

impl Portal {
    /// Serves the portal on `sandbox`'s bus (its address only), light at first.
    fn start(sandbox: &Sandbox) -> SandboxResult<Self> {
        let scheme = Arc::new(AtomicU32::new(PREFER_LIGHT));
        let settings = Settings {
            scheme: Arc::clone(&scheme),
        };
        let connection = Builder::address(sandbox.bus().address())?
            .name(PORTAL_NAME)?
            .serve_at(PORTAL_PATH, settings)?
            .build()?;
        Ok(Self { connection, scheme })
    }

    /// Sets the colour scheme and tells the bus, as a desktop does when the user switches.
    fn set(&self, scheme: u32) -> SandboxResult {
        self.scheme.store(scheme, Ordering::SeqCst);
        self.connection.emit_signal(
            None::<&str>,
            PORTAL_PATH,
            SETTINGS,
            "SettingChanged",
            &(APPEARANCE, COLOR_SCHEME, Value::U32(scheme)),
        )?;
        Ok(())
    }
}

/// The child running in the sandbox and the keyboard typing into it.
struct Session {
    running: Running,
    input: VirtualInput,
}

impl Session {
    /// Waits for the child's line `line`; the lines before it are skipped.
    fn expect(&mut self, line: &str) -> SandboxResult {
        self.running
            .line_where(WAIT, |got| got == line)
            .map(|_| ())
            .map_err(|error| self.running.fail(&format!("no line {line:?}: {error:?}")))
    }

    /// Waits for every line of `lines`, in any order (an action's handler runs before the event it emits is
    /// delivered); the other lines between are skipped.
    fn expect_all(&mut self, lines: &[&str]) -> SandboxResult {
        let mut missing: Vec<&str> = lines.to_vec();
        while !missing.is_empty() {
            let line = self
                .running
                .line_where(WAIT, |got| missing.contains(&got))
                .map_err(|error| self.running.fail(&format!("no lines {missing:?}: {error:?}")))?;
            missing.retain(|wanted| *wanted != line);
        }
        Ok(())
    }

    /// Waits for the text area to hold exactly `text`.
    fn expect_value(&mut self, text: &str) -> SandboxResult {
        self.expect(&format!("value {text:?}"))
    }

    fn chord(&mut self, modifier: Modifier, typed: char) -> SandboxResult {
        let (key, _) = self
            .input
            .keymap()
            .key_for(typed)
            .ok_or_else(|| format!("{typed:?} isn't on the US keymap"))?;
        self.input.key_with(&[modifier], key)
    }
}

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("teams-ui kit window", &options)? else {
        return Ok(());
    };
    let portal = Portal::start(&sandbox)?;
    drive(&sandbox, &portal).map_err(|error| format!("{error}{}", sandbox.logs()))?;
    a_message_s_link_opens_through_the_sandbox_s_opener(&sandbox)
        .map_err(|error| format!("{error}{}", sandbox.logs()))?;
    say("teams-ui kit window: ok");
    Ok(())
}

fn drive(sandbox: &Sandbox, portal: &Portal) -> SandboxResult {
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, "textarea")])?;
    let running = Running::new(child)?;
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| format!("no ready line: {error:?}"))?;
    let mut session = Session {
        running,
        input: sandbox.input()?,
    };
    session.expect("theme light")?;
    typing_and_the_clipboard(&mut session)?;
    enter_and_shift_enter(&mut session)?;
    a_long_line_wraps(&mut session)?;
    the_theme_follows_the_desktop(&mut session, portal)?;

    sandbox.niri_msg(&["action", "close-window"])?;
    let (status, stderr) = session.running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the child exited {status}: {stderr}").into());
    }
    Ok(())
}

/// Typing; a selection copied and pasted; everything cut and pasted back; undo and redo.
fn typing_and_the_clipboard(session: &mut Session) -> SandboxResult {
    session.input.type_text("hello world")?;
    session.expect_value("hello world")?;
    for _ in 0.."world".len() {
        session.input.key_with(&[Modifier::Shift], keys::LEFT)?;
    }
    session.chord(Modifier::Ctrl, 'c')?;
    // Right drops the selection at its end
    session.input.key(keys::RIGHT)?;
    session.chord(Modifier::Ctrl, 'v')?;
    session.expect_value("hello worldworld")?;
    session.chord(Modifier::Ctrl, 'a')?;
    session.chord(Modifier::Ctrl, 'x')?;
    session.expect_value("")?;
    session.chord(Modifier::Ctrl, 'v')?;
    session.expect_value("hello worldworld")?;
    session.chord(Modifier::Ctrl, 'z')?;
    session.expect_value("")?;
    session.chord(Modifier::Ctrl, 'y')?;
    session.expect_value("hello worldworld")
}

/// Shift-Enter breaks the line; Enter is the send and types nothing.
fn enter_and_shift_enter(session: &mut Session) -> SandboxResult {
    session.input.key_with(&[Modifier::Shift], keys::ENTER)?;
    session.expect("enter shift=true")?;
    session.input.type_text("x")?;
    session.expect_value("hello worldworld\nx")?;
    session.input.key(keys::ENTER)?;
    session.expect_all(&["enter shift=false", "send"])?;
    // had Enter typed a line break, this value would never come
    session.input.type_text("y")?;
    session.expect_value("hello worldworld\nxy")
}

/// A line too long for the box wraps over several rows without a line break.
fn a_long_line_wraps(session: &mut Session) -> SandboxResult {
    session.chord(Modifier::Ctrl, 'a')?;
    let long = "a line of words much too long for the narrow box";
    session.input.type_text(long)?;
    session.expect_value(long)?;
    let rows = session
        .running
        .line_where(WAIT, |line| {
            line.strip_prefix("rows ")
                .and_then(|rows| rows.parse::<usize>().ok())
                .is_some_and(|rows| rows > 1)
        })
        .map_err(|error| session.running.fail(&format!("the long line never wrapped: {error:?}")))?;
    say(&format!("wrapped: {rows}"));
    Ok(())
}

/// The desktop switches to dark and back; the window's theme follows each time.
fn the_theme_follows_the_desktop(session: &mut Session, portal: &Portal) -> SandboxResult {
    portal.set(PREFER_DARK)?;
    session.expect("theme dark")?;
    portal.set(PREFER_LIGHT)?;
    session.expect("theme light")
}

// --- the child: the window ---

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

/// The compose box: the kit's text area, reporting what it holds.
struct Pad {
    state: Entity<TextareaState>,
    _events: Subscription,
    _appearance: Subscription,
}

impl Pad {
    fn new(appearance: Subscription, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .submit_on_enter(true)
                .soft_wrap(true)
                .auto_grow(1, 8)
        });
        let events = cx.subscribe_in(&state, window, |_: &mut Self, state, event, window, cx| {
            report(state, event, window, cx);
        });
        state.update(cx, |state, cx| state.focus(window, cx));
        Self {
            state,
            _events: events,
            _appearance: appearance,
        }
    }
}

/// Says what the text area holds after a change (and, two frames on, over how many rows), and each Enter.
fn report(state: &Entity<TextareaState>, event: &InputEvent, window: &mut Window, cx: &mut App) {
    match event {
        InputEvent::Change => {
            say(&format!("value {:?}", state.read(cx).value().as_ref()));
            let state = state.clone();
            after_frames(window, 2, move |_, cx| {
                say(&format!("rows {}", state.read(cx).display_map().wrap_row_count()));
            });
        }
        InputEvent::PressEnter { shift, .. } => say(&format!("enter shift={shift}")),
        InputEvent::Focus | InputEvent::Blur => {}
    }
}

impl Render for Pad {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(theme::current(cx).background))
            .on_action(cx.listener(|_, _: &Enter, _, _| say("send")))
            .child(div().w(px(WIDTH)).child(Textarea::new(&self.state)))
    }
}

/// Says each time the theme's mode changes.
fn report_theme_changes(cx: &mut App) {
    let dark = Rc::new(Cell::new(false));
    cx.observe_global::<Theme>(move |cx| {
        let now = theme::mode(cx).is_dark();
        if dark.replace(now) != now {
            say(if now { "theme dark" } else { "theme light" });
        }
    })
    .detach();
}

fn open(cx: &mut App) -> gpui::Result<()> {
    let bounds = Bounds::centered(None, size(px(640.0), px(400.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        app_id: Some(APP_ID.to_owned()),
        ..WindowOptions::default()
    };
    cx.open_window(options, |window, cx| {
        let following = theme::follow(window, cx);
        after_frames(window, READY_FRAMES, |_, cx| {
            say("ready");
            say(if theme::mode(cx).is_dark() {
                "theme dark"
            } else {
                "theme light"
            });
        });
        cx.new(|cx| Pad::new(following, window, cx))
    })?;
    Ok(())
}

/// The link of the `link` child's message.
const LINK: &str = "https://example.invalid/teams-ui?link=1";

/// A message's link, drawn by our rich text renderer in a window filling the output, clicked with the sandbox's
/// virtual pointer: GPUI opens it through the sandbox's no-op opener (`xdg-open`, the private bus having no portal),
/// never the desktop's.
fn a_message_s_link_opens_through_the_sandbox_s_opener(sandbox: &Sandbox) -> SandboxResult {
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, "link")])?;
    let mut running = Running::new(child)?;
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    let mut input = sandbox.input()?;
    // the link's first words, at the window's top left (the window fills the output, with no gaps or border)
    input.click(24, 16, ui_sandbox::input::keys::BTN_LEFT)?;
    let wanted = ui_sandbox::opener::Opened {
        launcher: "xdg-open".to_owned(),
        args: vec![LINK.to_owned()],
    };
    ui_sandbox::wait_until(WAIT, || sandbox.opened().ok().filter(|opened| opened.contains(&wanted)))
        .ok_or_else(|| running.fail(&format!("the opener never got the link: {:?}", sandbox.opened())))?;
    sandbox.niri_msg(&["action", "close-window"])?;
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the link child exited {status}: {stderr}").into());
    }
    Ok(())
}

/// A message with one link, drawn large.
struct LinkMessage {
    rendered: Arc<teams_ui::rich_text::Rendered>,
}

impl Render for LinkMessage {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let id = gpui::SharedString::from("message-link");
        div()
            .size_full()
            .bg(rgb(theme::LIGHT_PALETTE.background))
            .text_size(px(32.0))
            .child(teams_ui::rich_text::render(&id, &self.rendered, theme::LIGHT_PALETTE))
    }
}

fn link_child() -> SandboxResult {
    ui_sandbox::window_allowed()?;
    let content = format!("<p><a href=\"{LINK}\">open the made-up page, a long link to click on</a></p>");
    let rendered = Arc::new(teams_ui::rich_text::layout(
        &teams_proto::chat::parse_html(&content),
        &teams_ui::rich_text::Mentions::default(),
    ));
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(640.0), px(400.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some(APP_ID.to_owned()),
            ..WindowOptions::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            after_frames(window, READY_FRAMES, |_, _| say("ready"));
            cx.new(|_| LinkMessage { rendered })
        });
        if let Err(error) = opened {
            eprintln!("failed to open the window: {error:#}");
            cx.quit();
        }
    });
    Ok(())
}

fn child() -> SandboxResult {
    ui_sandbox::window_allowed()?;
    let failed = Rc::new(Cell::new(false));
    let failed_in_app = Rc::clone(&failed);
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        theme::init(cx);
        report_theme_changes(cx);
        if let Err(error) = open(cx) {
            eprintln!("failed to open the window: {error:#}");
            failed_in_app.set(true);
            cx.quit();
        }
    });
    if failed.get() {
        return Err("the window didn't open".into());
    }
    Ok(())
}
