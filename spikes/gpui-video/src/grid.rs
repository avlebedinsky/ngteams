//! A 3×3 grid of 720p streams in GPUI, by either of the spike's two upload paths ([`Upload`]).
//!
//! A [`Source`] runs one thread per stream (`video-src-<n>`): every 1/fps it makes the stream's next NV12 frame, as
//! a decoder would hand it over, converts it to BGRA on the CPU (neither path takes NV12) and sends it to the
//! foreground. The [`Grid`] view keeps the newest frame of each tile:
//!
//! - [`Upload::Image`], approach (a), stock GPUI: the frame becomes a fresh `Arc<RenderImage>` and the one it replaces
//!   is dropped from GPUI's sprite atlas. GPUI uploads a `RenderImage` into its atlas the first time it's painted; a
//!   1280×720 tile takes a 1280×1024 atlas texture of its own, so every new frame is a texture allocation, a copy of
//!   the pixels into the upload queue and a 3.5 MiB `write_texture`, and the replaced texture is freed.
//! - [`Upload::Surface`], approach (b), GPUI with agterm-rs's external-surfaces patch: the frame becomes a
//!   `PixelFrame` of the tile's `PixelSurface`, painted with `Window::paint_external_surface`. The renderer keeps one
//!   texture per surface and writes each new frame into it (`write_texture` straight from the frame's buffer).
//!
//! Each draw records which frame each tile shows ([`ShowTracker`]): frames shown, frames replaced before any draw
//! showed them, and the latency from the source sending a frame to the draw that shows it.

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, Context, ExternalSurface, FrameCompletion, IntoElement, ObjectFit, ParentElement as _, PixelFrame,
    PixelRect, PixelSurface, Render, RenderImage, Styled as _, StyledImage as _, Window, canvas, div, img, px, rgb,
};
use image::{Frame, ImageBuffer, Rgba};

use crate::{
    bench::{ShowTracker, Stats},
    frames::{FrameGenerator, HEIGHT, Rect, WIDTH, nv12_to_bgra},
};

/// The grid: 3 columns by 3 rows, tiles numbered row by row.
pub const COLUMNS: u32 = 3;
pub const ROWS: u32 = 3;
pub const TILES: usize = 9;

/// Where tile `index` (row by row) sits in a grid filling `area`.
#[must_use]
pub fn tile_rect(index: usize, area: Rect) -> Rect {
    let index = u32::try_from(index).unwrap_or(u32::MAX);
    let (column, row) = (index % COLUMNS, index / COLUMNS);
    let edge = |length: u32, cells: u32, at: u32| {
        u32::try_from(u64::from(length) * u64::from(at) / u64::from(cells)).unwrap_or(length)
    };
    let (x0, x1) = (edge(area.width, COLUMNS, column), edge(area.width, COLUMNS, column + 1));
    let (y0, y1) = (edge(area.height, ROWS, row), edge(area.height, ROWS, row + 1));
    Rect::new(area.x + x0, area.y + y0, x1 - x0, y1 - y0)
}

/// One converted frame on its way to the grid.
pub struct VideoFrame {
    pub tile: usize,
    pub counter: u32,
    /// When the source sent it.
    pub sent: Instant,
    /// How long its NV12 → BGRA conversion took.
    pub convert: Duration,
    pub bgra: Vec<u8>,
}

/// The source threads, one per tile; stopped and joined on [`Source::stop`] or drop.
pub struct Source {
    stop: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl Source {
    /// Starts `tiles` streams at `fps`, staggered evenly over one frame period, sending into `sender`.
    ///
    /// # Errors
    /// A thread can't be spawned.
    pub fn start(tiles: usize, fps: u32, sender: &async_channel::Sender<VideoFrame>) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let period = Duration::from_secs(1) / fps.max(1);
        let start = Instant::now();
        let count = u32::try_from(tiles).unwrap_or(u32::MAX).max(1);
        let mut threads = Vec::with_capacity(tiles);
        for (tile, offset) in (0..tiles).zip(0..count) {
            let stream = Stream {
                tile,
                period,
                first: start + period * offset / count,
                sender: sender.clone(),
                stop: Arc::clone(&stop),
            };
            threads.push(
                thread::Builder::new()
                    .name(format!("video-src-{tile}"))
                    .spawn(move || stream.run())?,
            );
        }
        Ok(Self { stop, threads })
    }

