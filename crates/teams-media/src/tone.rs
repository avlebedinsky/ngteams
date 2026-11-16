//! A test tone: a sine generated frame after frame without phase jumps, and its level read back (Goertzel), at the
//! device seam's rate ([`SAMPLE_RATE`](crate::devices::SAMPLE_RATE)).

use std::f64::consts::TAU;

use crate::devices::SAMPLE_RATE;

/// A sine tone: its frequency in Hz and its peak as a fraction of full scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    pub hz: f32,
    pub amplitude: f32,
}

/// Generates a [`Tone`]; each [`ToneGenerator::fill`] continues where the last one stopped.
#[derive(Clone, Debug)]
pub struct ToneGenerator {
    /// Phase advance per sample, in radians.
    step: f64,
    peak: f64,
    /// Kept in `[0, TAU)`.
    phase: f64,
}

impl ToneGenerator {
    #[must_use]
    pub fn new(tone: Tone) -> Self {
        Self {
            step: TAU * f64::from(tone.hz) / f64::from(SAMPLE_RATE),
            peak: f64::from(tone.amplitude),
            phase: 0.0,
        }
    }

    /// Fills `out` with the tone's next samples.
    pub fn fill(&mut self, out: &mut [f32]) {
        for sample in out {
            *sample = to_f32(self.peak * self.phase.sin());
            self.phase = (self.phase + self.step) % TAU;
        }
    }
}

/// The peak amplitude of the component at `hz` in `samples` (at [`SAMPLE_RATE`]): about a tone's amplitude for a
/// tone at that frequency over a whole number of periods, about 0 far from it, 0 for silence or no samples.
#[must_use]
pub fn amplitude_at(samples: &[f32], hz: f32) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let coeff = 2.0 * (TAU * f64::from(hz) / f64::from(SAMPLE_RATE)).cos();
    let (mut s1, mut s2) = (0.0_f64, 0.0_f64);
    for &sample in samples {
        let s0 = f64::from(sample) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0);
    to_f32(2.0 * power.sqrt() / len_f64(samples.len()))
}

/// The mean amplitude of the component at `hz` over `samples`' 100 ms windows ([`amplitude_at`] each): a tone's level
/// even when its phase moves between them, as a jitter buffer stretching or concealing around a loss moves it (one
/// long window would read that as a weaker tone). Fewer samples than one window are read as one.
#[must_use]
pub fn level_at(samples: &[f32], hz: f32) -> f32 {
    let window = usize::try_from(SAMPLE_RATE / 10).unwrap_or(usize::MAX);
    if samples.len() < window {
        return amplitude_at(samples, hz);
    }
    let levels: Vec<f32> = samples
        .chunks_exact(window)
        .map(|chunk| amplitude_at(chunk, hz))
        .collect();
    to_f32(levels.iter().map(|level| f64::from(*level)).sum::<f64>() / len_f64(levels.len()))
}

/// Decibels of `level` against `reference` (both amplitudes); `-inf` for a silent `level`.
#[must_use]
pub fn decibels(level: f32, reference: f32) -> f32 {
    20.0 * (level / reference).log10()
}

/// How purely `samples` (at [`SAMPLE_RATE`]) carry a tone at `hz`: the amplitude of the sine at `hz` that fits them
/// best, and the decibels of that sine over the rest (a gap, a click or noise). A silent window reads as 0 dB.
#[must_use]
pub fn purity(samples: &[f32], hz: f32) -> (f32, f32) {
    let step = TAU * f64::from(hz) / f64::from(SAMPLE_RATE);
    let basis = |n: usize| {
        let phase = step * len_f64(n);
        (phase.sin(), phase.cos())
    };
    let (mut ss, mut sc, mut cc, mut xs, mut xc) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (n, &x) in samples.iter().enumerate() {
        let (s, c) = basis(n);
        let x = f64::from(x);
        (ss, sc, cc, xs, xc) = (ss + s * s, sc + s * c, cc + c * c, xs + x * s, xc + x * c);
    }
    let det = ss * cc - sc * sc;
    if samples.is_empty() || det.abs() < f64::EPSILON {
        return (0.0, 0.0);
    }
    let (a, b) = ((xs * cc - xc * sc) / det, (xc * ss - xs * sc) / det);
    let (mut tone, mut rest) = (0.0, 0.0);
    for (n, &x) in samples.iter().enumerate() {
        let (s, c) = basis(n);
        let fit = a * s + b * c;
        tone += fit * fit;
        rest += (f64::from(x) - fit).powi(2);
    }
    let db = if tone > 0.0 {
        10.0 * (tone / rest.max(tone * 1e-12)).log10()
    } else {
        0.0
    };
    (to_f32(a.hypot(b)), to_f32(db))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "audio samples and levels fit an f32's range"
)]
fn to_f32(value: f64) -> f32 {
    value as f32
}

#[expect(clippy::cast_precision_loss, reason = "sample counts stay far below 2^52")]
fn len_f64(len: usize) -> f64 {
    len as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purity_reads_a_clean_tone_high_and_a_cut_or_noise_low() {
        let tone = Tone {
            hz: 1000.0,
            amplitude: 0.5,
        };
        let mut samples = vec![0.0; 960];
        ToneGenerator::new(tone).fill(&mut samples);
        let (amplitude, clean) = purity(&samples, tone.hz);
        assert!((amplitude - 0.5).abs() < 1e-3, "{amplitude}");
        assert!(clean > 80.0, "{clean}");
        let mut cut = samples.clone();
        cut[100..160].fill(0.0);
        let (_, db) = purity(&cut, tone.hz);
        assert!((5.0..20.0).contains(&db), "a 60-sample gap reads {db} dB");
        let noisy: Vec<f32> = samples
            .iter()
            .enumerate()
            .map(|(n, s)| s + if n % 2 == 0 { 0.1 } else { -0.1 })
            .collect();
        let (_, db) = purity(&noisy, tone.hz);
        assert!((10.0..20.0).contains(&db), "noise at a fifth of the tone reads {db} dB");
        assert_eq!(purity(&[], tone.hz), (0.0, 0.0));
        assert_eq!(purity(&[0.0; 960], tone.hz), (0.0, 0.0));
    }
}
