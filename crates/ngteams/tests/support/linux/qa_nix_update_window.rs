//! QA of task 580: the update notice of a Nix install, as a user sees and uses it in the app's window, drawn on the
//! real platform in the UI sandbox (`harness = false`: GPUI runs on the process's main thread).
//!
//! The child is the app's desktop window over the fake engine, with the app's updater (`ngteams::update`) over a
//! made-up release tree of 0.1.0.5 and a local fake release feed that offers 0.1.0.7. The tree's marker says how the
//! copy was installed: each Nix kind (`nix-profile`, `nix-nixos`, `nix-home-manager`, `nix`), and the unpacked archive
//! as the control. The parent asks the child for a check, then looks at the title bar, hovers the button for its
//! tooltip, clicks it and reads the sandbox's clipboard, in Teams Light and Teams Dark. At the end the child reports
//! what the feed served and what the update folder holds: a Nix copy downloads nothing and installs nothing.
//!
//! The screenshots go to `target/ui-shots/nix-update-*.png`. Without weston 15, niri or `dbus-daemon` the test says so
//! and passes, unless `GPUI_SMOKE_REQUIRE=1`.

#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the shared fake engine of the view tests (test code, which unwraps): its test-context rig and trace \
              capture serve them, not a window; this binary runs without the test harness, so not as `cfg(test)`"
)]
#[path = "../../../../teams-ui/tests/common/mod.rs"]
mod common;

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use std::{fs, mem, thread};

use common::{FakeCore, FakeEvents};
use gpui::{App, AppContext as _, AsyncApp, WindowingModes};
use image::{GenericImageView as _, RgbaImage};
use ngteams::update::{self, Started, Updater};
use teams_ui::desktop::{Desktop, DesktopParts};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::update::{self as button, UpdateStatus};
use teams_ui::{Bridge, Models, ShellParts, ViewTrace};
use teams_update::fakes::{Feed, archive_bytes, publish, tree};
use teams_update::{Format, IDENTITY, Installation, NixInstall, Places, Source, Version};
use tokio::sync::mpsc;
use ui_sandbox::input::keys;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

/// The child's case (`<kind>:<theme>`) and the folder where the parent drops its signals.
const CASE_ENV: &str = "NGTEAMS_QA_NIX_UPDATE_CASE";
const SIGNALS_ENV: &str = "NGTEAMS_QA_NIX_UPDATE_SIGNALS";
const APP_ID: &str = "ngteams-qa-nix-update";
const STARTUP: Duration = Duration::from_secs(90);
const WAIT: Duration = Duration::from_secs(20);
const READY_FRAMES: usize = 30;
/// The title bar's height, over which the button's change is looked for.
const TITLE_ROWS: u32 = 48;
const NEWER: &str = "0.1.0.7";
/// What the clipboard holds before the click: another program's copy.
const BEFORE: &str = "made-up before";

/// A case: the marker's kind, its name in the shots, and the command that the notice must name.
struct Kind {
    key: &'static str,
    format: Format,
    command: Option<&'static str>,
}

const KINDS: [Kind; 5] = [
    Kind {
        key: "profile",
        format: Format::Nix(NixInstall::Profile),
        command: Some("nix profile upgrade ngteams"),
    },
    Kind {
        key: "nixos",
        format: Format::Nix(NixInstall::NixOs),
        command: Some(
            "sudo nix flake update ngteams --flake /etc/nixos && sudo nixos-rebuild switch --flake /etc/nixos",
        ),
    },
    Kind {
        key: "home-manager",
        format: Format::Nix(NixInstall::HomeManager),
        command: Some("nix flake update ngteams && home-manager switch --flake ."),
    },
    Kind {
        key: "other",
        format: Format::Nix(NixInstall::Other),
        command: None,
    },
    Kind {
        key: "archive",
        format: Format::Archive,
        command: None,
    },
];

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CASE_ENV) {
        Ok(case) => child(&case),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa nix update window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

// --- the parent: the user's eyes and hands ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams nix update window", &options)? else {
        return Ok(());
    };
    let mut failures = Vec::new();
    for kind in &KINDS {
        for theme in [LIGHT_KEY, DARK_KEY] {
            let name = format!("{}-{}", kind.key, if theme == DARK_KEY { "dark" } else { "light" });
            match case(&sandbox, kind, theme, &name) {
                Ok(()) => say(&format!("case {name}: ok")),
                Err(error) => {
                    say(&format!("case {name}: FAILED: {error}"));
                    failures.push(format!("{name}: {error}"));
                }
            }
        }
    }
    if failures.is_empty() {
        say("ngteams nix update window: ok");
        Ok(())
    } else {
        Err(format!(
            "{} case(s) failed:\n{}{}",
            failures.len(),
            failures.join("\n"),
            sandbox.logs()
        )
        .into())
    }
}