    /// Stops the streams and waits for their threads.
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.halt();
    }
}

/// One stream's thread.
struct Stream {
    tile: usize,
    period: Duration,
    first: Instant,
    sender: async_channel::Sender<VideoFrame>,
    stop: Arc<AtomicBool>,
}

impl Stream {
    fn run(self) {
        let Ok(generator) = FrameGenerator::new(u32::try_from(self.tile).unwrap_or(0), WIDTH, HEIGHT) else {
            return;
        };
        let mut nv12 = generator.nv12(0);
        let mut next = self.first;
        let mut counter = 0_u32;
        while !self.stop.load(Ordering::Relaxed) {
            if let Some(wait) = next.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
            // the "decoder" hands over an NV12 frame; stock GPUI needs BGRA
            generator.nv12_into(counter, &mut nv12);
            let converting = Instant::now();
            let mut bgra = Vec::new();
            nv12_to_bgra(&nv12, &mut bgra);
            let frame = VideoFrame {
                tile: self.tile,
                counter,
                sent: Instant::now(),
                convert: converting.elapsed(),
                bgra,
            };
            if self.sender.try_send(frame).is_err() {
                return;
            }
            counter = counter.wrapping_add(1);
            next += self.period;
            // fell more than a frame behind: skip ahead rather than burst
            if Instant::now() > next + self.period {
                next = Instant::now();
            }
        }
    }
}

/// How the grid hands frames to GPUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Upload {
    /// Approach (a), stock GPUI: a fresh `RenderImage` per frame, through the sprite atlas.
    Image,
    /// Approach (b), patched GPUI: a `PixelFrame` per frame, into a texture kept per tile.
    Surface,
}

impl Upload {
    /// The name the binary's `--upload` takes and the summary prints.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Surface => "surface",
        }
    }

    /// The mode named `name`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Image, Self::Surface]
            .into_iter()
            .find(|upload| upload.name() == name)
    }
}

/// A tile's frame in the form its upload path paints.
enum Shown {
    Image(Arc<RenderImage>),
    Surface(Arc<PixelFrame>),
}

/// The frame a tile shows.
struct Tile {
    shown: Shown,
    counter: u32,
    sent: Instant,
}

/// The grid view, and what it measured since [`Grid::start_measuring`].
pub struct Grid {
    upload: Upload,
    tiles: Vec<Option<Tile>>,
    /// Each tile's surface and the serial of its newest frame ([`Upload::Surface`]).
    surfaces: Vec<(Arc<PixelSurface>, u64)>,
    tracker: ShowTracker,
    draws: Vec<Instant>,
    converts: Vec<Duration>,
    received: usize,
    measuring_since: Option<Instant>,
    /// The window's size in device pixels at the last draw.
    viewport: (u32, u32),
}

impl Grid {
    #[must_use]
    pub fn new(upload: Upload) -> Self {
        Self {
            upload,
            tiles: (0..TILES).map(|_| None).collect(),
            surfaces: (0..TILES).map(|_| (PixelSurface::new(), 0)).collect(),
            tracker: ShowTracker::new(TILES),
            draws: Vec::new(),
            converts: Vec::new(),
            received: 0,
            measuring_since: None,
            viewport: (0, 0),
        }
    }

    /// Takes `frame` as its tile's newest. On the image path the frame it replaces is dropped from the window's
    /// sprite atlas (a no-op when that frame was never drawn).
    ///
    /// # Errors
    /// No such tile, or the frame isn't a 1280×720 BGRA buffer.
    pub fn push(&mut self, frame: VideoFrame, window: &mut Window) -> Result<(), String> {
        let (surface, serial) = self
            .surfaces
            .get_mut(frame.tile)
            .ok_or_else(|| format!("no tile {}", frame.tile))?;
        let shown = match self.upload {
            Upload::Image => {
                let buffer = ImageBuffer::<Rgba<u8>, _>::from_raw(WIDTH, HEIGHT, frame.bgra)
                    .ok_or("frame is not 1280×720 BGRA")?;
                Shown::Image(Arc::new(RenderImage::new(vec![Frame::new(buffer)])))
            }
            Upload::Surface => {
                *serial += 1;
                Shown::Surface(Arc::new(pixel_frame(surface, *serial, frame.bgra)?))
            }
        };
        let tile = Tile {
            shown,
            counter: frame.counter,
            sent: frame.sent,
        };
        if let Some(Tile {
            shown: Shown::Image(old),
            ..
        }) = self.tiles[frame.tile].replace(tile)
        {
            window
                .drop_image(old)
                .map_err(|error| format!("dropping a frame: {error:#}"))?;
        }
        self.received += 1;
        self.converts.push(frame.convert);
        Ok(())
    }

