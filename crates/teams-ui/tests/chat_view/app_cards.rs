//! A link's app card under a message's text (`properties.cards`, `teams_ui::card_view`), drawn as Teams draws a
//! YouTube link's card, on GPUI's test context over the fake engine: in the bubble under the text, the app's icon and
//! name over the card, the video's picture (asked of the engine by the gateway's thumbnail of YouTube's picture of the
//! video) 16:9 wide with a play mark, the title, the channel and the counts, the Open button; a click on the picture
//! or on Open opens the video in the browser; nothing says the message has content that can't be shown. The message
//! is the synthetic fixture `tests/fixtures/chatsvc/app-card.json`: every word, link and id is made up.

use std::sync::Arc;

use gpui::{Bounds, Modifiers, Pixels, VisualTestContext, px};
use serde_json::Value;
use teams_proto::LinkPreviewPicture;
use teams_store::Message;
use teams_ui::card_view::{CARD_WIDTH, cropped_16_9};

use super::common::LAB_B;
use super::{NOON, chat_window, draw, message, rig_with, trace};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/chatsvc/app-card.json");
const ID: &str = "405";
const WATCH: &str = "https://www.youtube.com/watch?v=Ab3_dEf-Gh9";
const VIDEO_PICTURE: &str = "https://i.ytimg.com/vi/Ab3_dEf-Gh9/hqdefault.jpg";
const ICON: &str = "https://res.cdn.office.net/teamsappdata/made-up/com.microsoft.teamspace.tab.youtube_largeimage.png";

/// The fixture's message, from b, as the store keeps it (its `properties` JSON text).
fn app_card_message() -> Message {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let read_back = &fixture["read_back"];
    let mut stored = message(ID, LAB_B, read_back["content"].as_str().unwrap(), NOON);
    stored.properties = Some(read_back["properties"].to_string());
    stored
}

/// A made-up PNG of `width` by `height`.
fn png(width: u32, height: u32) -> teams_core::PictureBytes {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(width, height, image::Rgba([200, 40, 40, 255]))
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
fn a_youtube_link_s_card_is_drawn_under_the_text_and_opens_the_video() {
    let rig = rig_with(vec![app_card_message()]);
    {
        let mut pictures = rig.core.pictures.lock().unwrap();
        // 4:3, as YouTube's picture of a 16:9 video is: the card crops it to 16:9
        pictures.insert(
            LinkPreviewPicture::card_picture_src(VIDEO_PICTURE).unwrap(),
            png(48, 36),
        );
        pictures.insert(LinkPreviewPicture::card_picture_src(ICON).unwrap(), png(16, 16));
    }
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    draw(cx);
    cx.run_until_parked();

    assert!(
        bounds(cx, &format!("unshown-{ID}")).is_none(),
        "a card this build draws is not content it can't show"
    );
    let bubble = bounds(cx, &format!("bubble-{ID}")).expect("the bubble");
    let card = bounds(cx, &format!("card-{ID}-c0")).expect("the app's card");
    assert!(
        card.top() >= bubble.top() + px(20.0),
        "under the text: {card:?} {bubble:?}"
    );
    assert!(
        card.left() >= bubble.left() && card.right() <= bubble.right() && card.bottom() <= bubble.bottom(),
        "in the bubble: {card:?} {bubble:?}"
    );
    assert_eq!(
        card.size.width,
        px(CARD_WIDTH),
        "as wide as Teams draws it, however short the link: {card:?}"
    );

    let app = bounds(cx, &format!("card-{ID}-c0app")).expect("the app's name over the card");
    assert!(
        bounds(cx, &format!("card-{ID}-c0app-icon")).is_some(),
        "the app's icon, asked of the engine"
    );
    let media = bounds(cx, &format!("card-{ID}-c0b0.")).expect("the video");
    assert!(media.top() >= app.bottom(), "under the app's name: {media:?} {app:?}");
    let ratio = f32::from(media.size.width) / f32::from(media.size.height);
    assert!((ratio - 16.0 / 9.0).abs() < 0.02, "16:9: {media:?}");
    assert!(
        bounds(cx, &format!("card-{ID}-c0b0.picture")).is_some(),
        "the video's picture"
    );
    let play = bounds(cx, &format!("card-{ID}-c0b0.play")).expect("the play mark");
    assert!(media.contains(&play.center()), "on the picture: {play:?} {media:?}");
    let title = bounds(cx, &format!("card-{ID}-c0b1.")).expect("the title");
    assert!(title.top() >= media.bottom(), "under the video: {title:?}");

    cx.simulate_click(media.center(), Modifiers::none());
    draw(cx);
    assert_eq!(cx.opened_url().as_deref(), Some(WATCH));

    let open = bounds(cx, &format!("card-{ID}-c0b4.0")).expect("the Open button");
    cx.simulate_click(open.center(), Modifiers::none());
    draw(cx);
    assert_eq!(cx.opened_url().as_deref(), Some(WATCH));
}

#[test]
fn a_video_s_picture_is_cropped_to_16_9_from_its_middle() {
    // YouTube's 4:3 picture of a 16:9 video: its bars above and below fall outside the box
    let (width, height) = cropped_16_9(480, 360);
    assert!(
        (width - 1.0).abs() < f32::EPSILON && (height - 4.0 / 3.0).abs() < 1e-4,
        "{width} {height}"
    );
    assert_eq!(cropped_16_9(1280, 720), (1.0, 1.0));
    let (width, height) = cropped_16_9(200, 90);
    assert!(
        (width - 1.25).abs() < 1e-4 && (height - 1.0).abs() < f32::EPSILON,
        "{width} {height}"
    );
    assert_eq!(cropped_16_9(0, 90), (1.0, 1.0));
}
