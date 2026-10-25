//! Tests only (`test-support`): the signals the audio processing is judged with, in 10 ms device frames, and their
//! levels read back.
//!
//! A steady tone is the wrong probe for the processing: noise suppression learns a sound that never changes as noise
//! and lowers it (a 440 Hz tone loses about 13 dB within two seconds), and an echo canceller can't tell one period of
//! it from the next. So the far end is [`speech_like`] (noise shaped into syllables, with pauses) and the near end is
//! a [`voice`]: a tone that moves between [`VOICE_HZ`] every segment and stops between them, as voiced speech does.

use crate::devices::{AudioFrame, FRAME_SAMPLES};
use crate::tone::{Tone, ToneGenerator, amplitude_at};

/// A voice segment: [`VOICED_FRAMES`] of a tone, then the rest silent.
pub const SEGMENT_FRAMES: usize = 25;
/// The voiced frames that start a segment (200 ms).
pub const VOICED_FRAMES: usize = 20;
/// The voice's frequencies, one per segment in turn; each fits a voiced stretch in whole periods.
pub const VOICE_HZ: [f32; 4] = [440.0, 660.0, 550.0, 880.0];

/// Deterministic white noise (xorshift64), samples uniform in `[-1, 1)`.
#[derive(Clone, Debug)]
pub struct Noise(u64);

impl Noise {
    /// Noise from `seed` (0 is taken as 1: xorshift never leaves 0).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_sample(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        // the top 24 bits, which an f32 holds exactly
        let top = u32::try_from(self.0 >> 40).unwrap_or(0);
        to_f32(f64::from(top) / f64::from(1_u32 << 24)).mul_add(2.0, -1.0)
    }

    /// Adds `peak` times the noise to every sample of `frames`.
    pub fn add_to(&mut self, frames: &mut [AudioFrame], peak: f32) {
        for frame in frames {
            for sample in frame.samples_mut() {
                *sample += peak * self.next_sample();
            }
        }
    }
}

/// `frames` of a speech-like far end at about `peak`: low-passed noise under 250 ms syllables, every fourth syllable
/// a pause.
#[must_use]
pub fn speech_like(frames: usize, seed: u64, peak: f32) -> Vec<AudioFrame> {
    const SYLLABLE: usize = 25;
    let mut noise = Noise::new(seed);
    let mut low = 0.0_f32;
    (0..frames)
        .map(|index| {
            let mut frame = AudioFrame::silence();
            let syllable = index / SYLLABLE;
            let envelope = if syllable % 4 == 3 {
                0.0
            } else {
                (ratio(index % SYLLABLE, SYLLABLE) * std::f32::consts::PI).sin()
            };
            for sample in frame.samples_mut() {
                low = 0.6f32.mul_add(low, 0.4 * noise.next_sample());
                *sample = peak * envelope * low;
            }
            frame
        })
        .collect()
}

/// `frames` of the [`voice`] at `amplitude`: segment `n` voiced at `VOICE_HZ[n % 4]`.
#[must_use]
pub fn voice(frames: usize, amplitude: f32) -> Vec<AudioFrame> {
    let mut generators = VOICE_HZ.map(|hz| ToneGenerator::new(Tone { hz, amplitude }));
    (0..frames)
        .map(|index| {
            let mut frame = AudioFrame::silence();
            if index % SEGMENT_FRAMES < VOICED_FRAMES {
                generators[(index / SEGMENT_FRAMES) % VOICE_HZ.len()].fill(frame.samples_mut());
            }
            frame
        })
        .collect()
}

/// The voice's mean amplitude in `frames` (a stream that started with segment 0), over the whole segments within
/// `from..to`, each judged on its voiced frames from `skip` on (a processing that delays the signal moves its onset
/// later). 0 when no whole segment lies in the range.
#[must_use]
pub fn voice_level(frames: &[AudioFrame], from: usize, to: usize, skip: usize) -> f32 {
    let to = to.min(frames.len());
    let levels: Vec<f32> = (from.div_ceil(SEGMENT_FRAMES)..to / SEGMENT_FRAMES)
        .map(|segment| {
            let start = segment * SEGMENT_FRAMES;
            let voiced = samples(&frames[start + skip..start + VOICED_FRAMES]);
            amplitude_at(&voiced, VOICE_HZ[segment % VOICE_HZ.len()])
        })
        .collect();
    if levels.is_empty() {
        return 0.0;
    }
    levels.iter().sum::<f32>() / ratio(levels.len(), 1)
}

/// The RMS level of the voice's silent frames within `from..to`, from `skip` frames after each voiced stretch ends.
#[must_use]
pub fn gap_level(frames: &[AudioFrame], from: usize, to: usize, skip: usize) -> f32 {
    let gaps: Vec<AudioFrame> = (from..to.min(frames.len()))
        .filter(|index| index % SEGMENT_FRAMES >= VOICED_FRAMES + skip)
        .map(|index| frames[index].clone())
        .collect();
    rms(&gaps)
}

/// The RMS level of `frames`; 0 for none.
#[must_use]
pub fn rms(frames: &[AudioFrame]) -> f32 {
    if frames.is_empty() {
        return 0.0;
    }
    let sum: f64 = frames
        .iter()
        .flat_map(AudioFrame::samples)
        .map(|&sample| f64::from(sample) * f64::from(sample))
        .sum();
    to_f32((sum / f64::from(ratio(frames.len() * FRAME_SAMPLES, 1))).sqrt())
}

/// `frames`' samples, one after another.
#[must_use]
pub fn samples(frames: &[AudioFrame]) -> Vec<f32> {
    frames
        .iter()
        .flat_map(|frame| frame.samples().iter().copied())
        .collect()
}

#[expect(clippy::cast_precision_loss, reason = "frame counts stay far below 2^24")]
fn ratio(part: usize, whole: usize) -> f32 {
    part as f32 / whole as f32
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "audio samples and levels fit an f32's range"
)]
fn to_f32(value: f64) -> f32 {
    value as f32
}
