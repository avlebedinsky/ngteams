//! What a message draws besides its text, as Teams draws it: the pictures pasted into it and its emoticons
//! (`rich_text` lays them out, this module draws them). Their bytes come only from the engine ([`Core`]), never from a
//! request of a view's own.
//!
//! - **A picture** ([`picture`]) is the view of an AMS object the message names, or a GIF on Giphy's media host (one
//!   picked from Teams' GIF picker, playing as Teams plays it): asked of the engine once a run per account and `src`
//!   ([`Core::message_picture`]), decoded here off the foreground (every frame scaled down to at most
//!   [`MAX_DECODED_SIDE`] on its longer side; a GIF with its frames up to the first that doesn't decode, at most
//!   [`MAX_GIF_FRAMES`] and [`MAX_GIF_BYTES`] of them, its canvas at most [`MAX_GIF_SIDE`]) and kept for the run. It is
//!   drawn at the size the message gives it (else its own), scaled down to the width there is and keeping its aspect
//!   ratio, as Teams draws a picture in its bubble; a box of that size stands in while it is read. One the engine has
//!   none of, or none can be asked of (no engine), is drawn as the message's fallback (its placeholder text).
//! - **An emoticon** ([`emoticon`]) is drawn from Teams' own animation of it (its CDN's strip of frames, read by the
//!   engine as a reaction's is, [`reactions::animation`]): playing when `animated` (an emoticon-only message's, drawn
//!   large), else standing on its first frame (one inside text, at the text's height, as Teams draws it until hovered).
//!   Until it arrives, or when there is none, the reaction pack's picture of it stands in, else its emoji as text.
//! - **A file's thumbnail** ([`thumbnail`]) is asked of the engine by the file's sharing link
//!   ([`Core::file_thumbnail`]) and decoded and kept as a picture is; `crate::files` draws it on the file's card.
//! - **Inline** ([`InlineText`]): an emoticon inside a paragraph keeps the place of a few spaces in the text, and is
//!   drawn over them where the text's layout put them, so it flows and wraps with the text.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    InteractiveElement, IntoElement, LayoutId, ParentElement as _, Pixels, RenderImage, RenderOnce, Size, Style,
    Styled as _, TextLayout, Window, div, img, px, size,
};
use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder as _, Frame, ImageDecoder as _, Limits, RgbaImage, imageops};
use teams_core::{PictureBytes, Reply, Strip};
use teams_proto::PastedFormat;

use crate::bridge::Core;
use crate::reactions;

/// The longer side a decoded picture is scaled down to, at most: no message is drawn wider.
pub const MAX_DECODED_SIDE: u32 = 2048;

/// The most frames of a GIF kept: the frames past it are left out, the animation looping over those before.
pub const MAX_GIF_FRAMES: usize = 300;

/// The most bytes a GIF's frames take once decoded and scaled down: the frames past it are left out (never the first,
/// which [`MAX_DECODED_SIDE`] keeps well below it). Every picture is kept for the run.
pub const MAX_GIF_BYTES: usize = 128 << 20;

/// The longer side a GIF's canvas may have to be decoded at all: each of its frames is composed at the canvas's size
/// before it is scaled down. A larger one is drawn as the message's fallback.
pub const MAX_GIF_SIDE: u32 = 4096;

/// The most the decoding of one GIF frame may allocate: the canvas kept between frames, the frame and its composite,
/// each RGBA at most [`MAX_GIF_SIDE`] square. A frame past it (one larger than the canvas) ends the frames.
const MAX_GIF_FRAME_ALLOC: u64 = 3 * 4 * (MAX_GIF_SIDE as u64) * (MAX_GIF_SIDE as u64);

/// The side an emoticon-only message's emoticons are drawn at (Teams' `animated-emoticon-58`).
pub const LARGE_EMOTICON: Pixels = px(58.0);

/// How long a picture whose read failed on the way is drawn as its fallback before it is asked for again.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// The size a picture is held at while it is read, when the message gives none.
const UNKNOWN_SIZE: (u32, u32) = (320, 180);

