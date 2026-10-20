//! A synthetic video source: 1280×720 NV12 frames, the format a hardware decoder hands out, with a frame counter
//! burned into the luma plane that can be read back from the pixels, after conversion to BGRA, after scaling, and
//! from a screenshot of a grid tile.
//!
//! The counter is two rows of [`COUNTER_BITS`] black/white cells in the top-left of the frame: the value (most
//! significant bit first) and its bitwise complement, so a damaged or absent counter reads as `None` rather than as a
//! wrong number. Cell positions are fractions of the frame (a 32×18 grid of cells), so the reader needs only the
//! rectangle the frame occupies in whatever image it reads from.

/// The benchmark's frame size: 720p.
pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
/// Bits of the burned-in counter; counters wrap above [`COUNTER_MAX`].
pub const COUNTER_BITS: u32 = 24;
pub const COUNTER_MAX: u32 = (1 << COUNTER_BITS) - 1;

/// The frame is a grid of this many cells across and down; the counter's cells are cells of that grid.
const CELLS_ACROSS: u32 = 32;
const CELLS_DOWN: u32 = 18;
/// Luma of a 0 and a 1 cell: video black and video white.
const LUMA_ZERO: u8 = 16;
const LUMA_ONE: u8 = 235;
/// A cell reads as 1 from this luma up.
const LUMA_THRESHOLD: u8 = 128;
/// Neutral chroma.
const CHROMA_NEUTRAL: u8 = 128;
/// Smallest frame the generator makes: the counter cells stay at least 20 px.
const MIN_WIDTH: u32 = 640;
const MIN_HEIGHT: u32 = 360;
/// Each stream's colour, as (U, V): nine hues around the colour wheel.
const STREAM_CHROMA: [(u8, u8); 9] = [
    (128, 176),
    (97, 165),
    (83, 136),
    (94, 104),
    (128, 80),
    (159, 91),
    (173, 120),
    (162, 152),
    (150, 170),
];

/// A frame in NV12: a full-resolution luma plane, then one interleaved U/V pair per 2×2 block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nv12Frame {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub uv: Vec<u8>,
}

/// Why a generator can't be made.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("NV12 needs an even width and height, got {width}×{height}")]
    OddSize { width: u32, height: u32 },
    #[error("{width}×{height} is below the {MIN_WIDTH}×{MIN_HEIGHT} the counter needs")]
    TooSmall { width: u32, height: u32 },
}

/// A rectangle in pixels of some image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// The pixel span `[start, end)` of cell `index` of `cells` cells along a side `length` pixels long.
fn cell_span(length: u32, cells: u32, index: u32) -> (u32, u32) {
    let at = |index: u32| u32::try_from(u64::from(length) * u64::from(index) / u64::from(cells)).unwrap_or(length);
    (at(index), at(index + 1))
}

/// The counter's cells as (column, row, bit): row 1 holds the value, row 2 its complement; columns 1 to 24.
fn counter_cells(counter: u32) -> impl Iterator<Item = (u32, u32, bool)> {
    let value = counter & COUNTER_MAX;
    (0..COUNTER_BITS).flat_map(move |bit| {
        let one = value >> (COUNTER_BITS - 1 - bit) & 1 == 1;
        [(1 + bit, 1, one), (1 + bit, 2, !one)]
    })
}

/// Makes the frames of one synthetic stream.
pub struct FrameGenerator {
    width: u32,
    height: u32,
    base: Nv12Frame,
}

impl FrameGenerator {
    /// The generator for stream `stream` (its colour) at `width`×`height`.
    ///
    /// # Errors
    /// An odd size, or one too small for the counter.
    pub fn new(stream: u32, width: u32, height: u32) -> Result<Self, FrameError> {
        if width % 2 == 1 || height % 2 == 1 {
            return Err(FrameError::OddSize { width, height });
        }
        if width < MIN_WIDTH || height < MIN_HEIGHT {
            return Err(FrameError::TooSmall { width, height });
        }
        Ok(Self {
            width,
            height,
            base: base_frame(stream, width, height),
        })
    }

