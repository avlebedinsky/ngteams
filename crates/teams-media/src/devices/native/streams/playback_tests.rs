//! What the default speaker plays, recorded through WASAPI's loopback of it: a tone fed at a call's pace comes out
//! whole. The playing tests need a Windows render endpoint and the console session, because Windows drops the audio of
//! session 0 (SSH).
//! Run it with `--ignored --test-threads=1` in the win11 console.

use std::sync::Mutex;
use std::time::Instant;

use super::*;
use crate::devices::{FRAME_DURATION, Reach};
use crate::tone::{Tone, ToneGenerator, purity};

/// 10.5 periods in each 10 ms frame: a dropped or repeated frame turns the tone's phase over, so the window that holds
/// the join is disturbed (a tone with whole periods in a frame would go on as one clean sine).
const TONE: Tone = Tone {
    hz: 1050.0,
    amplitude: 0.5,
};
/// 20 ms at 48 kHz: 21 whole periods of the tone.
const WINDOW: usize = 960;
/// A window is disturbed when what the tone does not explain is within this of the tone.
const DISTURBED_DB: f32 = 20.0;
/// The windows at the start of the tone that are not judged: the sink can jump once there, while its device starts.
const OPENING: usize = 20;
/// The 10 ms frames of the tone that a test plays: 4 s.
const FED: u32 = 400;
/// The windows that a whole tone gives at the least: the fed windows less one at each edge and the opening.
const WHOLE: usize = FED as usize * FRAME_SAMPLES / WINDOW - 2 - OPENING;

struct Recording {
    _running: Running,
    samples: Arc<Mutex<Vec<f32>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Recording {
    /// Records the default render endpoint through its loopback, from now until [`Recording::finish`].
    fn start() -> Self {
        let source = element("wasapi2src").unwrap();
        source.set_property("loopback", true);
        let sink = AppSink::builder().caps(&audio_caps()).sync(false).build();
        let running = playing(&[
            &source,
            &element("audioconvert").unwrap(),
            &element("audioresample").unwrap(),
            sink.upcast_ref(),
        ])
        .unwrap();
        let samples = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (into, stopped) = (Arc::clone(&samples), Arc::clone(&stop));
        let thread = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                if let Some(sample) = sample(&sink) {
                    let map = sample.buffer().unwrap().map_readable().unwrap();
                    let floats = map.as_slice().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b));
                    into.lock().unwrap().extend(floats);
                }
            }
        });
        Self {
            _running: running,
            samples,
            stop,
            thread: Some(thread),
        }
    }

    fn finish(mut self) -> Vec<f32> {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
        std::mem::take(&mut *self.samples.lock().unwrap())
    }
}

/// Plays `frames` of the tone 10 ms at a time. Each frame comes up to `late` after its due time, as libwebrtc's
/// playout timer gives it on Windows: a late frame makes the next ones come in a burst.
fn feed(speaker: &mut dyn AudioSink, frames: u32, late: Duration) {
    let mut tone = ToneGenerator::new(TONE);
    let mut seed = 0x2545_f491_u32;
    let start = Instant::now();
    for index in 0..frames {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let delay = late.mul_f64(f64::from(seed >> 8) / f64::from(1_u32 << 24));
        let due = start + FRAME_DURATION * index + delay;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        let mut frame = AudioFrame::silence();
        tone.fill(frame.samples_mut());
        speaker.play(&frame).unwrap();
    }
}

/// The windows of the played tone after its first `skip` (less one at each edge), and how many of them are disturbed.
fn disturbed(samples: &[f32], skip: usize) -> (usize, usize) {
    let fits: Vec<(f32, f32)> = samples
        .as_chunks::<WINDOW>()
        .0
        .iter()
        .map(|window| purity(window, TONE.hz))
        .collect();
    let audible = |(amplitude, _): &(f32, f32)| *amplitude > TONE.amplitude / 4.0;
    let (Some(first), Some(last)) = (fits.iter().position(audible), fits.iter().rposition(audible)) else {
        return (0, 0);
    };
    let stretch = fits.get(first + 1 + skip..last).unwrap_or_default();
    let bad = stretch.iter().filter(|(_, db)| *db < DISTURBED_DB).count();
    (stretch.len(), bad)
}

fn play_and_record(late: Duration) -> (usize, usize) {
    gst::init().unwrap();
    let devices = WindowsDevices::connect(Reach::Desktop).unwrap();
    let recording = Recording::start();
    thread::sleep(Duration::from_millis(300));
    let mut speaker = speaker(devices).unwrap();
    feed(speaker.as_mut(), FED, late);
    thread::sleep(Duration::from_millis(500));
    drop(speaker);
    let (windows, bad) = disturbed(&recording.finish(), OPENING);
    eprintln!("frames late by up to {late:?}: {bad} of {windows} windows disturbed");
    (windows, bad)
}

/// Nothing after the opening may be cut out or disturbed.
fn assert_whole((windows, bad): (usize, usize)) {
    assert!(windows >= WHOLE, "only {windows} windows of the tone played");
    assert_eq!(bad, 0, "{bad} of {windows} windows disturbed");
}

#[test]
#[ignore = "plays on the default speaker: run in the win11 console"]
fn a_tone_fed_on_time_plays_whole() {
    assert_whole(play_and_record(Duration::ZERO));
}

#[test]
#[ignore = "plays on the default speaker: run in the win11 console"]
fn a_tone_fed_with_the_playout_timer_s_jitter_plays_whole() {
    assert_whole(play_and_record(Duration::from_millis(16)));
}

#[test]
fn the_fit_finds_a_clean_tone_and_a_cut_in_it() {
    let mut tone = ToneGenerator::new(TONE);
    let mut samples = vec![0.0; WINDOW * 10];
    tone.fill(&mut samples);
    assert_eq!(disturbed(&samples, 0), (8, 0));
    samples[WINDOW * 4 + 100..WINDOW * 4 + 160].fill(0.0);
    assert_eq!(disturbed(&samples, 0), (8, 1));
    assert_eq!(disturbed(&samples, 9), (0, 0));
    assert_eq!(disturbed(&vec![0.0; WINDOW * 10], 0), (0, 0));
}

#[test]
fn the_fit_finds_a_dropped_or_a_repeated_frame() {
    let mut tone = ToneGenerator::new(TONE);
    let mut samples = vec![0.0; WINDOW * 11];
    tone.fill(&mut samples);
    let mut dropped = samples.clone();
    dropped.drain(WINDOW * 4 + 200..WINDOW * 4 + 200 + FRAME_SAMPLES);
    assert_eq!(disturbed(&dropped, 0), (8, 1));
    let mut repeated = samples[..WINDOW * 10].to_vec();
    let frame = repeated[WINDOW * 4 + 200..][..FRAME_SAMPLES].to_vec();
    repeated.splice(WINDOW * 4 + 200..WINDOW * 4 + 200, frame);
    assert_eq!(disturbed(&repeated[..WINDOW * 10], 0), (8, 1));
}

#[test]
fn a_whole_tone_of_the_fed_length_passes_where_ever_the_windows_fall() {
    for offset in [0, WINDOW / 5, WINDOW / 2, WINDOW * 4 / 5] {
        let mut tone = ToneGenerator::new(TONE);
        let mut samples = vec![0.0; WINDOW * 210];
        tone.fill(&mut samples[WINDOW * 5 + offset..][..FED as usize * FRAME_SAMPLES]);
        assert_whole(disturbed(&samples, OPENING));
    }
}
