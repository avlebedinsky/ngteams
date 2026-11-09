//! Which WPE `WebKit` the sign-in page runs on, found before its window opens: the system's when it's new enough, a
//! runtime already downloaded, or else (a release build's pinned runtime, `teams-web-engine`) a one-time download
//! shown in a small window of its own. That window has no Teams counterpart: the Teams client ships its web engine.
//!
//! The download runs on a thread of its own, so closing the window answers the sign-in at once; a download left running
//! finishes harmlessly and the next sign-in reuses it. Its window follows the system's light or dark appearance: no
//! account's theme is known before the first sign-in.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use gpui::{
    App, AppContext as _, Bounds, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, Rgba,
    SharedString, StatefulInteractiveElement as _, Styled as _, TitlebarOptions, Window, WindowAppearance,
    WindowBounds, WindowOptions, div, px, rgb, size,
};
use teams_auth::signin::SignInAnswer;
use teams_web_engine::{Pins, Progress, Resolution};
use teams_wpe_client::EngineSource;

use crate::window::TITLE;

const WAIT_SIZE: (u16, u16) = (440, 260);
const NO_ENGINE: &str =
    "The sign-in window needs WPE WebKit 2.54 or newer: install your distribution's wpewebkit package.";

/// What the sign-in's thread hears.
#[derive(Debug)]
pub(crate) enum Heard {
    Progress(Progress),
    Finished(Result<PathBuf, String>),
    Retry,
    Closed,
}

/// What the window shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Show {
    Progress(Progress),
    Failed(String),
    Done,
}

/// A wait window's two ends, handed to GPUI's foreground.
pub(crate) struct WaitChannels {
    pub(crate) show: async_channel::Receiver<Show>,
    pub(crate) heard: mpsc::Sender<Heard>,
}

/// The engine for a sign-in under `data_dir`; `open` opens the wait window if a download is needed.
///
/// # Errors
///
/// The answer to give the sign-in: no engine can be had, or the window was closed.
pub(crate) fn source(data_dir: &Path, open: impl FnOnce(WaitChannels) -> bool) -> Result<EngineSource, SignInAnswer> {
    teams_wpe_client::worker::pin_worker().map_err(SignInAnswer::Failed)?;
    match teams_web_engine::resolve(data_dir, teams_web_engine::KEY, teams_wpe_client::worker::probe_system) {
        Resolution::System => Ok(EngineSource::System),
        Resolution::Runtime(directory) => Ok(EngineSource::Runtime(directory)),
        Resolution::NeedsDownload => match Pins::compiled() {
            Ok(Some(pins)) => download(&pins, data_dir, open).map(EngineSource::Runtime),
            Ok(None) => Err(SignInAnswer::Failed(NO_ENGINE.to_owned())),
            Err(error) => Err(SignInAnswer::Failed(error.to_string())),
        },
    }
}

fn download(pins: &Pins, data_dir: &Path, open: impl FnOnce(WaitChannels) -> bool) -> Result<PathBuf, SignInAnswer> {
    let (heard, hearing) = mpsc::channel();
    let (show, showing) = async_channel::unbounded();
    let channels = WaitChannels {
        show: showing,
        heard: heard.clone(),
    };
    if !open(channels) {
        return Err(SignInAnswer::Failed(
            "the app has no window for the sign-in any more".into(),
        ));
    }
    let start = || {
        let pins = pins.clone();
        let data_dir = data_dir.to_owned();
        let heard = heard.clone();
        std::thread::spawn(move || {
            let mut throttle = Throttle::default();
            let result = pins.download(&data_dir, |progress| {
                if throttle.due(progress) {
                    let _ = heard.send(Heard::Progress(progress));
                }
            });
            let _ = heard.send(Heard::Finished(result.map_err(|error| error.to_string())));
        });
    };
    wait(&hearing, &show, &start)
}

/// Follows a download until it's installed or the user gives up, starting one with `start`, and again on a retry.
fn wait(
    hearing: &mpsc::Receiver<Heard>,
    show: &async_channel::Sender<Show>,
    start: &dyn Fn(),
) -> Result<PathBuf, SignInAnswer> {
    start();
    loop {
        match hearing.recv() {
            Ok(Heard::Progress(progress)) => {
                let _ = show.try_send(Show::Progress(progress));
            }
            Ok(Heard::Finished(Ok(directory))) => {
                let _ = show.try_send(Show::Done);
                return Ok(directory);
            }
            Ok(Heard::Finished(Err(reason))) => {
                log::warn!("the sign-in engine's download failed: {reason}");
                let _ = show.try_send(Show::Failed(reason));
            }
            Ok(Heard::Retry) => start(),
            Ok(Heard::Closed) | Err(_) => return Err(SignInAnswer::Closed),
        }
    }
}

