//! Bold, semibold and italic text drawn on the real platform, in the UI sandbox (`harness = false`: GPUI runs on the
//! process's main thread), measured from a screenshot: the UI family the kit names for the system's font, then each
//! of a few common families the machine has (static faces, and variable ones such as Adwaita Sans and Cantarell, whose
//! one face GPUI draws at the weight asked for on its `wght` axis), one row per style. A bold or semibold row must
//! carry clearly more ink than its regular row, and an italic row must differ from it without turning bold: text
//! drawn regular whatever it asked for, or in a weight it didn't ask for, is what this guards against. Screenshots
//! are kept under `target/ui-shots/font-styles*.png`. Windows draws one family per short centered window so shell
//! overlays near the screen's bottom cannot contaminate the measured rows.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use gpui::{
    App, AppContext as _, Bounds, Context, FontWeight, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Window, WindowBounds, WindowOptions, div, px, rgb, size,
};
use gpui_component::theme::Theme;
use teams_ui::theme;
#[path = "../window_fixture.rs"]
mod fixture;
use fixture::{Running, SandboxResult, sandbox_or_skip};

/// Which mode the child runs in.
const CHILD_ENV: &str = "TEAMS_UI_FONT_STYLES_TEST";
/// The child window's Wayland app id.
const APP_ID: &str = "ngteams-font-styles-test";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 10;
/// A row's height and its text's size, logical px.
const ROW: f32 = 34.0;
const TEXT: f32 = 22.0;
/// The text of every row.
const SAMPLE: &str = "Settings titles and mentions of me, 0123";
/// Families drawn after the kit's UI family, when the machine has them.
#[cfg(target_os = "linux")]
const FAMILIES: [&str; 4] = ["Noto Sans", "Adwaita Sans", "Cantarell", "DejaVu Sans"];
/// A family no machine has.
#[cfg(windows)]
const FAMILIES: [&str; 4] = ["Segoe UI", "Arial", "Times New Roman", "Courier New"];
const MISSING_FAMILY: &str = "No Such Family Teams UI Test";
/// At most this many families, so every row fits the output.
const MAX_FAMILIES: usize = 5;
/// The styles of a family's rows, in order.
const STYLES: [Style; 4] = [Style::Regular, Style::Semibold, Style::Bold, Style::Italic];
/// How much more ink a semibold and a bold row carry than the regular row, at least.
const SEMIBOLD_INK: f64 = 1.08;
const BOLD_INK: f64 = 1.15;

#[derive(Clone, Copy, Debug)]
enum Style {
    Regular,
    Semibold,
    Bold,
    Italic,
}

pub(super) fn main() -> ExitCode {
    let outcome = if std::env::var_os(CHILD_ENV).is_some() {
        child()
    } else {
        parent()
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("font styles test: {error}");
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
    let Some(sandbox) = sandbox_or_skip("teams-ui font styles")? else {
        return Ok(());
    };
    let mut index = 0;
    loop {
        let pages = page(&sandbox, index)?;
        index += 1;
        if index >= pages {
            break;
        }
    }
    say("teams-ui font styles: ok");
    Ok(())
}

fn page(sandbox: &fixture::Sandbox, index: usize) -> SandboxResult<usize> {
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, &index.to_string())])?;
    let mut running = Running::new(child)?;
    let missing = running
        .line_where(STARTUP, |line| line.starts_with("missing "))
        .map_err(|error| running.fail(&format!("no missing family line: {error:?}")))?;
    if missing.ends_with(MISSING_FAMILY) {
        return Err(running.fail(&"a family the machine lacks reached the kit's theme"));
    }
    let pages = running
        .line_where(STARTUP, |line| line.starts_with("pages "))
        .map_err(|error| running.fail(&format!("no page count: {error:?}")))?;
    let pages = pages.trim_start_matches("pages ").parse()?;
    let ready = running
        .line_where(STARTUP, |line| line.starts_with("ready "))
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    let (row_px, families) = parse_ready(&ready)?;
    let name = if cfg!(windows) {
        format!("font-styles-{index}.png")
    } else {
        "font-styles.png".to_owned()
    };
    let png = sandbox.screenshot(&name, true)?;
    let kept = shots_dir()?.join(name);
    fs::copy(&png, &kept)?;
    say(&format!("shot {}", kept.display()));
    sandbox.close_window()?;
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the child exited {status}: {stderr}{}", sandbox.logs()).into());
    }
    check(&image::open(&png)?.to_luma8(), row_px, &families)?;
    Ok(pages)
}

/// The child's `ready <row height, device px> <family>|<family>…` line.
fn parse_ready(line: &str) -> SandboxResult<(u32, Vec<String>)> {
    let rest = line.strip_prefix("ready ").unwrap_or_default();
    let (row_px, families) = rest.split_once(' ').ok_or("a ready line without families")?;
    Ok((row_px.parse()?, families.split('|').map(str::to_owned).collect()))
}

