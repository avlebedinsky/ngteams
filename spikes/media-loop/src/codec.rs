//! Opus at 48 kHz mono in 20 ms frames, the shape WebRTC audio uses (libopus via the `opus` crate).

use crate::{Error, FRAME_SAMPLES, SAMPLE_RATE};

/// Largest Opus packet we accept from the encoder; a 20 ms mono frame stays far below it.
const MAX_PACKET: usize = 1_500;

/// Encodes 20 ms frames of 48 kHz mono PCM into Opus packets.
pub struct OpusEncoder(opus::Encoder);

impl OpusEncoder {
    /// A new encoder tuned for general audio, so a test tone keeps its shape.
    ///
    /// # Errors
    /// [`Error::Opus`] if libopus refuses the setup.
    pub fn new() -> Result<Self, Error> {
        Ok(Self(opus::Encoder::new(
            SAMPLE_RATE,
            opus::Channels::Mono,
            opus::Application::Audio,
        )?))
    }

    /// Encodes one frame of [`FRAME_SAMPLES`] samples.
    ///
    /// # Errors
    /// [`Error::Opus`] for a frame of any other length, or if libopus fails.
    pub fn encode(&mut self, frame: &[i16]) -> Result<Vec<u8>, Error> {
        if frame.len() != FRAME_SAMPLES {
            return Err(Error::Opus(format!(
                "frame of {} samples, expected {FRAME_SAMPLES}",
                frame.len()
            )));
        }
        Ok(self.0.encode_vec(frame, MAX_PACKET)?)
    }
}

/// Decodes Opus packets back into 48 kHz mono PCM.
pub struct OpusDecoder(opus::Decoder);

impl OpusDecoder {
    /// A new decoder.
    ///
    /// # Errors
    /// [`Error::Opus`] if libopus refuses the setup.
    pub fn new() -> Result<Self, Error> {
        Ok(Self(opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono)?))
    }

    /// Decodes one packet into its samples (one frame for packets from [`OpusEncoder`]).
    ///
    /// # Errors
    /// [`Error::Opus`] if the packet is not valid Opus.
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>, Error> {
        // 120 ms is the longest packet Opus allows
        let mut pcm = vec![0; FRAME_SAMPLES * 6];
        let samples = self.0.decode(packet, &mut pcm, false)?;
        pcm.truncate(samples);
        Ok(pcm)
    }
}

impl From<opus::Error> for Error {
    fn from(error: opus::Error) -> Self {
        Self::Opus(error.to_string())
    }
}
