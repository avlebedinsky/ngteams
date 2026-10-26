//! What the run hears: a sandbox's speaker recording read in part as it grows, the level of a tone in it, and a
//! keyed tone (on and off in 200 ms slots of a shift register's pattern) whose every slot a played microphone marks
//! on the wall clock, so the delay of each stretch of it in another sandbox's recording can be measured on the one
//! clock of this machine ([`lags`]).

use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use teams_media::devices::SAMPLE_RATE;
use teams_media::tone::{Tone, ToneGenerator, amplitude_at};
use ui_sandbox::pipewire::timeline::Timeline;

/// The recording's WAV header (`teams_media::devices::wav`'s writer: 16-bit PCM mono).
const HEADER: u64 = 44;
/// Samples per millisecond at the devices' rate.
pub const PER_MS: usize = (SAMPLE_RATE / 1_000) as usize;

/// A sandbox's speaker recording, read in part while it is written.
#[derive(Clone)]
pub struct Recording {
    pub path: PathBuf,
    pub timeline: Timeline,
}

impl Recording {
    /// How many samples the file holds so far.
    pub fn len(&self) -> usize {
        std::fs::metadata(&self.path).map_or(0, |meta| {
            usize::try_from(meta.len().saturating_sub(HEADER) / 2).unwrap_or(0)
        })
    }

    /// Samples `from..to` (as far as the file holds them).
    pub fn slice(&self, from: usize, to: usize) -> Vec<f32> {
        let to = to.min(self.len());
        if from >= to {
            return Vec::new();
        }
        let Ok(mut file) = File::open(&self.path) else {
            return Vec::new();
        };
        let mut bytes = vec![0; (to - from) * 2];
        let offset = HEADER + u64::try_from(from * 2).unwrap_or(u64::MAX);
        if file.seek(SeekFrom::Start(offset)).is_err() || file.read_exact(&mut bytes).is_err() {
            return Vec::new();
        }
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32_768.0)
            .collect()
    }

    /// The last `stretch` of it.
    pub fn tail(&self, stretch: Duration) -> Vec<f32> {
        let len = self.len();
        let wanted = usize::try_from(stretch.as_millis()).unwrap_or(usize::MAX) * PER_MS;
        self.slice(len.saturating_sub(wanted), len)
    }

    /// What it heard between `from_us` and `to_us` (Unix µs), by its timeline.
    pub fn between(&self, from_us: i64, to_us: i64) -> Vec<f32> {
        let (Some(from), Some(to)) = (self.timeline.sample_at(from_us), self.timeline.sample_at(to_us)) else {
            return Vec::new();
        };
        self.slice(to_index(from), to_index(to))
    }
}

fn to_index(sample: u64) -> usize {
    usize::try_from(sample).unwrap_or(usize::MAX)
}

/// The loudest `hz` gets in any `window` of `samples` (a quarter window apart).
pub fn loudest(samples: &[f32], hz: f32, window: Duration) -> f32 {
    let window = usize::try_from(window.as_millis()).unwrap_or(usize::MAX) * PER_MS;
    if samples.len() < window || window == 0 {
        return 0.0;
    }
    (0..=samples.len() - window)
        .step_by((window / 4).max(1))
        .map(|at| amplitude_at(&samples[at..at + window], hz))
        .fold(0.0, f32::max)
}

/// How many dB `level` is under `reference` (a level of nothing counts as 1e-6).
pub fn db_under(reference: f32, level: f32) -> f32 {
    20.0 * (reference.max(1e-6) / level.max(1e-6)).log10()
}

// --- the speech-like signal ---

/// The frequencies a speech-like signal's level is read at ([`speech_level`]): inside its band, clear of the run's
/// tones (440, 550, 660, 990 Hz, the ringtone's 784 and 1047) and their first harmonics.
pub const SPEECH_BINS: [f32; 6] = [250.0, 350.0, 500.0, 730.0, 1_200.0, 1_700.0];

