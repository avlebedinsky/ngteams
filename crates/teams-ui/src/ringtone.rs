//! The incoming call's ringtone (M3 Task 17): two tones and a pause, played on the default output while an invitation
//! rings, through the device seam (`teams-media`'s [`Devices`]: the app's PipeWire, the one its build allows, so the
//! lab app's ringtone reaches only the UI sandbox's speaker).
//!
//! A [`Ringer`] starts it ([`Ringer::start`]) and the [`Ringtone`] it hands back stops it when dropped. The
//! [`DeviceRinger`] plays on a thread of its own, opening the speaker there (an open can wait on the audio server),
//! ahead of real time by at most [`LEAD`]; a speaker that can't be opened or goes away ends the ringtone quietly (the
//! banner and the notification still show the call).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use teams_media::devices::{AudioFrame, Devices, FRAME_DURATION, FRAME_SAMPLES};
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
/// How far ahead of real time the ringtone is queued on the speaker.
pub const LEAD: Duration = Duration::from_millis(100);

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

/// What plays the ringtone: in the app the [`DeviceRinger`]; in a view test a fake that counts.
pub trait Ringer: Send + Sync + 'static {
    /// Starts the ringtone; it plays until the [`Ringtone`] is dropped.
    fn start(&self) -> Ringtone;
}

/// The ringtone on the speaker of `devices` (see the module docs).
pub struct DeviceRinger {
    devices: Arc<dyn Devices + Send + Sync>,
}

impl std::fmt::Debug for DeviceRinger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceRinger").finish_non_exhaustive()
    }
}

impl DeviceRinger {
    #[must_use]
    pub fn new(devices: Arc<dyn Devices + Send + Sync>) -> Self {
        Self { devices }
    }
}

impl Ringer for DeviceRinger {
    fn start(&self) -> Ringtone {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let devices = Arc::clone(&self.devices);
        let started = thread::Builder::new()
            .name("teams-ui-ringtone".to_owned())
            .spawn(move || play(&*devices, &stopped));
        if let Err(error) = started {
            log::warn!("the ringtone can't start: {error}");
        }
        Ringtone::new(move || stop.store(true, Ordering::Release))
    }
}

/// Plays the ringtone on the speaker of `devices` until `stop` is set, in real time.
fn play(devices: &(dyn Devices + Send + Sync), stop: &AtomicBool) {
    let mut speaker = match devices.speaker() {
        Ok(speaker) => speaker,
        Err(error) => {
            log::warn!("the ringtone's speaker can't be opened: {error}");
            return;
        }
    };
    let start = Instant::now();
    for (played, frame) in RingtoneFrames::default().enumerate() {
        if stop.load(Ordering::Acquire) {
            return;
        }
        if let Err(error) = speaker.play(&frame) {
            log::warn!("the ringtone's speaker went away: {error}");
            return;
        }
        let due = start + FRAME_DURATION * u32::try_from(played + 1).unwrap_or(u32::MAX);
        thread::sleep(due.saturating_duration_since(Instant::now() + LEAD));
    }
}

#[cfg(test)]
mod tests {
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
