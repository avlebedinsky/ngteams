//! The tone a call sends and how it is found again in a recording (M3 Task 15): a tone keyed on and off in 200 ms
//! slots of a fixed pseudo-random pattern ([`keyed`]), so that what comes back can be timed against what went out, and
//! [`find`], which looks for that pattern in a recording at the tone's frequency (Goertzel, 10 ms windows).
//!
//! The pattern comes from a maximal-length shift register of degree 10: it repeats only every 1023 slots (204.6 s),
//! so a stretch of a few seconds of it matches the signal at one offset alone, however late it comes back. The
//! recording is cut into 2 s blocks of what was sent; each block's best offset in the recording is found on its own,
//! so an echo that plays back in pieces (a recording played later, at another offset) shows as blocks with offsets of
//! their own. Within an echoed block every onset (a slot that turns the tone on) is timed to the window it comes back
//! in, and its delay is one round trip: from the sample going into the microphone to the sample coming out of the
//! speaker, both counted from when the call's devices opened (the two open together).
//!
//! Nothing here reads the network or the clock: a recording in, a report out.

use std::time::Duration;

use teams_media::tone::{Tone, ToneGenerator};

/// How long the keyed tone stays on or off: one slot of the pattern.
pub const SLOT: Duration = Duration::from_millis(200);
/// Samples in a slot (200 ms at 48 kHz).
const SLOT_SAMPLES: usize = 9_600;
/// Samples in a window the level is read over (10 ms).
const WINDOW: usize = 480;
/// Windows in a slot.
const SLOT_WINDOWS: usize = SLOT_SAMPLES / WINDOW;
/// The windows at each end of a slot left out of its level (30 ms): a block's offset is found within that.
const EDGE: usize = 3;
/// Samples the tone takes to fade in or out at a slot's edge (5 ms), so a slot's edge carries no click.
const RAMP: usize = 240;
/// Slots in a block whose offset is found on its own (2 s).
const BLOCK_SLOTS: usize = 10;
/// The level an echoed block's on-slots reach at least, on average.
pub const MIN_LEVEL: f32 = 0.02;
/// How many times louder an echoed block's quietest on-slot is than its loudest off-slot: every slot of it reads as
/// what was sent. Any 10 slots in a row occur once in the pattern's period, so a block matches at its own offset
/// alone, never at another stretch of the pattern.
const SEPARATION: f32 = 2.0;
/// How many on-slots and off-slots a block has at least to be timed: a block of nearly one kind matches too much.
const MIN_KEYED: usize = 3;

/// Whether each of the first `slots` slots keys the tone on: a maximal-length shift register of degree 10
/// (x^10 + x^7 + 1), from a seed whose first 40 blocks (80 s) each key 3 to 7 of their 10 slots.
#[must_use]
pub fn pattern(slots: usize) -> Vec<bool> {
    let mut register: u16 = 0b00_1010_0000;
    (0..slots)
        .map(|_| {
            let bit = ((register >> 9) ^ (register >> 6)) & 1;
            register = ((register << 1) | bit) & 0x3ff;
            bit == 1
        })
        .collect()
}

/// `tone` keyed by [`pattern`] for `length` (whole slots, at least one), each slot's edges faded over 5 ms.
#[must_use]
pub fn keyed(tone: Tone, length: Duration) -> Vec<f32> {
    let slots = slot_count(length).max(1);
    let mut generator = ToneGenerator::new(tone);
    let mut samples = vec![0.0; slots * SLOT_SAMPLES];
    for (slot, on) in pattern(slots).into_iter().enumerate() {
        let part = &mut samples[slot * SLOT_SAMPLES..(slot + 1) * SLOT_SAMPLES];
        generator.fill(part);
        if !on {
            part.fill(0.0);
            continue;
        }
        for at in 0..RAMP {
            let gain = ramp(at);
            part[at] *= gain;
            part[SLOT_SAMPLES - 1 - at] *= gain;
        }
    }
    samples
}

