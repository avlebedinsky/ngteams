//! A call's media, apart from its signaling (`docs/protocol/calling.md`, `docs/protocol/calling-echo.md`).
//!
//! - [`sdp`]: Teams' SDP dialect, `application/sdp-ngc-1.0`, to and from the standard SDP `str0m` speaks, both ways
//!   and on both sides of a call: our offer written for the call start and the media controller's or the callee's
//!   answer read back; the caller's (or the mixer's) offer an invitation carries read in and our answer written for
//!   the acceptance; a renegotiation offered by either side. A browser's SDP is refused where the dialect is required.
//! - [`stun`] and [`turn`]: a sans-IO TURN client (RFC 8656, long-term credentials) and its STUN codec, with the TCP
//!   framings; [`relay`] puts it on a UDP socket or a TCP stream and falls back from UDP to TCP (Teams' relay: UDP
//!   3478, then TCP 443).
//!
//! - [`peer`]: the call's `str0m` audio peer, sans-IO, and [`peer::RelayedPeer`], the same behind a TURN allocation
//!   (relayed candidates only).
//! - [`codec`]: Opus as the calls configure it (20 ms packets, in-band FEC, DTX when asked), and [`jitter`]: the
//!   receiving side's jitter buffer, which plays packets in order and recovers or conceals what is missing.
//! - [`session`]: a call's audio on threads of its own: the microphone through Opus to the relayed peer, the peer
//!   through the jitter buffer to the speaker, with bounded handoffs to and from the devices.
//! - [`devices`]: the device seam (a microphone, a speaker, a camera): a file backend that opens no audio server, and
//!   a PipeWire backend that opens only the server its build allows (the UI sandbox's, or the desktop's for the
//!   personal build). [`tone`] makes and measures the test tone.
//!
//! The rest of the media (echo cancellation, video coding) comes in later, and nothing here sends signaling.

use std::io;

pub mod codec;
pub mod devices;
pub mod jitter;
pub mod peer;
mod queue;
pub mod relay;
pub mod sdp;
pub mod session;
pub mod stun;
#[cfg(feature = "test-support")]
pub mod test_turn;
pub mod tone;
pub mod turn;

/// Everything the media can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("socket: {0}")]
    Io(#[from] io::Error),
    #[error("TURN: {0}")]
    Turn(turn::TurnFailure),
    #[error("TURN client: {0}")]
    TurnUse(#[from] turn::TurnError),
    #[error("STUN: {0}")]
    Stun(#[from] stun::StunError),
    #[error("no relay server allocated: {}", describe(.0))]
    NoRelay(Vec<(relay::RelayServer, String)>),
    #[error("timed out: {0}")]
    Timeout(&'static str),
    /// `str0m` refused an input, an SDP or a candidate.
    #[error("str0m: {0}")]
    Rtc(String),
    /// libopus failed, or a packet or frame had the wrong shape.
    #[error("opus: {0}")]
    Opus(String),
    /// Media was written before the audio line was connected.
    #[error("the audio line is not connected")]
    NotConnected,
    #[error("device: {0}")]
    Device(#[from] devices::DeviceError),
    /// The media session's network thread is gone.
    #[error("the media session ended")]
    SessionEnded,
}

fn describe(failures: &[(relay::RelayServer, String)]) -> String {
    failures
        .iter()
        .map(|(server, error)| format!("{server}: {error}"))
        .collect::<Vec<_>>()
        .join("; ")
}
