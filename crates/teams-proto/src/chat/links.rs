//! The previews of the links a message carries (`properties.links`, `docs/protocol/chat.md`, "Link previews"): the
//! card Teams draws under a message's text for a link the sender's client had previewed, with the page's title, its
//! description, a picture of it and its site. Read, never fetched: [`LinkPreview`] keeps what the entry says, and a
//! view asks the engine for its picture by the entry's `previewurl` ([`crate::LinkPreviewPicture`]).
//!
//! - **The shape** (seen in a received message, written down as `tests/fixtures/chatsvc/link-preview.json`):
//!   `properties.links` is a JSON string of a list; an entry's `@type` is `http://schema.skype.com/HyperLink`, `url` the
//!   link, `itemid` the anchor's in the text, `previewenabled` whether the sender left its preview on, and `preview` an
//!   object of `title`, `description`, `previewurl` (a picture of the page through the async gateway's URL preview
//!   service: `https://<region>-prod.asyncgw.teams.microsoft.com/urlp/v1/url/image/Thumbnail?url=<the page's picture>`)
//!   and `previewMetadata` (`width` and `height` of that picture, as numbers like `320.0`).
//! - **Read strictly**: an entry is a preview only when its `@type` is the link's, its `url` an `http` or `https` URL,
//!   `previewenabled` isn't `false` and its `preview` has a title; a link whose preview the sender turned off, or one
//!   sent before its preview was read (`"preview": {"previewurl": ""}`, as the compose box sends one), draws no card.
//!   An entry of another shape is left out: the link itself stays in the text.

use serde_json::{Map, Value};
use url::Url;

/// An entry's `@type`.
pub const HYPERLINK_TYPE: &str = "http://schema.skype.com/HyperLink";

/// The longest title and description kept, in characters; a longer one is cut there.
const MAX_TEXT: usize = 1000;

/// The longest link kept, in bytes; an entry with a longer one isn't read.
const MAX_LINK: usize = 4096;

/// The largest side a preview picture's given size may have, in pixels; a larger one is no size.
const MAX_PICTURE_SIDE: f64 = 100_000.0;

/// The most previews read from one message.
const MAX_PREVIEWS: usize = 16;

/// A link's preview, as the card under the message shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkPreview {
    /// The link, opened when the card is clicked.
    pub url: String,
    /// The link's host, which the card names the site by (`fal.ai`).
    pub site: String,
    pub title: String,
    pub description: Option<String>,
    /// The picture of the page, as the entry's `previewurl` names it (read by [`crate::LinkPreviewPicture`]), and the
    /// size the entry gives it.
    pub picture: Option<String>,
    pub picture_size: Option<(u32, u32)>,
}

/// The previews `properties.links` holds (a JSON string of a list, or a list), in their order; none for anything else.
#[must_use]
pub fn link_previews(value: Option<&Value>) -> Vec<LinkPreview> {
    let list = match value {
        Some(Value::Array(list)) => list.clone(),
        Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(list)) => list,
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    list.iter()
        .filter_map(|entry| entry.as_object().and_then(read_entry))
        .take(MAX_PREVIEWS)
        .collect()
}

fn read_entry(entry: &Map<String, Value>) -> Option<LinkPreview> {
    if entry.get("@type").and_then(Value::as_str) != Some(HYPERLINK_TYPE)
        || entry.get("previewenabled") == Some(&Value::Bool(false))
    {
        return None;
    }
    let url = entry.get("url").and_then(Value::as_str)?.trim();
    let parsed = Url::parse(url).ok()?;
    if url.len() > MAX_LINK || !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let site = parsed.host_str()?.to_owned();
    let preview = entry.get("preview")?.as_object()?;
    let title = text(preview.get("title"))?;
    let picture = preview
        .get("previewurl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|link| !link.is_empty() && link.len() <= MAX_LINK)
        .map(str::to_owned);
    let picture_size = preview
        .get("previewMetadata")
        .and_then(Value::as_object)
        .and_then(|metadata| Some((side(metadata.get("width"))?, side(metadata.get("height"))?)));
    Some(LinkPreview {
        url: url.to_owned(),
        site,
        title,
        description: text(preview.get("description")),
        picture,
        picture_size,
    })
}

/// A title's or description's text: trimmed, not empty, cut at [`MAX_TEXT`] characters.
fn text(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    (!text.is_empty()).then(|| text.chars().take(MAX_TEXT).collect())
}

/// A picture's side as the entry gives it (a number, `320.0` as captured): a whole number of pixels, at least one.
fn side(value: Option<&Value>) -> Option<u32> {
    let side = value?.as_f64()?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked to be within 1 and MAX_PICTURE_SIDE first"
    )]
    (side.is_finite() && (1.0..=MAX_PICTURE_SIDE).contains(&side)).then(|| side.round() as u32)
}