    /// Forgets what was measured so far (the warm-up) and starts measuring at `now`.
    pub fn start_measuring(&mut self, now: Instant) {
        self.tracker.reset_counts();
        self.draws.clear();
        self.converts.clear();
        self.received = 0;
        self.measuring_since = Some(now);
    }

    /// The `summary key=value ...` line for the time since [`Grid::start_measuring`] up to `now`.
    #[must_use]
    pub fn summary(&self, now: Instant) -> String {
        let elapsed = self.measuring_since.map_or(Duration::ZERO, |since| now - since);
        let intervals: Vec<Duration> = self.draws.windows(2).map(|pair| pair[1] - pair[0]).collect();
        let last: Vec<String> = self
            .tracker
            .last()
            .iter()
            .map(|counter| counter.map_or_else(|| "-".to_owned(), |counter| counter.to_string()))
            .collect();
        format!(
            "summary upload={} elapsed_ms={} viewport={}x{} draws={} received={} shown={} skipped={} {} {} {} last={}",
            self.upload.name(),
            elapsed.as_millis(),
            self.viewport.0,
            self.viewport.1,
            self.draws.len(),
            self.received,
            self.tracker.shown(),
            self.tracker.skipped(),
            stats_fields("latency", Stats::of(self.tracker.latencies())),
            stats_fields("interval", Stats::of(&intervals)),
            stats_fields("convert", Stats::of(&self.converts)),
            last.join(","),
        )
    }
}

/// Frame `serial` of `surface`: the whole 1280×720 BGRA frame (opaque, so premultiplied as it is), all of it damaged.
fn pixel_frame(surface: &Arc<PixelSurface>, serial: u64, bgra: Vec<u8>) -> Result<PixelFrame, String> {
    let expected = usize::try_from(WIDTH * HEIGHT * 4).unwrap_or(usize::MAX);
    if bgra.len() != expected {
        return Err(format!("frame is {} bytes, not 1280×720 BGRA", bgra.len()));
    }
    Ok(PixelFrame {
        surface: Arc::clone(surface),
        serial,
        width: WIDTH,
        height: HEIGHT,
        pixels: Arc::new(bgra),
        damage: vec![PixelRect {
            x: 0,
            y: 0,
            width: WIDTH,
            height: HEIGHT,
        }],
        completion: FrameCompletion::none(),
    })
}

/// The element that paints `shown`, filling its cell.
fn tile_element(shown: &Shown) -> AnyElement {
    match shown {
        Shown::Image(image) => img(image.clone())
            .size_full()
            .object_fit(ObjectFit::Fill)
            .into_any_element(),
        Shown::Surface(frame) => {
            let frame = Arc::clone(frame);
            canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    window.paint_external_surface(bounds, ExternalSurface::Pixels(frame));
                },
            )
            .size_full()
            .into_any_element()
        }
    }
}

/// The window's size in device pixels, rounded.
fn device_size(window: &Window) -> (u32, u32) {
    let size = window.viewport_size().to_device_pixels(window.scale_factor());
    (u32::from(size.width), u32::from(size.height))
}

/// `<name>_p50_us=.. <name>_p95_us=.. <name>_max_us=..`, zeros when there are no samples.
fn stats_fields(name: &str, stats: Option<Stats>) -> String {
    let [p50, p95, max] = stats.map_or([0; 3], |stats| {
        [stats.p50, stats.p95, stats.max].map(|value| value.as_micros())
    });
    format!("{name}_p50_us={p50} {name}_p95_us={p95} {name}_max_us={max}")
}

