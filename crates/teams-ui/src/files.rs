//! The files a message carries, drawn as Teams draws them under the message's text: a card per file, side by side and
//! wrapping onto the next row when the bubble is narrow.
//!
//! - **A card** ([`FileCard`]): a header with the file type's icon (in the colour Teams gives that type), the file's
//!   name on one line, cut with an ellipsis, and "More options" (…); under it, for a video or an image, a picture of the
//!   file filling the card: its thumbnail, asked of the engine by the file's sharing link (`crate::inline::thumbnail`,
//!   never a request of the view's own), a tinted box while it is read, and a placeholder with the type's icon when
//!   there is none. Another kind of file is its header alone.
//! - **An image with a preview** (its entry's `filePreview`, a picture of it on AMS) is drawn as Teams draws it: the
//!   picture alone, no header, at the preview's size scaled down into [`PREVIEW_MAX`] (its aspect ratio kept), with
//!   rounded corners; "More options" over its corner while the pointer is on it. The picture is asked of the engine as
//!   a message's picture is (`crate::inline::picture`); a tinted box of its size stands in while it is read, and the
//!   card is drawn when there is none.
//! - **A click** on the name or the picture opens the file the way the web client does, at its sharing link in the
//!   browser (GPUI's `open_url`); "More options" offers "Open in browser", "Download" (the sharing link with
//!   `download=1`: the browser downloads it) and "Copy link". Nothing here reaches anyone: Teams' "Share" is left out
//!   (sharing a file is a contacting write the app doesn't make).
//! - Colours come from the palette but for the file types' own, fixed in every theme as Teams' file icons are.

use gpui::{
    AnyElement, App, ClipboardItem, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use teams_proto::chat::{FileKind, SharedFile};

use crate::icons::{Glyph, icon_button, selected_by};
use crate::inline::{PictureSource, PictureState, picture, thumbnail};
use crate::rich_text::tint;
use crate::theme::Palette;

/// A card's width, as Teams draws one.
pub const CARD_WIDTH: f32 = 316.0;

/// The height of a card's picture: a 16:9 frame of the card's width.
pub const PICTURE_HEIGHT: f32 = 178.0;

/// The box an image's preview is scaled down into (width, height), as Teams draws one in a message.
pub const PREVIEW_MAX: (f32, f32) = (360.0, 252.0);

/// The size a preview is held at while it is read, when its entry gives none.
const PREVIEW_UNKNOWN: (u32, u32) = (252, 252);

/// What "More options" offers, in Teams' words.
pub const OPEN_IN_BROWSER: &str = "Open in browser";
pub const DOWNLOAD: &str = "Download";
pub const COPY_LINK: &str = "Copy link";

/// The colour Teams draws a file type's icon in, the same in every theme.
#[must_use]
pub const fn kind_colour(kind: FileKind) -> u32 {
    match kind {
        FileKind::Video => 0x0088_6ce4,
        FileKind::Image => 0x0000_9fb4,
        FileKind::Audio => 0x00c2_39b3,
        FileKind::Pdf => 0x00d1_3438,
        FileKind::Word => 0x0029_5ec7,
        FileKind::Excel => 0x0010_8b4b,
        FileKind::PowerPoint => 0x00d3_5230,
        FileKind::Archive => 0x00c1_9c00,
        FileKind::Text | FileKind::Other => 0x0088_8888,
    }
}

/// The icon of a file type.
#[must_use]
pub const fn kind_glyph(kind: FileKind) -> Glyph {
    match kind {
        FileKind::Video => Glyph::FILE_VIDEO,
        FileKind::Image => Glyph::FILE_IMAGE,
        FileKind::Audio => Glyph::FILE_AUDIO,
        FileKind::Pdf | FileKind::Word | FileKind::Text => Glyph::TRANSCRIPT,
        FileKind::Excel => Glyph::FILE_SPREADSHEET,
        FileKind::PowerPoint => Glyph::FILE_CHART,
        FileKind::Archive => Glyph::FILE_ARCHIVE,
        FileKind::Other => Glyph::FILE,
    }
}

/// The cards of `files`, carried by the message `message`, in a row that wraps; `None` without files. `source` is the
/// engine their thumbnails are asked of (none: no thumbnail, each picture its placeholder).
#[must_use]
pub fn file_cards(
    message: &str,
    files: &[SharedFile],
    source: Option<&PictureSource>,
    palette: Palette,
) -> Option<AnyElement> {
    if files.is_empty() {
        return None;
    }
    let message = message.to_owned();
    let cards = files.iter().enumerate().map(|(at, file)| FileCard {
        id: format!("{message}-{at}"),
        file: file.clone(),
        source: source.cloned(),
        palette,
    });
    let cards: Vec<FileCard> = cards.collect();
    Some(
        div()
            .debug_selector(move || format!("file-cards-{message}"))
            .pt_2()
            .flex()
            .flex_wrap()
            .items_start()
            .gap_2()
            .children(cards)
            .into_any_element(),
    )
}

/// One file's card (see the module docs). `id` names it among every card drawn: the message's id and the file's place.
#[derive(IntoElement)]
pub struct FileCard {
    id: String,
    file: SharedFile,
    source: Option<PictureSource>,
    palette: Palette,
}

impl RenderOnce for FileCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let kind = self.file.kind();
        if let Some(preview) = self.preview(kind, cx) {
            return preview;
        }
        let picture = kind.has_thumbnail().then(|| self.picture(kind, cx));
        let id = self.id.clone();
        div()
            .debug_selector(move || format!("file-card-{id}"))
            .w(px(CARD_WIDTH))
            .max_w_full()
            .flex_none()
            .flex()
            .flex_col()
            .rounded_lg()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(self.palette.border))
            // the foreground faintly over the bubble: lighter on a dark one, darker on a light one
            .bg(tint(self.palette.foreground, 0.04))
            .child(self.header(kind))
            .children(picture)
            .into_any_element()
    }
}