    /// Frame `counter` of the stream.
    #[must_use]
    pub fn nv12(&self, counter: u32) -> Nv12Frame {
        let mut frame = self.base.clone();
        self.draw(counter, &mut frame);
        frame
    }

    /// Frame `counter` of the stream, written over `frame` (whose buffers are reused).
    pub fn nv12_into(&self, counter: u32, frame: &mut Nv12Frame) {
        frame.width = self.width;
        frame.height = self.height;
        frame.y.clone_from(&self.base.y);
        frame.uv.clone_from(&self.base.uv);
        self.draw(counter, frame);
    }

    /// Frame `counter` converted to BGRA.
    #[must_use]
    pub fn bgra(&self, counter: u32) -> Vec<u8> {
        let mut bgra = Vec::new();
        nv12_to_bgra(&self.nv12(counter), &mut bgra);
        bgra
    }

    /// Draws the moving bar and the counter over a copy of the base frame.
    fn draw(&self, counter: u32, frame: &mut Nv12Frame) {
        self.draw_bar(counter, frame);
        self.burn_counter(counter, frame);
    }

    /// A white bar below the counter that moves 12 px a frame, so every frame differs from the last.
    fn draw_bar(&self, counter: u32, frame: &mut Nv12Frame) {
        let width = to_usize(self.width);
        let bar = (width / 50).max(2);
        let left = to_usize(u32::try_from(u64::from(counter) * 12 % u64::from(self.width)).unwrap_or(0));
        let top = to_usize(cell_span(self.height, CELLS_DOWN, 4).0);
        for row in frame.y.chunks_exact_mut(width).skip(top) {
            let end = (left + bar).min(width);
            row[left..end].fill(LUMA_ONE);
        }
    }

    fn burn_counter(&self, counter: u32, frame: &mut Nv12Frame) {
        let width = to_usize(self.width);
        for (column, row, one) in counter_cells(counter) {
            let (x0, x1) = cell_span(self.width, CELLS_ACROSS, column);
            let (y0, y1) = cell_span(self.height, CELLS_DOWN, row);
            let luma = if one { LUMA_ONE } else { LUMA_ZERO };
            for line in frame.y.chunks_exact_mut(width).take(to_usize(y1)).skip(to_usize(y0)) {
                line[to_usize(x0)..to_usize(x1)].fill(luma);
            }
        }
    }
}

/// A stream's background: a luma gradient in the stream's colour, with neutral chroma behind the counter.
fn base_frame(stream: u32, width: u32, height: u32) -> Nv12Frame {
    let (columns, rows) = (to_usize(width), to_usize(height));
    let mut luma = vec![0_u8; columns * rows];
    for (row, line) in luma.chunks_exact_mut(columns).enumerate() {
        for (column, sample) in line.iter_mut().enumerate() {
            let value = 40 + column * 120 / columns + row * 60 / rows;
            *sample = u8::try_from(value).unwrap_or(u8::MAX);
        }
    }
    let (blue_diff, red_diff) = STREAM_CHROMA[to_usize(stream) % STREAM_CHROMA.len()];
    let mut chroma = [blue_diff, red_diff].repeat(columns * rows / 4);
    // neutral chroma under the counter, rounded out to whole 2×2 blocks, so its cells convert to black and white
    let left = to_usize(cell_span(width, CELLS_ACROSS, 1).0) / 2;
    let right = to_usize(cell_span(width, CELLS_ACROSS, COUNTER_BITS).1).div_ceil(2);
    let top = to_usize(cell_span(height, CELLS_DOWN, 1).0) / 2;
    let bottom = to_usize(cell_span(height, CELLS_DOWN, 2).1).div_ceil(2);
    for line in chroma.chunks_exact_mut(columns).take(bottom).skip(top) {
        line[left * 2..right * 2].fill(CHROMA_NEUTRAL);
    }
    Nv12Frame {
        width,
        height,
        y: luma,
        uv: chroma,
    }
}

fn clamp_u8(value: i32) -> u8 {
    u8::try_from(value.clamp(0, 255)).unwrap_or(u8::MAX)
}

