//! The file chooser of the account picture QA: a fake `org.freedesktop.portal.FileChooser` on the sandbox's private
//! bus, served from the parent, and the synthetic pictures the user picks in it.
//!
//! The sandbox's bus has no file chooser, so GPUI's `prompt_for_paths` (ashpd's `OpenFile`) reaches this fake. It
//! answers each `OpenFile` at once with the next answer the test queued: a file (its `file://` URI, as the real portal
//! gives it) or a cancel (response 1). It records each call with its title, its accept label, the filters it offered
//! and the filter chosen when it opened.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use image::{Delay, Frame, Rgba, RgbaImage};
use ui_sandbox::SandboxResult;
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::interface;
use zbus::message::Header;
use zbus::zvariant::{OwnedObjectPath, Value};

const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";

/// What the user does in the next file chooser.
#[derive(Clone, Debug)]
pub enum Answer {
    Pick(PathBuf),
    Cancel,
}

/// A filter of the chooser: its name and its rules (a glob such as `*.png` or a MIME type such as `image/png`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub name: String,
    pub rules: Vec<String>,
}

/// One `OpenFile` call.
#[derive(Clone, Debug)]
pub struct Asked {
    pub title: String,
    pub accept_label: Option<String>,
    pub filters: Vec<Filter>,
    pub current_filter: Option<Filter>,
    pub answered: String,
}

#[derive(Default)]
struct State {
    answers: VecDeque<Answer>,
    asked: Vec<Asked>,
}

type Shared = Arc<Mutex<State>>;

fn lock(state: &Shared) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The fake portal, while it holds its name on the bus.
pub struct Chooser {
    state: Shared,
    _connection: Connection,
}

impl Chooser {
    /// Serves the fake file chooser on the bus at `address`.
    pub fn start(address: &str) -> SandboxResult<Self> {
        let state = Shared::default();
        let portal = FileChooser {
            state: Arc::clone(&state),
        };
        let connection = Builder::address(address)?
            .name(PORTAL_NAME)?
            .serve_at(PORTAL_PATH, portal)?
            .build()?;
        Ok(Self {
            state,
            _connection: connection,
        })
    }

    /// The answer of the next file chooser.
    pub fn queue(&self, answer: Answer) {
        lock(&self.state).answers.push_back(answer);
    }

    /// Drops the answers not given yet.
    pub fn clear(&self) {
        lock(&self.state).answers.clear();
    }

    /// The calls so far.
    pub fn asked(&self) -> Vec<Asked> {
        lock(&self.state).asked.clone()
    }
}

struct FileChooser {
    state: Shared,
}

fn sender(header: &Header<'_>) -> String {
    header.sender().map(ToString::to_string).unwrap_or_default()
}

