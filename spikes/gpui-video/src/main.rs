//! M0 Task 15: GPUI on niri/Wayland smoke check.
//!
//! Opens one window on the Wayland compositor `WAYLAND_DISPLAY` names, draws a line of text and a checkerboard image
//! uploaded as a GPUI `RenderImage` (the path Task 16 takes for video frames), and once a few frames have been
//! presented prints one line to stdout:
//!
//! ```text
//! ready compositor=<name> gpu=<device> driver=<driver> software=<bool>
//! ```
//!
//! (`compositor` is what GPUI calls the display server: `Wayland`.) It quits when its window is closed, or after
//! `--exit-after-ms <N>`. It refuses to start (exit 1) when `WAYLAND_DISPLAY` is unset or empty. The build has no X11
//! backend and allows only Wayland (`WindowingModes::WAYLAND`), so it never falls back to X11 or to running headless:
//! a `WAYLAND_DISPLAY` it can't connect to makes GPUI panic at startup. GPUI's log goes to stderr as
//! `[LEVEL target] message`: GPUI's own records from info up, everyone else's from warn up.
//!
//! Run it only against a private compositor (the smoke test starts a headless weston with a nested niri). The GPU is
//! chosen by GPUI itself: `ZED_DEVICE_ID=<pci device id>` puts that adapter first, otherwise the compositor's GPU, then
//! a discrete one.

use std::{cell::Cell, ffi::OsStr, io::Write as _, process::ExitCode, rc::Rc, sync::Arc, time::Duration};

use gpui::{
    App, AppContext as _, Bounds, Context, GpuSpecs, IntoElement, ParentElement as _, Render, RenderImage,
    SharedString, Styled as _, Window, WindowBounds, WindowHandle, WindowOptions, WindowingModes, div, img, px, rgb,
    size,
};
use image::{Frame, ImageBuffer, Rgba};

/// The window's Wayland app id (the window has no title).
const APP_ID: &str = "ms-teams-gpui-smoke";
/// The text line the window draws.
const TEXT: &str = "ms-teams GPUI smoke";
/// Window background, 0xRRGGBB.
const BACKGROUND: u32 = 0x0020_2830;
/// Text colour, 0xRRGGBB: white, which the checkerboard never uses.
const TEXT_COLOR: u32 = 0x00ff_ffff;
/// The checkerboard's two colours, 0xRRGGBB: magenta and cyan, which neither the background nor the text uses.
const TILE_COLORS: [u32; 2] = [0x00ff_00ff, 0x0000_ffff];
/// The checkerboard's side and tile side, in pixels.
const IMAGE_SIDE: u32 = 128;
const TILE_SIDE: u32 = 16;
/// Frames presented before the window reports ready.
const READY_FRAMES: usize = 3;

/// Command-line options.
#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    /// Quit this long after the window reports ready, unless it's closed first.
    exit_after: Option<Duration>,
}

fn parse_options(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--exit-after-ms" => {
                let value = args.next().ok_or("--exit-after-ms needs a value")?;
                let millis = value
                    .parse::<u64>()
                    .map_err(|error| format!("--exit-after-ms {value}: {error}"))?;
                options.exit_after = Some(Duration::from_millis(millis));
            }
            other => {
                return Err(format!(
                    "unknown argument: {other} (usage: gpui-video [--exit-after-ms N])"
                ));
            }
        }
    }
    Ok(options)
}

/// A `side`×`side` checkerboard of `tile`-pixel squares in the two `colors` (0xRRGGBB, top-left first), as the BGRA
/// bytes GPUI's `RenderImage` holds (in an `image` buffer typed RGBA).
fn checkerboard_bgra(side: u32, tile: u32, colors: [u32; 2]) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let bgra = colors.map(|color| {
        let [_, red, green, blue] = color.to_be_bytes();
        Rgba([blue, green, red, u8::MAX])
    });
    ImageBuffer::from_fn(side, side, |x, y| bgra[usize::from((x / tile + y / tile) % 2 == 1)])
}

/// The line printed once the window has presented [`READY_FRAMES`] frames.
fn ready_line(compositor: &str, gpu: Option<&GpuSpecs>) -> String {
    match gpu {
        Some(gpu) => format!(
            "ready compositor={compositor} gpu={:?} driver={:?} software={}",
            gpu.device_name, gpu.driver_name, gpu.is_software_emulated
        ),
        None => format!("ready compositor={compositor} gpu=unknown driver=unknown software=unknown"),
    }
}

/// The window's view: a line of text over a checkerboard image.
struct Smoke {
    text: SharedString,
    image: Arc<RenderImage>,
}

impl Render for Smoke {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let side = px(f32::from(u16::try_from(IMAGE_SIDE).unwrap_or(u16::MAX)));
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_8()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT_COLOR))
            .text_3xl()
            .child(self.text.clone())
            .child(img(self.image.clone()).w(side).h(side))
    }
}

