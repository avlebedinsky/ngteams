//! A call's video (M3 Tasks 18, 19): a 1:1 call's far end's picture, a group call's participants' (one per receiving
//! video line), the far end's shared screen (its sharing line's), and the camera's own (the self view), as tiles of
//! the call window's stage
//! ([`crate::call_stage::CallStage`]).
//!
//! - **The feed** ([`VideoFeed`]) is what the engine's call media shows its frames on ([`teams_core::VideoOutput`]):
//!   the media's own threads hand each decoded frame (the far end's) and each frame the camera sends (the self view)
//!   to it; it turns each into the premultiplied BGRA a window draws, keeps the newest of each, and wakes the window.
//!   It also carries the settings' video switch ([`VideoFeed::set_enabled`]): while it is off, a call's media starts
//!   no video and the codec isn't used; and the camera's background the settings choose ([`VideoFeed::set_background`],
//!   applied by the media to each frame it sends). [`follow_settings`] keeps both as the settings say.
//! - **The tiles** ([`Tiles`]) draw the newest frame through the platform video adapter (persistent external
//!   textures on Linux, bounded BGRA image uploads on Windows). The trace's `video-tile
//!   presented` (Linux GPU completion) or `submitted` (Windows image paint) line names the tile (`remote`, `line-1` … `line-9`, `share`, `self`) and how many it presented so
//!   far, at the first frame and every [`TRACE_EVERY`] after it; `video-tile shown`/`hidden` as a tile comes and
//!   goes. A tile whose last frame is older than [`STALE`] is hidden (the far end's camera went off).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::video_surface::{self, Frame as PixelFrame, Surface as PixelSurface};
use gpui::{AnyElement, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, canvas, div};
use teams_media::devices::{DeviceError, VideoFrame, VideoSink};
use teams_media::session::SHARING_LINE;
use teams_media::video::convert::i420_to_bgra;
use teams_media::video::effects::{Background, BackgroundChoice};

use crate::settings::SettingsModel;
use crate::trace::{TraceLine, ViewTrace};
use crate::video_player::fitted;

/// How long a tile stays after its last frame.
pub const STALE: Duration = Duration::from_secs(2);
/// A tile's presented frames are traced at the first and every this many after it.
pub const TRACE_EVERY: u64 = 30;
/// The video lines a call shows: line 0 (a 1:1 call's far end) and a group call's receiving lines.
pub const LINES: usize = 1 + teams_core::GROUP_SLOTS;
/// Every tile: each line's, the share, and the self view.
const TILES: usize = LINES + 2;
/// The trace's names of the lines' tiles.
const LINE_NAMES: [&str; LINES] = [
    "remote", "line-1", "line-2", "line-3", "line-4", "line-5", "line-6", "line-7", "line-8", "line-9",
];

/// Which tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tile {
    /// What a video line brings: 0 a 1:1 call's far end, 1, 2, … a group call's participants.
    Line(usize),
    /// The screen the far end shares, on its sharing line.
    Share,
    /// The camera's own, as sent.
    Own,
}

impl Tile {
    /// A 1:1 call's far end.
    pub const REMOTE: Self = Self::Line(0);

    /// Every tile.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..LINES).map(Self::Line).chain([Self::Share, Self::Own])
    }

    /// The trace's name for it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Line(line) => LINE_NAMES.get(line).copied().unwrap_or("line"),
            Self::Share => "share",
            Self::Own => "self",
        }
    }

    /// Its place among the tiles; `None` for a line past the last.
    fn index(self) -> Option<usize> {
        match self {
            Self::Line(line) => (line < LINES).then_some(line),
            Self::Share => Some(LINES),
            Self::Own => Some(LINES + 1),
        }
    }
}

/// One frame as the window draws it.
#[derive(Debug)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// Premultiplied BGRA, opaque, rows `width * 4` bytes apart.
    pub bgra: Arc<Vec<u8>>,
    /// Counts the tile's frames, from 1.
    pub serial: u64,
    pub at: Instant,
}

/// What the media's threads and the window share.
struct Shared {
    enabled: AtomicBool,
    background: BackgroundChoice,
    pictures: Vec<Mutex<Option<Arc<Picture>>>>,
    serials: Vec<AtomicU64>,
    wake: async_channel::Sender<()>,
    woken: async_channel::Receiver<()>,
}