/// The part of the engine's request side a message's pictures and emoticons are asked of ([`Core`]'s, through
/// [`PictureSource::new`]; a view test's own fake through [`PictureSource::of`]).
pub trait PictureEngine: Send + Sync {
    /// The account the message is shown to: a picture is read with its token.
    fn me(&self) -> &str;
    /// As [`Core::message_picture`].
    fn message_picture(&self, src: String) -> Reply<Option<PictureBytes>>;
    /// As [`Core::file_thumbnail`]; `None` (there is none) unless the engine reads thumbnails.
    fn file_thumbnail(&self, share_url: String) -> Reply<Option<PictureBytes>> {
        let _ = share_url;
        Reply::ready(Ok(None))
    }
    /// As [`Core::reaction_animation`].
    fn reaction_animation(&self, id: String) -> Reply<Option<Strip>>;
    /// Which engine it is, for telling two sources apart.
    fn engine(&self) -> *const ();
}

/// An engine's request side as a [`PictureEngine`].
struct CoreEngine(Arc<dyn Core>);

impl PictureEngine for CoreEngine {
    fn me(&self) -> &str {
        self.0.me()
    }

    fn message_picture(&self, src: String) -> Reply<Option<PictureBytes>> {
        self.0.message_picture(src)
    }

    fn file_thumbnail(&self, share_url: String) -> Reply<Option<PictureBytes>> {
        self.0.file_thumbnail(share_url)
    }

    fn reaction_animation(&self, id: String) -> Reply<Option<Strip>> {
        self.0.reaction_animation(id)
    }

    fn engine(&self) -> *const () {
        Arc::as_ptr(&self.0).cast()
    }
}

/// Where a message's pictures and emoticons are asked for: the engine of the account the message is shown to.
#[derive(Clone)]
pub struct PictureSource(Arc<dyn PictureEngine>);

impl PictureSource {
    /// The engine `core` is the request side of.
    #[must_use]
    pub fn new(core: Arc<dyn Core>) -> Self {
        Self(Arc::new(CoreEngine(core)))
    }

    /// Any [`PictureEngine`] (a view test's fake).
    #[must_use]
    pub fn of(engine: Arc<dyn PictureEngine>) -> Self {
        Self(engine)
    }

    /// The engine it names.
    #[must_use]
    pub fn engine(&self) -> &dyn PictureEngine {
        self.0.as_ref()
    }
}

impl fmt::Debug for PictureSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PictureSource").field(&self.0.me()).finish()
    }
}

/// Two sources are the same when they are the same engine's.
impl PartialEq for PictureSource {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0.engine(), other.0.engine())
    }
}

impl Eq for PictureSource {}

/// What is known of one picture this run.
enum Known {
    /// Asked; its answer hasn't come.
    Asked,
    /// Decoded, with the size its bytes have.
    Decoded(Arc<RenderImage>, (u32, u32)),
    /// The engine has none, or its bytes can't be decoded.
    Missing,
    /// Its read failed on the way, then: asked again after [`RETRY_AFTER`].
    Failed(Instant),
}

/// What names a picture asked of the engine: a message's `<img>` by its `src`, or a file's thumbnail by its sharing
/// link.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Named {
    Src(String),
    Thumbnail(String),
}

/// The messages' pictures and files' thumbnails this run, by the account they are shown to and what names them.
#[derive(Default)]
struct Pictures {
    known: HashMap<(String, Named), Known>,
}

impl gpui::Global for Pictures {}

/// How a picture stands.
#[derive(Clone)]
pub enum PictureState {
    /// Being read.
    Reading,
    /// Read: its frames and the size its bytes have.
    Shown(Arc<RenderImage>, (u32, u32)),
    /// None to show.
    Missing,
}

/// The picture `src` names as `source`'s engine has it; asks for it once when it wasn't (the windows drawn again once it
/// is decoded).
pub fn picture(src: &str, source: &PictureSource, cx: &mut App) -> PictureState {
    asked(Named::Src(src.to_owned()), source, cx)
}

