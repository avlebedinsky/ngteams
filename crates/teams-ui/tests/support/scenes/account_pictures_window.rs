//! Two accounts with the same profile picture, one with a picture the user chose ([`teams_ui::account_picture`]),
//! drawn on the real platform in the UI sandbox (`harness = false`: GPUI runs on the process's main thread), for
//! their screenshots. The app's window holds both accounts over two fake engines (`common`); the pictures are made
//! here (synthetic). Each surface (`rail`: a shown, its rail and title bar; `menu`: b shown, its account menu open;
//! `settings`: Settings > Accounts) runs the test again inside the sandbox in Teams Light and Teams Dark; the parent
//! screenshots each window into `target/ui-shots/account-pictures-<surface>-<theme>.png`.
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

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use std::{fs, mem};

use common::{FakeCore, FakeEvents};
use gpui::{App, AppContext as _, Entity, Window};
use image::{Rgba, RgbaImage};
use teams_core::Person;
use teams_store::StoredPicture;
use teams_ui::accounts::AccountParts;
use teams_ui::desktop::{Desktop, DesktopParts};
use teams_ui::settings::{Settings, SettingsFile, SettingsModel};
use teams_ui::settings_view::Page;
use teams_ui::shell::{Shell, ShellParts};
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
#[path = "../window_fixture.rs"]
#[expect(dead_code, reason = "the shared fixture's unsized sandbox serves the other scenes")]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sized_sandbox_or_skip};

/// Which mode the child runs in: `<theme>:<surface>:<dir>`.
const CHILD_ENV: &str = "TEAMS_UI_ACCOUNT_PICTURES_WINDOW_TEST";
/// The child window's Wayland app id.
const APP_ID: &str = "ngteams-account-pictures-test";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// Frames the window presents before each step, and before the child says `ready`.
const FRAMES: usize = 30;
/// The surfaces shot.
const SURFACES: [&str; 3] = ["rail", "menu", "settings"];
/// The screen.
const SCREEN: (u32, u32) = (1280, 800);
/// The made-up accounts.
const LAB_A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(mode) => child(&mode),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("account pictures window test: {error}");
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
    let Some(sandbox) = sized_sandbox_or_skip("teams-ui account pictures window", Some(SCREEN))? else {
        return Ok(());
    };
    for (theme, name) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        for surface in SURFACES {
            shot(&sandbox, theme, name, surface).map_err(|error| format!("{error}{}", sandbox.logs()))?;
        }
    }
    say("teams-ui account pictures window: ok");
    Ok(())
}

/// Runs the child on `surface` in `theme` and screenshots its window.
fn shot(sandbox: &Sandbox, theme: &str, name: &str, surface: &str) -> SandboxResult {
    let dir = tempfile::tempdir()?;
    let mode = format!("{theme}:{surface}:{}", dir.path().display());
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, &mode)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    let file = format!("account-pictures-{surface}-{name}.png");
    let png = sandbox.screenshot(&file, false)?;
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

// --- the child: the app's window of two accounts over fake engines ---

/// The profile picture both accounts have: a grey face, as a photo of a person can look in a small circle.
fn profile_picture() -> StoredPicture {
    let side = 96_u32;
    let image = RgbaImage::from_fn(side, side, |x, y| {
        let (dx, dy) = (f64::from(x) - 48.0, f64::from(y) - 38.0);
        let head = dx * dx + dy * dy < 18.0 * 18.0;
        let (sx, sy) = (f64::from(x) - 48.0, f64::from(y) - 100.0);
        let body = sx * sx + sy * sy < 40.0 * 40.0;
        if head || body {
            Rgba([238, 238, 238, 255])
        } else {
            Rgba([122, 128, 140, 255])
        }
    });
    StoredPicture {
        media_type: "image/png".to_owned(),
        bytes: png_bytes(&image),
    }
}

/// The picture the user chose for b: an orange sun on teal, wider than high (the account's picture is its centre).
fn chosen_picture() -> RgbaImage {
    RgbaImage::from_fn(300, 200, |x, y| {
        let (dx, dy) = (f64::from(x) - 150.0, f64::from(y) - 100.0);
        if dx * dx + dy * dy < 55.0 * 55.0 {
            Rgba([255, 160, 40, 255])
        } else {
            Rgba([0, 128, 128, 255])
        }
    })
}

