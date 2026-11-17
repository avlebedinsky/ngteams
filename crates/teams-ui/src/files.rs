//! The files a message carries, drawn as Teams draws them under the message's text: a card per file, side by side and
//! wrapping onto the next row when the bubble is narrow.
//!
//! - **A card** ([`FileCard`]): a header with the file type's icon (in the colour Teams gives that type), the file's
//!   name on one line, cut with an ellipsis, Share and "More options" (…); under it, for a video the player
//!   that plays it in the card (`crate::video_player`, its thumbnail standing in until it plays), for another video or
//!   an image a picture of the file filling the card: its thumbnail, asked of the engine by the file's sharing link
//!   (`crate::inline::thumbnail`, never a request of the view's own), a tinted box while it is read, and a placeholder
//!   with the type's icon when there is none. Another kind of file is its header alone.
//! - **An image with a preview** (its entry's `filePreview`, a picture of it on AMS) is drawn as Teams draws it: the
//!   picture alone, no header, at the preview's size scaled down into [`PREVIEW_MAX`] (its aspect ratio kept), with
//!   rounded corners; "More options" over its corner while the pointer is on it. The picture is asked of the engine as
//!   a message's picture is (`crate::inline::picture`); a tinted box of its size stands in while it is read, and the
//!   card is drawn when there is none.
//! - **A click** on the name or the picture opens the file the way the web client does, at its sharing link in the
//!   browser (GPUI's `open_url`); "More options" offers "Open in browser", "Download" (the sharing link with
//!   `download=1`: the browser downloads it) and "Copy link". Share offers the file's link to copy, the way to share it
//!   that reaches nobody by itself (sharing it with people from the app would be a contacting write it doesn't make).
//! - **An own server's file** ([`OwnFile`]) is read through its account's engine, never in a browser: the server has
//!   no page that shows a file, and a browser holds no token for it. A click and "Open in desktop app" save it in a
//!   new folder in a folder of the user's alone and open it in the system's app for it (its `file://` link);
//!   "Download" saves it where the user chooses (the downloads folder without a chooser)
//!   (`crate::recording_download`). Each runs to its end, also when the other one starts while it runs. A line under
//!   the card tells how far the last one got.
//! - Colours come from the palette but for the file types' own, fixed in every theme as Teams' file icons are.

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, App, ClipboardItem, Global, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img, px,
    rgb,
};
use gpui_component::Sizable as _;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use teams_proto::chat::{FileKind, SharedFile};