/// The part of the screen where two shots differ, in `rows`: (left, top, right, bottom), right and bottom inclusive.
fn changed(before: &RgbaImage, after: &RgbaImage, rows: std::ops::Range<u32>) -> Option<(u32, u32, u32, u32)> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in rows.start..rows.end.min(after.height()).min(before.height()) {
        for x in 0..after.width().min(before.width()) {
            let (a, b) = (before.get_pixel(x, y).0, after.get_pixel(x, y).0);
            if a.iter().zip(b).any(|(a, b)| a.abs_diff(b) > 24) {
                found = Some(found.map_or((x, y, x, y), |(l, t, r, bm)| (l.min(x), t.min(y), r.max(x), bm.max(y))));
            }
        }
    }
    found
}

fn shot(sandbox: &Sandbox, file: &str) -> SandboxResult<RgbaImage> {
    let png = sandbox.screenshot(file, false)?;
    let kept = shots_dir()?.join(file);
    fs::copy(&png, &kept)?;
    Ok(image::open(&kept)?.to_rgba8())
}

/// Keeps the part `(x, y, w, h)` of `image` as `file` in the shots.
fn crop(image: &RgbaImage, (x, y, w, h): (u32, u32, u32, u32), file: &str) -> SandboxResult {
    let (x, y) = (x.min(image.width() - 1), y.min(image.height() - 1));
    let (w, h) = (w.min(image.width() - x), h.min(image.height() - y));
    image.view(x, y, w, h).to_image().save(shots_dir()?.join(file))?;
    Ok(())
}

fn clipboard_text(sandbox: &Sandbox) -> SandboxResult<Option<String>> {
    Ok(sandbox
        .read_clipboard()?
        .iter()
        .find(|(mime, _)| mime.starts_with("text/plain"))
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned()))
}

fn case(sandbox: &Sandbox, kind: &Kind, theme: &str, name: &str) -> SandboxResult {
    let signals = tempfile::tempdir()?;
    let signals_arg = signals.path().to_string_lossy().into_owned();
    let child = sandbox.spawn(
        &std::env::current_exe()?,
        &[],
        &[
            (CASE_ENV, &format!("{}:{theme}", kind.key)),
            (SIGNALS_ENV, &signals_arg),
        ],
    )?;
    let mut running = Running::new(child)?;
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    let before = shot(sandbox, &format!("nix-update-{name}-before.png"))?;
    fs::write(signals.path().join("check"), "")?;
    let status = running
        .line_where(WAIT, |line| line.starts_with("status "))
        .map_err(|error| running.fail(&format!("no status line: {error:?}")))?;
    say(&format!("{name}: {status}"));
    says_its_kind(kind, &status)?;
    thread::sleep(Duration::from_millis(600));
    let after = shot(sandbox, &format!("nix-update-{name}-after.png"))?;
    let button =
        changed(&before, &after, 0..TITLE_ROWS).ok_or("the title bar did not change after the check: no button")?;
    say(&format!("{name}: button at {button:?}"));
    crop(
        &after,
        (0, 0, after.width(), TITLE_ROWS),
        &format!("nix-update-{name}-titlebar.png"),
    )?;
    let hovered = hover(sandbox, &after, button, name)?;
    if kind.format != Format::Archive {
        click(sandbox, kind, &hovered, button, name)?;
    }
    sandbox.input()?.move_to(5, 400)?;
    fs::write(signals.path().join("report"), "")?;
    report(running, kind, name)
}

/// Whether the status line of the child is the notice of `kind`, or the archive's download.
fn says_its_kind(kind: &Kind, status: &str) -> SandboxResult {
    if kind.format == Format::Archive {
        if !status.contains(&format!("Ready(\"{NEWER}\")")) {
            return Err(format!("the archive copy must download and offer the restart: {status}").into());
        }
        return Ok(());
    }
    let expected = format!("command: {:?}", kind.command);
    if !status.contains("Manual") || !status.contains(&expected) || !status.contains(NEWER) {
        return Err(format!("expected Manual of {NEWER} with {expected}: {status}").into());
    }
    Ok(())
}

fn middle((left, top, right, bottom): (u32, u32, u32, u32)) -> (u32, u32) {
    (u32::midpoint(left, right), u32::midpoint(top, bottom))
}

