//! Where the person is in a frame: MediaPipe's selfie segmenter (landscape) on tract, its confidence smoothed over
//! frames, as an 8-bit mask at the model's size (255 surely the person, 0 surely the background; the compositor
//! sharpens the edge once it scaled the mask up).
//!
//! The model runs on a thread of its own (`teams-media-seg`), so the camera's thread never waits for it: each frame
//! hands it the frame scaled to the model's size (the newest one replaces one it hasn't started), and takes the newest
//! mask it made. The mask so trails the picture by about one run of the model, which the smoothing hides. Until the
//! first mask comes (the model loading, a fraction of a second) the whole picture is the background, never the
//! camera's picture as it is.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
#[cfg(test)]
use std::time::{Duration, Instant};

use tract_onnx::prelude::{
    DatumExt as _, Framework as _, InferenceModelExt as _, IntoRunnable as _, Tensor, TractResult, TypedRunnableModel,
    tvec,
};

use crate::devices::VideoFrame;
use crate::video::convert::to_rgb;

/// The model's input width, px.
pub const MODEL_WIDTH: usize = 256;
/// The model's input height, px.
pub const MODEL_HEIGHT: usize = 144;
/// The pixels of the model's input and of its mask.
const MODEL_PIXELS: usize = MODEL_WIDTH * MODEL_HEIGHT;

/// The model, converted to ONNX (`tools/selfie-segmenter/convert.py`).
const MODEL: &[u8] = include_bytes!("../../../assets/segmentation/selfie_segmenter_landscape.onnx");

/// How much of a new mask is its own; the rest is the masks before it (less flicker at the edges).
const NEW_WEIGHT: f32 = 0.7;

pub(super) type Plan = Arc<TypedRunnableModel>;

/// The model, loaded once per process ([`plan`]).
static PLAN: OnceLock<Result<Plan, String>> = OnceLock::new();

/// The model ready to run, loaded the first time it is asked for.
pub(super) fn plan() -> Result<Plan, String> {
    PLAN.get_or_init(|| load().map_err(|error| format!("{error:#}")))
        .clone()
}

fn load() -> TractResult<Plan> {
    let fact = f32::fact([1, 3, MODEL_HEIGHT, MODEL_WIDTH]);
    tract_onnx::onnx()
        .model_for_read(&mut &MODEL[..])?
        .with_input_fact(0, fact.into())?
        .into_optimized()?
        .into_runnable()
}

/// Runs the model on `input` ([`model_input`]): the person's confidence per pixel of the model's size.
pub(super) fn run(plan: &Plan, input: &[f32]) -> Result<Vec<f32>, String> {
    let input = Tensor::from_shape(&[1, 3, MODEL_HEIGHT, MODEL_WIDTH], input).map_err(|error| error.to_string())?;
    let outputs = plan.run(tvec!(input.into())).map_err(|error| format!("{error:#}"))?;
    let output = outputs.first().ok_or("the model gave no output")?;
    let view = output.to_plain_array_view::<f32>().map_err(|error| error.to_string())?;
    if view.len() != MODEL_PIXELS {
        return Err(format!("the model gave {} values", view.len()));
    }
    Ok(view.iter().copied().collect())
}

/// What the camera's thread and the model's share.
#[derive(Default)]
struct Exchange {
    state: Mutex<ExchangeState>,
    /// Wakes the model's thread: an input came, or it is to stop.
    wake: Condvar,
}

#[derive(Default)]
struct ExchangeState {
    /// The newest input the model hasn't taken yet.
    input: Option<Vec<f32>>,
    /// The newest mask's confidence and its count (from 1).
    output: Option<(u64, Vec<f32>)>,
    stop: bool,
}

