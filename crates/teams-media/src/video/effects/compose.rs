//! A frame put together from the person and a new background: the model's mask scaled up to the frame (bilinear),
//! its edge sharpened there (so it follows the person smoothly rather than the model's coarse pixels), and each plane
//! blended between the frame and the background, which is either the frame
//! itself blurred or a picture. The blur is made small (a cell per [`blur_factor`] pixels, the person left out of it
//! so they don't bleed into it as a halo), blurred there and scaled back up.

use std::ops::Range;
use std::path::Path;

use image::imageops::{self, FilterType};
use image::{ImageReader, RgbImage};

use crate::devices::{PixelFormat, VideoFrame};
use crate::video::convert::{MaxSize, to_i420};

use super::segment::{MODEL_HEIGHT, MODEL_WIDTH};
use super::{Backdrop, to_usize};

/// The blur's radius in cells, and its passes (three box blurs: near a Gaussian).
const BLUR_RADIUS: usize = 2;
const BLUR_PASSES: usize = 3;
/// The largest picture file read, bytes.
const MAX_PICTURE_FILE: u64 = 64 * 1024 * 1024;
/// The mask's values (0..=255) below which a pixel is the background, and above which it is the person; between
/// them the edge, smoothstepped.
const EDGE: (u32, u32) = (77, 179);

/// The mask's value as a pixel's weight: the edge between [`EDGE`]'s bounds smoothstepped to 0..=255.
fn edge_table() -> Vec<u8> {
    let (low, high) = EDGE;
    (0..=255_u32)
        .map(|value| {
            // t in 0..=256 across the edge, then 3t² − 2t³ in 0..=256
            let t = (value.clamp(low, high) - low) * 256 / (high - low);
            let smooth = t * t * (3 * 256 - 2 * t) / (256 * 256);
            u8::try_from(smooth.min(255)).unwrap_or(u8::MAX)
        })
        .collect()
}

/// Bilinear scaling of one 8-bit plane from one size to another (pixel centres matched), in 8-bit fixed point.
struct Resampler {
    from: (usize, usize),
    to: (usize, usize),
    /// The output columns in runs that share their two source columns: the source columns either side and the run's
    /// output columns.
    runs: Vec<(usize, usize, Range<usize>)>,
    /// Per output column: the right source column's weight (0..=256).
    across: Vec<u32>,
    /// Per output row: the source rows either side and the lower one's weight.
    rows: Vec<(usize, usize, u32)>,
}

impl Resampler {
    fn new(from: (usize, usize), to: (usize, usize)) -> Self {
        let columns = taps(from.0, to.0);
        let mut runs: Vec<(usize, usize, Range<usize>)> = Vec::new();
        for (index, &(left, right, _)) in columns.iter().enumerate() {
            match runs.last_mut() {
                Some((run_left, run_right, outputs)) if (*run_left, *run_right) == (left, right) => {
                    outputs.end = index + 1;
                }
                _ => runs.push((left, right, index..index + 1)),
            }
        }
        Self {
            from,
            to,
            runs,
            across: columns.iter().map(|&(_, _, across)| across).collect(),
            rows: taps(from.1, to.1),
        }
    }

    /// The same scaling, made again only when the sizes changed.
    fn kept(kept: Option<Self>, from: (usize, usize), to: (usize, usize)) -> Self {
        match kept {
            Some(resampler) if resampler.from == from && resampler.to == to => resampler,
            _ => Self::new(from, to),
        }
    }

    /// Output row `row` of `plane` (the source size) into `out` (the output width); `vertical` is scratch, kept by
    /// the caller between rows. The two source rows are blended first (a source row is short), then each output pixel
    /// takes its two columns of that.
    fn row(&self, plane: &[u8], row: usize, out: &mut [u8], vertical: &mut Vec<u32>) {
        let Some(&(above, below, down)) = self.rows.get(row) else {
            return;
        };
        let width = self.from.0;
        let (Some(top), Some(bottom)) = (
            plane.get(above * width..(above + 1) * width),
            plane.get(below * width..(below + 1) * width),
        ) else {
            return;
        };
        vertical.clear();
        vertical.extend(
            top.iter()
                .zip(bottom)
                .map(|(&upper, &lower)| u32::from(upper) * (256 - down) + u32::from(lower) * down),
        );
        for (left, right, outputs) in &self.runs {
            let (Some(&left), Some(&right), Some(out), Some(across)) = (
                vertical.get(*left),
                vertical.get(*right),
                out.get_mut(outputs.clone()),
                self.across.get(outputs.clone()),
            ) else {
                continue;
            };
            // the two source values are the run's own: a straight line between them, which vectorizes
            for (out, &across) in out.iter_mut().zip(across) {
                *out = narrow((left * (256 - across) + right * across + (1 << 15)) >> 16);
            }
        }
    }
}

