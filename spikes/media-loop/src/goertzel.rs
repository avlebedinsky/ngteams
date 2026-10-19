//! Goertzel tone detection: the power of one frequency, its share of the signal, and the strongest frequency in a
//! scanned band.

use std::f64::consts::TAU;

/// Lowest frequency [`dominant_frequency`] looks at, in Hz.
pub const SCAN_MIN_HZ: u32 = 50;
/// Highest frequency [`dominant_frequency`] looks at, in Hz.
pub const SCAN_MAX_HZ: u32 = 4_000;
/// Scan step of [`dominant_frequency`], in Hz; the detected frequency is within half a step of the tone.
pub const SCAN_STEP_HZ: u32 = 5;
/// Share of the energy the strongest scanned frequency must carry for [`dominant_frequency`] to call it a tone.
pub const MIN_TONE_FRACTION: f64 = 0.1;

/// Squared magnitude of the signal's component at `freq_hz` (generalized Goertzel, any real frequency).
#[must_use]
pub fn power(samples: &[i16], sample_rate: u32, freq_hz: f64) -> f64 {
    let coeff = 2.0 * (TAU * freq_hz / f64::from(sample_rate)).cos();
    let (mut s1, mut s2) = (0.0, 0.0);
    for &sample in samples {
        let s0 = f64::from(sample) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - coeff * s1 * s2
}

/// Share of the signal's energy at `freq_hz`: about 1 for a pure tone at that frequency, about 0 far from it, and 0
/// for silence.
#[must_use]
pub fn energy_fraction(samples: &[i16], sample_rate: u32, freq_hz: f64) -> f64 {
    let energy = energy(samples);
    if energy == 0.0 {
        return 0.0;
    }
    2.0 * power(samples, sample_rate, freq_hz) / (len_f64(samples.len()) * energy)
}

/// The scanned frequency (`SCAN_MIN_HZ..=SCAN_MAX_HZ` in `SCAN_STEP_HZ` steps) carrying the most energy, or `None` for
/// silence, an empty buffer, or a signal with no clear tone.
///
/// Each frequency's energy share is averaged over 100 ms blocks, so each block's main lobe (±10 Hz) is wider than the
/// scan step and a tone between two steps is not lost in a sidelobe.
#[must_use]
pub fn dominant_frequency(samples: &[i16], sample_rate: u32) -> Option<f64> {
    let energy = energy(samples);
    if energy == 0.0 {
        return None;
    }
    let block = usize::try_from(sample_rate / 10).ok()?.max(1);
    let (best_hz, best_power) = (SCAN_MIN_HZ..=SCAN_MAX_HZ)
        .step_by(usize::try_from(SCAN_STEP_HZ).ok()?)
        .map(|hz| {
            let freq = f64::from(hz);
            let total: f64 = samples
                .chunks(block)
                .map(|chunk| energy_fraction(chunk, sample_rate, freq))
                .sum();
            (freq, total)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    let blocks = len_f64(samples.len().div_ceil(block));
    (best_power / blocks >= MIN_TONE_FRACTION).then_some(best_hz)
}

fn energy(samples: &[i16]) -> f64 {
    samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum()
}

#[expect(clippy::cast_precision_loss, reason = "sample counts stay far below 2^52")]
fn len_f64(len: usize) -> f64 {
    len as f64
}
