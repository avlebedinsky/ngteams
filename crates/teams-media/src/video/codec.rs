//! How a call's camera is sent: libwebrtc encodes it (H.264 as the call negotiated it, through VA-API where the
//! machine has it, OpenH264 otherwise) and follows its own bandwidth estimate within these bounds.

/// The bounds the sender runs within.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EncoderSettings {
    /// The highest bitrate it is ever set to, in bits per second.
    pub max_bitrate: u32,
    /// The bitrate the bandwidth estimate starts from.
    pub start_bitrate: u32,
    /// The highest frame rate sent.
    pub fps: u16,
}

impl Default for EncoderSettings {
    fn default() -> Self {
        Self {
            max_bitrate: 1_500_000,
            start_bitrate: 300_000,
            fps: 30,
        }
    }
}
