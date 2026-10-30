//! The microphone's audio processing: echo cancellation, noise suppression and gain control, by libwebrtc's own audio
//! processing module (AEC3, the noise suppressor, AGC2), on the device seam's 10 ms mono frames at 48 kHz.
//!
//! Two sides feed one [`AudioProcessing`]: what the speaker plays is the far end ([`AudioProcessing::far_end`],
//! analysed, never changed), and what the microphone hears is the near end ([`AudioProcessing::near_end`], changed in
//! place: the far end's echo taken out, steady noise lowered, the level brought up or down). The two sides run on
//! different threads (the session's playout and capture threads), so one [`AudioProcessing`] is shared between them
//! behind an [`Arc`](std::sync::Arc), the module behind a lock. The echo path's delay is the module's own estimate:
//! the session never tells it one.
//!
//! The session runs it itself rather than leaving it to libwebrtc's audio source: the far end is played by the device
//! seam, not by a libwebrtc audio device, so only the session can hand the module what the speaker plays.

use std::sync::{Mutex, PoisonError};

use libwebrtc::native::apm::AudioProcessingModule;

use crate::Error;
use crate::devices::{AudioFrame, FRAME_SAMPLES, SAMPLE_RATE};

/// Which parts of the processing run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessingConfig {
    /// AEC3, with the high-pass filter it needs.
    pub echo_cancellation: bool,
    /// Steady background noise lowered.
    pub noise_suppression: bool,
    /// The adaptive digital gain controller (AGC2) and its limiter.
    pub gain_control: bool,
}

impl ProcessingConfig {
    /// What a call runs: all three.
    pub const CALL: Self = Self {
        echo_cancellation: true,
        noise_suppression: true,
        gain_control: true,
    };

    /// Nothing: the microphone's frames pass unchanged.
    pub const OFF: Self = Self {
        echo_cancellation: false,
        noise_suppression: false,
        gain_control: false,
    };

    /// Whether any part runs.
    #[must_use]
    pub fn any(self) -> bool {
        self.echo_cancellation || self.noise_suppression || self.gain_control
    }
}

impl Default for ProcessingConfig {
    fn default() -> Self {
        Self::CALL
    }
}

/// One call's audio processing: the far end it hears played, the near end it cleans.
pub struct AudioProcessing {
    module: Mutex<AudioProcessingModule>,
    config: ProcessingConfig,
}

impl std::fmt::Debug for AudioProcessing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioProcessing")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl AudioProcessing {
    /// A module running `config` on frames at [`SAMPLE_RATE`].
    ///
    /// # Errors
    /// Never today: kept for a module that can refuse its setup.
    pub fn new(config: ProcessingConfig) -> Result<Self, Error> {
        let module = AudioProcessingModule::new(
            config.echo_cancellation,
            config.gain_control,
            config.echo_cancellation || config.noise_suppression,
            config.noise_suppression,
        );
        Ok(Self {
            module: Mutex::new(module),
            config,
        })
    }

    #[must_use]
    pub fn config(&self) -> ProcessingConfig {
        self.config
    }

    /// Hands the module a frame the speaker plays (the far end, whose echo the microphone may hear), unchanged.
    ///
    /// # Errors
    /// [`Error::Processing`] when the module refuses the frame.
    pub fn far_end(&self, played: &AudioFrame) -> Result<(), Error> {
        if !self.config.any() {
            return Ok(());
        }
        let mut samples = to_i16(played);
        self.module
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .process_reverse_stream(&mut samples, rate(), 1)
            .map_err(|error| Error::Processing(error.message))
    }

    /// Processes a frame the microphone heard, in place.
    ///
    /// # Errors
    /// [`Error::Processing`] when the module refuses the frame.
    pub fn near_end(&self, captured: &mut AudioFrame) -> Result<(), Error> {
        if !self.config.any() {
            // nothing runs: the frame stays exactly as it was (not even through 16-bit samples)
            return Ok(());
        }
        let mut samples = to_i16(captured);
        self.module
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .process_stream(&mut samples, rate(), 1)
            .map_err(|error| Error::Processing(error.message))?;
        for (out, sample) in captured.samples_mut().iter_mut().zip(samples) {
            *out = from_i16(sample);
        }
        Ok(())
    }
}

fn rate() -> i32 {
    i32::try_from(SAMPLE_RATE).unwrap_or(i32::MAX)
}

/// A frame's samples as 16-bit PCM, what libwebrtc takes.
#[must_use]
pub fn to_i16(frame: &AudioFrame) -> [i16; FRAME_SAMPLES] {
    let mut out = [0; FRAME_SAMPLES];
    for (out, sample) in out.iter_mut().zip(frame.samples()) {
        #[expect(clippy::cast_possible_truncation, reason = "clamped to the 16-bit range first")]
        let scaled = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        *out = scaled;
    }
    out
}

/// A 16-bit PCM sample as the device seam's `[-1, 1]`.
#[must_use]
pub fn from_i16(sample: i16) -> f32 {
    f32::from(sample) / f32::from(i16::MAX)
}
