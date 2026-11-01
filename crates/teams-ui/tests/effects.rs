//! Background choices and user-added pictures through a mocked file chooser and temporary settings directory.

#![cfg(test)]

use std::sync::Arc;

use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_ui::effects::{EffectsPicker, PickerPlace};
use teams_ui::settings::{SettingsFile, SettingsModel, VideoEffect};
use teams_ui::{VideoFeed, ViewTrace};

#[derive(Default)]
struct SyntheticCamera {
    opened: std::sync::atomic::AtomicUsize,
    closed: Arc<std::sync::atomic::AtomicUsize>,
    fail: std::sync::atomic::AtomicBool,
}

impl teams_media::devices::Devices for SyntheticCamera {
    fn microphone(&self) -> Result<Box<dyn teams_media::devices::AudioSource>, teams_media::devices::DeviceError> {
        Err(teams_media::devices::DeviceError::Closed)
    }
    fn speaker(&self) -> Result<Box<dyn teams_media::devices::AudioSink>, teams_media::devices::DeviceError> {
        Err(teams_media::devices::DeviceError::Closed)
    }
    fn camera(&self) -> Result<Box<dyn teams_media::devices::VideoSource>, teams_media::devices::DeviceError> {
        use std::sync::atomic::Ordering;
        self.opened.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(teams_media::devices::DeviceError::Closed);
        }
        Ok(Box::new(SyntheticFrames(self.closed.clone())))
    }
}

struct SyntheticFrames(Arc<std::sync::atomic::AtomicUsize>);

impl teams_media::devices::VideoSource for SyntheticFrames {
    fn next_frame(
        &mut self,
        _: std::time::Duration,
    ) -> Result<Option<teams_media::devices::VideoFrame>, teams_media::devices::DeviceError> {
        std::thread::sleep(std::time::Duration::from_millis(10));
        Ok(Some(teams_media::devices::VideoFrame {
            width: 4,
            height: 4,
            stride: 16,
            format: teams_media::devices::PixelFormat::Bgra,
            data: vec![100; 64],
        }))
    }
}