/// The call media's video, as the window shows it (see the module docs). Cloned for each sink.
#[derive(Clone)]
pub struct VideoFeed {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for VideoFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFeed")
            .field("enabled", &self.enabled())
            .finish_non_exhaustive()
    }
}

impl Default for VideoFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoFeed {
    /// A feed with no frame yet, its codec switched on.
    #[must_use]
    pub fn new() -> Self {
        // one wake waiting is enough: the window draws the newest frames whenever it wakes
        let (wake, woken) = async_channel::bounded(1);
        Self {
            shared: Arc::new(Shared {
                enabled: AtomicBool::new(true),
                background: BackgroundChoice::default(),
                pictures: (0..TILES).map(|_| Mutex::new(None)).collect(),
                serials: (0..TILES).map(|_| AtomicU64::new(0)).collect(),
                wake,
                woken,
            }),
        }
    }

    /// Switches the calls' video on or off from now on (the settings' switch).
    pub fn set_enabled(&self, on: bool) {
        self.shared.enabled.store(on, Ordering::Relaxed);
    }

    /// Whether the calls may use the video codec.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.shared.enabled.load(Ordering::Relaxed)
    }

    /// Sends the camera with `background` from its next frame on (the settings' video effects), a call's camera on
    /// now included. It never turns a camera on.
    pub fn set_background(&self, background: Background) {
        self.shared.background.set(background);
    }

    /// The camera's background as chosen now.
    #[must_use]
    pub fn background(&self) -> Background {
        self.shared.background.get()
    }

    /// The newest frame of `tile`, unless it is older than [`STALE`].
    #[must_use]
    pub fn picture(&self, tile: Tile) -> Option<Arc<Picture>> {
        let picture = lock(&self.shared.pictures[tile.index()?]).clone()?;
        (picture.at.elapsed() < STALE).then_some(picture)
    }

    /// Forgets every frame (the call ended).
    pub fn clear(&self) {
        for picture in &self.shared.pictures {
            *lock(picture) = None;
        }
    }

    /// What wakes the window: one message after a new frame (or a few).
    #[must_use]
    pub fn woken(&self) -> async_channel::Receiver<()> {
        self.shared.woken.clone()
    }

    /// Wakes the window without a frame (the preview stopped: its picture went).
    pub(crate) fn wake(&self) {
        // full means a wake is already waiting
        let _ = self.shared.wake.try_send(());
    }

    /// Takes `frame` (I420) as `tile`'s newest.
    fn take(&self, tile: Tile, frame: &VideoFrame) -> Result<(), DeviceError> {
        // a line past the last has no tile: its frames are dropped
        let Some(index) = tile.index() else { return Ok(()) };
        let bgra = i420_to_bgra(frame).ok_or_else(|| DeviceError::Refused("a frame not in I420".into()))?;
        let serial = self.shared.serials[index].fetch_add(1, Ordering::Relaxed) + 1;
        *lock(&self.shared.pictures[index]) = Some(Arc::new(Picture {
            width: frame.width,
            height: frame.height,
            bgra: Arc::new(bgra),
            serial,
            at: Instant::now(),
        }));
        // full means a wake is already waiting
        let _ = self.shared.wake.try_send(());
        Ok(())
    }
}

impl teams_core::VideoOutput for VideoFeed {
    fn remote(&self, line: usize) -> Box<dyn VideoSink> {
        let tile = if line == SHARING_LINE {
            Tile::Share
        } else {
            Tile::Line(line)
        };
        Box::new(Sink {
            feed: self.clone(),
            tile,
        })
    }

    fn preview(&self) -> Option<Box<dyn VideoSink>> {
        Some(Box::new(Sink {
            feed: self.clone(),
            tile: Tile::Own,
        }))
    }

    fn enabled(&self) -> bool {
        VideoFeed::enabled(self)
    }

    fn background(&self) -> BackgroundChoice {
        self.shared.background.clone()
    }
}

/// Sets `feed`'s video switch and the camera's background as `settings` say (an app calls it at the start and on
/// every change of its settings).
pub fn follow_settings(feed: &VideoFeed, settings: &SettingsModel) {
    feed.set_enabled(settings.settings().video_calls);
    feed.set_background(settings.background());
}

/// A sink of one tile's frames, on the media's threads.
struct Sink {
    feed: VideoFeed,
    tile: Tile,
}

impl VideoSink for Sink {
    fn show(&mut self, frame: &VideoFrame) -> Result<(), DeviceError> {
        self.feed.take(self.tile, frame)
    }
}

