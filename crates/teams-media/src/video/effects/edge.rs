//! The person's edge, made soft the way Teams draws it. The model's mask is coarse (a model pixel is five of a 720p
//! frame's), so scaled up as it is its edge is a blocky line that cuts through hair and shoulders. Here it is
//!
//! 1. **refined against the frame's own picture**: a guided filter (He, Sun and Tang) with the frame's luma as its
//!    guide moves the mask's edge onto the edge the frame shows near it, so it hugs the person rather than the
//!    model's steps;
//! 2. **settled**: what the filter left half-sure inside the person or out in the room is pushed back to sure (a
//!    smoothstep over [`SETTLE`]), so neither the person goes see-through nor the room shows through the blur;
//! 3. **feathered**: blurred a few pixels ([`FEATHER_RADIUS`]), so the person fades into the background over a narrow
//!    band instead of a cut line, as Teams' blur does.
//!
//! All of it at a working size of at most [`WORK_MAX`] (the frame's luma scaled down to it, about a 720p frame's
//! fifth), where it costs a few milliseconds; the compositor scales the result up to the frame.

use super::compose::Resampler;
use super::segment::{MODEL_HEIGHT, MODEL_WIDTH};

/// The largest working size, px: the edge is refined at the frame's shape scaled to fit it.
pub(super) const WORK_MAX: (usize, usize) = (512, 288);
/// The guided filter's window radius, working pixels: how far the mask's edge may move onto the frame's.
const GUIDE_RADIUS: usize = 4;
/// The guided filter's regularization: the luma variance (luma in `[0, 1]`) below which a window counts as flat (its
/// mask kept as the model made it, rather than following the frame's texture).
const GUIDE_EPSILON: f32 = 0.004;
/// The refined mask's values below which a pixel is the background, and above which it is the person; between them
/// the edge, smoothstepped.
const SETTLE: (f32, f32) = (0.2, 0.8);
/// The feather's radius, working pixels, and its passes (two box blurs: a soft ramp with no corners).
const FEATHER_RADIUS: usize = 2;
const FEATHER_PASSES: usize = 2;

/// The working size for frames `width` by `height`: their shape scaled down to fit [`WORK_MAX`] (never up), each side
/// at least 2.
pub(super) fn work_size(width: usize, height: usize) -> (usize, usize) {
    let (max_width, max_height) = WORK_MAX;
    let (width, height) = (width.max(1), height.max(1));
    let size = if width <= max_width && height <= max_height {
        (width, height)
    } else if width * max_height >= height * max_width {
        (max_width, (height * max_width).div_ceil(width))
    } else {
        ((width * max_height).div_ceil(height), max_height)
    };
    (size.0.max(2), size.1.max(2))
}

/// The edge refiner of one camera thread: its scalings and scratch, kept between frames of one size.
#[derive(Default)]
pub(super) struct EdgeRefiner {
    /// The model's mask to the working size, and the frame's luma to it.
    mask_up: Option<Resampler>,
    luma_down: Option<Resampler>,
    /// The working size's planes: the mask, the guide (luma), and the filter's scratch.
    mask: Vec<u8>,
    guide: Vec<u8>,
    planes: Planes,
    /// The refined mask, 0..=255, at the working size.
    alpha: Vec<u8>,
}

/// The guided filter's planes at the working size, kept between frames.
#[derive(Default)]
struct Planes {
    guide: Vec<f32>,
    mask: Vec<f32>,
    guide_mask: Vec<f32>,
    guide_squared: Vec<f32>,
    guide_mean: Vec<f32>,
    scratch: Vec<f32>,
}

impl EdgeRefiner {
    /// The person's weight per pixel (255 the person, 0 the background) at the working size for a frame `width` by
    /// `height` of luma `luma` (stride its width), the model's `mask` ([`MODEL_WIDTH`] by [`MODEL_HEIGHT`]) refined
    /// as the module docs say. Returns the weights and the working size.
    pub(super) fn refine(&mut self, mask: &[u8], luma: &[u8], width: usize, height: usize) -> (&[u8], (usize, usize)) {
        let work = work_size(width, height);
        let mask_up = Resampler::kept(self.mask_up.take(), (MODEL_WIDTH, MODEL_HEIGHT), work);
        let luma_down = Resampler::kept(self.luma_down.take(), (width, height), work);
        mask_up.scale(mask, &mut self.mask);
        luma_down.scale(luma, &mut self.guide);
        self.mask_up = Some(mask_up);
        self.luma_down = Some(luma_down);
        self.planes.filter(&self.guide, &self.mask, work);
        let refined = &mut self.planes.mask;
        for value in refined.iter_mut() {
            *value = smoothstep(SETTLE.0, SETTLE.1, *value);
        }
        for _ in 0..FEATHER_PASSES {
            box_mean(refined, work, FEATHER_RADIUS, &mut self.planes.scratch);
        }
        self.alpha.clear();
        self.alpha.extend(refined.iter().map(|&value| unit_to_u8(value)));
        (&self.alpha, work)
    }
}

