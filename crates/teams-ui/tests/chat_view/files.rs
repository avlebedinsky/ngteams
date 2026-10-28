//! The files a message carries, drawn as Teams draws them (`teams_ui::files`), on GPUI's test context over the fake
//! engine: a card per file under the text, side by side and wrapping when the window is narrow; a video's thumbnail
//! asked of the engine by its sharing link and drawn on its card, a placeholder without one, a document's card its
//! header alone; a click on the name opens the sharing link, "More options" downloads it or copies the link; nothing
//! about the files is said to be unshown. The message is the synthetic fixture `tests/fixtures/chatsvc/files.json`:
//! every name, link and id is made up.

use std::sync::Arc;

use gpui::{Bounds, Entity, Modifiers, Pixels, VisualTestContext, px, size};
use serde_json::Value;
use teams_proto::chat::shared_files;
use teams_store::Message;
use teams_ui::chat_view::{ChatView, UnshownParts};

use super::common::{LAB_B, Rig};
use super::{NOON, chat_window, draw, items, message, rig_with, trace};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/chatsvc/files.json");
const ID: &str = "301";

/// The fixture's message, from b, as the store keeps it (its `properties` JSON text).
fn files_message() -> Message {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let read_back = &fixture["read_back"];
    let mut stored = message(ID, LAB_B, read_back["content"].as_str().unwrap(), NOON);
    stored.properties = Some(read_back["properties"].to_string());
    stored
}

/// The sharing links of the fixture's files, in order.
fn share_urls() -> Vec<String> {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    shared_files(Some(&fixture["read_back"]["properties"]["files"]))
        .files
        .into_iter()
        .map(|file| file.share_url.unwrap())
        .collect()
}

/// A made-up PNG thumbnail, 16 by 9.
fn thumbnail() -> teams_core::PictureBytes {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(160, 90, image::Rgba([30, 160, 60, 255]))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    Arc::new(teams_proto::PastedPicture {
        format: teams_proto::PastedFormat::Png,
        bytes,
    })
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<Bounds<Pixels>> {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let found = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(found.center(), Modifiers::none());
    draw(cx);
}

fn rig() -> Rig {
    rig_with(vec![files_message()])
}

/// The chat with the fixture's message, the engine holding the video's thumbnail.
fn shown_with_thumbnail() -> (Rig, Entity<ChatView>, &'static mut VisualTestContext) {
    let rig = rig();
    rig.core
        .thumbnails
        .lock()
        .unwrap()
        .insert(share_urls()[0].clone(), thumbnail());
    let (written_trace, _) = trace();
    let (view, cx) = chat_window(&rig, written_trace);
    draw(cx);
    (rig, view, cx)
}

#[test]
fn a_messages_files_are_cards_side_by_side_under_its_text() {
    let (_rig, view, cx) = shown_with_thumbnail();
    let item = items(&view, cx).remove(0);
    assert_eq!(item.files.len(), 2);
    assert_eq!(
        item.unshown,
        UnshownParts::default(),
        "every file read: nothing said unshown"
    );
    assert!(bounds(cx, &format!("unshown-{ID}")).is_none());

    let video = bounds(cx, &format!("file-card-{ID}-0")).expect("the video's card");
    let document = bounds(cx, &format!("file-card-{ID}-1")).expect("the document's card");
    // side by side, Teams' width each
    assert!(
        document.origin.x > video.origin.x + video.size.width - px(1.0),
        "{video:?} {document:?}"
    );
    assert_eq!(document.origin.y, video.origin.y);
    assert!(
        (f32::from(video.size.width) - teams_ui::files::CARD_WIDTH).abs() < 1.0,
        "{video:?}"
    );
    // under the text
    let text = bounds(cx, &format!("bubble-{ID}")).expect("the bubble");
    assert!(video.origin.y > text.origin.y + px(40.0), "{video:?} in {text:?}");
    assert!(
        document.size.height < video.size.height,
        "a document's card is its header alone"
    );
    // the name stays within its card, cut with an ellipsis rather than widening it
    let name = bounds(cx, &format!("file-name-{ID}-0")).expect("the name");
    assert!(
        name.origin.x + name.size.width <= video.origin.x + video.size.width,
        "{name:?}"
    );
}

#[test]
fn a_videos_thumbnail_is_asked_of_the_engine_and_a_click_opens_the_file() {
    let (rig, _view, cx) = shown_with_thumbnail();
    let shares = share_urls();
    let video = bounds(cx, &format!("file-card-{ID}-0")).expect("the video's card");
    // the video's thumbnail, asked of the engine by its sharing link, fills its card's width; the document has none
    let asked = rig.core.asked();
    assert_eq!(asked.iter().filter(|asked| **asked == "file_thumbnail").count(), 1);
    let picture = bounds(cx, &format!("file-thumbnail-{ID}-0")).expect("the thumbnail is drawn");
    assert!(
        picture.size.width >= video.size.width - px(3.0),
        "{picture:?} in {video:?}"
    );
    assert!(bounds(cx, &format!("file-thumbnail-{ID}-1")).is_none());
    assert!(bounds(cx, &format!("file-thumbnail-none-{ID}-1")).is_none());

    // a click on the name opens the file at its sharing link, through GPUI's opener
    click(cx, &format!("file-name-{ID}-0"));
    assert_eq!(cx.opened_url().as_deref(), Some(shares[0].as_str()));
    // as does one on its picture; another file's name opens its own
    click(cx, &format!("file-thumbnail-{ID}-0"));
    assert_eq!(cx.opened_url().as_deref(), Some(shares[0].as_str()));
    click(cx, &format!("file-name-{ID}-1"));
    assert_eq!(cx.opened_url().as_deref(), Some(shares[1].as_str()));
}

#[test]
fn a_video_without_a_thumbnail_shows_its_placeholder_and_narrow_cards_wrap() {
    let rig = rig();
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    cx.simulate_resize(size(px(480.0), px(700.0)));
    draw(cx);
    assert!(
        bounds(cx, &format!("file-thumbnail-none-{ID}-0")).is_some(),
        "the placeholder"
    );
    let video = bounds(cx, &format!("file-card-{ID}-0")).expect("the video's card");
    let document = bounds(cx, &format!("file-card-{ID}-1")).expect("the document's card");
    assert!(
        document.origin.y >= video.origin.y + video.size.height,
        "wrapped: {video:?} {document:?}"
    );
}

#[test]
fn more_options_download_the_file_and_copy_its_link() {
    let rig = rig();
    let shares = share_urls();
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    click(cx, &format!("file-more-{ID}-0"));
    assert!(
        bounds(cx, &format!("file-menu-open-{ID}-0")).is_some(),
        "Open in browser"
    );
    click(cx, &format!("file-menu-download-{ID}-0"));
    assert_eq!(cx.opened_url(), Some(format!("{}?download=1", shares[0])));
    click(cx, &format!("file-more-{ID}-0"));
    click(cx, &format!("file-menu-copy-{ID}-0"));
    let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some(shares[0].as_str()));
}
