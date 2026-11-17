//! The settings' AI providers box, drawn on the real platform in the UI sandbox (`harness = false`: GPUI runs on the
//! process's main thread), for its screenshots: the app's window over the fake engine (`common`) on the General page,
//! with Claude Code found at a made-up path, an Anthropic API provider with a key, and an OpenAI-compatible server on
//! a mock on the loopback (synthetic data; no key is real, nothing leaves the loopback). Each surface (the list, the
//! editor of the server after "Test connection", a new provider's editor) runs the test again inside the sandbox in
//! Teams Light and Teams Dark; the parent screenshots each window into
//! `target/ui-shots/ai-providers-<surface>-<theme>.png`.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the shared fake engine of the view tests (test code, which unwraps): its test-context rig and trace \
              capture serve them, not a window; this binary runs without the test harness, so not as `cfg(test)`"
)]
#[path = "../../common/mod.rs"]
mod common;

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use std::{fs, mem, thread};

use common::{FakeCore, FakeEvents};
use gpui::{App, AppContext as _, Entity, Window};
use teams_ui::local_ai::{Backend, Provider, Tools};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::settings_view::Page;
use teams_ui::shell::{Shell, ShellParts};
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
#[path = "../window_fixture.rs"]
#[expect(dead_code, reason = "the shared fixture's unsized sandbox serves the other scenes")]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sized_sandbox_or_skip};

/// Which mode the child runs in: `<theme>:<surface>`.
const CHILD_ENV: &str = "TEAMS_UI_AI_PROVIDERS_WINDOW_TEST";
/// The child window's Wayland app id.
const APP_ID: &str = "ngteams-ai-providers-test";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// Frames the window presents before each step, and before the child says `ready`.
const FRAMES: usize = 30;
/// The surfaces shot.
const SURFACES: [&str; 3] = ["list", "editor", "new"];
/// The screen: tall enough for the whole General page with an editor open.
const SCREEN: (u32, u32) = (1280, 1800);
/// What the mock lists.
const MODELS: &str = r#"{"data":[{"id":"llama-4-scout"},{"id":"qwen-3-coder"},{"id":"gemma-4"}]}"#;

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(mode) => child(&mode),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ai providers window test: {error}");
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

/// Where the offline runs' screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn parent() -> SandboxResult {
    let Some(sandbox) = sized_sandbox_or_skip("teams-ui ai providers window", Some(SCREEN))? else {
        return Ok(());
    };
    for (theme, name) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        for surface in SURFACES {
            shot(&sandbox, theme, name, surface).map_err(|error| format!("{error}{}", sandbox.logs()))?;
        }
    }
    say("teams-ui ai providers window: ok");
    Ok(())
}

/// Runs the child on `surface` in `theme` and screenshots its window.
fn shot(sandbox: &Sandbox, theme: &str, name: &str, surface: &str) -> SandboxResult {
    let mode = format!("{theme}:{surface}");
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, &mode)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    let file = format!("ai-providers-{surface}-{name}.png");
    let png = sandbox.screenshot(&file, true)?;
    let kept = shots_dir()?.join(&file);
    fs::copy(&png, &kept)?;
    say(&format!("shot {}", kept.display()));
    sandbox.close_window()?;
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the child exited {status}: {stderr}").into());
    }
    Ok(())
}

// --- the child: the app's window over the fake engine ---

/// A mock of an OpenAI-compatible server on the loopback: each request gets the made-up models; its base URL.
fn mock() -> io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = answer(stream);
        }
    });
    Ok(base)
}

fn answer(mut stream: TcpStream) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim_end().is_empty() {
            break;
        }
    }
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{MODELS}",
        MODELS.len()
    )
}

/// The made-up providers: Claude Code (found), an Anthropic API with a key, the mock's server.
fn providers(base: String) -> Vec<Provider> {
    let anthropic = Provider {
        name: "Anthropic".to_owned(),
        has_key: true,
        ..Provider::new_api(Backend::Anthropic, &[])
    };
    let server = Provider {
        id: "api-2".to_owned(),
        name: "Ollama on this computer".to_owned(),
        base_url: Some(base),
        model: Some("llama-4-scout".to_owned()),
        ..Provider::new_api(Backend::Compatible, &[])
    };
    vec![Provider::cli(Backend::Claude), anthropic, server]
}

fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

fn child(mode: &str) -> SandboxResult {
    fixture::window_allowed()?;
    let (theme, surface) = mode.split_once(':').ok_or("bad child mode")?;
    let theme: &'static str = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let surface = surface.to_owned();
    let base = mock()?;
    let application = fixture::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Err(error) = open(theme, surface, base, cx) {
            eprintln!("failed to open the window: {error:#}");
            cx.quit();
        }
    });
    Ok(())
}

fn open(theme: &'static str, surface: String, base: String, cx: &mut App) -> gpui::Result<()> {
    let core = Arc::new(FakeCore::default());
    let (events, received) = mpsc::unbounded_channel();
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let models = Models::new(cx);
    let shared: Arc<dyn Core> = core;
    let bridge = cx.new(|cx| Bridge::start(source, shared, models.clone(), cx));
    let settings = Settings {
        appearance: Appearance {
            follow_system: false,
            theme,
            ..Appearance::default()
        },
        ai_providers: providers(base),
        ..Settings::default()
    };
    // Claude Code "installed" at a path that nothing runs
    cx.set_global(Tools::with(vec![(Backend::Claude, "/made-up/claude".into())]));
    let settings = SettingsModel::in_memory(settings);
    settings.publish_ai(cx);
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings: cx.new(|_| settings),
        trace: Arc::new(ViewTrace::off()),
    };
    let window = teams_ui::window::open(cx, APP_ID, "AI providers".into(), parts)?;
    let shell = teams_ui::window::content(window, cx)
        .and_then(|root| root.read(cx).shell_in(cx).cloned())
        .ok_or_else(|| io::Error::other("missing the window's shell"))?;
    // the bridge and the event channel live as long as the app
    mem::forget((bridge, events));
    window.update(cx, |_, window, _| {
        after_frames(window, FRAMES, move |window, cx| {
            show(&shell, &surface, window, cx);
            after_frames(window, FRAMES, |_, _| say("ready"));
        });
    })?;
    Ok(())
}

/// Puts the window in `surface`'s state.
fn show(shell: &Entity<Shell>, surface: &str, window: &mut Window, cx: &mut App) {
    shell.update(cx, |shell, cx| shell.open_settings_page(Page::General, window, cx));
    let view = shell.read(cx).settings_view().clone();
    let providers = view.update(cx, teams_ui::settings_view::SettingsView::ai_providers);
    providers.update(cx, |providers, cx| match surface {
        "editor" => {
            providers.edit("api-2", window, cx);
            providers.test_connection(cx);
        }
        "new" => providers.add(Backend::OpenAi, window, cx),
        _ => {}
    });
}