/// `seconds` of a speech-like signal at about `peak`, as `teams_media::test_signal::speech_like` makes the far end the
/// echo canceller is judged with: low-passed noise (a one-pole filter, about 3 kHz) under 250 ms syllables, every
/// fourth one a pause. A steady tone is the wrong probe: noise suppression lowers it as noise, and an echo canceller
/// can't tell one period of it from the next.
pub fn speech_like(seconds: usize, seed: u64, peak: f32) -> Arc<[f32]> {
    let syllable = 250 * PER_MS;
    let mut state = seed.max(1);
    let mut low = 0.0_f32;
    (0..seconds * 1_000 * PER_MS)
        .map(|at| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let top = u32::try_from(state >> 40).unwrap_or(0);
            let white = f32::from(u16::try_from(top >> 8).unwrap_or(0)) / 32_768.0 - 1.0;
            low = 0.6f32.mul_add(low, 0.4 * white);
            let index = at / syllable;
            let into =
                f32::from(u16::try_from(at % syllable).unwrap_or(0)) / f32::from(u16::try_from(syllable).unwrap_or(1));
            let envelope = if index % 4 == 3 {
                0.0
            } else {
                (into * std::f32::consts::PI).sin()
            };
            peak * envelope * low
        })
        .collect()
}

/// A speech-like signal's level in `samples`: the median, over 400 ms windows, of its mean level at [`SPEECH_BINS`]
/// (a tone elsewhere left out).
pub fn speech_level(samples: &[f32]) -> f32 {
    let window = 400 * PER_MS;
    let mut levels: Vec<f32> = samples
        .chunks_exact(window)
        .map(|chunk| SPEECH_BINS.iter().map(|hz| amplitude_at(chunk, *hz)).sum::<f32>() / 6.0)
        .collect();
    if levels.is_empty() {
        return 0.0;
    }
    levels.sort_by(f32::total_cmp);
    levels[levels.len() / 2]
}

// --- the keyed tone ---

/// Samples per slot of the keyed tone (200 ms).
const SLOT_SAMPLES: usize = 200 * PER_MS;
/// The fade at each edge of an on slot (5 ms): no click for the codec to smear.
const RAMP: usize = 5 * PER_MS;

/// The on/off pattern of a degree-9 maximal-length shift register (x⁹ + x⁵ + 1): 511 slots (about 102 s), so a
/// delay under that is never taken for another.
pub fn pattern() -> Vec<bool> {
    let mut state: u16 = 0x1FF;
    (0..511)
        .map(|_| {
            let bit = ((state >> 8) ^ (state >> 4)) & 1;
            state = ((state << 1) | bit) & 0x1FF;
            bit == 1
        })
        .collect()
}

/// `tone` keyed by [`pattern`], each on slot's edges faded: what the run's keyed microphone plays over and over.
pub fn keyed(tone: Tone) -> Arc<[f32]> {
    let slots = pattern();
    let mut samples = vec![0.0; slots.len() * SLOT_SAMPLES];
    ToneGenerator::new(tone).fill(&mut samples);
    for (slot, on) in slots.iter().enumerate() {
        let part = &mut samples[slot * SLOT_SAMPLES..(slot + 1) * SLOT_SAMPLES];
        if !on {
            part.fill(0.0);
            continue;
        }
        for at in 0..RAMP {
            let gain = (f32::from(u16::try_from(at).unwrap_or(0)) + 0.5) / f32::from(u16::try_from(RAMP).unwrap_or(1));
            part[at] *= gain;
            part[SLOT_SAMPLES - 1 - at] *= gain;
        }
    }
    Arc::from(samples)
}

/// Whether the keyed microphone whose plays `played` marks was in an on slot at `unix_us`; `None` before it played.
fn keyed_on(played: &Timeline, slots: &[bool], unix_us: i64) -> Option<bool> {
    let sample = to_index(played.sample_at(unix_us)?);
    let slot = (sample / SLOT_SAMPLES) % slots.len();
    Some(slots[slot])
}

/// The step the envelope is read in, and the window each step's level is read over.
const STEP_MS: i64 = 10;
const ENVELOPE_WINDOW: usize = 20 * PER_MS;
/// A block whose best lag correlates under this isn't counted (silence, the echo service's own prompt).
const MIN_CORRELATION: f64 = 0.6;

