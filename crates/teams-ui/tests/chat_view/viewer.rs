//! A message's picture opened in the picture viewer (`teams_ui::picture_viewer`), on GPUI's test context over the fake
//! engine: a click on a shown picture opens the viewer's window on it (no text selected), with the chat's other
//! pictures before and after it; Next reads the next one; the zoom keys zoom; Escape closes the window. Every id and
//! picture is made up.

use std::sync::Arc;

use gpui::{Modifiers, VisualTestContext, size};
use teams_ui::ViewTrace;
use teams_ui::picture_viewer::{self, Placement};

use super::common::LAB_B;
use super::{MINUTE, NOON, chat_window, draw, message, rig_with};

/// A made-up picture's view on the media service's host.
fn src(object: &str) -> String {
    format!("https://us-api.asm.skype.com/v1/objects/0-eus-d9-{object}/views/imgo")
}

/// A message of one pasted picture of `width` by `height`.
fn picture_message(id: &str, object: &str, (width, height): (u32, u32), at: i64) -> teams_store::Message {
    message(
        id,
        LAB_B,
        &format!(
            "<p><img src=\"{}\" itemtype=\"http://schema.skype.com/AMSImage\" width=\"{width}\" height=\"{height}\" \
             alt=\"image\"></p>",
            src(object)
        ),
        at,
    )
}

/// A made-up PNG of `width` by `height`.
fn png(width: u32, height: u32) -> teams_core::PictureBytes {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(width, height, image::Rgba([40, 120, 200, 255]))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    Arc::new(teams_proto::PastedPicture {
        format: teams_proto::PastedFormat::Png,
        bytes,
    })
}

const FIRST: &str = "00000000000000000000000000000a01";
const SECOND: &str = "00000000000000000000000000000a02";

#[test]
fn a_click_on_a_picture_opens_it_in_the_viewer_which_goes_through_the_chat_s_pictures() {
    let rig = rig_with(vec![
        picture_message("1", FIRST, (2400, 1200), NOON),
        picture_message("2", SECOND, (300, 200), NOON + MINUTE),
    ]);
    rig.core.pictures.lock().unwrap().insert(src(FIRST), png(2400, 1200));
    rig.core.pictures.lock().unwrap().insert(src(SECOND), png(300, 200));
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    draw(cx);
    assert!(cx.update(|_, cx| picture_viewer::shown(cx).is_none()));

    let picture = cx
        .debug_bounds("message-text-1-open-[0]")
        .expect("the first message's picture, a click target once shown");
    cx.simulate_click(picture.center(), Modifiers::none());
    draw(cx);
    // a click on a picture selects no text
    assert!(cx.update(|_, cx| view.read(cx).text_selection().is_empty()));
    let viewer = cx
        .update(|_, cx| picture_viewer::shown(cx))
        .expect("the viewer, opened on the picture");
    let handle = cx.update(|_, cx| picture_viewer::window(cx)).expect("its window");
    let mut viewer_cx = VisualTestContext::from_window(handle.into(), &rig.cx);
    viewer_cx.simulate_resize(size(gpui::px(1000.0), gpui::px(700.0)));
    draw(&mut viewer_cx);
    let shown = viewer_cx.update(|_, cx| (viewer.read(cx).at(), viewer.read(cx).natural()));
    assert_eq!(
        shown,
        ((0, 2), Some((2400, 1200))),
        "the first of the chat's two, read at its own size"
    );
    // fitted: within the stage, never above its own size
    let stage = size(gpui::px(1000.0), gpui::px(700.0 - picture_viewer::HEADER_HEIGHT));
    let fitted = Placement::fit((2400, 1200), stage);
    assert!(fitted < 1.0);
    assert!(viewer_cx.debug_bounds("picture-viewer-image").is_some());
    assert!(
        viewer_cx.debug_bounds("picture-previous").is_none(),
        "nothing before the first"
    );
    assert!(viewer_cx.debug_bounds("picture-next").is_some());

    // zoomed in a step from the fit by its key, then fitted again
    viewer_cx.simulate_keystrokes("ctrl-=");
    draw(&mut viewer_cx);
    let zoom = viewer_cx.update(|_, cx| viewer.read(cx).placement().zoom);
    assert!(
        zoom.is_some_and(|zoom| zoom > fitted),
        "zoomed in past the fit: {zoom:?}"
    );
    viewer_cx.simulate_keystrokes("ctrl-0");
    draw(&mut viewer_cx);
    assert_eq!(
        viewer_cx.update(|_, cx| viewer.read(cx).placement()),
        Placement::default()
    );

    // the next one: the second message's, at its own size (smaller than the stage)
    viewer_cx.simulate_keystrokes("right");
    draw(&mut viewer_cx);
    let shown = viewer_cx.update(|_, cx| (viewer.read(cx).at(), viewer.read(cx).natural()));
    assert_eq!(shown, ((1, 2), Some((300, 200))));
    assert!(
        (Placement::fit((300, 200), stage) - 1.0).abs() < f32::EPSILON,
        "never upscaled"
    );
    assert!(
        viewer_cx.debug_bounds("picture-next").is_none(),
        "nothing after the last"
    );

    viewer_cx.simulate_keystrokes("escape");
    // its window is gone: nothing of it is drawn again
    viewer_cx.run_until_parked();
    assert!(
        cx.update(|_, cx| picture_viewer::shown(cx).is_none()),
        "Escape closes it"
    );
}

