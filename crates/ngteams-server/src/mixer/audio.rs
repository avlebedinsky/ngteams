//! A room's audio math, without a codec: each leg's sample buffer, the mix each listener hears, who is active and who
//! speaks. Everything runs on the room's 20 ms tick.
//!
//! The buffer is the one piece of media logic of our own (libwebrtc's `NetEq` has no maintained Rust equivalent): it
//! holds samples, not packets, so any packet length works; it starts playing once [`TARGET`] is buffered, is concealed
//! for a few ticks when it runs dry, and is trimmed back to [`TARGET`] when a sender's clock runs ahead of ours.

use std::collections::VecDeque;

/// The sample rate of every leg's audio.
pub(crate) const RATE: u32 = 48_000;
/// One tick's samples: 20 ms.
pub(crate) const FRAME: usize = 960;
/// How much a buffer holds before it plays, and is trimmed back to: 60 ms.
const TARGET: usize = 3 * FRAME;
/// The most a buffer holds before it is trimmed: 200 ms.
const MOST: usize = 10 * FRAME;
/// How many ticks in a row a dry buffer is concealed before it falls silent.
const CONCEALED: u8 = 3;
/// A source is active while its level was above this within [`ACTIVE_TICKS`].
const ACTIVE_DBFS: f32 = -50.0;
const ACTIVE_TICKS: u32 = 10;
/// A source speaks while its level is above this in [`SPEAKING_OF`] of the last five ticks.
const SPEAKING_DBFS: f32 = -45.0;
const SPEAKING_OF: u32 = 3;
/// The level of silence.
const FLOOR_DBFS: f32 = -100.0;

/// What a tick took from a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Took {
    /// A frame of what came.
    Played,
    /// Nothing came in time: the decoder conceals the frame.
    Conceal,
    /// Nothing to play: silence.
    Silent,
}

/// One leg's samples, from its decoder to the mix.
#[derive(Debug, Default)]
pub(crate) struct Buffer {
    samples: VecDeque<f32>,
    playing: bool,
    dry: u8,
}

impl Buffer {
    /// Decoded samples that came; above [`MOST`] the oldest go, down to [`TARGET`].
    pub(crate) fn push(&mut self, samples: &[f32]) {
        self.samples.extend(samples);
        if self.samples.len() > MOST {
            let over = self.samples.len() - TARGET;
            self.samples.drain(..over);
        }
    }

    /// The next tick's frame into `frame`: what came, or what the tick does instead ([`Took`]).
    pub(crate) fn take(&mut self, frame: &mut [f32; FRAME]) -> Took {
        if !self.playing && self.samples.len() >= TARGET {
            self.playing = true;
        }
        if !self.playing {
            frame.fill(0.0);
            return Took::Silent;
        }
        if self.samples.len() >= FRAME {
            for (sample, kept) in frame.iter_mut().zip(self.samples.drain(..FRAME)) {
                *sample = kept;
            }
            self.dry = 0;
            return Took::Played;
        }
        self.dry += 1;
        if self.dry > CONCEALED {
            self.playing = false;
            self.dry = 0;
            frame.fill(0.0);
            return Took::Silent;
        }
        Took::Conceal
    }

    /// How many samples it holds.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.samples.len()
    }
}

/// The level of `frame` in dBFS (its RMS against full scale); silence is [`FLOOR_DBFS`].
pub(crate) fn dbfs(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return FLOOR_DBFS;
    }
    let length = f32::from(u16::try_from(frame.len()).unwrap_or(u16::MAX));
    let power = frame.iter().map(|sample| sample * sample).sum::<f32>() / length;
    if power <= 0.0 {
        return FLOOR_DBFS;
    }
    (10.0 * power.log10()).max(FLOOR_DBFS)
}

/// One leg's frame of a tick, as the mix takes it.
#[derive(Clone, Debug)]
pub(crate) struct Heard {
    pub frame: [f32; FRAME],
    /// Whether it is mixed at all ([`Activity`]).
    pub active: bool,
}

/// What `listener` hears of `heard`: the sum of every active frame but its own; `None` when nobody else is active (a
/// silent frame).
pub(crate) fn mix_for(listener: usize, heard: &[Heard]) -> Option<[f32; FRAME]> {
    let mut mixed: Option<[f32; FRAME]> = None;
    for (index, source) in heard.iter().enumerate() {
        if index == listener || !source.active {
            continue;
        }
        let sum = mixed.get_or_insert([0.0; FRAME]);
        for (out, sample) in sum.iter_mut().zip(source.frame.iter()) {
            *out += sample;
        }
    }
    mixed
}

/// Whether a leg's audio is mixed: it was above [`ACTIVE_DBFS`] within the last [`ACTIVE_TICKS`].
#[derive(Debug, Default)]
pub(crate) struct Activity {
    since_loud: Option<u32>,
}

