//! Spike 3a: an offline media loop. Two `str0m` peers in one process talk over loopback UDP (ICE, DTLS, SRTP); one
//! sends an Opus-encoded tone as RTP, the other decodes it, checks the frequency with Goertzel and echoes the packets
//! back, and the loop records one-way and round-trip latency.
//!
//! The pieces are separate so the native echo call (Task 19) can reuse them: [`tone`] and [`goertzel`] make and check
//! the signal, [`codec`] wraps Opus, [`peer`] puts the sans-IO `str0m` peer (`teams_media::peer::Peer`, moved there
//! in M3 with its tests) on a UDP socket, and [`loopback`] is the in-process harness with its latency statistics.

use std::time::Duration;

pub mod codec;
pub mod goertzel;
pub mod loopback;
pub mod peer;
pub mod tone;

/// Sample rate of all audio here, as WebRTC's Opus uses.
pub const SAMPLE_RATE: u32 = 48_000;
/// Samples in one 20 ms mono frame.
pub const FRAME_SAMPLES: usize = 960;
/// Duration of one frame.
pub const FRAME_DURATION: Duration = Duration::from_millis(20);

/// Everything the media loop can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A socket failed.
    #[error("socket: {0}")]
    Io(#[from] std::io::Error),
    /// `str0m` refused an SDP or a candidate.
    #[error("str0m: {0}")]
    Rtc(String),
    /// The peer failed (`str0m` refused an input, or media was written before the audio line was connected).
    #[error(transparent)]
    Peer(#[from] teams_media::Error),
    /// libopus failed, or a frame had the wrong shape.
    #[error("opus: {0}")]
    Opus(String),
    /// The loop's settings can't work.
    #[error("bad loop config: {0}")]
    Config(String),
    /// A step did not finish in time.
    #[error("timed out: {0}")]
    Timeout(&'static str),
}