/// How long a burst of [`bursts`] lasts, and the silence after it: the web client's fake microphone's (the lab
/// browser's), whose bursts the page's audio tap reads as the tone.
pub const BURST_ON: Duration = Duration::from_millis(1_600);
pub const BURST_OFF: Duration = Duration::from_millis(900);

/// `tone` in bursts for `length`: [`BURST_ON`] of it, then [`BURST_OFF`] of silence, each burst's edges faded over
/// 5 ms. What a lab browser's page hears as the tone (a keyed 200 ms slot is too short for its audio tap).
#[must_use]
pub fn bursts(tone: Tone, length: Duration) -> Vec<f32> {
    let total = samples_in(length);
    let (on, period) = (samples_in(BURST_ON), samples_in(BURST_ON + BURST_OFF));
    let mut samples = vec![0.0; total];
    ToneGenerator::new(tone).fill(&mut samples);
    for (at, sample) in samples.iter_mut().enumerate() {
        let into = at % period;
        *sample *= if into >= on {
            0.0
        } else {
            ramp(into.min(on - 1 - into).min(RAMP))
        };
    }
    samples
}

/// Samples in `length` at the devices' rate.
fn samples_in(length: Duration) -> usize {
    usize::try_from(length.as_millis()).unwrap_or(usize::MAX) * (SLOT_SAMPLES / 200)
}

/// A stretch of a recording [`presence`] counts the tone in (100 ms).
const STRETCH_WINDOWS: usize = 10;
/// How much of a recording must hold the tone for it to be heard: 1 s of stretches, all told.
const MIN_HEARD_STRETCHES: usize = 10;

/// How much of a tone a recording holds, whatever its keying ([`presence`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Presence {
    /// The 100 ms stretches whose level at the tone's frequency reaches [`MIN_LEVEL`].
    pub stretches: usize,
    /// The first of them, from the recording's start.
    pub first: Option<Duration>,
    /// The loudest stretch's level.
    pub peak: f32,
}

impl Presence {
    /// Whether the tone is there: at least 1 s of it, all told.
    #[must_use]
    pub fn heard(&self) -> bool {
        self.stretches >= MIN_HEARD_STRETCHES
    }

    /// How long the tone sounds in the recording, all told.
    #[must_use]
    pub fn length(&self) -> Duration {
        window_time(self.stretches * STRETCH_WINDOWS)
    }
}

/// How much of the tone at `hz` `received` holds, in 100 ms stretches (each the mean level of its 10 ms windows,
/// [`Meter`]), however it was keyed: the check for a far end that doesn't play [`keyed`]'s pattern (a lab browser's
/// page plays bursts).
#[must_use]
pub fn presence(hz: f32, received: &[f32]) -> Presence {
    let meter = Meter::new(hz);
    let levels: Vec<f32> = received
        .as_chunks::<WINDOW>()
        .0
        .iter()
        .map(|window| meter.level(window))
        .collect();
    let mut found = Presence {
        stretches: 0,
        first: None,
        peak: 0.0,
    };
    for (index, stretch) in levels.as_chunks::<STRETCH_WINDOWS>().0.iter().enumerate() {
        let level = stretch.iter().sum::<f32>() / small_f32(STRETCH_WINDOWS);
        found.peak = found.peak.max(level);
        if level >= MIN_LEVEL {
            found.stretches += 1;
            found.first.get_or_insert_with(|| window_time(index * STRETCH_WINDOWS));
        }
    }
    found
}

/// The fade-in gain of a slot's `at`th sample (a raised cosine over [`RAMP`] samples).
fn ramp(at: usize) -> f32 {
    let x = std::f32::consts::PI * small_f32(at) / small_f32(RAMP);
    0.5 - 0.5 * x.cos()
}

/// A block of what was sent that came back: where it starts in what was sent, how much later it came back, and how
/// loud.
#[derive(Clone, Debug, PartialEq)]
pub struct EchoedBlock {
    /// Its start in what was sent (from the devices' opening).
    pub sent_at: Duration,
    /// How much later it came back, to the window.
    pub offset: Duration,
    /// Its on-slots' level in the recording.
    pub level: f32,
}