/// Logs GPUI's records to stderr as `[LEVEL target] message`.
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log_level(metadata.target())
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let _ = writeln!(
                std::io::stderr().lock(),
                "[{} {}] {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

/// The most detailed level logged for `target`: info for GPUI (its adapter choice), warn for the rest (zbus's
/// handshake chatter, reaching `log` through tracing's `log` feature).
fn log_level(target: &str) -> log::LevelFilter {
    if target.starts_with("gpui") {
        log::LevelFilter::Info
    } else {
        log::LevelFilter::Warn
    }
}

/// Whether a `WAYLAND_DISPLAY` value names a compositor at all.
fn names_wayland_display(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
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

/// Prints the ready line and arms the `exit_after` timer.
fn report_ready(window: &Window, cx: &mut App, exit_after: Option<Duration>) {
    let line = ready_line(cx.compositor_name(), window.gpu_specs().as_ref());
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
    if let Some(delay) = exit_after {
        let timer = cx.background_executor().timer(delay);
        cx.spawn(async move |cx| {
            timer.await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
}

fn open_window(cx: &mut App, exit_after: Option<Duration>) -> Result<WindowHandle<Smoke>, String> {
    let bounds = Bounds::centered(None, size(px(640.0), px(400.0)), cx);
    let image = Arc::new(RenderImage::new(vec![Frame::new(checkerboard_bgra(
        IMAGE_SIDE,
        TILE_SIDE,
        TILE_COLORS,
    ))]));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        app_id: Some(APP_ID.to_owned()),
        ..WindowOptions::default()
    };
    cx.open_window(options, |window, cx| {
        after_frames(window, READY_FRAMES, move |window, cx| {
            report_ready(window, cx, exit_after);
        });
        cx.new(|_| Smoke {
            text: TEXT.into(),
            image,
        })
    })
    .map_err(|error| format!("{error:#}"))
}

fn run(exit_after: Option<Duration>) -> ExitCode {
    let failed = Rc::new(Cell::new(false));
    let failed_in_app = Rc::clone(&failed);
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Err(error) = open_window(cx, exit_after) {
            log::error!("failed to open the window: {error}");
            failed_in_app.set(true);
            cx.quit();
        }
    });
    if failed.get() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn main() -> ExitCode {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);
    let options = match parse_options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            log::error!("{error}");
            return ExitCode::from(2);
        }
    };
    if !names_wayland_display(std::env::var_os("WAYLAND_DISPLAY").as_deref()) {
        log::error!("no Wayland compositor: WAYLAND_DISPLAY is unset or empty (this build never uses X11)");
        return ExitCode::FAILURE;
    }
    run(options.exit_after)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn options_default_to_no_exit_timer() {
        assert_eq!(parse_options(args(&[])), Ok(Options::default()));
    }

    #[test]
    fn exit_after_takes_milliseconds() {
        let options = parse_options(args(&["--exit-after-ms", "1500"])).unwrap();
        assert_eq!(options.exit_after, Some(Duration::from_millis(1500)));
    }

    #[test]
    fn bad_arguments_are_refused() {
        assert!(
            parse_options(args(&["--exit-after-ms"]))
                .unwrap_err()
                .contains("needs a value")
        );
        assert!(
            parse_options(args(&["--exit-after-ms", "soon"]))
                .unwrap_err()
                .contains("soon")
        );
        assert!(
            parse_options(args(&["--x11"]))
                .unwrap_err()
                .contains("unknown argument: --x11")
        );
    }

    #[test]
    fn checkerboard_alternates_tiles_in_bgra() {
        let board = checkerboard_bgra(32, 16, [0x0011_2233, 0x0044_5566]);
        assert_eq!(board.dimensions(), (32, 32));
        let first = Rgba([0x33, 0x22, 0x11, 0xff]);
        let second = Rgba([0x66, 0x55, 0x44, 0xff]);
        assert_eq!(*board.get_pixel(0, 0), first);
        assert_eq!(*board.get_pixel(15, 15), first);
        assert_eq!(*board.get_pixel(16, 0), second);
        assert_eq!(*board.get_pixel(0, 16), second);
        assert_eq!(*board.get_pixel(31, 31), first);
    }

    #[test]
    fn smoke_colours_stay_apart() {
        assert!(!TILE_COLORS.contains(&TEXT_COLOR));
        assert!(!TILE_COLORS.contains(&BACKGROUND));
        assert_ne!(TEXT_COLOR, BACKGROUND);
    }

    #[test]
    fn only_a_non_empty_wayland_display_counts() {
        assert!(!names_wayland_display(None));
        assert!(!names_wayland_display(Some(OsStr::new(""))));
        assert!(names_wayland_display(Some(OsStr::new("wayland-1"))));
        assert!(names_wayland_display(Some(OsStr::new("/run/user/1000/wayland-1"))));
    }

    #[test]
    fn gpui_logs_from_info_and_the_rest_from_warn() {
        assert_eq!(log_level("gpui_wgpu::wgpu_context"), log::LevelFilter::Info);
        assert_eq!(log_level("gpui_linux::linux::platform"), log::LevelFilter::Info);
        assert_eq!(log_level("zbus::connection::handshake::common"), log::LevelFilter::Warn);
        assert_eq!(log_level("tracing::span"), log::LevelFilter::Warn);
    }

    #[test]
    fn ready_line_names_compositor_and_gpu() {
        let gpu = GpuSpecs {
            is_software_emulated: false,
            device_name: "Intel(R) Graphics".to_owned(),
            driver_name: "Intel open-source Mesa driver".to_owned(),
            driver_info: String::new(),
        };
        assert_eq!(
            ready_line("Wayland", Some(&gpu)),
            "ready compositor=Wayland gpu=\"Intel(R) Graphics\" driver=\"Intel open-source Mesa driver\" software=false"
        );
        assert_eq!(
            ready_line("weston", None),
            "ready compositor=weston gpu=unknown driver=unknown software=unknown"
        );
    }
}