use crate::icons::{Glyph, icon_button, selected_by};
use crate::inline::{PictureSource, PictureState, picture, thumbnail};
use crate::recording_download::{self, Failure, Progress, Then, download_of};
use crate::rich_text::tint;
use crate::theme::Palette;
use crate::video_player::{player_of, plays_inline};

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
pub const OPEN_IN_APP: &str = "Open in desktop app";
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
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let kind = self.file.kind();
        if let Some(preview) = self.preview(kind, cx) {
            return preview;
        }
        let picture = match &self.source {
            Some(source) if plays_inline(&self.file) => {
                Some(player_of(&self.id, &self.file, source, window, cx).into_any_element())
            }
            _ => kind.has_thumbnail().then(|| self.picture(kind, cx)),
        };
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
            .children(self.status(cx))
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
            self.own(),
        );
        let status = self.status(cx);
        let preview = div()
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
            .into_any_element();
        Some(match status {
            Some(status) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(preview)
                .child(status)
                .into_any_element(),
            None => preview,
        })
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
            self.own(),
        );
        div()
            .h(px(48.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .child(opens)
            .children(self.share())
            .children(self.more())
            .into_any_element()
    }

    /// Share: the file's link to copy, as Teams' Share offers it first.
    fn share(&self) -> Option<AnyElement> {
        let link = self.file.open_link()?.to_owned();
        let id = self.id.clone();
        let button = icon_button(SharedString::from(format!("file-share-{id}")), Glyph::SHARE, "Share")
            .small()
            .dropdown_menu(move |menu, _, _| {
                let link = link.clone();
                menu.item(
                    menu_item(&id, "share-copy", Glyph::LINK, COPY_LINK)
                        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(link.clone()))),
                )
            });
        Some(selected_by(format!("file-share-{}", self.id), button).into_any_element())
    }

    /// "More options": the file's local actions, when it has a link to act on.
    fn more(&self) -> Option<AnyElement> {
        let open = self.file.open_link()?.to_owned();
        if let Some(own) = self.own() {
            return Some(self.own_more(own, open));
        }
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

    /// "More options" of an own server's file: opened and downloaded through its engine (see the module docs).
    fn own_more(&self, own: OwnFile, link: String) -> AnyElement {
        let id = self.id.clone();
        let button = icon_button(
            SharedString::from(format!("file-more-{id}")),
            Glyph::MORE,
            "More options",
        )
        .small()
        .dropdown_menu(move |menu, _, _| {
            let (opened, downloaded, copied) = (own.clone(), own.clone(), link.clone());
            menu.item(
                menu_item(&id, "open", Glyph::EXTERNAL_LINK, OPEN_IN_APP).on_click(move |_, _, cx| opened.open(cx)),
            )
            .item(
                menu_item(&id, "download", Glyph::DOWNLOAD, DOWNLOAD).on_click(move |_, _, cx| downloaded.download(cx)),
            )
            .item(
                menu_item(&id, "copy", Glyph::LINK, COPY_LINK)
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))),
            )
        });
        selected_by(format!("file-more-{}", self.id), button).into_any_element()
    }

    /// The file as its own server's, read through `source`'s engine: when it has an engine and its links aren't on
    /// Microsoft's file service.
    fn own(&self) -> Option<OwnFile> {
        let source = self.source.clone()?;
        let link = self.file.share_url.clone()?;
        (!teams_proto::chat::is_file_link(&link)).then(|| OwnFile {
            link,
            name: self.file.name.clone(),
            source,
        })
    }

    /// How far the own server's file's download got, as a line under its card; none without one this run.
    fn status(&self, cx: &App) -> Option<AnyElement> {
        let own = self.own()?;
        download_line(
            &own.link,
            &own.name,
            &format!("file-download-{}", self.id),
            self.palette,
            cx,
        )
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
            self.own(),
        )
    }
}

/// `element` opening `file` when clicked, with the pointer's hand: an own server's through its engine ([`OwnFile::open`]),
/// any other in the browser (its sharing link, else its own); as it was when the file has no link to open.
fn open_on_click(element: gpui::Stateful<gpui::Div>, file: &SharedFile, own: Option<OwnFile>) -> AnyElement {
    let Some(link) = file.open_link().map(str::to_owned) else {
        return element.into_any_element();
    };
    element
        .cursor_pointer()
        .hover(|style| style.opacity(0.85))
        .on_click(move |_, _, cx| match &own {
            Some(own) => own.open(cx),
            None => cx.open_url(&link),
        })
        .into_any_element()
}

/// Why an own server's file couldn't be downloaded, as its card's line says it.
fn failed_words(failure: &Failure) -> String {
    match failure {
        Failure::Refused => "You may no longer have access to the file.".to_owned(),
        other => other.words(),
    }
}

/// How far the last download this run of the own server's file at `link` (saved as `name`) got, as a line; none
/// without one, or after it was cancelled. Its selector is `<selector> <state>`.
pub(crate) fn download_line(link: &str, name: &str, selector: &str, palette: Palette, cx: &App) -> Option<AnyElement> {
    let download = download_of(link, cx)?;
    let download = download.read(cx);
    let (state, words) = match download.progress() {
        Progress::Running { .. } => (
            "running",
            match download.progress().fraction() {
                Some(fraction) => format!("Downloading {name}… {:.0}%", fraction * 100.0),
                None => format!("Downloading {name}…"),
            },
        ),
        Progress::Saved if download.then() == Then::Open => ("saved", format!("Opened {name}")),
        Progress::Saved => ("saved", format!("Saved to {}", download.target().display())),
        Progress::Failed(Failure::Cancelled) => return None,
        Progress::Failed(failure) => ("failed", format!("Couldn't download {name}. {}", failed_words(failure))),
    };
    let selector = format!("{selector} {state}");
    Some(
        div()
            .debug_selector(move || selector.clone())
            .px_3()
            .pb_2()
            .text_xs()
            .text_color(rgb(palette.muted_foreground))
            .child(words)
            .into_any_element(),
    )
}