/// The keyed tone (`hz`, played by the microphone whose plays `played` marks) as `heard` heard it between `from_us`
/// and `to_us`: for each `block` of the recording, the delay in `lag_ms` (from, to) that lines its level at `hz` up
/// best with the pattern's slots (Pearson correlation of the level, read every 10 ms, and the slot's on or off at
/// that time less the delay); the blocks that line up at [`MIN_CORRELATION`] or better, each its delay (ms).
pub fn lags(
    heard: &Recording,
    played: &Timeline,
    hz: f32,
    (from_us, to_us): (i64, i64),
    (lag_from_ms, lag_to_ms): (i64, i64),
    block: Duration,
) -> Vec<i64> {
    let slots = pattern();
    let (Some(first), Some(last)) = (heard.timeline.sample_at(from_us), heard.timeline.sample_at(to_us)) else {
        return Vec::new();
    };
    let samples = heard.slice(to_index(first), to_index(last));
    let step = usize::try_from(STEP_MS).unwrap_or(10) * PER_MS;
    // (time heard, level) every step
    let envelope: Vec<(i64, f64)> = (0..samples.len().saturating_sub(ENVELOPE_WINDOW))
        .step_by(step)
        .filter_map(|at| {
            let time = heard.timeline.time_of(first + u64::try_from(at).ok()?)?;
            Some((time, f64::from(amplitude_at(&samples[at..at + ENVELOPE_WINDOW], hz))))
        })
        .collect();
    let per_block = usize::try_from(block.as_millis()).unwrap_or(2_000) / usize::try_from(STEP_MS).unwrap_or(10);
    envelope
        .chunks(per_block.max(1))
        .filter(|chunk| chunk.len() == per_block)
        .filter_map(|chunk| best_lag(chunk, played, &slots, (lag_from_ms, lag_to_ms)))
        .collect()
}

/// The delay (ms) that lines `chunk` up best with the pattern, if it lines up at [`MIN_CORRELATION`] or better.
fn best_lag(chunk: &[(i64, f64)], played: &Timeline, slots: &[bool], (from, to): (i64, i64)) -> Option<i64> {
    let mut best: Option<(f64, i64)> = None;
    let mut lag = from;
    while lag <= to {
        let pairs: Vec<(f64, f64)> = chunk
            .iter()
            .filter_map(|(time, level)| {
                keyed_on(played, slots, time - lag * 1_000).map(|on| (*level, if on { 1.0 } else { 0.0 }))
            })
            .collect();
        if pairs.len() * 10 >= chunk.len() * 9
            && let Some(correlation) = pearson(&pairs)
            && best.is_none_or(|(top, _)| correlation > top)
        {
            best = Some((correlation, lag));
        }
        lag += STEP_MS;
    }
    best.filter(|(correlation, _)| *correlation >= MIN_CORRELATION)
        .map(|(_, lag)| lag)
}

/// How far apart two blocks' delays ([`lags`]) may be and still agree.
pub const AGREE_MS: i64 = 50;
/// How many blocks must agree on a delay for it to be taken.
pub const AGREEING_BLOCKS: usize = 3;

/// The delays among `lags` that agree: the largest group within [`AGREE_MS`] of one of them (the earliest such group
/// on a tie), when it holds at least [`AGREEING_BLOCKS`]; `None` otherwise. Each block's best lag is the best of
/// thousands of alignments, so one block lining up is no proof: a block of something else (noise, a prompt, another
/// slot of the pattern) lines up at a delay of its own, and only the delay several blocks agree on is the tone's.
pub fn agreeing(lags: &[i64]) -> Option<Vec<i64>> {
    let near = |center: i64| -> Vec<i64> {
        lags.iter()
            .copied()
            .filter(|lag| (lag - center).abs() <= AGREE_MS)
            .collect()
    };
    let mut centers = lags.to_vec();
    centers.sort_unstable();
    // the largest; `max_by_key` takes the last of equals, so the centers go from the latest
    let group = centers.into_iter().rev().map(near).max_by_key(Vec::len)?;
    (group.len() >= AGREEING_BLOCKS).then_some(group)
}