/// Converts NV12 (BT.601, limited range) to BGRA, the byte order GPUI's `RenderImage` holds, into `out`.
pub fn nv12_to_bgra(frame: &Nv12Frame, out: &mut Vec<u8>) {
    let width = to_usize(frame.width);
    out.resize(width * to_usize(frame.height) * 4, 0);
    for (row, (luma_row, out_row)) in frame
        .y
        .chunks_exact(width)
        .zip(out.chunks_exact_mut(width * 4))
        .enumerate()
    {
        let chroma_row = &frame.uv[(row / 2) * width..(row / 2 + 1) * width];
        convert_row(luma_row, chroma_row, out_row);
    }
}

/// One row: each pair of luma samples shares one U/V pair and becomes 8 BGRA bytes.
fn convert_row(luma_row: &[u8], chroma_row: &[u8], out_row: &mut [u8]) {
    let (luma_pairs, _) = luma_row.as_chunks::<2>();
    let (chroma_pairs, _) = chroma_row.as_chunks::<2>();
    let (out_pairs, _) = out_row.as_chunks_mut::<8>();
    for ((&[first, second], &[u, v]), out) in luma_pairs.iter().zip(chroma_pairs).zip(out_pairs) {
        let (u, v) = (i32::from(u) - 128, i32::from(v) - 128);
        let (red, green, blue) = (409 * v, -100 * u - 208 * v, 516 * u);
        let pixel = |luma: u8| {
            let scaled = 298 * (i32::from(luma) - 16) + 128;
            [
                clamp_u8((scaled + blue) >> 8),
                clamp_u8((scaled + green) >> 8),
                clamp_u8((scaled + red) >> 8),
                u8::MAX,
            ]
        };
        out[..4].copy_from_slice(&pixel(first));
        out[4..].copy_from_slice(&pixel(second));
    }
}

/// The luma of one BGRA pixel (BT.601 weights, full range).
#[must_use]
pub fn bgra_luma(pixel: &[u8]) -> u8 {
    let [blue, green, red] = [0, 1, 2].map(|index| u32::from(pixel.get(index).copied().unwrap_or(0)));
    u8::try_from((77 * red + 150 * green + 29 * blue) >> 8).unwrap_or(u8::MAX)
}

/// The centre, in pixels of the image, of counter cell (`column`, `row`) of a frame shown in `rect`.
fn cell_centre(rect: Rect, column: u32, row: u32) -> (u32, u32) {
    let centre = |origin: u32, length: u32, cells: u32, index: u32| {
        let offset = (u64::from(length) * u64::from(2 * index + 1)) / u64::from(2 * cells);
        origin.saturating_add(u32::try_from(offset).unwrap_or(u32::MAX))
    };
    (
        centre(rect.x, rect.width, CELLS_ACROSS, column),
        centre(rect.y, rect.height, CELLS_DOWN, row),
    )
}

/// Reads the counter of a frame shown in `rect` of an image whose luma `luma_at(x, y)` gives (`None` outside it).
/// `None` when the cells are missing, or the complement row doesn't match the value row.
pub fn read_counter(rect: Rect, luma_at: impl Fn(u32, u32) -> Option<u8>) -> Option<u32> {
    let bit = |column: u32, row: u32| {
        let (x, y) = cell_centre(rect, column, row);
        luma_at(x, y).map(|luma| luma >= LUMA_THRESHOLD)
    };
    let mut value = 0;
    for index in 0..COUNTER_BITS {
        let one = bit(1 + index, 1)?;
        if bit(1 + index, 2)? == one {
            return None;
        }
        value = value << 1 | u32::from(one);
    }
    Some(value)
}

/// Reads the counter of a full `width`×`height` BGRA frame.
#[must_use]
pub fn read_counter_bgra(bgra: &[u8], width: u32, height: u32) -> Option<u32> {
    read_counter(Rect::new(0, 0, width, height), |x, y| {
        if x >= width || y >= height {
            return None;
        }
        let index = (to_usize(y) * to_usize(width) + to_usize(x)) * 4;
        bgra.get(index..index + 4).map(bgra_luma)
    })
}