/// An own server's file, read through its account's engine (see the module docs).
#[derive(Clone)]
pub struct OwnFile {
    /// Its sharing link: what the engine reads it by, and what its download is known by.
    link: String,
    name: String,
    source: PictureSource,
}

/// Where Open in desktop app saves an own server's files, in place of `ngteams-files` in the runtime folder (a test's
/// own folder).
pub struct OpenedFilesFolder(pub PathBuf);

impl Global for OpenedFilesFolder {}

/// A new folder in `folder` for one opened file: `1`, `2`, …, the first that is not there. It is made at once, so two
/// files of one name opened together never write to one file.
fn new_folder_in(folder: &Path) -> std::io::Result<PathBuf> {
    for n in 1_u32.. {
        let made = folder.join(n.to_string());
        match std::fs::create_dir(&made) {
            Ok(()) => return Ok(made),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::ErrorKind::AlreadyExists.into())
}

impl OwnFile {
    /// The file at `link` (its sharing link), saved as `name`, read through the engine of `source`.
    #[must_use]
    pub fn new(link: String, name: String, source: PictureSource) -> Self {
        Self { link, name, source }
    }

    /// Open in desktop app: saved in a new folder (see [`new_folder_in`]) in a folder of the user's alone (the
    /// runtime one, else the temporary one, or [`OpenedFilesFolder`]; the downloads folder when it can't be had),
    /// then opened in the system's app for it.
    pub fn open(&self, cx: &mut App) {
        let private = cx.try_global::<OpenedFilesFolder>().map_or_else(
            || {
                std::env::var_os("XDG_RUNTIME_DIR")
                    .map(PathBuf::from)
                    .filter(|dir| dir.is_dir())
                    .unwrap_or_else(std::env::temp_dir)
                    .join("ngteams-files")
            },
            |folder| folder.0.clone(),
        );
        let name = safe_name(&self.name);
        let target = teams_platform::private::create_dir_all(&private)
            .and_then(|()| teams_platform::private::restrict_dir(&private))
            .and_then(|()| new_folder_in(&private))
            .map_or_else(
                |_| {
                    let folder = crate::picture_viewer::downloads().unwrap_or_else(|_| std::env::temp_dir());
                    crate::picture_viewer::free_name(&folder, &name)
                },
                |folder| folder.join(&name),
            );
        recording_download::start_with(
            self.source.file_opener(self.link.clone()),
            self.link.clone(),
            target,
            Then::Open,
            cx,
        );
    }

    /// Download: saved where the user chooses (the downloads folder without a chooser).
    pub fn download(&self, cx: &mut App) {
        let folder = crate::picture_viewer::downloads().unwrap_or_else(|_| std::env::temp_dir());
        let name = safe_name(&self.name);
        let chosen = cx.prompt_for_new_path(&folder, Some(&name));
        let own = self.clone();
        cx.spawn(async move |cx| {
            let target = match chosen.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                Ok(Err(_)) | Err(_) => crate::picture_viewer::free_name(&folder, &name),
            };
            cx.update(|cx| {
                recording_download::start_with(
                    own.source.file_opener(own.link.clone()),
                    own.link.clone(),
                    target,
                    Then::Keep,
                    cx,
                );
            });
        })
        .detach();
    }
}

/// `name` as a file name in a folder: the sender's words, so never a path or a name the system refuses.
#[must_use]
pub fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if cleaned.is_empty() {
        "file".to_owned()
    } else {
        cleaned.to_owned()
    }
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