impl Activity {
    /// One tick's level; whether the leg is active now.
    pub(crate) fn hear(&mut self, dbfs: f32) -> bool {
        self.since_loud = if dbfs > ACTIVE_DBFS {
            Some(0)
        } else {
            self.since_loud
                .map(|ticks| ticks + 1)
                .filter(|ticks| *ticks < ACTIVE_TICKS)
        };
        self.since_loud.is_some()
    }
}

/// One source's recent levels: which of the last five ticks were loud.
#[derive(Debug, Default)]
struct Voice {
    loud: u8,
}

impl Voice {
    /// One tick's level; whether it speaks now.
    fn hear(&mut self, dbfs: f32) -> bool {
        self.loud = ((self.loud << 1) | u8::from(dbfs > SPEAKING_DBFS)) & 0b1_1111;
        self.loud.count_ones() >= SPEAKING_OF
    }
}

/// Who spoke in a room: the audio source ids, the newest speaker first.
#[derive(Debug, Default)]
pub(crate) struct Speakers {
    /// Each source's voice, and whether it spoke at the last tick.
    voices: Vec<(u32, Voice, bool)>,
    history: Vec<u32>,
}

impl Speakers {
    /// The source `source`'s level this tick: one that starts to speak leads the history, and a leader that stops
    /// gives way to the newest one still speaking.
    pub(crate) fn hear(&mut self, source: u32, dbfs: f32) {
        let at = if let Some(at) = self.voices.iter().position(|(id, ..)| *id == source) {
            at
        } else {
            self.voices.push((source, Voice::default(), false));
            self.voices.len() - 1
        };
        let Some((_, voice, speaking)) = self.voices.get_mut(at) else {
            return;
        };
        let now = voice.hear(dbfs);
        let was = std::mem::replace(speaking, now);
        if now && !was {
            self.lead(source);
        } else if was && !now && self.history.first() == Some(&source) {
            self.give_way();
        }
    }

    /// The source `source` is gone from the room.
    pub(crate) fn forget(&mut self, source: u32) {
        self.voices.retain(|(id, ..)| *id != source);
        self.history.retain(|id| *id != source);
        self.give_way();
    }

    pub(crate) fn history(&self) -> &[u32] {
        &self.history
    }

    fn lead(&mut self, source: u32) {
        self.history.retain(|id| *id != source);
        self.history.insert(0, source);
    }