#[test]
fn a_zoomed_picture_pans_only_as_far_as_its_edges_and_zooms_about_the_pointer() {
    let stage = size(gpui::px(800.0), gpui::px(600.0));
    let natural = (1600, 1200);
    let at_own = Placement::default().zoomed(1.0, gpui::point(gpui::px(0.0), gpui::px(0.0)), natural, stage);
    assert_eq!(
        at_own.bounds(natural, stage).size,
        size(gpui::px(1600.0), gpui::px(1200.0))
    );
    // as far as the picture's edge, no further
    let panned = at_own.panned(gpui::point(gpui::px(5000.0), gpui::px(-5000.0)), natural, stage);
    assert_eq!(panned.pan, gpui::point(gpui::px(400.0), gpui::px(-300.0)));
    let bounds = panned.bounds(natural, stage);
    assert_eq!(bounds.origin, gpui::point(gpui::px(0.0), gpui::px(-600.0)));
    // the point under the pointer stays there as it zooms
    let anchor = gpui::point(gpui::px(100.0), gpui::px(50.0));
    let before = at_own.bounds(natural, stage);
    let zoomed = at_own.zoomed(2.0, anchor, natural, stage);
    let after = zoomed.bounds(natural, stage);
    let centre = gpui::point(gpui::px(400.0), gpui::px(300.0)) + anchor;
    let under = |bounds: gpui::Bounds<gpui::Pixels>| {
        (
            (centre.x - bounds.origin.x) / bounds.size.width,
            (centre.y - bounds.origin.y) / bounds.size.height,
        )
    };
    let (was, is) = (under(before), under(after));
    assert!(
        (was.0 - is.0).abs() < 1e-4 && (was.1 - is.1).abs() < 1e-4,
        "{was:?} then {is:?}"
    );
}

#[test]
fn the_downloads_folder_comes_from_user_dirs() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let dirs = "# made up\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/Fetched\"\n";
    assert_eq!(
        picture_viewer::user_dir(dirs, "XDG_DOWNLOAD_DIR", home),
        Some(home.join("Fetched"))
    );
    assert_eq!(
        picture_viewer::user_dir("XDG_DOWNLOAD_DIR=\"relative\"", "XDG_DOWNLOAD_DIR", home),
        None
    );
    let name = picture_viewer::file_name(NOON, teams_proto::PastedFormat::Jpeg);
    assert!(
        name.starts_with("Image 2026-01-0")
            && std::path::Path::new(&name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jpg")),
        "{name}"
    );
}