/// The thumbnail of the file `share_url` is the sharing link of, as `source`'s engine has it; asked for once, as a
/// picture is ([`picture`]).
pub fn thumbnail(share_url: &str, source: &PictureSource, cx: &mut App) -> PictureState {
    asked(Named::Thumbnail(share_url.to_owned()), source, cx)
}

/// The picture `named` names as `source`'s engine has it; asks for it once when it wasn't (the windows drawn again
/// once it is decoded).
fn asked(named: Named, source: &PictureSource, cx: &mut App) -> PictureState {
    let key = (source.engine().me().to_owned(), named);
    let pictures = cx.default_global::<Pictures>();
    match pictures.known.get(&key) {
        Some(Known::Asked) => return PictureState::Reading,
        Some(Known::Decoded(image, natural)) => return PictureState::Shown(Arc::clone(image), *natural),
        Some(Known::Missing) => return PictureState::Missing,
        Some(Known::Failed(at)) if at.elapsed() < RETRY_AFTER => return PictureState::Missing,
        Some(Known::Failed(_)) | None => {}
    }
    pictures.known.insert(key.clone(), Known::Asked);
    let reply = match &key.1 {
        Named::Src(src) => source.engine().message_picture(src.clone()),
        Named::Thumbnail(share_url) => source.engine().file_thumbnail(share_url.clone()),
    };
    cx.spawn(async move |cx| {
        let known = match reply.await {
            Ok(Some(bytes)) => cx
                .background_executor()
                .spawn(async move { decode(&bytes) })
                .await
                .map_or(Known::Missing, |(image, natural)| {
                    Known::Decoded(Arc::new(image), natural)
                }),
            Ok(None) => Known::Missing,
            Err(_) => Known::Failed(Instant::now()),
        };
        cx.update(|cx| {
            cx.default_global::<Pictures>().known.insert(key, known);
            cx.refresh_windows();
        });
    })
    .detach();
    PictureState::Reading
}

/// A picture's bytes as the frames GPUI draws (BGRA), and the size the bytes have; `None` when they don't decode.
#[must_use]
pub fn decode(picture: &PictureBytes) -> Option<(RenderImage, (u32, u32))> {
    let format = match picture.format {
        PastedFormat::Gif => return decode_gif(&picture.bytes),
        PastedFormat::Png => image::ImageFormat::Png,
        PastedFormat::Jpeg => image::ImageFormat::Jpeg,
        PastedFormat::Webp => image::ImageFormat::WebP,
    };
    let decoded = image::load_from_memory_with_format(&picture.bytes, format)
        .ok()?
        .into_rgba8();
    let natural = decoded.dimensions();
    let mut shown = scaled_down(decoded);
    to_bgra(&mut shown);
    Some((RenderImage::new(vec![Frame::new(shown)]), natural))
}

/// A GIF's frames, each scaled down as a still picture is, up to the first that doesn't decode (a truncated file's
/// decoder keeps failing rather than ending) and within [`MAX_GIF_FRAMES`] and [`MAX_GIF_BYTES`]; `None` when not even
/// the first decodes or its canvas is larger than [`MAX_GIF_SIDE`].
fn decode_gif(bytes: &[u8]) -> Option<(RenderImage, (u32, u32))> {
    let mut decoder = GifDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_GIF_SIDE);
    limits.max_image_height = Some(MAX_GIF_SIDE);
    limits.max_alloc = Some(MAX_GIF_FRAME_ALLOC);
    decoder.set_limits(limits).ok()?;
    let natural = decoder.dimensions();
    let mut frames = Vec::new();
    let mut kept = 0_usize;
    for frame in decoder.into_frames().take(MAX_GIF_FRAMES).map_while(Result::ok) {
        let (left, top, delay) = (frame.left(), frame.top(), frame.delay());
        let mut shown = scaled_down(frame.into_buffer());
        let size = shown.as_raw().len();
        if !frames.is_empty() && kept.saturating_add(size) > MAX_GIF_BYTES {
            break;
        }
        kept = kept.saturating_add(size);
        to_bgra(&mut shown);
        frames.push(Frame::from_parts(shown, left, top, delay));
    }
    (!frames.is_empty()).then(|| (RenderImage::new(frames), natural))
}

