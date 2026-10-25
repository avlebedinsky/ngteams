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
//! - [`devices`]: the device seam (a microphone, a speaker, a camera): a file backend that opens no audio server, and
//!   a PipeWire backend that opens only the server its build allows (the UI sandbox's, or the desktop's for the
//!   personal build). [`tone`] makes and measures the test tone.
//!
//! The rest of the media (Opus, echo cancellation, video coding) comes in later, and nothing here sends signaling.

use std::io;

pub mod devices;
pub mod relay;
pub mod sdp;
pub mod stun;
#[cfg(feature = "test-support")]
pub mod test_turn;
pub mod tone;
pub mod turn;

/// Everything the relay can fail with.
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
}

fn describe(failures: &[(relay::RelayServer, String)]) -> String {
    failures
        .iter()
        .map(|(server, error)| format!("{server}: {error}"))
        .collect::<Vec<_>>()
        .join("; ")
}