impl Render for Grid {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        self.viewport = device_size(window);
        self.draws.push(now);
        for (index, tile) in self.tiles.iter().enumerate() {
            if let Some(tile) = tile {
                self.tracker.show(index, tile.counter, now - tile.sent);
            }
        }
        let rows = self.tiles.chunks(TILES / 3).map(|row| {
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_h(px(0.0))
                .children(row.iter().map(|tile| {
                    let cell = div().flex_1().h_full().min_w(px(0.0)).overflow_hidden();
                    match tile {
                        Some(tile) => cell.child(tile_element(&tile.shown)),
                        None => cell,
                    }
                }))
        });
        div().size_full().flex().flex_col().bg(rgb(0)).children(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_cover_the_area_row_by_row() {
        let area = Rect::new(10, 20, 1920, 1080);
        assert_eq!(tile_rect(0, area), Rect::new(10, 20, 640, 360));
        assert_eq!(tile_rect(1, area), Rect::new(650, 20, 640, 360));
        assert_eq!(tile_rect(4, area), Rect::new(650, 380, 640, 360));
        assert_eq!(tile_rect(8, area), Rect::new(1290, 740, 640, 360));
        // sides that don't divide by 3 leave no gaps
        let odd = Rect::new(0, 0, 1000, 500);
        let widths: u32 = (0..3).map(|index| tile_rect(index, odd).width).sum();
        assert_eq!(widths, 1000);
        assert_eq!(tile_rect(2, odd).x + tile_rect(2, odd).width, 1000);
        let heights: u32 = [0, 3, 6].iter().map(|&index| tile_rect(index, odd).height).sum();
        assert_eq!(heights, 500);
    }

    #[test]
    fn upload_names_round_trip() {
        for upload in [Upload::Image, Upload::Surface] {
            assert_eq!(Upload::from_name(upload.name()), Some(upload));
        }
        assert_eq!(Upload::from_name("dmabuf"), None);
    }

    #[test]
    fn pixel_frames_are_whole_720p_frames_of_their_surface() {
        let surface = PixelSurface::new();
        let frame = pixel_frame(&surface, 7, vec![0; 1280 * 720 * 4]).unwrap();
        assert!(Arc::ptr_eq(&frame.surface, &surface));
        assert_eq!((frame.serial, frame.width, frame.height), (7, 1280, 720));
        assert_eq!(
            frame.damage,
            vec![PixelRect {
                x: 0,
                y: 0,
                width: 1280,
                height: 720
            }]
        );
        assert!(!frame.completion.is_pending());
        let error = pixel_frame(&surface, 8, vec![0; 16]).unwrap_err();
        assert!(error.contains("16 bytes"), "{error}");
    }

    #[test]
    fn stats_fields_are_in_microseconds() {
        let stats = Stats::of(&[Duration::from_millis(2), Duration::from_millis(4)]);
        assert_eq!(
            stats_fields("latency", stats),
            "latency_p50_us=2000 latency_p95_us=4000 latency_max_us=4000"
        );
        assert_eq!(
            stats_fields("convert", None),
            "convert_p50_us=0 convert_p95_us=0 convert_max_us=0"
        );
    }

    #[test]
    fn source_streams_send_converted_frames_with_counters() {
        let (sender, receiver) = async_channel::unbounded();
        let source = Source::start(2, 200, &sender).unwrap();
        let mut seen = [Vec::new(), Vec::new()];
        while seen.iter().any(|counters| counters.len() < 2) {
            let frame = receiver.recv_blocking().unwrap();
            assert_eq!(frame.bgra.len(), 1280 * 720 * 4);
            assert_eq!(
                crate::frames::read_counter_bgra(&frame.bgra, WIDTH, HEIGHT),
                Some(frame.counter)
            );
            seen[frame.tile].push(frame.counter);
        }
        source.stop();
        for counters in seen {
            assert_eq!(&counters[..2], &[0, 1]);
        }
    }

    #[test]
    fn a_source_stops_when_the_grid_is_gone() {
        let (sender, receiver) = async_channel::unbounded();
        let source = Source::start(1, 100, &sender).unwrap();
        drop(receiver);
        // the stream sees the closed channel and returns; stop() then joins at once
        let started = Instant::now();
        source.stop();
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