/// `image` scaled down to at most [`MAX_DECODED_SIDE`] on its longer side, keeping its aspect ratio.
fn scaled_down(image: RgbaImage) -> RgbaImage {
    let (width, height) = image.dimensions();
    let longer = width.max(height);
    if longer <= MAX_DECODED_SIDE {
        return image;
    }
    let scale = |side: u32| {
        u32::try_from(u64::from(side) * u64::from(MAX_DECODED_SIDE) / u64::from(longer))
            .unwrap_or(MAX_DECODED_SIDE)
            .max(1)
    };
    imageops::resize(&image, scale(width), scale(height), imageops::FilterType::Triangle)
}

/// GPUI draws BGRA.
fn to_bgra(image: &mut RgbaImage) {
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
}

/// A picture of a message, drawn as the module docs say. `given` is the size the message gives it.
pub struct Picture {
    id: ElementId,
    src: String,
    given: Option<(u32, u32)>,
    source: Option<PictureSource>,
    /// Drawn while it is read.
    reading: Hsla,
    /// Drawn when there is none.
    fallback: Option<AnyElement>,
    child: Option<AnyElement>,
}

impl Picture {
    #[must_use]
    pub fn new(
        id: impl Into<ElementId>,
        src: &str,
        given: Option<(u32, u32)>,
        source: Option<PictureSource>,
        reading: Hsla,
        fallback: AnyElement,
    ) -> Self {
        Self {
            id: id.into(),
            src: src.to_owned(),
            given,
            source,
            reading,
            fallback: Some(fallback),
            child: None,
        }
    }
}

/// How a picture was laid out.
pub enum Laid {
    /// As its fallback, laid out as a child.
    Fallback,
    /// Measured: its frames, or `None` while it is read.
    Measured(Option<Arc<RenderImage>>),
}