impl Drop for SyntheticFrames {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn wait(wanted: impl Fn() -> bool) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !wanted() {
        assert!(std::time::Instant::now() < until, "synthetic preview did not settle");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn previews_use_only_the_injected_camera_replace_prior_sessions_and_recover_from_open_failure() {
    use std::sync::atomic::Ordering;
    use teams_ui::effects::CameraPreview;
    use teams_ui::video::Tile;

    let devices = Arc::new(SyntheticCamera::default());
    let preview = CameraPreview::new(devices.clone());
    assert_eq!(devices.opened.load(Ordering::SeqCst), 0);
    assert!(format!("{preview:?}").contains("CameraPreview"));
    preview.set_background(teams_media::video::effects::Background::None);
    let first = preview.start();
    first.seen();
    wait(|| preview.feed().picture(Tile::Own).is_some());
    assert!(first.running());
    let second = preview.start();
    wait(|| devices.opened.load(Ordering::SeqCst) == 2);
    assert!(!first.running());
    drop(first);
    assert!(second.running(), "dropping a replaced session keeps the current camera");
    drop(second);
    wait(|| devices.closed.load(Ordering::SeqCst) == 2);
    assert!(preview.feed().picture(Tile::Own).is_none());
    devices.fail.store(true, Ordering::SeqCst);
    let failed = preview.start();
    wait(|| preview.failure().is_some());
    assert!(preview.failure().unwrap().contains("can't be opened"));
    drop(failed);
    devices.fail.store(false, Ordering::SeqCst);
    let recovered = preview.start();
    wait(|| preview.feed().picture(Tile::Own).is_some());
    assert!(preview.failure().is_none());
    drop(recovered);
    wait(|| devices.closed.load(Ordering::SeqCst) == 3);
}

#[test]
fn the_preview_buttons_start_and_stop_only_the_injected_camera() {
    use std::sync::atomic::Ordering;

    let directory = tempfile::tempdir().unwrap();
    let (settings, picker, cx) = window(&directory, PickerPlace::Settings);
    let devices = Arc::new(SyntheticCamera::default());
    let preview = teams_ui::effects::CameraPreview::new(devices.clone());
    cx.update(|_, cx| teams_ui::settings::attach_camera_preview(&settings, preview.clone(), cx));
    draw(cx);
    click(cx, "effects-preview-start");
    wait(|| preview.feed().picture(teams_ui::video::Tile::Own).is_some());
    draw(cx);
    assert!(cx.update(|_, cx| picker.read(cx).previewing()));
    click(cx, "effects-stop-preview");
    wait(|| devices.closed.load(Ordering::SeqCst) == 1);
    assert!(!cx.update(|_, cx| picker.read(cx).previewing()));
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    draw(cx);
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

fn window(
    directory: &tempfile::TempDir,
    place: PickerPlace,
) -> (
    Entity<SettingsModel>,
    Entity<EffectsPicker>,
    &'static mut VisualTestContext,
) {
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    app.update(teams_ui::window::init);
    let settings =
        app.update(|cx| cx.new(|_| SettingsModel::load(SettingsFile::new(directory.path().join("settings.toml")))));
    let model = settings.clone();
    let (root, cx) = app.add_window_view(|window, cx| {
        let picker = cx.new(|cx| {
            let picker = EffectsPicker::new(model, Arc::new(ViewTrace::off()), place, cx);
            if place == PickerPlace::CallPane {
                picker.in_call(VideoFeed::new(), 42)
            } else {
                picker
            }
        });
        teams_ui::window::KitRoot::new(picker, window, cx)
    });
    let picker = cx.update(|_, cx| root.read(cx).view().clone().downcast::<EffectsPicker>().unwrap());
    cx.simulate_resize(size(px(850.), px(800.)));
    draw(cx);
    (settings, picker, cx)
}

#[test]
fn stock_and_blur_choices_are_saved_without_starting_a_camera() {
    for place in [PickerPlace::Settings, PickerPlace::CallPane] {
        let directory = tempfile::tempdir().unwrap();
        let (settings, picker, cx) = window(&directory, place);
        assert!(!cx.update(|_, cx| picker.read(cx).previewing()));
        assert!(
            cx.debug_bounds("effects-preview-start").is_none(),
            "no devices were attached"
        );
        click(cx, "effect-blur");
        assert_eq!(
            cx.update(|_, cx| settings.read(cx).settings().video_effect.clone()),
            VideoEffect::Blur
        );
        click(cx, "effect-stock-waves");
        assert_eq!(
            cx.update(|_, cx| settings.read(cx).settings().video_effect.clone()),
            VideoEffect::Stock("waves")
        );
        click(cx, "effect-none");
        assert_eq!(
            cx.update(|_, cx| settings.read(cx).settings().video_effect.clone()),
            VideoEffect::None
        );
        cx.update(|_, cx| picker.update(cx, EffectsPicker::start_preview));
        assert!(!cx.update(|_, cx| picker.read(cx).previewing()));
        let file = SettingsModel::load(SettingsFile::new(directory.path().join("settings.toml")));
        assert_eq!(file.settings().video_effect, VideoEffect::None);
    }
}

#[test]
fn adding_a_picture_handles_cancel_and_invalid_files_then_removes_only_the_imported_copy() {
    let directory = tempfile::tempdir().unwrap();
    let (settings, _picker, cx) = window(&directory, PickerPlace::Settings);
    click(cx, "effect-add");
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| None);
    draw(cx);
    let invalid = directory.path().join("not-an-image.txt");
    std::fs::write(&invalid, "Synthetic text, not an image").unwrap();
    click(cx, "effect-add");
    cx.simulate_path_prompt_response(|_| Some(vec![invalid]));
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| settings.read(cx).settings().video_effect.clone()),
        VideoEffect::None
    );
    let original = directory.path().join("synthetic.png");
    image::RgbaImage::from_pixel(32, 18, image::Rgba([30, 80, 120, 255]))
        .save(&original)
        .unwrap();
    let bytes = std::fs::read(&original).unwrap();
    click(cx, "effect-add");
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files && !options.directories && !options.multiple);
        Some(vec![original.clone()])
    });
    draw(cx);
    let effect = cx.update(|_, cx| settings.read(cx).settings().video_effect.clone());
    let VideoEffect::Added(name) = effect else {
        panic!("the imported picture is selected");
    };
    let imported = cx.update(|_, cx| settings.read(cx).backgrounds_dir().unwrap().join(&name));
    assert!(imported.is_file());
    let tile = cx.debug_bounds("effect-added-0").unwrap();
    cx.simulate_mouse_move(tile.center(), None, Modifiers::none());
    draw(cx);
    click(cx, "effect-added-0-remove");
    assert!(!imported.exists());
    assert_eq!(std::fs::read(&original).unwrap(), bytes);
    assert_eq!(
        cx.update(|_, cx| settings.read(cx).settings().video_effect.clone()),
        VideoEffect::None
    );
}

#[test]
fn a_call_picker_uses_the_call_feed_and_never_starts_an_unrequested_preview() {
    let directory = tempfile::tempdir().unwrap();
    let (_, picker, cx) = window(&directory, PickerPlace::CallPane);
    cx.update(|_, cx| {
        picker.update(cx, |picker, cx| {
            picker.set_camera_on(true, cx);
            assert!(!picker.previewing());
        });
    });
    draw(cx);
    assert!(cx.debug_bounds("effects-preview").is_some());
    assert!(cx.debug_bounds("effects-preview-start").is_none());
    cx.update(|_, cx| {
        picker.update(cx, |picker, cx| {
            picker.set_camera_on(false, cx);
            assert!(!picker.previewing());
        });
    });
    draw(cx);
    assert!(!cx.update(|_, cx| picker.read(cx).previewing()));
}
