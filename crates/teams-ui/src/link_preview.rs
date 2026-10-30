//! A link's preview card under a message's text, as Teams draws one (`docs/protocol/chat.md`, "Link previews"): the
//! page's picture on the left, cropped to fill, and beside it the page's title (one line), its description (two lines
//! at most) and the site, on a surface a shade off the bubble's; a click opens the link through GPUI's `open_url`.
//!
//! The previews are read from the message's `properties.links` (`teams_proto::chat::link_previews`), never fetched by
//! the view: the picture is asked of the engine by the entry's `previewurl` as a message's picture is
//! (`crate::inline::picture`, the engine reading it as `teams_proto::LinkPreviewPicture`); while it is read a tinted
//! block stands in, and a card whose picture there is none of is drawn without one.

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img, px, rgb,
};
use teams_proto::chat::{LinkPreview, link_previews};

use crate::inline::{PictureSource, PictureState, picture};
use crate::rich_text::{opens, tint};
use crate::theme::Palette;

/// The card's height, which its picture fills (Teams' `9.6rem`, its `rem` 10 px).
pub const CARD_HEIGHT: f32 = 96.0;

/// The width of the card's picture.
pub const PICTURE_WIDTH: f32 = 120.0;

/// The widest a card is drawn.
pub const CARD_MAX_WIDTH: f32 = 460.0;

/// The previews of the links a stored message carries ([`link_previews`] of its `properties.links`), in their order;
/// none for a message without `properties` or with `properties` that aren't a JSON object.
#[must_use]
pub fn previews_of(properties: Option<&str>) -> Vec<LinkPreview> {
    properties
        .and_then(|text| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(text).ok())
        .map(|properties| link_previews(properties.get("links")))
        .unwrap_or_default()
}

/// The cards of `previews`, carried by the message `message`, one under another; `None` without previews. `source` is
/// the engine their pictures are asked of (none: no picture).
#[must_use]
pub fn cards(
    message: &str,
    previews: &[LinkPreview],
    source: Option<&PictureSource>,
    palette: Palette,
) -> Option<AnyElement> {
    if previews.is_empty() {
        return None;
    }
    let message = message.to_owned();
    let cards: Vec<Card> = previews
        .iter()
        .enumerate()
        .map(|(at, preview)| Card {
            id: format!("{message}-{at}"),
            preview: preview.clone(),
            source: source.cloned(),
            palette,
        })
        .collect();
    Some(
        div()
            .debug_selector(move || format!("link-previews-{message}"))
            .pt_2()
            .flex()
            .flex_col()
            .gap_2()
            .children(cards)
            .into_any_element(),
    )
}

/// One link's card. `id` names it among every card drawn: the message's id and the link's place.
#[derive(IntoElement)]
pub struct Card {
    id: String,
    preview: LinkPreview,
    source: Option<PictureSource>,
    palette: Palette,
}

impl Card {
    /// Its picture, filling its side of the card; `None` when there is none to show.
    fn picture(&self, cx: &mut App) -> Option<AnyElement> {
        let src = self.preview.picture.as_deref()?;
        let shown = match picture(src, self.source.as_ref()?, cx) {
            PictureState::Missing => return None,
            PictureState::Reading => div()
                .size_full()
                .bg(tint(self.palette.foreground, 0.06))
                .into_any_element(),
            PictureState::Shown(image, _) => img(image).size_full().object_fit(ObjectFit::Cover).into_any_element(),
        };
        let id = self.id.clone();
        Some(
            div()
                .debug_selector(move || format!("link-preview-picture-{id}"))
                .flex_none()
                .w(px(PICTURE_WIDTH))
                .h_full()
                .overflow_hidden()
                .child(shown)
                .into_any_element(),
        )
    }
}

impl RenderOnce for Card {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = self.palette;
        let picture = self.picture(cx);
        let id = self.id.clone();
        let url = self.preview.url.clone();
        let text = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .child(
                div()
                    .debug_selector({
                        let id = id.clone();
                        move || format!("link-preview-title-{id}")
                    })
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.foreground))
                    .truncate()
                    .child(SharedString::from(self.preview.title.clone())),
            )
            .children(self.preview.description.clone().map(|description| {
                div()
                    .text_sm()
                    .line_height(px(18.0))
                    .text_color(rgb(palette.foreground))
                    .line_clamp(2)
                    .text_ellipsis()
                    .child(SharedString::from(description))
            }))
            .child(
                div()
                    .mt_auto()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .truncate()
                    .child(SharedString::from(self.preview.site.clone())),
            );
        div()
            .id(SharedString::from(format!("link-preview-{id}")))
            .debug_selector(move || format!("link-preview-{id}"))
            .w_full()
            .max_w(px(CARD_MAX_WIDTH))
            .h(px(CARD_HEIGHT))
            .flex()
            .rounded_md()
            .overflow_hidden()
            .bg(rgb(palette.background))
            .shadow_sm()
            .cursor_pointer()
            .children(picture)
            .child(text)
            .on_click(move |_, _, cx| {
                if opens(&url) {
                    cx.open_url(&url);
                }
            })
    }
}
