//! Synthetic calendar screenshots at 1024px and 1280px, in light and dark themes. No tenant is reached.
#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared synthetic view-test engine, no live requests"
)]
#[path = "../../common/mod.rs"]
mod common;

use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use gpui::{App, AppContext as _, Window};
use teams_ui::{
    Models, ViewTrace,
    calendar::Mode,
    settings::{Settings, SettingsModel},
    shell::ShellParts,
    theme::{Appearance, DARK_KEY, LIGHT_KEY},
};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

const CHILD: &str = "TEAMS_UI_CALENDAR_WINDOW_TEST";

pub(super) fn main() -> ExitCode {
    let result = match std::env::var(CHILD) {
        Ok(mode) => child(&mode),
        Err(_) => parent(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("calendar window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(text: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

fn shots() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn parent() -> SandboxResult {
    for width in [1024, 1280] {
        let options = SandboxOptions {
            width,
            height: 768,
            niri_extra: NIRI_FULL_WINDOW,
            ..SandboxOptions::default()
        };
        let Some(sandbox) = sandbox_or_skip("calendar toolbar", &options)? else {
            return Ok(());
        };
        for (theme, name) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
            for mode in ["week", "day"] {
                let child_mode = format!("{theme}:{mode}");
                let file = format!("calendar-{width}-{mode}-{name}.png");
                shot(&sandbox, &child_mode, &file, width).map_err(|error| format!("{error}{}", sandbox.logs()))?;
            }
        }
    }
    Ok(())
}

fn shot(sandbox: &Sandbox, mode: &str, file: &str, width: u32) -> SandboxResult {
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD, mode)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(Duration::from_secs(90), |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready: {error:?}")))?;
    // Nested niri's winit output can keep its default size even when the outer Weston output is smaller.
    // Set the actual window's column width, then verify the captured pixels rather than trusting the options.
    sandbox.niri_msg(&["action", "set-column-width", &width.to_string()])?;
    std::thread::sleep(Duration::from_millis(1500));
    let png = sandbox.screenshot(file, true)?;
    let (actual, _) = image::image_dimensions(&png)?;
    if actual != width {
        return Err(format!("calendar screenshot width {actual}, expected {width}").into());
    }
    let kept = shots()?.join(file);
    fs::copy(png, &kept)?;
    say(&format!("shot {}", kept.display()));
    sandbox.niri_msg(&["action", "close-window"])?;
    let (status, stderr) = running.finish(Duration::from_secs(20))?;
    if !status.success() {
        return Err(format!("child {status}: {stderr}").into());
    }
    Ok(())
}

fn after_frames(window: &mut Window, n: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if n <= 1 {
            done(window, cx);
        } else {
            after_frames(window, n - 1, done);
        }
    });
    window.refresh();
}

fn child(mode: &str) -> SandboxResult {
    ui_sandbox::window_allowed()?;
    let (theme, mode) = mode.split_once(':').ok_or("bad calendar child mode")?;
    let theme = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let day = mode == "day";
    gpui_platform::linux(gpui::WindowingModes::WAYLAND)
        .with_assets(teams_ui::AppAssets)
        .run(move |cx: &mut App| {
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            if let Err(error) = open(theme, day, cx) {
                eprintln!("calendar open: {error}");
                cx.quit();
            }
        });
    Ok(())
}

fn open(theme: &'static str, day: bool, cx: &mut App) -> gpui::Result<()> {
    let parts = ShellParts {
        core: Arc::new(common::FakeCore::default()),
        models: Models::new(cx),
        settings: cx.new(|_| {
            SettingsModel::in_memory(Settings {
                appearance: Appearance {
                    follow_system: false,
                    theme,
                    ..Appearance::default()
                },
                ..Settings::default()
            })
        }),
        trace: Arc::new(ViewTrace::off()),
    };
    let window = teams_ui::window::open(cx, "ngteams-calendar-test", "Calendar".into(), parts)?;
    let root = teams_ui::window::content(window, cx).ok_or_else(|| io::Error::other("missing shell"))?;
    let shell = root
        .read(cx)
        .shell_in(cx)
        .cloned()
        .ok_or_else(|| io::Error::other("missing shell"))?;
    window.update(cx, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.show_calendar(window, cx));
        let calendar = shell.read(cx).calendar_view().clone();
        calendar.update(cx, |calendar, cx| {
            if let Some(date) = chrono::NaiveDate::from_ymd_opt(2026, 11, 10) {
                calendar.go_to(date, cx);
            }
            calendar.set_mode(if day { Mode::Day } else { Mode::WorkWeek }, cx);
        });
        after_frames(window, 30, |_, _| say("ready"));
    })?;
    Ok(())
}