/// What [`find`] found of the keyed tone in a recording.
#[derive(Clone, Debug, PartialEq)]
pub struct Echo {
    /// The blocks that came back, in the order they were sent.
    pub blocks: Vec<EchoedBlock>,
    /// Each onset's round trip within those blocks, sorted.
    pub delays: Vec<Duration>,
    /// The silence [`find_with_lead`] put before the recording: every offset and round trip above counts it in, and
    /// one shorter than it came back before the recording started ([`Echo::signed_ms`]).
    pub lead: Duration,
}

impl Echo {
    /// `measured` (an offset or a round trip above) less [`Echo::lead`], in ms: negative for a far end whose recording
    /// started after what it heard went out (another process's devices opened later).
    #[must_use]
    pub fn signed_ms(&self, measured: Duration) -> i64 {
        let ms = |duration: Duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
        ms(measured) - ms(self.lead)
    }

    /// The on-slots' level, the median over the echoed blocks.
    #[must_use]
    pub fn level(&self) -> f32 {
        let mut levels: Vec<f32> = self.blocks.iter().map(|block| block.level).collect();
        levels.sort_by(f32::total_cmp);
        levels.get(levels.len() / 2).copied().unwrap_or(0.0)
    }

    /// The nearest-rank `percent` percentile of the round trips, the lead taken off (none below zero); `None` without
    /// any.
    #[must_use]
    pub fn delay_percentile(&self, percent: usize) -> Option<Duration> {
        self.raw_percentile(percent)
            .map(|delay| delay.saturating_sub(self.lead))
    }

    /// The nearest-rank `percent` percentile of the round trips as measured, the lead counted in.
    #[must_use]
    pub fn raw_percentile(&self, percent: usize) -> Option<Duration> {
        let rank = (self.delays.len() * percent).div_ceil(100).max(1);
        self.delays.get(rank - 1).copied()
    }

    /// The span of what was sent that came back: from the first echoed block's start to the last one's end.
    #[must_use]
    pub fn sent_span(&self) -> Option<(Duration, Duration)> {
        let first = self.blocks.first()?.sent_at;
        let last = self.blocks.last()?.sent_at + SLOT * slots_u32(BLOCK_SLOTS);
        Some((first, last))
    }
}

/// [`find`] in `received` with `lead` of silence put before it: a block that came back up to `lead` *before* the
/// recording's own start is found too (a far end in another process, whose devices opened after the sender's), at an
/// offset [`Echo::signed_ms`] reads below zero. Whole 10 ms windows of lead.
#[must_use]
pub fn find_with_lead(hz: f32, sent: Duration, received: &[f32], max_offset: Duration, lead: Duration) -> Option<Echo> {
    let lead_samples = usize::try_from(lead.as_millis() / 10).unwrap_or(0) * WINDOW;
    let mut padded = vec![0.0; lead_samples];
    padded.extend_from_slice(received);
    let mut echo = find(hz, sent, &padded, max_offset + lead)?;
    echo.lead = window_time(lead_samples / WINDOW);
    Some(echo)
}

/// Looks for the keyed tone at `hz` (what [`keyed`] made of the first `sent` of it) in `received`, at most
/// `max_offset` later: `None` when no block of it came back.
#[must_use]
pub fn find(hz: f32, sent: Duration, received: &[f32], max_offset: Duration) -> Option<Echo> {
    let slots = pattern(slot_count(sent));
    let meter = Meter::new(hz);
    let levels: Vec<f32> = received
        .as_chunks::<WINDOW>()
        .0
        .iter()
        .map(|window| meter.level(window))
        .collect();
    let max_offset = usize::try_from(max_offset.as_millis() / 10).unwrap_or(usize::MAX);
    let mut echo = Echo {
        blocks: Vec::new(),
        delays: Vec::new(),
        lead: Duration::ZERO,
    };
    for (index, block) in slots.as_chunks::<BLOCK_SLOTS>().0.iter().enumerate() {
        let first = index * BLOCK_SLOTS;
        let Some((offset, slots)) = best_offset(block, first, &levels, max_offset) else {
            continue;
        };
        if slots.on < MIN_LEVEL || slots.quietest_on < SEPARATION * slots.loudest_off.max(f32::EPSILON) {
            continue;
        }
        echo.blocks.push(EchoedBlock {
            sent_at: SLOT * slots_u32(first),
            offset: window_time(offset),
            level: slots.on,
        });
        let span = (slots.on, slots.off);
        echo.delays
            .extend(onsets(block, first).filter_map(|window| onset_delay(&levels, window, offset, span)));
    }
    echo.delays.sort();
    (!echo.blocks.is_empty()).then_some(echo)
}