/// The Pearson correlation of `pairs`; `None` when either side doesn't vary.
fn pearson(pairs: &[(f64, f64)]) -> Option<f64> {
    let n = f64::from(u32::try_from(pairs.len()).unwrap_or(u32::MAX));
    let (mean_x, mean_y) = pairs.iter().fold((0.0, 0.0), |(x, y), (a, b)| (x + a / n, y + b / n));
    let (mut cov, mut var_x, mut var_y) = (0.0, 0.0, 0.0);
    for (x, y) in pairs {
        cov += (x - mean_x) * (y - mean_y);
        var_x += (x - mean_x).powi(2);
        var_y += (y - mean_y).powi(2);
    }
    (var_x > 0.0 && var_y > 0.0).then(|| cov / (var_x * var_y).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pattern_is_a_maximal_sequence_half_on() {
        let slots = pattern();
        assert_eq!(slots.len(), 511);
        assert_eq!(slots.iter().filter(|on| **on).count(), 256);
        // no shorter period
        for period in [1, 7, 73] {
            assert!(slots.iter().zip(slots.iter().skip(period)).any(|(a, b)| a != b));
        }
    }

    #[test]
    fn the_keyed_tone_is_on_in_on_slots_and_silent_in_the_others() {
        let tone = Tone {
            hz: 990.0,
            amplitude: 0.5,
        };
        let samples = keyed(tone);
        let slots = pattern();
        for (slot, on) in slots.iter().enumerate().take(40) {
            let middle = &samples[slot * SLOT_SAMPLES + RAMP..(slot + 1) * SLOT_SAMPLES - RAMP];
            let level = amplitude_at(middle, 990.0);
            if *on {
                assert!((level - 0.5).abs() < 0.02, "slot {slot}: {level}");
            } else {
                assert!(level < 1e-6, "slot {slot}: {level}");
            }
        }
    }

    #[test]
    fn the_speech_like_signal_is_read_at_its_bins_and_a_tone_beside_it_isn_t() {
        let speech = speech_like(4, 7, 0.5);
        let level = speech_level(&speech);
        assert!(level > 0.002, "{level}");
        let quieter: Vec<f32> = speech.iter().map(|sample| sample * 0.01).collect();
        let down = db_under(level, speech_level(&quieter));
        assert!((down - 40.0).abs() < 0.5, "{down}");
        // a's tone, b's tone: not speech
        let mut tones = vec![0.0_f32; 4 * 48_000];
        for hz in [440.0, 660.0] {
            let mut tone = vec![0.0_f32; tones.len()];
            ToneGenerator::new(Tone { hz, amplitude: 0.5 }).fill(&mut tone);
            for (sum, add) in tones.iter_mut().zip(tone) {
                *sum += add;
            }
        }
        assert!(db_under(level, speech_level(&tones)) > 20.0);
        assert!(speech_level(&[]).abs() < f32::EPSILON);
    }

    #[test]
    fn a_level_under_a_reference_is_said_in_db() {
        assert!((db_under(0.1, 0.001) - 40.0).abs() < 1e-3);
        assert!((db_under(0.1, 0.0) - 100.0).abs() < 1e-3);
        assert!(db_under(0.1, 0.2) < 0.0);
    }

    #[test]
    fn a_tone_s_loudest_window_finds_it_and_a_burst_beside_it() {
        let mut samples = vec![0.0_f32; 48_000 * 4];
        ToneGenerator::new(Tone {
            hz: 440.0,
            amplitude: 0.2,
        })
        .fill(&mut samples);
        let mut burst = vec![0.0_f32; 24_000];
        ToneGenerator::new(Tone {
            hz: 660.0,
            amplitude: 0.4,
        })
        .fill(&mut burst);
        for (sample, add) in samples[48_000..72_000].iter_mut().zip(&burst) {
            *sample += add;
        }
        assert!((loudest(&samples, 440.0, Duration::from_millis(400)) - 0.2).abs() < 0.01);
        assert!(loudest(&samples, 660.0, Duration::from_millis(400)) > 0.35);
    }

    /// The keyed tone the tests' microphone plays.
    fn played() -> Arc<[f32]> {
        keyed(Tone {
            hz: 990.0,
            amplitude: 0.3,
        })
    }

    /// A keyed microphone marked from `start_us`, a recording that heard it `delay_ms` later at `gain` (each sample
    /// of it marked at its own time), and the level read back.
    fn heard_after(delay_ms: i64, gain: f32, dir: &std::path::Path) -> (Recording, Timeline, i64) {
        heard_instead(&played(), delay_ms, gain, dir)
    }

    /// As [`heard_after`], the recording hearing `heard` (looped) where the keyed microphone played its tone.
    fn heard_instead(heard: &[f32], delay_ms: i64, gain: f32, dir: &std::path::Path) -> (Recording, Timeline, i64) {
        let played = played();
        let start_us: i64 = 1_800_000_000_000_000;
        let played_marks = Timeline::new();
        for at in (0..played.len()).step_by(480) {
            let at64 = u64::try_from(at).unwrap();
            played_marks.mark(at64, start_us + ui_sandbox::pipewire::timeline::samples_to_us(at64));
        }
        // 45 s of what was heard, from 1 s into the playing, delay_ms behind it, quietly
        let seconds = 45;
        let heard_from: usize = 48_000;
        let delay = usize::try_from(delay_ms).unwrap() * PER_MS;
        let samples: Vec<f32> = (0..seconds * 48_000)
            .map(|at| {
                let source = (heard_from + at).checked_sub(delay);
                source.map_or(0.0, |source| heard[source % heard.len()] * gain)
            })
            .collect();
        let path = dir.join("heard.wav");
        let mut writer = teams_media::devices::wav::WavWriter::create(&path).unwrap();
        writer.write(&samples).unwrap();
        drop(writer);
        let heard_marks = Timeline::new();
        for at in (0..samples.len()).step_by(480) {
            let at64 = u64::try_from(at).unwrap();
            let heard_at = u64::try_from(heard_from).unwrap() + at64;
            heard_marks.mark(at64, start_us + ui_sandbox::pipewire::timeline::samples_to_us(heard_at));
        }
        (
            Recording {
                path,
                timeline: heard_marks,
            },
            played_marks,
            start_us,
        )
    }

    #[test]
    fn each_block_s_delay_is_found_on_the_one_clock() {
        let dir = tempfile::tempdir().unwrap();
        for delay in [150, 640] {
            let (heard, played, start) = heard_after(delay, 0.2, dir.path());
            let found = lags(
                &heard,
                &played,
                990.0,
                (start + 1_000_000, start + 21_000_000),
                (0, 2_000),
                Duration::from_secs(2),
            );
            assert!(found.len() >= 8, "{delay}: {found:?}");
            assert!(found.iter().all(|lag| (lag - delay).abs() <= 20), "{delay}: {found:?}");
            assert_eq!(agreeing(&found), Some(found.clone()));
        }
        // a long delay (the echo service's), looked for where it is
        let (heard, played, start) = heard_after(16_700, 0.2, dir.path());
        let found = lags(
            &heard,
            &played,
            990.0,
            (start + 1_000_000, start + 41_000_000),
            (5_000, 30_000),
            Duration::from_secs(2),
        );
        assert!(found.len() >= AGREEING_BLOCKS, "{found:?}");
        assert!(found.iter().all(|lag| (lag - 16_700).abs() <= 20), "{found:?}");
        assert_eq!(agreeing(&found), Some(found.clone()));
    }

    #[test]
    fn only_the_delay_several_blocks_agree_on_is_taken() {
        assert_eq!(agreeing(&[]), None);
        assert_eq!(agreeing(&[16_760, 16_750]), None);
        // one block lined up with a later slot: left out
        assert_eq!(
            agreeing(&[16_760, 16_730, 21_800, 16_770, 16_760]),
            Some(vec![16_760, 16_730, 16_770, 16_760])
        );
        // the group the most blocks agree on, wherever the others are
        assert_eq!(
            agreeing(&[5_000, 9_120, 16_760, 16_750, 16_770, 27_000, 29_990]),
            Some(vec![16_760, 16_750, 16_770])
        );
        // scattered: no delay
        assert_eq!(agreeing(&[5_000, 9_120, 16_760, 21_800, 27_000]), None);
    }

    /// `seconds` of white noise at about `level`.
    fn noise(seconds: usize, level: f32) -> Vec<f32> {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        (0..seconds * 48_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let top = u16::try_from(state >> 48).unwrap();
                (f32::from(top) / 32_768.0 - 1.0) * level
            })
            .collect()
    }

    /// The tests' tone keyed by another on/off pattern (a seeded random one) in the same 200 ms slots.
    fn other_pattern() -> Vec<f32> {
        let mut samples = vec![0.0_f32; 511 * SLOT_SAMPLES];
        ToneGenerator::new(Tone {
            hz: 990.0,
            amplitude: 0.3,
        })
        .fill(&mut samples);
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        for slot in samples.chunks_mut(SLOT_SAMPLES) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            if state.leading_zeros() > 0 {
                slot.fill(0.0);
            }
        }
        samples
    }

    #[test]
    fn noise_or_a_tone_in_another_pattern_gives_no_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        for (what, heard, gain) in [
            ("low-level noise", noise(30, 1.0), 0.002),
            ("noise", noise(30, 1.0), 0.3),
            ("another pattern", other_pattern(), 0.2),
        ] {
            let (heard, played, start) = heard_instead(&heard, 16_700, gain, dir.path());
            let found = lags(
                &heard,
                &played,
                990.0,
                (start + 1_000_000, start + 41_000_000),
                (5_000, 30_000),
                Duration::from_secs(2),
            );
            assert_eq!(agreeing(&found), None, "{what}: {found:?}");
            if what == "another pattern" {
                // its blocks line up one by one, each at a delay of its own: one block alone proves nothing
                assert!(found.len() >= AGREEING_BLOCKS, "{found:?}");
            }
        }
    }

    #[test]
    fn silence_lines_up_with_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (heard, played, start) = heard_after(150, 0.0, dir.path());
        let found = lags(
            &heard,
            &played,
            990.0,
            (start + 1_000_000, start + 21_000_000),
            (0, 2_000),
            Duration::from_secs(2),
        );
        assert!(found.is_empty(), "{found:?}");
    }
}