/// Puts the pointer over `button` and waits for its tooltip; the screen with it.
fn hover(sandbox: &Sandbox, after: &RgbaImage, button: (u32, u32, u32, u32), name: &str) -> SandboxResult<RgbaImage> {
    let (x, y) = middle(button);
    sandbox.input()?.move_to(x, y)?;
    // the tooltip opens after a short hover; wait until something below the title bar changed
    let mut hovered = None;
    let found = wait_until(Duration::from_secs(5), || {
        thread::sleep(Duration::from_millis(400));
        let image = shot(sandbox, &format!("nix-update-{name}-tooltip.png")).ok()?;
        let tip = changed(after, &image, TITLE_ROWS..image.height())?;
        hovered = Some(image);
        Some(tip)
    });
    let (Some(tip), Some(hovered)) = (found, hovered) else {
        return Err("no tooltip showed while the pointer was over the button".into());
    };
    say(&format!("{name}: tooltip at {tip:?}"));
    let margin = 12;
    let left = tip.0.min(button.0);
    crop(
        &hovered,
        (
            left.saturating_sub(margin),
            0,
            tip.2.max(button.2) - left + 2 * margin,
            tip.3 + margin,
        ),
        &format!("nix-update-{name}-tooltip-crop.png"),
    )?;
    Ok(hovered)
}

/// Clicks `button` over another program's copy: the clipboard holds the command of `kind` after it, and a toast says
/// so; a copy without a command leaves the clipboard alone.
fn click(
    sandbox: &Sandbox,
    kind: &Kind,
    hovered: &RgbaImage,
    button: (u32, u32, u32, u32),
    name: &str,
) -> SandboxResult {
    sandbox.set_clipboard(vec![(
        "text/plain;charset=utf-8".to_owned(),
        BEFORE.as_bytes().to_vec(),
    )])?;
    thread::sleep(Duration::from_millis(300));
    let (x, y) = middle(button);
    sandbox.input()?.click(x, y, keys::BTN_LEFT)?;
    let copied = wait_until(Duration::from_secs(5), || {
        clipboard_text(sandbox).ok().flatten().filter(|text| text != BEFORE)
    });
    let clicked = shot(sandbox, &format!("nix-update-{name}-clicked.png"))?;
    say(&format!("{name}: clipboard {copied:?}"));
    if copied.as_deref() != kind.command {
        return Err(format!("the clipboard holds {copied:?}, expected {:?}", kind.command).into());
    }
    if kind.command.is_some() && changed(hovered, &clicked, TITLE_ROWS..clicked.height()).is_none() {
        return Err("no toast showed after the click".into());
    }
    Ok(())
}

/// What the child reports at its end: a Nix copy downloaded nothing and installed nothing; the archive downloaded.
fn report(running: Running, kind: &Kind, name: &str) -> SandboxResult {
    let line = |prefix: &'static str| {
        running
            .line_where(WAIT, |line| line.starts_with(prefix))
            .map_err(|error| format!("no {prefix}line: {error:?}"))
    };
    let requests = line("requests ")?;
    let staged = line("staged ")?;
    let identity = line("identity ")?;
    say(&format!("{name}: {requests}\n{name}: {staged}\n{name}: {identity}"));
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the child exited {status}: {stderr}").into());
    }
    let downloaded = requests.contains("/download/");
    if kind.format == Format::Archive {
        if !downloaded || staged == "staged 0" {
            return Err(format!("the archive copy must download its update: {requests}; {staged}").into());
        }
    } else if downloaded || staged != "staged 0" {
        return Err(format!("a Nix copy must download nothing: {requests}; {staged}").into());
    }
    if identity != "identity 0.1.0.5" {
        return Err(format!("nothing may be installed: {identity}").into());
    }
    Ok(())
}

// --- the child: the app's window with its updater ---

/// Every file under `dir`, at any depth.
fn files(dir: &Path) -> usize {
    fs::read_dir(dir).map_or(0, |entries| {
        entries
            .map_while(Result::ok)
            .map(|entry| {
                let path = entry.path();
                if path.is_dir() { files(&path) } else { 1 }
            })
            .sum()
    })
}

/// What the child watches: the parent's signals, the feed and the made-up installation.
struct Watch {
    signals: PathBuf,
    feed: Feed,
    home: tempfile::TempDir,
    root: PathBuf,
    checked: bool,
    said: bool,
}

impl Watch {
    /// One look at the signals and the status; true once the child reported and quits.
    fn tick(&mut self, cx: &mut App) -> bool {
        if !self.checked && self.signals.join("check").exists() {
            self.checked = true;
            if let Some(task) = update::check(cx) {
                task.detach();
            }
        }
        let status = button::status(cx);
        let settled = !matches!(status, UpdateStatus::None | UpdateStatus::Downloading { .. });
        if self.checked && settled && !self.said {
            self.said = true;
            say(&format!("status {status:?}"));
        }
        if !self.signals.join("report").exists() {
            return false;
        }
        self.report();
        cx.quit();
        true
    }