/// A block's slots' levels at an offset.
#[derive(Clone, Copy, Debug)]
struct SlotLevels {
    /// The on-slots' mean level.
    on: f32,
    /// The off-slots' mean level.
    off: f32,
    quietest_on: f32,
    loudest_off: f32,
}

/// The offset (in windows) at which a block's on-slots are loudest against its off-slots in `levels`, with its slots'
/// levels there; `None` for a block of fewer than [`MIN_KEYED`] slots of either kind, or one no offset fits.
fn best_offset(block: &[bool], first: usize, levels: &[f32], max_offset: usize) -> Option<(usize, SlotLevels)> {
    let ons = block.iter().filter(|on| **on).count();
    if ons < MIN_KEYED || block.len() - ons < MIN_KEYED {
        return None;
    }
    let end = (first + block.len()) * SLOT_WINDOWS;
    let last = levels.len().checked_sub(end)?.min(max_offset);
    (0..=last)
        .map(|offset| (offset, slot_levels(block, first, levels, offset)))
        .max_by(|(_, a), (_, b)| (a.on - a.off).total_cmp(&(b.on - b.off)))
}

/// A block's slots' levels at `offset`, each slot's [`EDGE`] windows at either end left out.
fn slot_levels(block: &[bool], first: usize, levels: &[f32], offset: usize) -> SlotLevels {
    let mut found = SlotLevels {
        on: 0.0,
        off: 0.0,
        quietest_on: f32::INFINITY,
        loudest_off: 0.0,
    };
    let (mut ons, mut offs) = (0_usize, 0_usize);
    for (slot, keyed) in block.iter().enumerate() {
        let start = (first + slot) * SLOT_WINDOWS + offset;
        let middle = &levels[start + EDGE..start + SLOT_WINDOWS - EDGE];
        let level = middle.iter().sum::<f32>() / small_f32(middle.len());
        if *keyed {
            found.on += level;
            found.quietest_on = found.quietest_on.min(level);
            ons += 1;
        } else {
            found.off += level;
            found.loudest_off = found.loudest_off.max(level);
            offs += 1;
        }
    }
    found.on /= small_f32(ons.max(1));
    found.off /= small_f32(offs.max(1));
    found
}

/// The windows (in what was sent) at which a block's slots turn the tone on.
fn onsets(block: &[bool], first: usize) -> impl Iterator<Item = usize> + '_ {
    block
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| !pair[0] && pair[1])
        .map(move |(slot, _)| (first + slot + 1) * SLOT_WINDOWS)
}

/// The round trip of the onset sent at window `sent`, near `offset` windows later in `levels`: the first window
/// within [`EDGE`] windows either side whose level reaches the midpoint of `on` and `off`, and within it the point the
/// tone set in (its level grows with the share of the window's weight the tone fills, [`Meter`]).
fn onset_delay(levels: &[f32], sent: usize, offset: usize, (on, off): (f32, f32)) -> Option<Duration> {
    let threshold = f32::midpoint(on, off);
    let from = (sent + offset).checked_sub(EDGE)?.max(1);
    let to = (sent + offset + EDGE).min(levels.len().checked_sub(1)?);
    let crossing = (from..=to).find(|&at| levels[at - 1] < threshold && levels[at] >= threshold)?;
    let filled = ((levels[crossing] - off) / (on - off)).clamp(0.0, 1.0);
    let windows = small_f32(crossing.checked_sub(sent)?) + set_in(filled);
    Some(Duration::from_secs_f32(windows.max(0.0) * 0.01))
}