/// Each family's styled rows against its regular row.
fn check(shot: &image::GrayImage, row_px: u32, families: &[String]) -> SandboxResult {
    let row = |index: usize| -> Vec<u8> {
        let top = u32::try_from(index).unwrap_or(u32::MAX).saturating_mul(row_px);
        let bottom = (top + row_px).min(shot.height());
        (top..bottom)
            .flat_map(|y| (0..shot.width()).map(move |x| (x, y)))
            .map(|(x, y)| shot.get_pixel(x, y).0[0])
            .collect()
    };
    let ink = |pixels: &[u8]| -> f64 { pixels.iter().map(|&luma| f64::from(255 - luma)).sum() };
    let mut failures = Vec::new();
    for (index, family) in families.iter().enumerate() {
        let first = index * STYLES.len();
        let regular = row(first);
        let regular_ink = ink(&regular);
        if regular_ink == 0.0 {
            failures.push(format!("{family}: the regular row is blank"));
            continue;
        }
        for (offset, style) in STYLES.iter().enumerate().skip(1) {
            let styled = row(first + offset);
            let ratio = ink(&styled) / regular_ink;
            say(&format!("{family} {style:?}: {ratio:.2}x the regular row's ink"));
            let fine = match style {
                Style::Regular => true,
                Style::Semibold => ratio >= SEMIBOLD_INK,
                Style::Bold => ratio >= BOLD_INK,
                Style::Italic => styled != regular && ratio < BOLD_INK,
            };
            if !fine {
                failures.push(format!(
                    "{family} {style:?} is drawn wrong ({ratio:.2}x the regular row's ink)"
                ));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}

// --- the child: the window ---

/// The families' rows, one per style.
struct Rows {
    families: Vec<SharedString>,
}

impl Render for Rows {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.families.iter().flat_map(|family| {
            STYLES.iter().map(move |style| {
                let row = div()
                    .h(px(ROW))
                    .px_4()
                    .flex()
                    .items_center()
                    .font_family(family.clone())
                    .text_size(px(TEXT))
                    .text_color(rgb(0x00_0000))
                    .child(SAMPLE);
                match style {
                    Style::Regular => row,
                    Style::Semibold => row.font_weight(FontWeight::SEMIBOLD),
                    Style::Bold => row.font_weight(FontWeight::BOLD),
                    Style::Italic => row.italic(),
                }
            })
        });
        div().size_full().bg(rgb(0xff_ffff)).flex().flex_col().children(rows)
    }
}

/// A family the machine lacks chosen in the settings: says the family the kit draws in then (GPUI would draw a missing
/// one through its own fallback list, regular and upright whatever was asked), and goes back to the system's font.
fn say_missing_family(cx: &mut App) {
    let missing = theme::Appearance {
        font_family: Some(MISSING_FAMILY.to_owned()),
        ..theme::Appearance::default()
    };
    theme::set_appearance(&missing, cx);
    say(&format!("missing {}", Theme::global(cx).font_family));
    theme::set_appearance(&theme::Appearance::default(), cx);
}

/// The kit's UI family, then the [`FAMILIES`] the machine has.
fn families(cx: &App) -> Vec<SharedString> {
    let installed = cx.text_system().all_font_names();
    let mut families = vec![Theme::global(cx).font_family.clone()];
    for family in FAMILIES {
        let family = SharedString::from(family);
        if installed.iter().any(|name| *name == *family) && !families.contains(&family) {
            families.push(family);
        }
    }
    families.truncate(MAX_FAMILIES);
    families
}

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

fn child() -> SandboxResult {
    fixture::window_allowed()?;
    let page: usize = std::env::var(CHILD_ENV)?.parse()?;
    fixture::application().run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        theme::init(cx);
        say_missing_family(cx);
        let families = families(cx);
        let pages = if cfg!(windows) { families.len() } else { 1 };
        say(&format!("pages {pages}"));
        let families = if cfg!(windows) {
            families.into_iter().skip(page).take(1).collect()
        } else {
            families
        };
        let height = if cfg!(windows) { 220.0 } else { 800.0 };
        let bounds = Bounds::centered(None, size(px(1280.0), px(height)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some(APP_ID.to_owned()),
            #[cfg(windows)]
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("NGTEAMS SYNTHETIC FONT STYLES".into()),
                ..Default::default()
            }),
            ..WindowOptions::default()
        };
        let names = families.iter().map(SharedString::as_ref).collect::<Vec<_>>().join("|");
        let opened = cx.open_window(options, move |window, cx| {
            after_frames(window, READY_FRAMES, move |window, _| {
                say(&format!("ready {:.0} {names}", ROW * window.scale_factor()));
            });
            cx.new(|_| Rows { families })
        });
        if let Err(error) = opened {
            eprintln!("failed to open the window: {error:#}");
            cx.quit();
        }
    });
    Ok(())
}