    fn report(&self) {
        let lines: Vec<String> = self
            .feed
            .requests()
            .iter()
            .map(|request| request.lines().next().unwrap_or_default().to_owned())
            .collect();
        say(&format!("requests {}", lines.join(", ")));
        // the update folder, or a folder beside the app's: anything the updater wrote
        say(&format!("staged {}", files(self.home.path()) - files(&self.root)));
        let identity = fs::read_to_string(self.root.join(IDENTITY)).unwrap_or_default();
        say(&format!("identity {}", identity.trim()));
    }
}

/// A made-up release tree of 0.1.0.5 marked as `kind`, its updater, and a feed that offers [`NEWER`].
fn made_up(kind: &Kind, signals: PathBuf) -> SandboxResult<(Updater, Watch)> {
    let home = tempfile::tempdir()?;
    let root = home.path().join("opt/ngteams");
    tree(&root, "0.1.0.5", kind.format)?;
    let installation = Installation::detect(&Places {
        executable: root.join("usr/bin/ngteams"),
        appimage: None,
        appdir: None,
        stage_dir: home.path().join("updates"),
        protected: Vec::new(),
    })?;
    if installation.format() != kind.format {
        return Err(format!("detected {:?}, expected {:?}", installation.format(), kind.format).into());
    }
    let feed = Feed::start()?;
    let newer = Version::parse(NEWER).ok_or("version")?;
    publish(
        &feed,
        &format!("v{NEWER}"),
        &Format::Archive.asset(newer),
        &archive_bytes(NEWER, |_| {})?,
    );
    let updater = Updater {
        current: Version::parse("0.1.0.5").ok_or("version")?,
        installation,
        source: Source::local(&feed.base),
    };
    let watch = Watch {
        signals,
        feed,
        home,
        root,
        checked: false,
        said: false,
    };
    Ok((updater, watch))
}

fn child(case: &str) -> SandboxResult {
    ui_sandbox::window_allowed()?;
    let (key, theme) = case.split_once(':').ok_or("a case is <kind>:<theme>")?;
    let kind = KINDS.iter().find(|kind| kind.key == key).ok_or("no such kind")?;
    let theme: &'static str = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let (updater, mut watch) = made_up(kind, PathBuf::from(std::env::var(SIGNALS_ENV)?))?;
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        if let Err(error) = open(theme, updater, cx) {
            eprintln!("failed to open the window: {error:#}");
            cx.quit();
            return;
        }
        cx.spawn(async move |cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                if cx.update(|cx| watch.tick(cx)) {
                    break;
                }
            }
        })
        .detach();
    });
    Ok(())
}

fn after_frames(window: &gpui::Window, frames: usize, done: impl FnOnce(&mut gpui::Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

fn open(theme: &'static str, updater: Updater, cx: &mut App) -> gpui::Result<()> {
    let core: Arc<dyn teams_ui::Core> = Arc::new(FakeCore::default());
    let (events, received) = mpsc::unbounded_channel();
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let models = Models::new(cx);
    let drained = models.clone();
    let bridge = cx.new(|cx| Bridge::start(source, core, drained, cx));
    let settings = Settings {
        appearance: Appearance {
            follow_system: false,
            theme,
            ..Appearance::default()
        },
        ..Settings::default()
    };
    let shell = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings: cx.new(|_| SettingsModel::in_memory(settings)),
        trace: Arc::new(ViewTrace::off()),
    };
    let desktop = cx.new(|cx| {
        Desktop::new(
            DesktopParts {
                app_id: APP_ID,
                app_name: "Teams (QA)".into(),
                title: "Teams (QA)".into(),
                shell,
                account_name: "Lab A".into(),
                account_detail: "synthetic account".into(),
                account_server: None,
                can_add: false,
                ringer: None,
                pictures_dir: None,
            },
            cx,
        )
    });
    update::start(
        Started {
            updater,
            holds: update::holds(desktop.downgrade()),
            restart: Rc::default(),
            quit: Rc::new(|cx: &mut App| cx.quit()),
            first_check: None,
        },
        cx,
    );
    let window = desktop.update(cx, Desktop::open_window)?;
    mem::forget((bridge, events, desktop));
    window.update(cx, |_, window, _| {
        after_frames(window, READY_FRAMES, |_, _| say("ready"));
    })?;
    Ok(())
}