/// Where in a window (0 its start, 1 its end) a tone set in that fills `filled` of the window's Hann weight: the
/// inverse of the weight from there to the end, found by bisection.
fn set_in(filled: f32) -> f32 {
    // the Hann weight before `x`, as a share of the whole
    let before = |x: f32| x - (std::f32::consts::TAU * x).sin() / std::f32::consts::TAU;
    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    for _ in 0..24 {
        let middle = f32::midpoint(low, high);
        if 1.0 - before(middle) > filled {
            low = middle;
        } else {
            high = middle;
        }
    }
    f32::midpoint(low, high)
}

/// The level of one frequency in a 10 ms window: Goertzel over the window weighted by a Hann window (so a tone a few
/// hundred Hz away leaks next to nothing into it), scaled so a tone filling the window reads its amplitude.
struct Meter {
    weights: Vec<f32>,
    coeff: f64,
    scale: f64,
}

impl Meter {
    fn new(hz: f32) -> Self {
        let weights: Vec<f32> = (0..WINDOW)
            .map(|at| 0.5 - 0.5 * (std::f32::consts::TAU * small_f32(at) / small_f32(WINDOW)).cos())
            .collect();
        let sum: f64 = weights.iter().copied().map(f64::from).sum();
        let rate = f64::from(teams_media::devices::SAMPLE_RATE);
        Self {
            weights,
            coeff: 2.0 * (std::f64::consts::TAU * f64::from(hz) / rate).cos(),
            scale: 2.0 / sum,
        }
    }

