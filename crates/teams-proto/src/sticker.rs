//! A sticker from Teams' own packs, as a message carries it (`docs/protocol/chat.md`, "Stickers"): an `<img>` of
//! itemtype `http://schema.skype.com/Sticker` whose `src` is the sticker's GIF on Teams' public sticker CDN
//! (`https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/<pack>-<w>x<h>/<locale>/<name>.gif`,
//! with `?v=5`).
//! The web client draws it as a browser draws any `<img>`: a public file, read with no credential. [`StickerPicture`]
//! is that read, `GET <pack folder>[/<locale>]/<file>` on [`Service::Stickers`], with no token: nothing is sent but the
//! folder, the locale and the file's name taken from the message (the query is left out: the CDN serves the same file
//! without it).

use url::Url;

use crate::ams::{PastedFormat, PastedPicture};
use crate::client::Response;
use crate::endpoint::{Endpoint, Service};
use crate::read::Read;

/// The host Teams' web client reads its stickers from ([`Service::Stickers`]).
pub const STICKERS_HOST: &str = "teams.public.onecdn.static.microsoft";

/// The other host of Teams' public `evergreen-assets`, which serves the same sticker files.
const STATICS_HOST: &str = "statics.teams.cdn.office.net";

/// The path of the sticker files on both hosts.
pub(crate) const STICKERS_PATH: [&str; 2] = ["evergreen-assets", "stickerassets"];

/// The longest pack folder read (`clippy-250x250`, `officedrama-250x250`, `meme-500x500`).
const MAX_FOLDER: usize = 64;

/// The longest locale read (`en-us`).
const MAX_LOCALE: usize = 16;

/// The longest file name read (`Clippy_HelloThere.gif`).
const MAX_FILE: usize = 96;

/// `GET https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/<folder>[/<locale>]/<file>`: a
/// sticker a message shows. Its answer is read with [`StickerPicture::read_answer`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StickerPicture {
    folder: String,
    locale: Option<String>,
    file: String,
}

impl StickerPicture {
    /// The read of the sticker an `<img>`'s `src` names: `https` on [`STICKERS_HOST`] or Teams' static CDN host, no
    /// port, user info or fragment (a query is left out), the path `/evergreen-assets/stickerassets/<folder>/<file>`
    /// or `…/<folder>/<locale>/<file>`: the folder lowercase ASCII letters, digits and `-`; the locale lowercase ASCII
    /// letters and `-`; the file ASCII letters, digits, `-` and `_` ending in `.gif`. `None` for anything else.
    #[must_use]
    pub fn of(src: &str) -> Option<Self> {
        let url = Url::parse(src.trim()).ok()?;
        let plain = url.scheme() == "https"
            && matches!(url.host_str(), Some(STICKERS_HOST | STATICS_HOST))
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none();
        if !plain {
            return None;
        }
        let segments: Vec<&str> = url.path_segments()?.collect();
        let (folder, locale, file) = match segments[..] {
            ["evergreen-assets", "stickerassets", folder, file] => (folder, None, file),
            ["evergreen-assets", "stickerassets", folder, locale, file] if is_locale(locale) => {
                (folder, Some(locale), file)
            }
            _ => return None,
        };
        (is_folder(folder) && is_file(file)).then(|| Self {
            folder: folder.to_owned(),
            locale: locale.map(str::to_owned),
            file: file.to_owned(),
        })
    }

    /// The pack's folder (`clippy-250x250`).
    #[must_use]
    pub fn folder(&self) -> &str {
        &self.folder
    }

    /// The locale its picture is drawn for (`en-us`), when the path names one.
    #[must_use]
    pub fn locale(&self) -> Option<&str> {
        self.locale.as_deref()
    }

    /// Its file's name (`Clippy_HelloThere.gif`).
    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }

    /// The picture the answer gives: its bytes when they are a GIF (by their signature), else `None`.
    #[must_use]
    pub fn read_answer(&self, answer: &Response) -> Option<PastedPicture> {
        let bytes = answer.body();
        (PastedFormat::sniff(bytes)? == PastedFormat::Gif).then(|| PastedPicture {
            format: PastedFormat::Gif,
            bytes: bytes.to_vec(),
        })
    }
}

impl Read for StickerPicture {
    fn service(&self) -> Service {
        Service::Stickers
    }

    fn endpoint(&self) -> Endpoint {
        let mut segments = vec![self.folder.as_str()];
        segments.extend(self.locale.as_deref());
        segments.push(self.file.as_str());
        Endpoint::new(segments)
    }
}

fn is_folder(folder: &str) -> bool {
    (1..=MAX_FOLDER).contains(&folder.len())
        && folder
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_locale(locale: &str) -> bool {
    (2..=MAX_LOCALE).contains(&locale.len()) && locale.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'-')
}

fn is_file(file: &str) -> bool {
    file.len() <= MAX_FILE
        && file.strip_suffix(".gif").is_some_and(|stem| {
            !stem.is_empty()
                && stem
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}