impl Planes {
    /// The guided filter of `mask` by `guide` (both 0..=255, `size`), its result in [`Planes::mask`] in `[0, 1]`.
    fn filter(&mut self, guide: &[u8], mask: &[u8], size: (usize, usize)) {
        let unit = |values: &[u8], out: &mut Vec<f32>| {
            out.clear();
            out.extend(values.iter().map(|&value| f32::from(value) / 255.0));
        };
        unit(guide, &mut self.guide);
        unit(mask, &mut self.mask);
        let Self {
            guide,
            mask,
            guide_mask,
            guide_squared,
            guide_mean,
            scratch,
        } = self;
        guide_mask.clear();
        guide_mask.extend(guide.iter().zip(mask.iter()).map(|(i, p)| i * p));
        guide_squared.clear();
        guide_squared.extend(guide.iter().map(|i| i * i));
        // the window means: of the guide (kept aside, the guide itself is needed again), the mask, and the products
        guide_mean.clone_from(guide);
        for plane in [&mut *guide_mean, &mut *mask, &mut *guide_mask, &mut *guide_squared] {
            box_mean(plane, size, GUIDE_RADIUS, scratch);
        }
        // per window, the line mask = a·guide + b that fits best: a into guide_mask, b into mask
        for (((a, b), &mean_i), &mean_ii) in guide_mask
            .iter_mut()
            .zip(mask.iter_mut())
            .zip(guide_mean.iter())
            .zip(guide_squared.iter())
        {
            let variance = (mean_ii - mean_i * mean_i).max(0.0);
            let slope = (*a - mean_i * *b) / (variance + GUIDE_EPSILON);
            *a = slope;
            *b -= slope * mean_i;
        }
        box_mean(guide_mask, size, GUIDE_RADIUS, scratch);
        box_mean(mask, size, GUIDE_RADIUS, scratch);
        for ((out, &a), &i) in mask.iter_mut().zip(guide_mask.iter()).zip(guide.iter()) {
            *out = (a * i + *out).clamp(0.0, 1.0);
        }
    }
}

/// `plane` (`size`, rows of its width) replaced by the mean of each pixel's window of `radius` each way (clipped at
/// the edges: the window's mean over the pixels it holds). `scratch` is kept by the caller.
pub(super) fn box_mean(plane: &mut [f32], size: (usize, usize), radius: usize, scratch: &mut Vec<f32>) {
    let (width, height) = size;
    if width == 0 || height == 0 || plane.len() < width * height {
        return;
    }
    let plane = &mut plane[..width * height];
    for row in plane.chunks_exact_mut(width) {
        mean_row(row, radius, scratch);
    }
    mean_columns(plane, width, radius, scratch);
}

/// The running window mean along one row.
fn mean_row(values: &mut [f32], radius: usize, scratch: &mut Vec<f32>) {
    let count = values.len();
    scratch.clear();
    scratch.extend_from_slice(values);
    let mut sum: f32 = scratch.iter().take(radius).sum();
    for (index, value) in values.iter_mut().enumerate() {
        if let Some(&entering) = scratch.get(index + radius) {
            sum += entering;
        }
        if let Some(&leaving) = index.checked_sub(radius + 1).and_then(|at| scratch.get(at)) {
            sum -= leaving;
        }
        *value = sum / window(index, radius, count);
    }
}

/// The running window mean down every column at once, a row at a time (the rows' sums kept in a row of their own:
/// the plane is read in order, never down a column).
fn mean_columns(plane: &mut [f32], width: usize, radius: usize, scratch: &mut Vec<f32>) {
    let height = plane.len() / width;
    scratch.clear();
    scratch.extend_from_slice(plane);
    let source = &scratch[..];
    let row = |index: usize| source.get(index * width..(index + 1) * width);
    let mut sums = vec![0.0_f32; width];
    for index in 0..radius {
        if let Some(line) = row(index) {
            sums.iter_mut().zip(line).for_each(|(sum, value)| *sum += value);
        }
    }
    for (index, out) in plane.chunks_exact_mut(width).enumerate() {
        if let Some(entering) = row(index + radius) {
            sums.iter_mut().zip(entering).for_each(|(sum, value)| *sum += value);
        }
        if let Some(leaving) = index.checked_sub(radius + 1).and_then(row) {
            sums.iter_mut().zip(leaving).for_each(|(sum, value)| *sum -= value);
        }
        let count = window(index, radius, height);
        out.iter_mut().zip(&sums).for_each(|(out, sum)| *out = sum / count);
    }
}

/// How many of `count` values the window of `radius` around `index` holds.
fn window(index: usize, radius: usize, count: usize) -> f32 {
    let first = index.saturating_sub(radius);
    let last = (index + radius).min(count.saturating_sub(1));
    to_f32(last - first + 1)
}

/// `value` from 0 at `low` to 1 at `high`, eased at both ends.
pub(super) fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A count as a float: the planes are a few hundred thousand pixels at most.
#[expect(clippy::cast_precision_loss, reason = "a window or plane size, far below f32's 2^24")]
fn to_f32(value: usize) -> f32 {
    value as f32
}

/// A value in `[0, 1]` as 0..=255.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to [0, 255] first"
)]
fn unit_to_u8(value: f32) -> u8 {
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}