    fn level(&self, window: &[f32; WINDOW]) -> f32 {
        let (mut s1, mut s2) = (0.0_f64, 0.0_f64);
        for (sample, weight) in window.iter().zip(&self.weights) {
            let s0 = f64::from(sample * weight) + self.coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let power = (s1 * s1 + s2 * s2 - self.coeff * s1 * s2).max(0.0);
        narrow(power.sqrt() * self.scale)
    }
}

#[expect(clippy::cast_possible_truncation, reason = "a level fits an f32's range")]
fn narrow(value: f64) -> f32 {
    value as f32
}

/// Whole slots in `length`.
fn slot_count(length: Duration) -> usize {
    usize::try_from(length.as_millis() / SLOT.as_millis()).unwrap_or(usize::MAX)
}

fn window_time(windows: usize) -> Duration {
    Duration::from_millis(u64::try_from(windows).unwrap_or(u64::MAX) * 10)
}

fn slots_u32(slots: usize) -> u32 {
    u32::try_from(slots).unwrap_or(u32::MAX)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "sample, window and slot counts of a call stay far below 2^24"
)]
fn small_f32(count: usize) -> f32 {
    count as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const TONE: Tone = Tone {
        hz: 1_000.0,
        amplitude: 0.5,
    };

    fn delayed(signal: &[f32], by: Duration, gain: f32, total: usize) -> Vec<f32> {
        let shift = usize::try_from(by.as_micros() * 48 / 1_000).unwrap();
        let mut out = vec![0.0; total];
        for (at, sample) in signal.iter().enumerate() {
            if let Some(slot) = out.get_mut(at + shift) {
                *slot = sample * gain;
            }
        }
        out
    }

    #[test]
    fn a_slot_is_200_ms_at_the_devices_rate() {
        assert_eq!(
            u64::from(teams_media::devices::SAMPLE_RATE) * 200 / 1_000,
            u64::try_from(SLOT_SAMPLES).unwrap()
        );
    }

    #[test]
    fn the_pattern_repeats_only_after_1023_slots_and_keys_about_half_of_them() {
        let slots = pattern(2_046);
        assert_eq!(slots[..1_023], slots[1_023..]);
        let on = slots[..1_023].iter().filter(|on| **on).count();
        assert_eq!(
            on, 512,
            "a maximal-length register's period has one more one than zeros"
        );
        assert!((1..1_023).all(|period| slots[..1_023] != slots[period..period + 1_023]));
        for block in slots[..400].chunks(BLOCK_SLOTS) {
            let on = block.iter().filter(|on| **on).count();
            assert!((MIN_KEYED..=BLOCK_SLOTS - MIN_KEYED).contains(&on), "{block:?}");
        }
    }

    #[test]
    fn the_keyed_tone_is_the_tone_in_its_on_slots_and_silence_in_its_off_slots() {
        let samples = keyed(TONE, Duration::from_secs(2));
        assert_eq!(samples.len(), 10 * SLOT_SAMPLES);
        for (slot, on) in pattern(10).into_iter().enumerate() {
            let middle = &samples[slot * SLOT_SAMPLES + RAMP..(slot + 1) * SLOT_SAMPLES - RAMP];
            let level = teams_media::tone::amplitude_at(middle, TONE.hz);
            if on {
                assert!((level - 0.5).abs() < 0.02, "slot {slot}: {level}");
            } else {
                assert!(level < 1e-6, "slot {slot}: {level}");
            }
        }
    }

    #[test]
    fn an_echo_delayed_as_a_whole_is_found_at_its_delay_in_every_block() {
        let sent = Duration::from_secs(6);
        let signal = keyed(TONE, sent);
        let by = Duration::from_millis(1_234);
        let received = delayed(&signal, by, 0.4, signal.len() + 48_000 * 2);
        let echo = find(TONE.hz, sent, &received, Duration::from_secs(10)).unwrap();
        assert_eq!(echo.blocks.len(), 3);
        for block in &echo.blocks {
            assert!(block.offset.abs_diff(by) <= Duration::from_millis(30), "{block:?}");
            // the tone's 0.5 at the echo's gain of 0.4
            assert!((block.level - 0.2).abs() < 0.02, "{block:?}");
        }
        assert!(echo.delays.len() >= 6, "{:?}", echo.delays);
        for delay in &echo.delays {
            assert!(delay.abs_diff(by) < Duration::from_millis(10), "{delay:?}");
        }
        assert_eq!(echo.sent_span(), Some((Duration::ZERO, Duration::from_secs(6))));
    }

    #[test]
    fn an_echo_that_records_a_stretch_and_plays_it_back_later_is_found_in_that_stretch_alone_amid_other_sound() {
        let sent = Duration::from_secs(20);
        let signal = keyed(TONE, sent);
        // the echo keeps 4 s..10 s of what it heard and plays it back from 14 s, over a hum near the tone and noise
        let kept = &signal[4 * 48_000..10 * 48_000];
        let mut received = vec![0.0; 26 * 48_000];
        for (at, sample) in received.iter_mut().enumerate() {
            let t = small_f32(at) / 48_000.0;
            *sample = 0.05 * (std::f32::consts::TAU * 700.0 * t).sin() + 0.01 * (t * 7_919.0).sin();
        }
        for (at, sample) in kept.iter().enumerate() {
            received[14 * 48_000 + at] += sample * 0.3;
        }
        let echo = find(TONE.hz, sent, &received, Duration::from_secs(20)).unwrap();
        let starts: Vec<_> = echo.blocks.iter().map(|block| block.sent_at.as_secs()).collect();
        assert_eq!(starts, [4, 6, 8]);
        for block in &echo.blocks {
            assert!(
                block.offset.abs_diff(Duration::from_secs(10)) <= Duration::from_millis(30),
                "{block:?}"
            );
        }
        let p50 = echo.delay_percentile(50).unwrap();
        assert!(
            p50.abs_diff(Duration::from_secs(10)) < Duration::from_millis(10),
            "{p50:?}"
        );
        assert!(echo.delay_percentile(95).unwrap() >= p50);
    }

    #[test]
    fn silence_or_a_steady_tone_is_no_echo() {
        let sent = Duration::from_secs(4);
        assert_eq!(
            find(TONE.hz, sent, &vec![0.0; 48_000 * 8], Duration::from_secs(4)),
            None
        );
        let mut steady = vec![0.0; 48_000 * 8];
        ToneGenerator::new(TONE).fill(&mut steady);
        assert_eq!(find(TONE.hz, sent, &steady, Duration::from_secs(4)), None);
        assert_eq!(find(TONE.hz, sent, &[], Duration::from_secs(4)), None);
    }

    #[test]
    fn the_keyed_tone_isn_t_heard_at_a_frequency_a_few_hundred_hz_away() {
        let sent = Duration::from_secs(6);
        let received = delayed(&keyed(TONE, sent), Duration::from_millis(300), 1.0, 48_000 * 8);
        assert!(find(TONE.hz, sent, &received, Duration::from_secs(2)).is_some());
        assert_eq!(find(660.0, sent, &received, Duration::from_secs(2)), None);
        assert_eq!(find(1_340.0, sent, &received, Duration::from_secs(2)), None);
    }

    #[test]
    fn with_a_lead_a_far_end_that_started_recording_later_is_found_at_a_negative_offset() {
        let sent = Duration::from_secs(12);
        let signal = keyed(TONE, sent);
        // the far end's speaker opened 3 s after the microphone did; the way there takes 200 ms
        let at_far_end = delayed(&signal, Duration::from_millis(200), 0.5, signal.len() + 48_000);
        let received = &at_far_end[3 * 48_000..];
        assert_eq!(find(TONE.hz, sent, received, sent), None, "not without a lead");
        let echo = find_with_lead(TONE.hz, sent, received, sent, Duration::from_secs(5)).unwrap();
        assert_eq!(echo.lead, Duration::from_secs(5));
        for block in &echo.blocks {
            assert!((echo.signed_ms(block.offset) + 2_800).abs() <= 30, "{block:?}");
        }
        let p50 = echo.raw_percentile(50).unwrap();
        assert!((echo.signed_ms(p50) + 2_800).abs() <= 10, "{p50:?}");
        assert_eq!(echo.delay_percentile(50), Some(Duration::ZERO), "none below zero");
    }

    #[test]
    fn bursts_are_the_tone_for_1_6_s_then_0_9_s_of_silence() {
        let samples = bursts(TONE, Duration::from_secs(5));
        assert_eq!(samples.len(), 5 * 48_000);
        let level =
            |from_ms: usize, to_ms: usize| teams_media::tone::amplitude_at(&samples[from_ms * 48..to_ms * 48], TONE.hz);
        assert!((level(100, 1_500) - 0.5).abs() < 0.02);
        assert!(level(1_650, 2_450) < 1e-6);
        assert!((level(2_600, 4_000) - 0.5).abs() < 0.02);
        assert!(level(4_150, 4_950) < 1e-6);
    }

    #[test]
    fn presence_counts_the_tone_however_it_is_keyed_and_finds_none_in_silence_or_at_another_frequency() {
        let mut received = vec![0.0; 48_000 * 2];
        received.extend(bursts(TONE, Duration::from_secs(5)).iter().map(|sample| sample * 0.3));
        let found = presence(TONE.hz, &received);
        assert!(found.heard(), "{found:?}");
        // two bursts of 1.6 s, their faded edges a stretch or so short of it
        assert!((28..=32).contains(&found.stretches), "{found:?}");
        assert_eq!(found.first, Some(Duration::from_secs(2)));
        assert!((found.peak - 0.15).abs() < 0.01, "{found:?}");
        assert!(!presence(660.0, &received).heard());
        let silent = presence(TONE.hz, &vec![0.0; 48_000 * 3]);
        assert_eq!((silent.heard(), silent.stretches, silent.first), (false, 0, None));
        // the keyed tone, heard too
        assert!(presence(TONE.hz, &keyed(TONE, Duration::from_secs(6))).heard());
    }

    #[test]
    fn the_meter_reads_a_tone_filling_the_window_at_its_amplitude_and_the_weight_s_share_back_to_where_it_set_in() {
        let mut window = [0.0; WINDOW];
        ToneGenerator::new(TONE).fill(&mut window);
        assert!((Meter::new(TONE.hz).level(&window) - 0.5).abs() < 0.01);
        assert!(Meter::new(660.0).level(&window) < 0.005);
        assert!((set_in(1.0)).abs() < 1e-3 && (set_in(0.0) - 1.0).abs() < 1e-3);
        assert!((set_in(0.5) - 0.5).abs() < 1e-3);
    }
}