impl IntoElement for Picture {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Picture {
    type RequestLayoutState = Laid;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Laid) {
        let state = match &self.source {
            Some(source) => picture(&self.src, source, cx),
            None => PictureState::Missing,
        };
        let (image, natural) = match state {
            PictureState::Missing => {
                let mut fallback = self.fallback.take().unwrap_or_else(|| div().into_any_element());
                let layout = fallback.request_layout(window, cx);
                self.child = Some(fallback);
                return (layout, Laid::Fallback);
            }
            PictureState::Reading => (None, self.given.unwrap_or(UNKNOWN_SIZE)),
            PictureState::Shown(image, own) => (Some(image), self.given.unwrap_or(own)),
        };
        let layout = window.request_measured_layout(Style::default(), move |known, available, _, _| {
            fitted(natural, known, available)
        });
        (layout, Laid::Measured(image))
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        laid: &mut Laid,
        window: &mut Window,
        cx: &mut App,
    ) {
        match laid {
            Laid::Fallback => {
                if let Some(child) = &mut self.child {
                    child.prepaint(window, cx);
                }
            }
            Laid::Measured(image) => {
                let shown = match image {
                    // an id of its own: GPUI keeps a GIF's frame in the element's state
                    Some(image) => InteractiveElement::id(img(Arc::clone(image)), self.id.clone())
                        .size_full()
                        .rounded(px(4.0))
                        .into_any_element(),
                    None => div().size_full().rounded(px(4.0)).bg(self.reading).into_any_element(),
                };
                let mut sized = div()
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .child(shown)
                    .into_any_element();
                sized.prepaint_as_root(bounds.origin, bounds.size.map(AvailableSpace::Definite), window, cx);
                self.child = Some(sized);
            }
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _laid: &mut Laid,
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = &mut self.child {
            child.paint(window, cx);
        }
    }
}

/// The size a picture of `natural` size is drawn at: its own, scaled down to the width there is (`known`'s, else a
/// definite `available`), its aspect ratio kept; under a min-content constraint none (it shrinks with its bubble, as
/// an image of `max-width: 100%` does).
#[must_use]
pub fn fitted(natural: (u32, u32), known: Size<Option<Pixels>>, available: Size<AvailableSpace>) -> Size<Pixels> {
    #[expect(clippy::cast_precision_loss, reason = "a picture's side is at most 100 000 pixels")]
    let (width, height) = (natural.0.max(1) as f32, natural.1.max(1) as f32);
    let room = known.width.or(match available.width {
        AvailableSpace::Definite(room) => Some(room),
        AvailableSpace::MaxContent => None,
        AvailableSpace::MinContent => Some(px(0.0)),
    });
    let shown = room.map_or(width, |room| width.min(f32::from(room).max(0.0)));
    size(px(shown), px(shown * height / width))
}

/// An emoticon of a message, drawn as the module docs say.
#[derive(IntoElement)]
pub struct Emoticon {
    id: String,
    alt: Option<String>,
    side: Pixels,
    animated: bool,
    source: Option<PictureSource>,
}

impl Emoticon {
    #[must_use]
    pub fn new(id: &str, alt: Option<&str>, side: Pixels, animated: bool, source: Option<PictureSource>) -> Self {
        Self {
            id: id.to_owned(),
            alt: alt.map(str::to_owned),
            side,
            animated,
            source,
        }
    }
}

impl RenderOnce for Emoticon {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let frames = self
            .source
            .as_ref()
            .and_then(|source| reactions::animation_of(&self.id, source.engine(), cx));
        match frames {
            // with an id GPUI plays it (the frame it is on kept in the element's state); without one it stands on its
            // first frame
            Some(frames) if self.animated => {
                InteractiveElement::id(img(frames), ElementId::Name(format!("emoticon-{}", self.id).into()))
                    .size(self.side)
                    .flex_none()
                    .into_any_element()
            }
            Some(frames) => img(frames).size(self.side).flex_none().into_any_element(),
            None if reactions::catalog().get(&self.id).is_some() => reactions::emoji(&self.id, self.side),
            None => div()
                .flex_none()
                .line_height(self.side)
                .text_size(self.side * 0.8)
                .child(self.alt.unwrap_or_default())
                .into_any_element(),
        }
    }
}

/// A paragraph's text with emoticons drawn over the places it keeps for them (see the module docs): `text` is the
/// paragraph's element, `layout` its text's layout, and each emoticon the byte range of its place in the text.
pub struct InlineText {
    text: AnyElement,
    layout: TextLayout,
    emoticons: Vec<(Range<usize>, AnyElement)>,
}

impl InlineText {
    #[must_use]
    pub fn new(text: AnyElement, layout: TextLayout, emoticons: Vec<(Range<usize>, AnyElement)>) -> Self {
        Self {
            text,
            layout,
            emoticons,
        }
    }
}

impl IntoElement for InlineText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InlineText {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let text = self.text.request_layout(window, cx);
        let style = Style {
            size: Size {
                width: gpui::relative(1.0).into(),
                height: gpui::Length::Auto,
            },
            ..Style::default()
        };
        (window.request_layout(style, [text], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text.prepaint(window, cx);
        let line_height = self.layout.line_height();
        for (place, emoticon) in &mut self.emoticons {
            let Some(start) = self.layout.position_for_index(place.start) else {
                continue;
            };
            // the place's width on its line (a place the line wraps in takes the line height)
            let end = self
                .layout
                .position_for_index(place.end)
                .filter(|end| end.y == start.y && end.x > start.x);
            let width = end.map_or(line_height, |end| end.x - start.x);
            let side = line_height;
            let origin = gpui::point(start.x + (width - side) / 2.0, start.y);
            emoticon.prepaint_as_root(origin, size(side, side).map(AvailableSpace::Definite), window, cx);
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text.paint(window, cx);
        for (_, emoticon) in &mut self.emoticons {
            emoticon.paint(window, cx);
        }
    }
}