/// The size `natural` is drawn at inside [`PREVIEW_MAX`]: scaled down to fit, its aspect ratio kept, never up.
#[must_use]
pub fn preview_size(natural: (u32, u32)) -> (f32, f32) {
    #[expect(clippy::cast_precision_loss, reason = "a preview's side is at most 100 000")]
    let (width, height) = (natural.0.max(1) as f32, natural.1.max(1) as f32);
    let scale = (PREVIEW_MAX.0 / width).min(PREVIEW_MAX.1 / height).min(1.0);
    ((width * scale).round(), (height * scale).round())
}

impl FileCard {
    /// An image drawn as its preview (see the module docs); `None` when it is drawn as its card: another kind of file,
    /// no preview, no engine to ask, or none to have.
    fn preview(&self, kind: FileKind, cx: &mut App) -> Option<AnyElement> {
        let preview = self.file.preview.as_ref().filter(|_| kind == FileKind::Image)?;
        let state = picture(&preview.src, self.source.as_ref()?, cx);
        let given = preview.size;
        let id = self.id.clone();
        let (selector, size, shown) = match state {
            PictureState::Missing => return None,
            PictureState::Reading => (
                "file-preview-reading",
                preview_size(given.unwrap_or(PREVIEW_UNKNOWN)),
                div()
                    .size_full()
                    .bg(tint(self.palette.foreground, 0.06))
                    .into_any_element(),
            ),
            PictureState::Shown(image, natural) => (
                "file-preview",
                preview_size(given.unwrap_or(natural)),
                img(image).size_full().object_fit(ObjectFit::Cover).into_any_element(),
            ),
        };
        let group = SharedString::from(format!("file-preview-{id}"));
        let more = self.more().map(|more| {
            div()
                .absolute()
                .top_1()
                .right_1()
                .rounded_md()
                .bg(rgb(self.palette.surface))
                .opacity(0.0)
                .group_hover(group.clone(), |style| style.opacity(1.0))
                .child(more)
        });
        let picture = open_on_click(
            div()
                .id(SharedString::from(format!("file-picture-{id}")))
                .size_full()
                .child(shown),
            &self.file,
        );
        Some(
            div()
                .id(SharedString::from(format!("file-preview-box-{id}")))
                .debug_selector(move || format!("{selector}-{id}"))
                .group(group)
                .relative()
                .w(px(size.0))
                .h(px(size.1))
                .max_w_full()
                .flex_none()
                .rounded_lg()
                .overflow_hidden()
                .border_1()
                .border_color(rgb(self.palette.border))
                .child(picture)
                .children(more)
                .into_any_element(),
        )
    }