/// A value known to be at most 255 as a byte.
#[expect(clippy::cast_possible_truncation, reason = "the callers' values are at most 255")]
fn narrow(value: u32) -> u8 {
    value as u8
}

/// For each of `to` output positions, the two source positions of `from` either side of its centre and the second
/// one's weight out of 256.
fn taps(from: usize, to: usize) -> Vec<(usize, usize, u32)> {
    let last = from.saturating_sub(1);
    (0..to)
        .map(|index| {
            // the output pixel's centre in source pixels, times 256, less half a pixel
            let centre = ((2 * index + 1) * from * 256 / (2 * to.max(1))).saturating_sub(128);
            let first = (centre >> 8).min(last);
            let weight = if first == last {
                0
            } else {
                u32::try_from(centre & 255).unwrap_or(0)
            };
            (first, (first + 1).min(last), weight)
        })
        .collect()
}

/// How many pixels a side of the blur's cell takes in a frame `width` wide: about 80 cells across, even (a chroma
/// cell is half as many).
fn blur_factor(width: usize) -> usize {
    ((width / 80).max(2) + 1) & !1
}

/// The compositor of one camera thread: its scalings and scratch planes, kept between frames of one size.
#[derive(Default)]
pub(super) struct Compositor {
    /// The mask to the frame's luma size, and to its chroma size.
    mask_luma: Option<Resampler>,
    mask_chroma: Option<Resampler>,
    /// The blur's cells to the frame's luma size, and to its chroma size.
    blur_luma: Option<Resampler>,
    blur_chroma: Option<Resampler>,
    /// The mask at the frame's luma size, and at its chroma size.
    alpha: Vec<u8>,
    alpha_chroma: Vec<u8>,
    /// [`edge_table`], made on the first frame.
    edge: Vec<u8>,
}

/// One plane of a frame: its bytes, width and height.
struct Plane<'a> {
    data: &'a mut [u8],
    width: usize,
    height: usize,
}

impl Compositor {
    /// Blurs `frame`'s background, `mask` saying where the person is.
    pub(super) fn blur(&mut self, frame: &mut VideoFrame, mask: &[u8]) {
        let (width, height) = (to_usize(frame.width), to_usize(frame.height));
        self.scale_mask(mask, width, height);
        let factor = blur_factor(width);
        let cells = (width.div_ceil(factor), height.div_ceil(factor));
        let chroma_cells = ((width / 2).div_ceil(factor / 2), (height / 2).div_ceil(factor / 2));
        self.blur_luma = Some(Resampler::kept(self.blur_luma.take(), cells, (width, height)));
        self.blur_chroma = Some(Resampler::kept(
            self.blur_chroma.take(),
            chroma_cells,
            (width / 2, height / 2),
        ));
        let [luma, u_plane, v_plane] = planes(frame);
        let (alpha, alpha_chroma) = (&self.alpha, &self.alpha_chroma);
        for (plane, alpha, step, resampler) in [
            (luma, alpha, factor, &self.blur_luma),
            (u_plane, alpha_chroma, factor / 2, &self.blur_chroma),
            (v_plane, alpha_chroma, factor / 2, &self.blur_chroma),
        ] {
            let Some(resampler) = resampler else { continue };
            let small = blurred_cells(&plane, alpha, step, resampler.from);
            let mut vertical = Vec::new();
            blend_rows(plane, alpha, |row, out| resampler.row(&small, row, out, &mut vertical));
        }
    }

    /// Puts `picture` (I420 at `frame`'s size) behind the person in `frame`, `mask` saying where the person is.
    pub(super) fn replace(&mut self, frame: &mut VideoFrame, mask: &[u8], picture: &VideoFrame) {
        let (width, height) = (to_usize(frame.width), to_usize(frame.height));
        if picture.data.len() < frame.data.len() || (picture.width, picture.height) != (frame.width, frame.height) {
            return;
        }
        self.scale_mask(mask, width, height);
        let luma_size = width * height;
        let chroma_size = luma_size / 4;
        let [luma, u_plane, v_plane] = planes(frame);
        for (plane, alpha, from) in [
            (luma, &self.alpha, 0),
            (u_plane, &self.alpha_chroma, luma_size),
            (v_plane, &self.alpha_chroma, luma_size + chroma_size),
        ] {
            let width = plane.width;
            let source = &picture.data[from..];
            blend_rows(plane, alpha, |row, out| {
                if let Some(line) = source.get(row * width..(row + 1) * width) {
                    out.copy_from_slice(line);
                }
            });
        }
    }