fn png_bytes(image: &RgbaImage) -> Vec<u8> {
    let mut png = io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).unwrap_or_default();
    png.into_inner()
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
    let mut parts = mode.splitn(3, ':');
    let (Some(theme), Some(surface), Some(dir)) = (parts.next(), parts.next(), parts.next()) else {
        return Err("bad child mode".into());
    };
    let theme: &'static str = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let (surface, dir) = (surface.to_owned(), PathBuf::from(dir));
    let chosen = dir.join("sun.png");
    chosen_picture().save(&chosen)?;
    let settings = Settings {
        appearance: Appearance {
            follow_system: false,
            theme,
            ..Appearance::default()
        },
        ..Settings::default()
    };
    let file = SettingsFile::new(dir.join("settings.toml"));
    file.save(&settings)?;
    let application = fixture::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Err(error) = open(&surface, file, chosen, cx) {
            eprintln!("failed to open the window: {error:#}");
            cx.quit();
        }
    });
    Ok(())
}

/// An account's fake engine with the shared profile picture, and its bridge.
fn account(me: &str, name: &str, cx: &mut App) -> (Arc<dyn Core>, Models) {
    let core = Arc::new(FakeCore {
        me: me.to_owned().into(),
        ..FakeCore::default()
    });
    core.people
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            me.to_owned(),
            Person {
                mri: me.to_owned(),
                name: Some(name.to_owned()),
                from: None,
                picture: Some(profile_picture()),
                looking: false,
            },
        );
    let (events, received) = mpsc::unbounded_channel();
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let models = Models::new(cx);
    let shared: Arc<dyn Core> = core;
    let bridge: Entity<Bridge> = cx.new(|cx| Bridge::start(source, shared, models.clone(), cx));
    let core = bridge.read(cx).core();
    // the bridge and the event channel live as long as the app
    mem::forget((bridge, events));
    (core, models)
}

fn open(surface: &str, file: SettingsFile, chosen: PathBuf, cx: &mut App) -> gpui::Result<()> {
    let (core, models) = account(LAB_A, "Aye Work", cx);
    let shell = ShellParts {
        core,
        models,
        settings: cx.new(|_| SettingsModel::load(file)),
        trace: Arc::new(ViewTrace::off()),
    };
    let desktop = cx.new(|cx| {
        Desktop::new(
            DesktopParts {
                app_id: APP_ID,
                app_name: "Teams (test)".into(),
                title: "Account pictures".into(),
                shell,
                account_name: "Aye Work".into(),
                account_detail: "aye@example.test".into(),
                account_server: None,
                can_add: true,
                ringer: None,
                pictures_dir: None,
            },
            cx,
        )
    });
    let (core, models) = account(LAB_B, "Aye Work", cx);
    let b = AccountParts {
        core,
        models,
        name: "Aye Work".into(),
        detail: "aye@contoso.example.test".into(),
        server: None,
    };
    desktop.update(cx, |desktop, cx| desktop.add_account(b, cx));
    let accounts = desktop.read(cx).accounts().clone();
    accounts
        .update(cx, |accounts, cx| accounts.use_picture(LAB_B, chosen, cx))
        .detach();
    let window = desktop.update(cx, Desktop::open_window)?;
    // the desktop lives as long as the app
    mem::forget(desktop.clone());
    let surface = surface.to_owned();
    window.update(cx, |_, window, _| {
        after_frames(window, FRAMES, move |window, cx| {
            show(&desktop, &surface, window, cx);
            after_frames(window, FRAMES, |_, _| say("ready"));
        });
    })?;
    Ok(())
}

/// Puts the window in `surface`'s state.
fn show(desktop: &Entity<Desktop>, surface: &str, window: &mut Window, cx: &mut App) {
    match surface {
        "menu" => {
            desktop.update(cx, |desktop, cx| desktop.switch(LAB_B, cx));
            if let Some(shell) = shown_shell(window, cx) {
                shell.update(cx, Shell::toggle_status_menu);
            }
        }
        "settings" => {
            if let Some(shell) = shown_shell(window, cx) {
                shell.update(cx, |shell, cx| shell.open_settings_page(Page::Accounts, window, cx));
            }
        }
        _ => {}
    }
}

/// The shell `window` shows, read from the window itself (its handle can't be read while it draws).
fn shown_shell(window: &Window, cx: &App) -> Option<Entity<Shell>> {
    let kit = window.root::<teams_ui::window::KitRoot>()??;
    let root = kit.read(cx).view().clone().downcast::<teams_ui::Root>().ok()?;
    root.read(cx).shell_in(cx).cloned()
}
