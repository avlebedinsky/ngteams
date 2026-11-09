//! The incoming call's ringtone (M3 Task 17): by default two tones and a pause, played on the calls' chosen speaker
//! (or the system default) while an invitation rings, through the device seam
//! (`teams-media`'s [`teams_media::devices::Devices`]): the PipeWire opened with the reach selected
//! at startup, desktop for the normal app and sandbox for lab and test runs. Which sound it is, how loud and whether
//! it plays are the settings' ([`crate::sounds`]).
//!
//! A [`Ringer`] starts it ([`Ringer::start`]) and the [`Ringtone`] it hands back stops it when dropped. The
//! [`crate::sounds::SoundBoard`] plays it; a speaker that can't be opened or goes away ends the ringtone
//! quietly (the banner and the notification still show the call).

use teams_media::devices::{AudioFrame, FRAME_SAMPLES};
use teams_media::tone::{Tone, ToneGenerator};

/// The ringtone's two tones, one after the other.
pub const RINGTONE: [Tone; 2] = [
    Tone {
        hz: 784.0,
        amplitude: 0.2,
    },
    Tone {
        hz: 1_047.0,
        amplitude: 0.2,
    },
];
/// How many 10 ms frames each tone lasts (400 ms).
pub const TONE_FRAMES: usize = 40;
/// How many frames a whole ring lasts, the pause after the tones included (2 s).
pub const RING_FRAMES: usize = 200;

/// The ringtone's frames, one after the other, ring after ring.
#[derive(Clone, Debug)]
pub struct RingtoneFrames {
    tones: [ToneGenerator; 2],
    next: usize,
}

impl Default for RingtoneFrames {
    fn default() -> Self {
        Self {
            tones: RINGTONE.map(ToneGenerator::new),
            next: 0,
        }
    }
}

impl Iterator for RingtoneFrames {
    type Item = AudioFrame;

    fn next(&mut self) -> Option<AudioFrame> {
        let at = self.next % RING_FRAMES;
        self.next += 1;
        let mut frame = AudioFrame::silence();
        if let Some(tone) = self.tones.get_mut(at / TONE_FRAMES) {
            tone.fill(&mut frame.samples_mut()[..FRAME_SAMPLES]);
        }
        Some(frame)
    }
}

/// The ringtone while it plays: dropped, it stops.
pub struct Ringtone {
    stop: Option<Box<dyn FnOnce() + Send>>,
}

impl std::fmt::Debug for Ringtone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ringtone").finish_non_exhaustive()
    }
}

impl Ringtone {
    /// A ringtone that `stop` stops.
    #[must_use]
    pub fn new(stop: impl FnOnce() + Send + 'static) -> Self {
        Self {
            stop: Some(Box::new(stop)),
        }
    }
}

impl Drop for Ringtone {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}

/// What plays the ringtone: in the app the [`crate::sounds::SoundBoard`]; in a view test a fake that counts.
pub trait Ringer: Send + Sync + 'static {
    /// Starts the ringtone; it plays until the [`Ringtone`] is dropped.
    fn start(&self) -> Ringtone;

    /// Whether a ringtone would be heard for `account` now (`in_call`: it is in a call), as the settings say.
    fn audible(&self, _account: &str, _in_call: bool) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use teams_media::tone::amplitude_at;

    use super::*;

    fn samples(frames: impl Iterator<Item = AudioFrame>) -> Vec<f32> {
        frames.flat_map(|frame| frame.samples().to_vec()).collect()
    }

    #[test]
    fn a_ring_is_its_two_tones_then_a_pause() {
        let ring = samples(RingtoneFrames::default().take(RING_FRAMES));
        let tone = TONE_FRAMES * FRAME_SAMPLES;
        let (first, second, pause) = (&ring[..tone], &ring[tone..2 * tone], &ring[2 * tone..]);
        assert!(amplitude_at(first, RINGTONE[0].hz) > 0.15);
        assert!(amplitude_at(first, RINGTONE[1].hz) < 0.02);
        assert!(amplitude_at(second, RINGTONE[1].hz) > 0.15);
        assert!(pause.iter().all(|sample| *sample == 0.0));
        // and again
        let next = samples(RingtoneFrames::default().skip(RING_FRAMES).take(TONE_FRAMES));
        assert!(amplitude_at(&next, RINGTONE[0].hz) > 0.15);
    }

    #[test]
    fn a_ringtone_stops_once_dropped() {
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stopped);
        let ringtone = Ringtone::new(move || flag.store(true, Ordering::Release));
        assert!(!stopped.load(Ordering::Acquire));
        drop(ringtone);
        assert!(stopped.load(Ordering::Acquire));
    }
}