/// Lets a download's progress through once per percent, and the first and last always.
#[derive(Default)]
struct Throttle {
    shown: Option<u64>,
}

impl Throttle {
    fn due(&mut self, progress: Progress) -> bool {
        let percent = percent(progress);
        let due = self.shown != Some(percent);
        self.shown = Some(percent);
        due
    }
}

fn percent(progress: Progress) -> u64 {
    if progress.total == 0 {
        return 0;
    }
    progress.received.min(progress.total).saturating_mul(100) / progress.total
}

fn megabytes(progress: Progress) -> String {
    format!("{} of {} MB", progress.received / 1_000_000, progress.total / 1_000_000)
}

/// Opens the wait window for `channels` under `app_id`. A window that can't be opened counts as closed.
pub(crate) fn open_wait(channels: WaitChannels, app_id: &str, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(f32::from(WAIT_SIZE.0)), px(f32::from(WAIT_SIZE.1))), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(TITLE.into()),
            ..TitlebarOptions::default()
        }),
        app_id: Some(app_id.to_owned()),
        ..WindowOptions::default()
    };
    let heard = channels.heard.clone();
    let opened = cx.open_window(options, move |window, cx| {
        cx.new(|cx| EngineWaitView::new(channels, window, cx))
    });
    if opened.is_err() {
        let _ = heard.send(Heard::Closed);
    }
}

/// The wait window with no download behind it, for a test to step through what it shows.
#[cfg(feature = "test-support")]
pub struct Preview {
    show: async_channel::Sender<Show>,
    // kept so the window's sends have somewhere to go
    _hearing: mpsc::Receiver<Heard>,
}

#[cfg(feature = "test-support")]
impl Preview {
    /// Opens the window.
    #[must_use]
    pub fn open(app_id: &str, cx: &mut App) -> Self {
        let (heard, hearing) = mpsc::channel();
        let (show, showing) = async_channel::unbounded();
        open_wait(WaitChannels { show: showing, heard }, app_id, cx);
        Self {
            show,
            _hearing: hearing,
        }
    }

    /// Shows a download's progress.
    pub fn progress(&self, received: u64, total: u64) {
        let _ = self.show.try_send(Show::Progress(Progress { received, total }));
    }

    /// Shows a failed download.
    pub fn failed(&self, reason: &str) {
        let _ = self.show.try_send(Show::Failed(reason.to_owned()));
    }
}

/// The wait window's root view.
pub(crate) struct EngineWaitView {
    state: Show,
    heard: mpsc::Sender<Heard>,
}

impl EngineWaitView {
    pub(crate) fn new(channels: WaitChannels, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let WaitChannels { show, heard } = channels;
        cx.spawn_in(window, async move |view, cx| {
            while let Ok(step) = show.recv().await {
                let followed = view.update_in(cx, |view, window, cx| view.step(step, window, cx));
                if followed.is_err() {
                    return;
                }
            }
        })
        .detach();
        let closing = heard.clone();
        window.on_window_should_close(cx, move |_, _| {
            let _ = closing.send(Heard::Closed);
            true
        });
        Self {
            state: Show::Progress(Progress { received: 0, total: 0 }),
            heard,
        }
    }

    fn step(&mut self, step: Show, window: &mut Window, cx: &mut Context<Self>) {
        if step == Show::Done {
            window.remove_window();
            return;
        }
        self.state = step;
        cx.notify();
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        self.state = Show::Progress(Progress { received: 0, total: 0 });
        let _ = self.heard.send(Heard::Retry);
        cx.notify();
    }

    fn close(&mut self, window: &mut Window) {
        let _ = self.heard.send(Heard::Closed);
        window.remove_window();
    }
}

/// The window's colours, for the system's appearance.
#[derive(Clone, Copy)]
struct Colors {
    background: Rgba,
    foreground: Rgba,
    muted: Rgba,
    track: Rgba,
    brand: Rgba,
    on_brand: Rgba,
}

fn colors(appearance: WindowAppearance) -> Colors {
    let dark = matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark);
    Colors {
        background: rgb(if dark { 0x001f_1f1f } else { 0x00ff_ffff }),
        foreground: rgb(if dark { 0x00ff_ffff } else { 0x0024_2424 }),
        muted: rgb(if dark { 0x00ad_adad } else { 0x0061_6161 }),
        track: rgb(if dark { 0x0040_4040 } else { 0x00e0_e0e0 }),
        brand: rgb(if dark { 0x007f_85f5 } else { 0x005b_5fc7 }),
        on_brand: rgb(if dark { 0x0000_0000 } else { 0x00ff_ffff }),
    }
}