    /// The icon, the name and "More options".
    fn header(&self, kind: FileKind) -> AnyElement {
        let name = SharedString::from(self.file.name.clone());
        let opens = open_on_click(
            div()
                .id(SharedString::from(format!("file-open-{}", self.id)))
                .debug_selector({
                    let id = self.id.clone();
                    move || format!("file-name-{id}")
                })
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .tooltip({
                    let name = name.clone();
                    move |window, cx| Tooltip::new(name.clone()).build(window, cx)
                })
                .child(
                    kind_glyph(kind)
                        .icon()
                        .size(px(24.0))
                        .flex_none()
                        .text_color(rgb(kind_colour(kind))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(name),
                ),
            &self.file,
        );
        div()
            .h(px(48.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .child(opens)
            .children(self.more())
            .into_any_element()
    }

    /// "More options": the file's local actions, when it has a link to act on.
    fn more(&self) -> Option<AnyElement> {
        let open = self.file.open_link()?.to_owned();
        let download = self.file.download_link();
        let id = self.id.clone();
        let button = icon_button(
            SharedString::from(format!("file-more-{id}")),
            Glyph::MORE,
            "More options",
        )
        .small()
        .dropdown_menu(move |menu, _, _| {
            let open_link = open.clone();
            let copied = open.clone();
            let menu = menu.item(
                menu_item(&id, "open", Glyph::EXTERNAL_LINK, OPEN_IN_BROWSER)
                    .on_click(move |_, _, cx| cx.open_url(&open_link)),
            );
            let menu = match download.clone() {
                Some(download) => menu.item(
                    menu_item(&id, "download", Glyph::DOWNLOAD, DOWNLOAD)
                        .on_click(move |_, _, cx| cx.open_url(&download)),
                ),
                None => menu,
            };
            menu.item(
                menu_item(&id, "copy", Glyph::LINK, COPY_LINK)
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))),
            )
        });
        Some(selected_by(format!("file-more-{}", self.id), button).into_any_element())
    }

    /// The picture of the file under its header: its thumbnail, a tinted box while it is read, its placeholder.
    fn picture(&self, kind: FileKind, cx: &mut App) -> AnyElement {
        let palette = self.palette;
        let state = match (&self.source, self.file.share_url.as_deref()) {
            (Some(source), Some(share_url)) => thumbnail(share_url, source, cx),
            _ => PictureState::Missing,
        };
        let id = self.id.clone();
        let (selector, shown) = match state {
            PictureState::Shown(image, _) => (
                "file-thumbnail",
                img(image).size_full().object_fit(ObjectFit::Cover).into_any_element(),
            ),
            PictureState::Reading => ("file-thumbnail-reading", div().size_full().into_any_element()),
            PictureState::Missing => (
                "file-thumbnail-none",
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        kind_glyph(kind)
                            .icon()
                            .size(px(40.0))
                            .text_color(rgb(palette.muted_foreground)),
                    )
                    .into_any_element(),
            ),
        };
        open_on_click(
            div()
                .id(SharedString::from(format!("file-picture-{id}")))
                .debug_selector(move || format!("{selector}-{id}"))
                .w_full()
                .h(px(PICTURE_HEIGHT))
                .bg(tint(palette.foreground, 0.06))
                .child(shown),
            &self.file,
        )
    }
}

/// `element` opening `file` in the browser when clicked (its sharing link, else its own), with the pointer's hand; as
/// it was when the file has no link to open.
fn open_on_click(element: gpui::Stateful<gpui::Div>, file: &SharedFile) -> AnyElement {
    let Some(link) = file.open_link().map(str::to_owned) else {
        return element.into_any_element();
    };
    element
        .cursor_pointer()
        .hover(|style| style.opacity(0.85))
        .on_click(move |_, _, cx| cx.open_url(&link))
        .into_any_element()
}

/// A "More options" row: its icon and words, its test selector `file-menu-<what>-<card>`.
fn menu_item(card: &str, what: &str, glyph: Glyph, label: &'static str) -> PopupMenuItem {
    let selector = format!("file-menu-{what}-{card}");
    PopupMenuItem::element(move |_, _| {
        let selector = selector.clone();
        div()
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .gap_2()
            .child(glyph.icon().size(px(14.0)))
            .child(label)
    })
}