    /// A leader that doesn't speak gives way to the newest one who does: the client marks the history's first as the
    /// one speaking while it hears anything.
    fn give_way(&mut self) {
        let speaks = |source: &u32| self.voices.iter().any(|(id, _, speaking)| id == source && *speaking);
        if self.history.first().is_none_or(speaks) {
            return;
        }
        if let Some(next) = self.history.iter().copied().find(speaks) {
            self.lead(next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amplitude: f32) -> [f32; FRAME] {
        let mut frame = [0.0; FRAME];
        let step = std::f32::consts::TAU * 440.0 / 48_000.0;
        let mut phase = 0.0_f32;
        for sample in &mut frame {
            *sample = amplitude * phase.sin();
            phase += step;
        }
        frame
    }

    fn heard(level: f32) -> Heard {
        Heard {
            frame: [level; FRAME],
            active: level != 0.0,
        }
    }

    #[test]
    fn each_listener_hears_everyone_but_itself() {
        let room = [heard(0.1), heard(0.2), heard(0.4)];
        let mixed: Vec<f32> = (0..3).map(|listener| mix_for(listener, &room).unwrap()[0]).collect();
        let expected = [0.6, 0.5, 0.3];
        for (got, want) in mixed.iter().zip(expected) {
            assert!((got - want).abs() < 1e-6, "{mixed:?}");
        }
        // an inactive source isn't mixed, and someone alone hears nothing
        let quiet = [heard(0.1), heard(0.0)];
        assert!(mix_for(0, &quiet).is_none());
        assert!((mix_for(1, &quiet).unwrap()[0] - 0.1).abs() < 1e-6);
        assert!(mix_for(0, &[heard(0.3)]).is_none());
    }

    #[test]
    fn a_late_or_fast_sender_is_concealed_then_trimmed() {
        let mut buffer = Buffer::default();
        let mut frame = [0.0; FRAME];
        // nothing plays until 60 ms are in
        buffer.push(&[0.5; 2 * FRAME]);
        assert_eq!(buffer.take(&mut frame), Took::Silent);
        buffer.push(&[0.5; FRAME]);
        for _ in 0..3 {
            assert_eq!(buffer.take(&mut frame), Took::Played);
        }
        assert!((frame[0] - 0.5).abs() < 1e-6);
        // late: three ticks concealed, then silence until the buffer fills again
        for _ in 0..3 {
            assert_eq!(buffer.take(&mut frame), Took::Conceal);
        }
        assert_eq!(buffer.take(&mut frame), Took::Silent);
        assert!(frame.iter().all(|sample| sample.abs() < f32::EPSILON));
        buffer.push(&[0.25; FRAME]);
        assert_eq!(buffer.take(&mut frame), Took::Silent);
        // fast: past 200 ms it is trimmed to 60 ms, the oldest going
        let mut fast = Buffer::default();
        fast.push(&vec![0.1; 5 * FRAME]);
        fast.push(&vec![0.9; 6 * FRAME]);
        assert_eq!(fast.len(), TARGET);
        assert_eq!(fast.take(&mut frame), Took::Played);
        assert!((frame[0] - 0.9).abs() < 1e-6);
    }

    #[test]
    fn a_level_is_measured_against_full_scale() {
        assert!((dbfs(&tone(1.0)) - (-3.01)).abs() < 0.1);
        assert!((dbfs(&tone(0.01)) - (-43.01)).abs() < 0.1);
        assert!((dbfs(&[0.0; FRAME]) - FLOOR_DBFS).abs() < f32::EPSILON);
        assert!((dbfs(&[]) - FLOOR_DBFS).abs() < f32::EPSILON);
    }

    #[test]
    fn a_source_stays_active_for_200_ms_after_its_last_loud_frame() {
        let mut activity = Activity::default();
        assert!(!activity.hear(-90.0));
        assert!(activity.hear(-20.0));
        for _ in 0..9 {
            assert!(activity.hear(-90.0));
        }
        assert!(!activity.hear(-90.0));
    }

    #[test]
    fn the_loudest_recent_speaker_leads_the_history() {
        let mut speakers = Speakers::default();
        let tick = |speakers: &mut Speakers, loud: &[u32]| {
            for source in [1000, 1010, 1020] {
                speakers.hear(source, if loud.contains(&source) { -20.0 } else { -90.0 });
            }
        };
        // a blip of one or two ticks isn't speech
        tick(&mut speakers, &[1010]);
        tick(&mut speakers, &[1010]);
        tick(&mut speakers, &[]);
        assert!(speakers.history().is_empty());
        for _ in 0..3 {
            tick(&mut speakers, &[1000]);
        }
        assert_eq!(speakers.history(), [1000]);
        for _ in 0..3 {
            tick(&mut speakers, &[1020]);
        }
        assert_eq!(speakers.history(), [1020, 1000]);
        // a speaker who speaks again leads again
        for _ in 0..3 {
            tick(&mut speakers, &[1000]);
        }
        assert_eq!(speakers.history(), [1000, 1020]);
        for _ in 0..30 {
            tick(&mut speakers, &[]);
        }
        for _ in 0..3 {
            tick(&mut speakers, &[1020]);
        }
        assert_eq!(speakers.history(), [1020, 1000]);
        speakers.forget(1020);
        assert_eq!(speakers.history(), [1000]);
    }

    /// `ticks` ticks of the sources 1000, 1010 and 1020, those in `loud` speaking.
    fn talk(speakers: &mut Speakers, ticks: usize, loud: &[u32]) {
        for _ in 0..ticks {
            for source in [1000, 1010, 1020] {
                speakers.hear(source, if loud.contains(&source) { -20.0 } else { -90.0 });
            }
        }
    }

    #[test]
    fn a_speaker_who_stops_while_another_talks_on_gives_the_head_to_the_one_who_talks() {
        let mut speakers = Speakers::default();
        talk(&mut speakers, 3, &[1000]);
        talk(&mut speakers, 3, &[1000, 1010]);
        assert_eq!(speakers.history(), [1010, 1000]);
        // 1010 stops (or mutes) and 1000 talks on without a pause: the one who talks leads
        talk(&mut speakers, 5, &[1000]);
        assert_eq!(speakers.history(), [1000, 1010]);
        // nobody talks: the last speaker stays first
        talk(&mut speakers, 5, &[]);
        assert_eq!(speakers.history(), [1000, 1010]);
        // the newest who still talks leads, past one who stopped
        talk(&mut speakers, 3, &[1020]);
        talk(&mut speakers, 3, &[1020, 1010]);
        talk(&mut speakers, 3, &[1020, 1010, 1000]);
        assert_eq!(speakers.history(), [1000, 1010, 1020]);
        talk(&mut speakers, 5, &[1020, 1000]);
        assert_eq!(speakers.history(), [1000, 1010, 1020]);
        talk(&mut speakers, 5, &[1020]);
        assert_eq!(speakers.history(), [1020, 1000, 1010]);
        // a head who leaves gives the head to one who talks, past one who doesn't
        talk(&mut speakers, 3, &[1020, 1010]);
        talk(&mut speakers, 3, &[1020, 1010, 1000]);
        talk(&mut speakers, 5, &[1020, 1000]);
        assert_eq!(speakers.history(), [1000, 1010, 1020]);
        speakers.forget(1000);
        assert_eq!(speakers.history(), [1020, 1010]);
    }
}
