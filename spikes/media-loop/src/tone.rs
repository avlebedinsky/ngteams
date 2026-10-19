//! A sine tone as 16-bit PCM, generated frame by frame without phase jumps between frames.

use std::f64::consts::TAU;

/// Generates a continuous sine tone; consecutive [`ToneGenerator::fill`] calls continue where the last one stopped.
#[derive(Debug, Clone)]
pub struct ToneGenerator {
    /// Phase advance per sample, in radians.
    step: f64,
    /// Peak value in PCM units.
    peak: f64,
    /// Current phase, kept in `[0, TAU)`.
    phase: f64,
}

impl ToneGenerator {
    /// A tone of `freq_hz` at `sample_rate`, with `amplitude` as a fraction of full scale (values over 1 clip).
    #[must_use]
    pub fn new(freq_hz: f64, sample_rate: u32, amplitude: f64) -> Self {
        Self {
            step: TAU * freq_hz / f64::from(sample_rate),
            peak: amplitude * f64::from(i16::MAX),
            phase: 0.0,
        }
    }

    /// Fills `out` with the next samples of the tone.
    pub fn fill(&mut self, out: &mut [i16]) {
        for sample in out {
            *sample = to_pcm(self.peak * self.phase.sin());
            self.phase = (self.phase + self.step) % TAU;
        }
    }
}

/// Rounds and clips a sample to the 16-bit range.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is clamped to the i16 range first"
)]
fn to_pcm(value: f64) -> i16 {
    value.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}