fn button(id: &'static str, label: &'static str, primary: bool, colors: Colors) -> gpui::Stateful<gpui::Div> {
    let face = div()
        .id(id)
        .debug_selector(|| id.to_owned())
        .px_4()
        .py_1p5()
        .rounded_md()
        .cursor_pointer();
    if primary {
        face.bg(colors.brand).text_color(colors.on_brand).child(label)
    } else {
        face.border_1()
            .border_color(colors.track)
            .text_color(colors.foreground)
            .child(label)
    }
}

fn progress_body(progress: Progress, colors: Colors) -> impl IntoElement {
    let fraction = u8::try_from(percent(progress)).map_or(0., f32::from) / 100.;
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_sm()
                .text_color(colors.muted)
                .child("Microsoft's sign-in page needs a web engine this computer doesn't have. It's downloaded once."),
        )
        .child(
            div().h(px(4.)).w_full().rounded_full().bg(colors.track).child(
                div()
                    .h_full()
                    .w(gpui::relative(fraction))
                    .rounded_full()
                    .bg(colors.brand),
            ),
        )
        .child(
            div()
                .debug_selector(|| "engine-wait-progress".to_owned())
                .text_xs()
                .text_color(colors.muted)
                .child(if progress.total == 0 {
                    SharedString::from("Starting the download")
                } else {
                    megabytes(progress).into()
                }),
        )
}