    /// The mask scaled to the frame's luma and chroma sizes, into [`Compositor::alpha`] and its chroma twin.
    fn scale_mask(&mut self, mask: &[u8], width: usize, height: usize) {
        let model = (MODEL_WIDTH, MODEL_HEIGHT);
        let luma = Resampler::kept(self.mask_luma.take(), model, (width, height));
        let chroma = Resampler::kept(self.mask_chroma.take(), model, (width / 2, height / 2));
        if self.edge.is_empty() {
            self.edge = edge_table();
        }
        for (resampler, alpha) in [(&luma, &mut self.alpha), (&chroma, &mut self.alpha_chroma)] {
            scale_into(resampler, mask, alpha);
            for value in alpha.iter_mut() {
                *value = self.edge[usize::from(*value)];
            }
        }
        self.mask_luma = Some(luma);
        self.mask_chroma = Some(chroma);
    }
}

/// `plane` scaled by `resampler` into `out`.
fn scale_into(resampler: &Resampler, plane: &[u8], out: &mut Vec<u8>) {
    let (width, height) = resampler.to;
    out.resize(width * height, 0);
    let mut vertical = Vec::new();
    for (row, line) in out.chunks_exact_mut(width).enumerate() {
        resampler.row(plane, row, line, &mut vertical);
    }
}

/// The frame's three planes (I420, each plane's stride its width).
fn planes(frame: &mut VideoFrame) -> [Plane<'_>; 3] {
    let (width, height) = (to_usize(frame.width), to_usize(frame.height));
    let luma_size = width * height;
    let (luma, chroma) = frame.data.split_at_mut(luma_size);
    let (u_plane, rest) = chroma.split_at_mut(luma_size / 4);
    let v_plane = &mut rest[..luma_size / 4];
    let chroma = |data| Plane {
        data,
        width: width / 2,
        height: height / 2,
    };
    [
        Plane {
            data: luma,
            width,
            height,
        },
        chroma(u_plane),
        chroma(v_plane),
    ]
}

/// Blends each row of `plane` toward the background row `background` makes, by `alpha` (255 keeps the frame).
fn blend_rows(plane: Plane<'_>, alpha: &[u8], mut background: impl FnMut(usize, &mut [u8])) {
    let mut behind = vec![0_u8; plane.width];
    for (row, (line, weights)) in plane
        .data
        .chunks_exact_mut(plane.width)
        .zip(alpha.chunks_exact(plane.width))
        .take(plane.height)
        .enumerate()
    {
        if weights.iter().all(|&weight| weight == 255) {
            continue;
        }
        background(row, &mut behind);
        for ((front, &back), &weight) in line.iter_mut().zip(&behind).zip(weights) {
            let weight = u32::from(weight);
            *front = narrow((u32::from(*front) * weight + u32::from(back) * (255 - weight) + 127) / 255);
        }
    }
}

/// `plane`'s background in cells of `step` pixels (`cells` across and down), blurred: each cell the mean of its
/// pixels weighted by how much they are background (the person counts next to nothing, so they don't spread into it),
/// then box-blurred [`BLUR_PASSES`] times each way.
fn blurred_cells(plane: &Plane<'_>, alpha: &[u8], step: usize, cells: (usize, usize)) -> Vec<u8> {
    let (across, down) = cells;
    let mut sums = vec![0.0_f64; across * down];
    let mut weights = vec![0.0_f64; across * down];
    // every other pixel each way is plenty for a blur this wide
    let sample = if step >= 4 { 2 } else { 1 };
    for (row, (line, line_alpha)) in plane
        .data
        .chunks_exact(plane.width)
        .zip(alpha.chunks_exact(plane.width))
        .take(plane.height)
        .enumerate()
        .step_by(sample)
    {
        let cell_row = (row / step).min(down - 1) * across;
        for (cell, (pixels, pixel_alpha)) in line.chunks(step).zip(line_alpha.chunks(step)).enumerate() {
            let (mut sum, mut weight) = (0_u32, 0_u32);
            for (&value, &person) in pixels.iter().zip(pixel_alpha).step_by(sample) {
                let background = 256 - u32::from(person);
                sum += u32::from(value) * background;
                weight += background;
            }
            let at = cell_row + cell.min(across - 1);
            sums[at] += f64::from(sum);
            weights[at] += f64::from(weight);
        }
    }
    for _ in 0..BLUR_PASSES {
        for values in [&mut sums, &mut weights] {
            box_blur(values, across, down);
        }
    }
    sums.iter()
        .zip(&weights)
        .map(|(&sum, &weight)| to_u8(sum / weight.max(1.0)))
        .collect()
}