/// A filter as the portal gets it: `(sa(us))`, its name and its rules.
fn filter_of(value: &Value<'_>) -> Option<Filter> {
    let Value::Structure(filter) = value else {
        return None;
    };
    let [Value::Str(name), Value::Array(rules)] = filter.fields() else {
        return None;
    };
    let rules = rules
        .inner()
        .iter()
        .filter_map(|rule| match rule {
            Value::Structure(rule) => match rule.fields() {
                [Value::U32(_), Value::Str(pattern)] => Some(pattern.as_str().to_owned()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    Some(Filter {
        name: name.as_str().to_owned(),
        rules,
    })
}

fn filters_of(options: &HashMap<&str, Value<'_>>) -> Vec<Filter> {
    match options.get("filters") {
        Some(Value::Array(filters)) => filters.inner().iter().filter_map(filter_of).collect(),
        _ => Vec::new(),
    }
}

fn failed(error: impl std::fmt::Display) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(error.to_string())
}

#[interface(name = "org.freedesktop.portal.FileChooser")]
impl FileChooser {
    /// Answers at once with the next answer queued (a cancel when none is), as the `Response` of the request object
    /// the caller's token names.
    async fn open_file(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        parent_window: &str,
        title: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let _ = parent_window;
        let sender = sender(&header);
        let token = match options.get("handle_token") {
            Some(Value::Str(token)) => token.as_str().to_owned(),
            _ => "t".to_owned(),
        };
        let path = format!(
            "{PORTAL_PATH}/request/{}/{token}",
            sender.trim_start_matches(':').replace('.', "_")
        );
        let accept_label = match options.get("accept_label") {
            Some(Value::Str(label)) => Some(label.as_str().to_owned()),
            _ => None,
        };
        let answer = lock(&self.state).answers.pop_front().unwrap_or(Answer::Cancel);
        let (code, uris, answered) = match &answer {
            Answer::Pick(file) => {
                let uri = format!("file://{}", file.display());
                (0_u32, vec![uri], file.display().to_string())
            }
            Answer::Cancel => (1, Vec::new(), "cancel".to_owned()),
        };
        lock(&self.state).asked.push(Asked {
            title: title.to_owned(),
            accept_label,
            filters: filters_of(&options),
            current_filter: options.get("current_filter").and_then(filter_of),
            answered,
        });
        let mut results: HashMap<&str, Value<'_>> = HashMap::new();
        if code == 0 {
            results.insert("uris", Value::from(uris));
        }
        connection
            .emit_signal(
                Some(sender.as_str()),
                path.as_str(),
                REQUEST_INTERFACE,
                "Response",
                &(code, results),
            )
            .await?;
        OwnedObjectPath::try_from(path).map_err(failed)
    }

    #[zbus(property, name = "version")]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn version(&self) -> u32 {
        3
    }
}

// --- the synthetic pictures ---

/// An RGB colour.
pub type Rgb = [u8; 3];

/// The profile picture of each mock account (the same for each one).
pub const PROFILE: Rgb = [0, 128, 128];
pub const PNG: Rgb = [230, 0, 230];
pub const JPEG: Rgb = [0, 200, 0];
pub const GIF: Rgb = [250, 250, 0];
/// The second frame of the GIF: it never shows.
pub const GIF_LATER: Rgb = [0, 250, 250];
pub const WEBP: Rgb = [255, 90, 0];
/// PNG content in a file named `.gif`.
pub const RENAMED: Rgb = [130, 70, 0];
/// The centre square of the wide picture; its sides are white.
pub const WIDE: Rgb = [0, 100, 0];
/// The centre square of the very large picture; its sides are white.
pub const LARGE: Rgb = [128, 128, 0];
pub const WHITE: Rgb = [255, 255, 255];

/// The synthetic files the user picks, in a directory of their own.
pub struct Files {
    pub dir: tempfile::TempDir,
}

/// The good files: their name and the colour the avatar must show.
pub const GOOD: [(&str, Rgb); 7] = [
    ("red-and-pink.png", PNG),
    ("green.jpeg", JPEG),
    ("yellow.gif", GIF),
    ("orange.webp", WEBP),
    ("brown-png-named.gif", RENAMED),
    ("wide-1200x400.png", WIDE),
    ("large-5000x3000.png", LARGE),
];

/// The files the app must refuse: their name and the words its reason must hold.
pub const BAD: [(&str, &str); 4] = [
    ("empty.png", "can't be used"),
    ("broken.png", "can't be used"),
    ("notes.txt", "can't be used"),
    ("huge-65mb.png", "too large"),
];

impl Files {
    /// Writes each good and bad file.
    pub fn make() -> SandboxResult<Self> {
        let dir = tempfile::tempdir()?;
        let at = |name: &str| dir.path().join(name);
        square(PNG).save_with_format(at(GOOD[0].0), image::ImageFormat::Png)?;
        image::DynamicImage::ImageRgba8(square(JPEG))
            .to_rgb8()
            .save_with_format(at(GOOD[1].0), image::ImageFormat::Jpeg)?;
        write_gif(&at(GOOD[2].0))?;
        image::DynamicImage::ImageRgba8(square(WEBP))
            .to_rgb8()
            .save_with_format(at(GOOD[3].0), image::ImageFormat::WebP)?;
        square(RENAMED).save_with_format(at(GOOD[4].0), image::ImageFormat::Png)?;
        centred(1200, 400, WIDE).save_with_format(at(GOOD[5].0), image::ImageFormat::Png)?;
        centred(5000, 3000, LARGE).save_with_format(at(GOOD[6].0), image::ImageFormat::Png)?;
        std::fs::write(at(BAD[0].0), b"")?;
        let mut broken = b"\x89PNG\r\n\x1a\n".to_vec();
        broken.extend_from_slice(&[0x42; 64]);
        std::fs::write(at(BAD[1].0), broken)?;
        std::fs::write(at(BAD[2].0), "A made-up note, not a picture.\n")?;
        std::fs::write(at(BAD[3].0), vec![0_u8; 65 * 1024 * 1024])?;
        Ok(Self { dir })
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

/// A 256 px square of one colour.
fn square(colour: Rgb) -> RgbaImage {
    let [r, g, b] = colour;
    RgbaImage::from_pixel(256, 256, Rgba([r, g, b, 255]))
}

/// A `width` × `height` picture: its centre square `colour`, the rest white.
fn centred(width: u32, height: u32, colour: Rgb) -> RgbaImage {
    let side = width.min(height);
    let (left, top) = ((width - side) / 2, (height - side) / 2);
    let [r, g, b] = colour;
    RgbaImage::from_fn(width, height, |x, y| {
        if (left..left + side).contains(&x) && (top..top + side).contains(&y) {
            Rgba([r, g, b, 255])
        } else {
            Rgba([255, 255, 255, 255])
        }
    })
}

/// A GIF of two frames: [`GIF`], then [`GIF_LATER`].
fn write_gif(path: &Path) -> SandboxResult {
    let file = std::fs::File::create(path)?;
    let mut encoder = image::codecs::gif::GifEncoder::new(file);
    let delay = Delay::from_numer_denom_ms(500, 1);
    encoder.encode_frames([
        Frame::from_parts(square(GIF), 0, 0, delay),
        Frame::from_parts(square(GIF_LATER), 0, 0, delay),
    ])?;
    Ok(())
}

/// The PNG of the profile picture the mock gives each account (96 px, as Graph's `96x96`).
pub fn profile_png() -> SandboxResult<Vec<u8>> {
    let [r, g, b] = PROFILE;
    let picture = RgbaImage::from_pixel(96, 96, Rgba([r, g, b, 255]));
    let mut png = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut png, image::ImageFormat::Png)?;
    Ok(png.into_inner())
}