/// One tile's drawing state: its surface (the renderer's texture), the frame drawn, and what it presented.
struct TileState {
    surface: Arc<PixelSurface>,
    drawn: Option<(u64, Arc<PixelFrame>)>,
    presented: Arc<AtomicU64>,
    shown: bool,
}

impl TileState {
    fn new() -> Self {
        Self {
            surface: PixelSurface::new(),
            drawn: None,
            presented: Arc::new(AtomicU64::new(0)),
            shown: false,
        }
    }
}

/// The call window's video tiles (see the module docs).
pub struct Tiles {
    feed: VideoFeed,
    trace: Arc<ViewTrace>,
    states: Vec<TileState>,
}

impl Tiles {
    #[must_use]
    pub fn new(feed: VideoFeed, trace: Arc<ViewTrace>) -> Self {
        Self {
            feed,
            trace,
            states: (0..TILES).map(|_| TileState::new()).collect(),
        }
    }

    /// The feed the tiles draw.
    #[must_use]
    pub fn feed(&self) -> &VideoFeed {
        &self.feed
    }

    /// `tile` of call `call`, when it has a fresh frame, filling the box it is put in (the picture fitted inside it,
    /// its shape kept); traces it when it comes or goes.
    pub fn fill(&mut self, tile: Tile, call: u64) -> Option<AnyElement> {
        let (picture, frame) = self.frame(tile, call)?;
        Some(fill_element(tile, &picture, frame))
    }

    /// The newest picture of `tile` and the frame that draws it, when it is fresh; traces the tile coming or going.
    fn frame(&mut self, tile: Tile, call: u64) -> Option<(Arc<Picture>, Arc<PixelFrame>)> {
        let picture = self.feed.picture(tile);
        let state = &mut self.states[tile.index()?];
        if picture.is_some() != state.shown {
            state.shown = picture.is_some();
            let change = if state.shown { "shown" } else { "hidden" };
            self.trace
                .record(&TraceLine::new("video-tile", change).kind(tile.name()).call(call));
        }
        let Some(picture) = picture else {
            state.drawn = None;
            return None;
        };
        let frame = frame_of(state, &picture, tile, call, &self.trace);
        Some((picture, frame))
    }

    /// Forgets what was drawn and the frames (the call ended); traces the tiles that go.
    pub fn clear(&mut self, call: u64) {
        self.feed.clear();
        for tile in Tile::all() {
            let Some(index) = tile.index() else { continue };
            let state = &mut self.states[index];
            state.drawn = None;
            if state.shown {
                state.shown = false;
                self.trace
                    .record(&TraceLine::new("video-tile", "hidden").kind(tile.name()).call(call));
            }
        }
    }
}

/// The frame `state` draws for `picture`: the one made for it already, or a new one whose presentation is counted.
fn frame_of(
    state: &mut TileState,
    picture: &Picture,
    tile: Tile,
    call: u64,
    trace: &Arc<ViewTrace>,
) -> Arc<PixelFrame> {
    if let Some((serial, frame)) = &state.drawn
        && *serial == picture.serial
    {
        return Arc::clone(frame);
    }
    let presented = Arc::clone(&state.presented);
    let trace = Arc::clone(trace);
    let completion = move |outcome: video_surface::Outcome| {
        let count = presented.fetch_add(1, Ordering::Relaxed) + 1;
        if count == 1 || count.is_multiple_of(TRACE_EVERY) {
            trace.record(
                &TraceLine::new("video-tile", outcome.change())
                    .kind(tile.name())
                    .call(call)
                    .frames(count),
            );
        }
    };
    let frame = video_surface::frame(
        &state.surface,
        picture.serial,
        (picture.width, picture.height),
        &picture.bgra,
        completion,
    );
    state.drawn = Some((picture.serial, Arc::clone(&frame)));
    frame
}

/// A tile filling its box: `frame` fitted inside it, centred, its shape kept.
fn fill_element(tile: Tile, picture: &Picture, frame: Arc<PixelFrame>) -> AnyElement {
    let (width, height) = (picture.width, picture.height);
    let name = tile.name();
    div()
        .debug_selector(move || format!("video-tile-{name}"))
        .size_full()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, cx| {
                    video_surface::paint(&frame, fitted(bounds, width, height), window, cx);
                },
            )
            .size_full(),
        )
        .into_any_element()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