/// One box blur of [`BLUR_RADIUS`] along the rows, then along the columns, of a grid `across` by `down`; the edges
/// repeat.
fn box_blur(values: &mut [f64], across: usize, down: usize) {
    let mut line = Vec::new();
    for row in values.chunks_exact_mut(across) {
        blur_line(row, &mut line);
    }
    let mut column = vec![0.0; down];
    for x in 0..across {
        for (y, value) in column.iter_mut().enumerate() {
            *value = values[y * across + x];
        }
        blur_line(&mut column, &mut line);
        for (y, value) in column.iter().enumerate() {
            values[y * across + x] = *value;
        }
    }
}

/// A running box blur of [`BLUR_RADIUS`] over `values`, `scratch` its copy.
fn blur_line(values: &mut [f64], scratch: &mut Vec<f64>) {
    let Some(last) = values.len().checked_sub(1) else {
        return;
    };
    scratch.clear();
    scratch.extend_from_slice(values);
    let at = |index: isize| scratch[usize::try_from(index).unwrap_or(0).min(last)];
    let radius = isize::try_from(BLUR_RADIUS).unwrap_or(1);
    let width = f64::from(u32::try_from(2 * BLUR_RADIUS + 1).unwrap_or(1));
    let mut sum: f64 = (-radius..=radius).map(at).sum();
    for (index, value) in values.iter_mut().enumerate() {
        *value = sum / width;
        let index = isize::try_from(index).unwrap_or(0);
        sum += at(index + radius + 1) - at(index - radius);
    }
}

/// A value in `[0, 255]` as a byte.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to [0, 255] first"
)]
fn to_u8(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// `backdrop` as I420 at `width` by `height`: scaled to cover the frame and cropped to it, centred.
pub(super) fn backdrop_frame(backdrop: &Backdrop, width: u32, height: u32) -> Result<VideoFrame, String> {
    let picture = match backdrop {
        Backdrop::Stock(stock) => image::load_from_memory(stock.jpeg).map_err(|error| error.to_string())?,
        Backdrop::File(path) => read_picture(path)?,
    };
    let rgb = cover(&picture.to_rgb8(), width, height);
    let frame = VideoFrame {
        width,
        height,
        format: PixelFormat::Rgb,
        stride: width * 3,
        data: rgb.into_raw(),
    };
    to_i420(&frame, MaxSize { width, height }).ok_or_else(|| "the picture can't be converted".to_owned())
}

/// The picture file at `path`, decoded (refused when it is larger than [`MAX_PICTURE_FILE`]).
pub(super) fn read_picture(path: &Path) -> Result<image::DynamicImage, String> {
    let size = std::fs::metadata(path).map_err(|error| error.to_string())?.len();
    if size > MAX_PICTURE_FILE {
        return Err(format!(
            "the picture is {size} bytes, more than the {MAX_PICTURE_FILE} read"
        ));
    }
    ImageReader::open(path)
        .and_then(ImageReader::with_guessed_format)
        .map_err(|error| error.to_string())?
        .decode()
        .map_err(|error| error.to_string())
}

/// `picture` scaled to cover `width` by `height` (its shape kept) and cropped to it, centred.
#[must_use]
fn cover(picture: &RgbImage, width: u32, height: u32) -> RgbImage {
    let (from_width, from_height) = (u64::from(picture.width().max(1)), u64::from(picture.height().max(1)));
    let (to_width, to_height) = (u64::from(width), u64::from(height));
    // wider than the frame: its height fits and its sides are cropped; else its width fits
    let (scaled_width, scaled_height) = if from_width * to_height > from_height * to_width {
        ((from_width * to_height).div_ceil(from_height), to_height)
    } else {
        (to_width, (from_height * to_width).div_ceil(from_width))
    };
    let (scaled_width, scaled_height) = (
        u32::try_from(scaled_width).unwrap_or(width).max(width),
        u32::try_from(scaled_height).unwrap_or(height).max(height),
    );
    let scaled = imageops::resize(picture, scaled_width, scaled_height, FilterType::Triangle);
    let (left, top) = ((scaled_width - width) / 2, (scaled_height - height) / 2);
    imageops::crop_imm(&scaled, left, top, width, height).to_image()
}
