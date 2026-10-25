//! The file backend: no audio server and no camera. The microphone is a tone, a WAV file or silence; the speaker
//! records to a WAV file (or nowhere); the camera is the [`TestPattern`]; received video is counted
//! ([`FrameCounter`]).
//!
//! With [`Pacing::RealTime`] (what a call needs) a source hands out frames as fast as a device would: the next frame
//! is due one frame's time after the last, counted from the first, and a read waits for it (or gives `None` when the
//! timeout ends first). [`Pacing::Unpaced`] hands them out at once, for tests that don't need the clock.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::pattern::{self, TestPattern};
use super::wav::{self, WavWriter};
use super::{
    AudioFrame, AudioSink, AudioSource, DeviceError, Devices, FRAME_DURATION, FRAME_SAMPLES, VideoFrame, VideoSink,
    VideoSource,
};
use crate::tone::{Tone, ToneGenerator};

/// How fast sources hand out frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pacing {
    /// As a device would: one frame per frame time.
    RealTime,
    /// At once.
    Unpaced,
}

/// What the microphone hears.
#[derive(Clone, Debug, PartialEq)]
pub enum AudioInput {
    Tone(Tone),
    /// A WAV file at 48 kHz, played once, then silence.
    Wav(PathBuf),
    Silence,
}

/// The camera stand-in: the test pattern at a size and frame rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PatternCamera {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Default for PatternCamera {
    fn default() -> Self {
        Self {
            width: 640,
            height: 360,
            fps: 15,
        }
    }
}

/// The file backend's devices.
#[derive(Clone, Debug, PartialEq)]
pub struct FileDevices {
    pub input: AudioInput,
    /// Where the speaker records; `None` drops what is played.
    pub recording: Option<PathBuf>,
    pub camera: PatternCamera,
    pub pacing: Pacing,
}

impl Devices for FileDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        let samples = match &self.input {
            AudioInput::Tone(tone) => Samples::Tone(ToneGenerator::new(*tone)),
            AudioInput::Wav(path) => Samples::Wav {
                samples: wav::read(path)?,
                at: 0,
            },
            AudioInput::Silence => Samples::Silence,
        };
        Ok(Box::new(FileSource {
            samples,
            clock: Clock::new(self.pacing, FRAME_DURATION),
        }))
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        Ok(match &self.recording {
            Some(path) => Box::new(Recorder(WavWriter::create(path)?)),
            None => Box::new(Discard),
        })
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        let fps = self.camera.fps.max(1);
        Ok(Box::new(PatternSource {
            pattern: TestPattern::new(self.camera.width, self.camera.height),
            index: 0,
            clock: Clock::new(self.pacing, Duration::from_secs(1) / fps),
        }))
    }
}

/// When the next frame is due.
struct Clock {
    pacing: Pacing,
    period: Duration,
    start: Option<Instant>,
    handed: u32,
}

impl Clock {
    fn new(pacing: Pacing, period: Duration) -> Self {
        Self {
            pacing,
            period,
            start: None,
            handed: 0,
        }
    }

    /// Waits for the next frame's time, up to `timeout`; whether it came.
    fn wait(&mut self, timeout: Duration) -> bool {
        if self.pacing == Pacing::Unpaced {
            return true;
        }
        let now = Instant::now();
        let start = *self.start.get_or_insert(now);
        let due = start + self.period * self.handed;
        let wait = due.saturating_duration_since(now);
        if wait > timeout {
            thread::sleep(timeout);
            return false;
        }
        thread::sleep(wait);
        true
    }

    fn handed_one(&mut self) {
        self.handed = self.handed.saturating_add(1);
    }
}

enum Samples {
    Tone(ToneGenerator),
    Wav { samples: Vec<f32>, at: usize },
    Silence,
}

struct FileSource {
    samples: Samples,
    clock: Clock,
}

impl AudioSource for FileSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<AudioFrame>, DeviceError> {
        if !self.clock.wait(timeout) {
            return Ok(None);
        }
        self.clock.handed_one();
        let mut frame = AudioFrame::silence();
        match &mut self.samples {
            Samples::Tone(generator) => generator.fill(frame.samples_mut()),
            Samples::Wav { samples, at } => {
                let end = (*at + FRAME_SAMPLES).min(samples.len());
                let part = samples.get(*at..end).unwrap_or_default();
                frame.samples_mut()[..part.len()].copy_from_slice(part);
                *at = end;
            }
            Samples::Silence => {}
        }
        Ok(Some(frame))
    }
}

struct Recorder(WavWriter);

impl AudioSink for Recorder {
    fn play(&mut self, frame: &AudioFrame) -> Result<(), DeviceError> {
        self.0.write(frame.samples())
    }
}

struct Discard;

impl AudioSink for Discard {
    fn play(&mut self, _frame: &AudioFrame) -> Result<(), DeviceError> {
        Ok(())
    }
}

struct PatternSource {
    pattern: TestPattern,
    index: u32,
    clock: Clock,
}

impl VideoSource for PatternSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, DeviceError> {
        if !self.clock.wait(timeout) {
            return Ok(None);
        }
        self.clock.handed_one();
        let frame = self.pattern.frame(self.index);
        self.index = self.index.wrapping_add(1);
        Ok(Some(frame))
    }
}

/// What a [`FrameCounter`] saw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameCounts {
    /// Every frame received.
    pub frames: u64,
    /// Frames that differ from the one before (a frozen picture doesn't count).
    pub changed: u64,
    /// Frames carrying a test pattern index ([`pattern::index_of`]).
    pub indexed: u64,
    /// Indexed frames whose index wasn't the one after the last indexed frame's (a frame lost on the way).
    pub gaps: u64,
    pub last_index: Option<u32>,
}

/// Counts received frames (`teams-headless` summarises them on its output); [`FrameCounter::counts`] reads them from
/// any thread.
#[derive(Clone, Debug, Default)]
pub struct FrameCounter {
    counts: Arc<Mutex<FrameCounts>>,
    last: Arc<Mutex<Option<VideoFrame>>>,
}

impl FrameCounter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The counts so far.
    #[must_use]
    pub fn counts(&self) -> FrameCounts {
        *self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl VideoSink for FrameCounter {
    fn show(&mut self, frame: &VideoFrame) -> Result<(), DeviceError> {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        counts.frames += 1;
        if last.as_ref() != Some(frame) {
            counts.changed += 1;
        }
        if let Some(index) = pattern::index_of(frame) {
            counts.indexed += 1;
            if counts
                .last_index
                .is_some_and(|previous| previous.wrapping_add(1) != index)
            {
                counts.gaps += 1;
            }
            counts.last_index = Some(index);
        }
        *last = Some(frame.clone());
        Ok(())
    }
}
