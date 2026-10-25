//! The microphone's audio processing: echo cancellation, noise suppression and gain control, by WebRTC's audio
//! processing module (`webrtc-audio-processing`, AEC3), on the device seam's 10 ms mono frames at 48 kHz.
//!
//! Two sides feed one [`AudioProcessing`]: what the speaker plays is the far end ([`AudioProcessing::far_end`],
//! analysed, never changed), and what the microphone hears is the near end ([`AudioProcessing::near_end`], changed in
//! place: the far end's echo taken out, steady noise lowered, the level brought up or down). The two sides run on
//! different threads (the session's playout and capture threads); the module locks each side itself, so one
//! [`AudioProcessing`] is shared between them behind an [`Arc`](std::sync::Arc). The echo path's delay is the
//! module's own estimate: the session never tells it one.
//!
//! The C++ library is built from the source the binding bundles (against the system's Abseil), and logs nothing: its
//! build defines `NDEBUG`, which leaves its logger off.

use webrtc_audio_processing::Processor;
use webrtc_audio_processing::config::{
    AdaptiveDigital, Config, EchoCanceller, GainController, GainController2, HighPassFilter, NoiseSuppression,
    NoiseSuppressionLevel,
};

use crate::Error;
use crate::devices::{AudioFrame, SAMPLE_RATE};

/// Which parts of the processing run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessingConfig {
    /// AEC3, with the high-pass filter it needs.
    pub echo_cancellation: bool,
    /// Steady background noise lowered (the moderate level: speech keeps its shape).
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

    fn module_config(self) -> Config {
        Config {
            high_pass_filter: (self.echo_cancellation || self.noise_suppression).then(HighPassFilter::default),
            echo_canceller: self.echo_cancellation.then(EchoCanceller::default),
            noise_suppression: self.noise_suppression.then_some(NoiseSuppression {
                level: NoiseSuppressionLevel::Moderate,
                analyze_linear_aec_output: false,
            }),
            gain_controller: self
                .gain_control
                .then_some(GainController::GainController2(GainController2 {
                    input_volume_controller_enabled: false,
                    adaptive_digital: Some(AdaptiveDigital::default()),
                    ..GainController2::default()
                })),
            ..Config::default()
        }
    }
}

impl Default for ProcessingConfig {
    fn default() -> Self {
        Self::CALL
    }
}

/// One call's audio processing: the far end it hears played, the near end it cleans.
#[derive(Debug)]
pub struct AudioProcessing {
    processor: Processor,
    config: ProcessingConfig,
}

impl AudioProcessing {
    /// A module running `config` on frames at [`SAMPLE_RATE`].
    ///
    /// # Errors
    /// [`Error::Processing`] when the library can't be set up.
    pub fn new(config: ProcessingConfig) -> Result<Self, Error> {
        let processor = Processor::new(SAMPLE_RATE).map_err(|error| processing(&error))?;
        processor.set_config(config.module_config());
        Ok(Self { processor, config })
    }

    #[must_use]
    pub fn config(&self) -> ProcessingConfig {
        self.config
    }

    /// Hands the module a frame the speaker plays (the far end, whose echo the microphone may hear), unchanged.
    ///
    /// # Errors
    /// [`Error::Processing`] when the library refuses the frame.
    pub fn far_end(&self, played: &AudioFrame) -> Result<(), Error> {
        self.processor
            .analyze_render_frame([played.samples().as_slice()])
            .map_err(|error| processing(&error))
    }

    /// Processes a frame the microphone heard, in place.
    ///
    /// # Errors
    /// [`Error::Processing`] when the library refuses the frame.
    pub fn near_end(&self, captured: &mut AudioFrame) -> Result<(), Error> {
        self.processor
            .process_capture_frame([captured.samples_mut().as_mut_slice()])
            .map_err(|error| processing(&error))
    }
}

fn processing(error: &webrtc_audio_processing::Error) -> Error {
    Error::Processing(error.to_string())
}