impl Render for EngineWaitView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = colors(window.appearance());
        let (title, body) = match &self.state {
            Show::Failed(reason) => (
                "The download didn't finish",
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(div().text_sm().text_color(colors.muted).child(reason.clone()))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                button("engine-wait-retry", "Try again", true, colors)
                                    .on_click(cx.listener(|view, _, _, cx| view.retry(cx))),
                            )
                            .child(
                                button("engine-wait-close", "Close", false, colors)
                                    .on_click(cx.listener(|view, _, window, _| view.close(window))),
                            ),
                    )
                    .into_any_element(),
            ),
            Show::Progress(progress) => (
                "Getting sign-in ready",
                progress_body(*progress, colors).into_any_element(),
            ),
            Show::Done => ("Getting sign-in ready", div().into_any_element()),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .gap_3()
            .p_8()
            .bg(colors.background)
            .text_color(colors.foreground)
            .child(div().text_xl().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(received: u64, total: u64) -> Progress {
        Progress { received, total }
    }

    fn heard_then(
        steps: Vec<Heard>,
    ) -> (
        mpsc::Receiver<Heard>,
        async_channel::Receiver<Show>,
        async_channel::Sender<Show>,
    ) {
        let (heard, hearing) = mpsc::channel();
        for step in steps {
            heard.send(step).unwrap();
        }
        let (show, showing) = async_channel::unbounded();
        (hearing, showing, show)
    }

    #[test]
    fn a_finished_download_is_shown_done_and_answers_its_directory() {
        let (hearing, showing, show) = heard_then(vec![
            Heard::Progress(progress(1, 2)),
            Heard::Finished(Ok("/runtime".into())),
        ]);
        let started = std::cell::Cell::new(0);
        let answer = wait(&hearing, &show, &|| started.set(started.get() + 1));
        assert_eq!(answer.ok(), Some(PathBuf::from("/runtime")));
        assert_eq!(started.get(), 1);
        assert_eq!(showing.try_recv(), Ok(Show::Progress(progress(1, 2))));
        assert_eq!(showing.try_recv(), Ok(Show::Done));
    }

    #[test]
    fn a_failure_waits_for_the_user_and_a_retry_starts_again() {
        let (hearing, showing, show) = heard_then(vec![
            Heard::Finished(Err("no network".into())),
            Heard::Retry,
            Heard::Finished(Ok("/runtime".into())),
        ]);
        let started = std::cell::Cell::new(0);
        assert!(wait(&hearing, &show, &|| started.set(started.get() + 1)).is_ok());
        assert_eq!(started.get(), 2);
        assert_eq!(showing.try_recv(), Ok(Show::Failed("no network".into())));
    }

    #[test]
    fn closing_the_window_answers_closed_even_with_a_download_running() {
        let (hearing, _showing, show) = heard_then(vec![Heard::Closed]);
        assert!(matches!(wait(&hearing, &show, &|| {}), Err(SignInAnswer::Closed)));
    }

    #[test]
    fn a_window_that_went_away_counts_as_closed() {
        let (heard, hearing) = mpsc::channel();
        drop(heard);
        let (show, _showing) = async_channel::unbounded();
        assert!(matches!(wait(&hearing, &show, &|| {}), Err(SignInAnswer::Closed)));
    }

    #[test]
    fn progress_is_let_through_once_per_percent() {
        let mut throttle = Throttle::default();
        assert!(throttle.due(progress(0, 1000)));
        assert!(!throttle.due(progress(5, 1000)));
        assert!(throttle.due(progress(10, 1000)));
        assert!(throttle.due(progress(1000, 1000)));
    }

    #[test]
    fn progress_reads_in_megabytes_and_stays_in_range() {
        assert_eq!(megabytes(progress(42_000_000, 118_000_000)), "42 of 118 MB");
        assert_eq!(percent(progress(5, 0)), 0);
        assert_eq!(percent(progress(200, 100)), 100);
    }

    fn pins(url: &str) -> Pins {
        Pins::from_values(Some(url), Some("1"), Some(&"0".repeat(64)), Some("key"), None)
            .unwrap()
            .unwrap()
    }

    fn installed_runtime(data_dir: &Path) -> PathBuf {
        let root = teams_web_engine::runtime_root(data_dir).join("key");
        for file in [
            "MANIFEST",
            "lib/libWPEWebKit-2.0.so.1",
            "lib/wpe-webkit-2.0/WPEWebProcess",
            "lib/wpe-webkit-2.0/WPENetworkProcess",
            "libexec/gstreamer-1.0/gst-plugin-scanner",
        ] {
            let file = root.join(file);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "fixture").unwrap();
        }
        for directory in [
            "lib/gio/modules",
            "lib/gstreamer-1.0",
            "lib/wpe-webkit-2.0/injected-bundle",
        ] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        root
    }

    #[test]
    fn a_runtime_already_installed_answers_without_network() {
        let data = tempfile::tempdir().unwrap();
        let root = installed_runtime(data.path());
        let kept = std::sync::Mutex::new(None);
        let answer = download(&pins("https://127.0.0.1:1/runtime"), data.path(), |channels| {
            *kept.lock().unwrap() = Some(channels);
            true
        });
        assert_eq!(answer.ok(), Some(root));
    }

    #[test]
    fn a_failed_download_is_shown_and_closing_ends_the_wait() {
        let data = tempfile::tempdir().unwrap();
        let answer = download(&pins("https://127.0.0.1:1/runtime"), data.path(), |channels| {
            std::thread::spawn(move || {
                while !matches!(channels.show.recv_blocking(), Ok(Show::Failed(_))) {}
                channels.heard.send(Heard::Closed).unwrap();
            });
            true
        });
        assert!(matches!(answer, Err(SignInAnswer::Closed)));
    }

    #[test]
    fn an_app_with_no_window_for_it_fails_the_download() {
        let data = tempfile::tempdir().unwrap();
        let answer = download(&pins("https://127.0.0.1:1/runtime"), data.path(), |_| false);
        assert!(matches!(answer, Err(SignInAnswer::Failed(reason)) if reason.contains("no window")));
    }

    #[gpui::test]
    fn the_view_follows_the_download_and_answers_its_buttons(cx: &mut gpui::TestAppContext) {
        let (heard, hearing) = mpsc::channel();
        let (show, showing) = async_channel::unbounded();
        let channels = WaitChannels { show: showing, heard };
        let (view, cx) = cx.add_window_view(|window, cx| EngineWaitView::new(channels, window, cx));
        show.try_send(Show::Progress(progress(5, 10))).unwrap();
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert_eq!(view.state, Show::Progress(progress(5, 10))));
        show.try_send(Show::Failed("no network".into())).unwrap();
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert_eq!(view.state, Show::Failed("no network".into())));
        view.update(cx, EngineWaitView::retry);
        assert!(matches!(hearing.try_recv(), Ok(Heard::Retry)));
        view.update_in(cx, |view, window, _| view.close(window));
        assert!(matches!(hearing.try_recv(), Ok(Heard::Closed)));
    }

    #[gpui::test]
    fn the_window_goes_away_once_the_engine_is_installed(cx: &mut gpui::TestAppContext) {
        let (heard, _hearing) = mpsc::channel();
        let (show, showing) = async_channel::unbounded();
        let channels = WaitChannels { show: showing, heard };
        let (_view, cx) = cx.add_window_view(|window, cx| EngineWaitView::new(channels, window, cx));
        show.try_send(Show::Done).unwrap();
        cx.run_until_parked();
        assert!(cx.windows().is_empty());
    }

    #[test]
    fn dark_and_light_appearances_differ() {
        let dark = colors(WindowAppearance::Dark);
        let light = colors(WindowAppearance::Light);
        assert_ne!(dark.background, light.background);
        assert_ne!(dark.foreground, light.foreground);
    }
}
