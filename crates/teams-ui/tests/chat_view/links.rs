//! A link's preview card under a message's text, drawn as Teams draws it (`teams_ui::link_preview`), on GPUI's test
//! context over the fake engine: the card in the bubble under the text, its picture asked of the engine by the entry's
//! `previewurl` and drawn on its left, as high as the card; a click on the card opens the link; a link whose preview is
//! off draws none. The message is the synthetic fixture `tests/fixtures/chatsvc/link-preview.json`: every word, link
//! and id is made up.

use std::sync::Arc;

use gpui::{Bounds, Modifiers, Pixels, VisualTestContext, px};
use serde_json::Value;
use teams_store::Message;
use teams_ui::link_preview::{CARD_HEIGHT, CARD_MAX_WIDTH, PICTURE_WIDTH};

use super::common::LAB_B;
use super::{NOON, chat_window, draw, message, rig_with, trace};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/chatsvc/link-preview.json");
const ID: &str = "401";

/// The fixture's message, from b, as the store keeps it (its `properties` JSON text), and its preview's picture link.
fn link_message() -> (Message, String) {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let read_back = &fixture["read_back"];
    let mut stored = message(ID, LAB_B, read_back["content"].as_str().unwrap(), NOON);
    stored.properties = Some(read_back["properties"].to_string());
    let links: Value = serde_json::from_str(read_back["properties"]["links"].as_str().unwrap()).unwrap();
    let picture = links[0]["preview"]["previewurl"].as_str().unwrap().to_owned();
    (stored, picture)
}

/// A made-up PNG, 32 by 16.
fn png() -> teams_core::PictureBytes {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(32, 16, image::Rgba([200, 160, 40, 255]))
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

#[test]
fn a_link_s_preview_is_drawn_as_its_card_under_the_text_and_opens_the_link() {
    let (stored, picture) = link_message();
    let rig = rig_with(vec![stored]);
    rig.core.pictures.lock().unwrap().insert(picture, png());
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    draw(cx);
    cx.run_until_parked();
    let card = bounds(cx, &format!("link-preview-{ID}-0")).expect("the preview's card");
    let bubble = bounds(cx, &format!("bubble-{ID}")).expect("the bubble");
    // under the text's line
    assert!(
        card.top() >= bubble.top() + px(20.0),
        "under the text: {card:?} {bubble:?}"
    );
    assert!(
        card.left() >= bubble.left() && card.right() <= bubble.right() && card.bottom() <= bubble.bottom(),
        "in the bubble: {card:?} {bubble:?}"
    );
    assert_eq!(card.size.height, px(CARD_HEIGHT), "{card:?}");
    assert!(card.size.width <= px(CARD_MAX_WIDTH), "{card:?}");
    let shown = bounds(cx, &format!("link-preview-picture-{ID}-0")).expect("its picture, asked of the engine");
    assert_eq!(
        (shown.size.width, shown.size.height),
        (px(PICTURE_WIDTH), px(CARD_HEIGHT))
    );
    assert_eq!(shown.left(), card.left(), "on the card's left");
    assert!(bounds(cx, &format!("link-preview-title-{ID}-0")).is_some());
    // only the preview left on: the second link's is off
    assert!(bounds(cx, &format!("link-preview-{ID}-1")).is_none());

    cx.simulate_click(card.center(), Modifiers::none());
    draw(cx);
    assert_eq!(cx.opened_url().as_deref(), Some("http://made-up.example.test"));
}
