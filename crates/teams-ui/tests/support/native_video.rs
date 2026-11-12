//! Exercises the actual private adapter without an account, media device or network.
use crate::video_surface::{self, Frame, Surface};
use gpui::{
    App, AppContext as _, AsyncApp, Bounds, Context, IntoElement, ParentElement as _, Render, Styled as _, Window,
    WindowBounds, WindowHandle, WindowOptions, canvas, div, px, rgb, size,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

struct Fixture {
    surface: Arc<Surface>,
    clipped: bool,
    frame: Option<Arc<Frame>>,
    submitted: Arc<AtomicU64>,
}
impl Fixture {
    fn replace(&mut self, serial: u64) {
        let mut pixels = Vec::with_capacity(320 * 180 * 4);
        for y in 0..180 {
            for x in 0..320 {
                // BGRA, with distinct red/green/blue/white quadrants to catch channel/order/shape mistakes.
                pixels.extend_from_slice(match (x < 160, y < 90) {
                    (true, true) if serial.is_multiple_of(2) => &[0, 0, 255, 255],
                    (true, true) => &[255, 0, 255, 255],
                    (false, true) => &[0, 255, 0, 255],
                    (true, false) => &[255, 0, 0, 255],
                    (false, false) => &[255, 255, 255, 255],
                });
            }
        }
        let count = Arc::clone(&self.submitted);
        self.frame = Some(video_surface::frame(
            &self.surface,
            serial,
            (320, 180),
            &Arc::new(pixels),
            move |outcome| {
                assert_eq!(outcome.change(), "submitted");
                count.fetch_add(1, Ordering::Relaxed);
            },
        ));
    }
}
impl Render for Fixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let clipped = self.clipped;
        div()
            .size_full()
            .bg(rgb(0x0010_1010))
            .p_8()
            .children(self.frame.clone().map(|frame| {
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        let mask = clipped.then(|| gpui::ContentMask {
                            bounds: Bounds::default(),
                        });
                        window.with_content_mask(mask, |window| {
                            video_surface::paint(&frame, video_surface::fitted(bounds, 320, 180), window, cx);
                        });
                    },
                )
                .size_full()
            }))
    }
}

pub fn main() -> std::process::ExitCode {
    if std::env::var_os("NGTEAMS_NATIVE_VIDEO_REQUIRE").is_none() {
        println!("native video requires the authorized win11 console helper");
        return std::process::ExitCode::SUCCESS;
    }
    if let Err(error) = teams_platform::windows_sandbox::window_allowed_here() {
        eprintln!("native video sandbox: {error}");
        return std::process::ExitCode::FAILURE;
    }
    let result = Arc::new(Mutex::new(Err("fixture did not finish".to_owned())));
    let outcome = Arc::clone(&result);
    gpui_platform::application().run(move |cx: &mut App| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(720.), px(450.)),
                cx,
            ))),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("NGTEAMS SYNTHETIC VIDEO".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let window = cx.open_window(options, |_, cx| {
            cx.new(|_| Fixture {
                surface: Surface::new(),
                clipped: true,
                frame: None,
                submitted: Arc::new(AtomicU64::new(0)),
            })
        });
        let Ok(window) = window else {
            cx.quit();
            return;
        };
        cx.spawn(async move |cx: &mut AsyncApp| {
            let result = exercise(window, cx).await;
            *outcome.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = result;
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    match result
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        Ok(()) => {
            println!("native video upload, replacement, redraw and release passed");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("native video: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn pause(cx: &AsyncApp) {
    cx.background_executor().timer(Duration::from_millis(60)).await;
}

async fn exercise(window: WindowHandle<Fixture>, cx: &mut AsyncApp) -> Result<(), String> {
    window
        .update(cx, |_, window, _| window.activate_window())
        .map_err(|e| e.to_string())?;
    clipped_frame(window, cx).await?;
    let mut previous: Option<Arc<gpui::RenderImage>> = None;
    for serial in 1..=100 {
        window
            .update(cx, |view, _, cx| {
                view.replace(serial);
                cx.notify();
            })
            .map_err(|e| e.to_string())?;
        pause(cx).await;
        let next = window
            .update(cx, |view, window, _| {
                if let Some(previous) = previous.take() {
                    assert!(!window.has_image_atlas_entry(&previous), "replaced image evicted");
                }
                let image = view
                    .frame
                    .as_ref()
                    .and_then(|frame| frame.image())
                    .expect("valid BGRA")
                    .clone();
                assert!(window.has_image_atlas_entry(&image), "latest image uploaded");
                assert_eq!(
                    view.submitted.load(Ordering::Relaxed),
                    serial,
                    "one submission per frame"
                );
                image
            })
            .map_err(|e| e.to_string())?;
        previous = Some(next);
    }
    window.update(cx, |_, _, cx| cx.notify()).map_err(|e| e.to_string())?;
    pause(cx).await;
    window
        .update(cx, |view, _, _| {
            assert_eq!(
                view.submitted.load(Ordering::Relaxed),
                100,
                "redraw is not another frame"
            );
        })
        .map_err(|e| e.to_string())?;
    if std::env::var_os("NGTEAMS_NATIVE_VIDEO_HOLD").is_some() {
        println!("synthetic video ready for client-area screenshot");
        cx.background_executor().timer(Duration::from_secs(8)).await;
    }
    window
        .update(cx, |view, _, cx| {
            view.frame = None;
            cx.notify();
        })
        .map_err(|e| e.to_string())?;
    pause(cx).await;
    window
        .update(cx, |_, window, _| {
            assert!(
                !window.has_image_atlas_entry(&previous.expect("last frame")),
                "hidden image evicted"
            );
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

async fn clipped_frame(window: WindowHandle<Fixture>, cx: &mut AsyncApp) -> Result<(), String> {
    window
        .update(cx, |view, _, cx| {
            view.replace(0);
            cx.notify();
        })
        .map_err(|e| e.to_string())?;
    pause(cx).await;
    window
        .update(cx, |view, window, cx| {
            let image = view.frame.as_ref().and_then(|frame| frame.image()).expect("valid BGRA");
            assert!(
                !window.has_image_atlas_entry(image),
                "fully clipped frame is not uploaded"
            );
            assert_eq!(
                view.submitted.load(Ordering::Relaxed),
                0,
                "fully clipped frame is not submitted"
            );
            view.clipped = false;
            cx.notify();
        })
        .map_err(|e| e.to_string())
}