impl Exchange {
    fn lock(&self) -> MutexGuard<'_, ExchangeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The model's thread.
struct Worker {
    exchange: Arc<Exchange>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    fn start() -> Result<Self, String> {
        let exchange = Arc::new(Exchange::default());
        let shared = Arc::clone(&exchange);
        let handle = thread::Builder::new()
            .name("teams-media-seg".into())
            .spawn(move || segment_loop(&shared))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            exchange,
            handle: Some(handle),
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.exchange.lock().stop = true;
        self.exchange.wake.notify_all();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The model's thread: runs the model on each newest input until told to stop. A model that can't load or run gives
/// masks of no person (said once).
fn segment_loop(exchange: &Exchange) {
    let plan = plan();
    let mut count = 0_u64;
    let mut said = false;
    loop {
        let input = {
            let mut state = exchange.lock();
            loop {
                if state.stop {
                    return;
                }
                if let Some(input) = state.input.take() {
                    break input;
                }
                state = exchange.wake.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let confidence = match plan.as_ref().map_err(Clone::clone).and_then(|plan| run(plan, &input)) {
            Ok(confidence) => confidence,
            Err(error) => {
                if !said {
                    log::warn!("the person segmentation can't run, the whole picture is the background: {error}");
                    said = true;
                }
                vec![0.0; MODEL_PIXELS]
            }
        };
        count += 1;
        exchange.lock().output = Some((count, confidence));
    }
}

/// The segmenter of one camera thread: the model's thread (started on the first frame), and the masks so far.
#[derive(Default)]
pub(super) struct Segmenter {
    worker: Option<Worker>,
    /// The masks so far, smoothed; empty before the first.
    confidence: Vec<f32>,
    /// The count of the newest mask taken.
    taken: u64,
    /// The mask handed out last.
    mask: Vec<u8>,
}

impl Segmenter {
    /// Forgets the masks so far and stops the model's thread (the effects were turned off).
    pub(super) fn forget(&mut self) {
        self.worker = None;
        self.confidence.clear();
        self.taken = 0;
    }

    /// The mask for `frame` (I420), [`MODEL_WIDTH`] by [`MODEL_HEIGHT`]: 255 where the person is, as the newest mask
    /// the model made says; all background until it made one. `frame` goes to the model for the masks to come.
    pub(super) fn mask(&mut self, frame: &VideoFrame) -> &[u8] {
        if self.worker.is_none() {
            match Worker::start() {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => log::warn!("the person segmentation can't start: {error}"),
            }
        }
        if let Some(worker) = &self.worker {
            worker.exchange.lock().input = Some(model_input(frame));
            worker.exchange.wake.notify_one();
            let newest = worker.exchange.lock().output.clone();
            if let Some((count, confidence)) = newest.filter(|(count, _)| *count > self.taken) {
                self.taken = count;
                self.blend(confidence);
            }
        }
        self.mask.clear();
        if self.confidence.len() == MODEL_PIXELS {
            self.mask.extend(self.confidence.iter().map(|&value| unit_to_u8(value)));
        } else {
            self.mask.resize(MODEL_PIXELS, 0);
        }
        &self.mask
    }

    /// Takes a new mask's confidence into the ones so far.
    fn blend(&mut self, confidence: Vec<f32>) {
        if self.confidence.len() == MODEL_PIXELS {
            for (kept, new) in self.confidence.iter_mut().zip(&confidence) {
                *kept = *kept * (1.0 - NEW_WEIGHT) + new * NEW_WEIGHT;
            }
        } else {
            self.confidence = confidence;
        }
    }

    /// Waits up to `timeout` for the model to have made a mask of `frame` (a test's frame, the same each time).
    #[cfg(test)]
    pub(super) fn settle(&mut self, frame: &VideoFrame, timeout: Duration) {
        let until = Instant::now() + timeout;
        let start = self.taken;
        while Instant::now() < until && self.taken < start + 3 {
            let _ = self.mask(frame);
            thread::sleep(Duration::from_millis(5));
        }
    }
}

/// `frame` (I420) scaled to the model's size as planar RGB in `[0, 1]`: each input pixel the mean of the luma under
/// it (up to 4 by 4 samples) with the chroma at its centre.
pub(super) fn model_input(frame: &VideoFrame) -> Vec<f32> {
    let (width, height) = (to_usize(frame.width), to_usize(frame.height));
    let (luma, chroma) = frame.data.split_at(width * height);
    let (u_plane, v_plane) = chroma.split_at(width * height / 4);
    let mut input = vec![0.0_f32; 3 * MODEL_PIXELS];
    let (red, rest) = input.split_at_mut(MODEL_PIXELS);
    let (green, blue) = rest.split_at_mut(MODEL_PIXELS);
    let columns: Vec<(usize, usize)> = (0..MODEL_WIDTH).map(|x| span(x, MODEL_WIDTH, width)).collect();
    for y in 0..MODEL_HEIGHT {
        let rows = span(y, MODEL_HEIGHT, height);
        for (x, &(left, right)) in columns.iter().enumerate() {
            let luma_mean = mean(luma, width, (left, right), rows);
            let at = ((rows.0 + rows.1) / 4) * (width / 2) + (left + right) / 4;
            let cb = u_plane.get(at).copied().unwrap_or(128);
            let cr = v_plane.get(at).copied().unwrap_or(128);
            let (r, g, b) = to_rgb(luma_mean, cb, cr);
            let index = y * MODEL_WIDTH + x;
            red[index] = f32::from(r) / 255.0;
            green[index] = f32::from(g) / 255.0;
            blue[index] = f32::from(b) / 255.0;
        }
    }
    input
}

/// The source pixels `[start, end)` model pixel `index` of `count` covers in a side of `size` pixels, sampled at most
/// 4 wide.
fn span(index: usize, count: usize, size: usize) -> (usize, usize) {
    let start = index * size / count;
    let end = ((index + 1) * size / count).max(start + 1);
    (start, end.min(start + 4))
}

/// The mean of `plane`'s values in the columns and rows given.
fn mean(plane: &[u8], stride: usize, columns: (usize, usize), rows: (usize, usize)) -> u8 {
    let mut sum = 0_u32;
    let mut count = 0_u32;
    for row in rows.0..rows.1 {
        if let Some(line) = plane.get(row * stride + columns.0..row * stride + columns.1) {
            sum += line.iter().map(|&value| u32::from(value)).sum::<u32>();
            count += u32::try_from(line.len()).unwrap_or(0);
        }
    }
    if count == 0 {
        return 0;
    }
    u8::try_from(sum / count).unwrap_or(u8::MAX)
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

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
