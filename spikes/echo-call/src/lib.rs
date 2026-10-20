//! Spike 3b: a native echo call. What exists and is tested offline:
//!
//! - [`stun`] and [`turn`]: a sans-IO TURN client (RFC 8656, long-term credentials) and its STUN codec, with the TCP
//!   framings; [`relay`] puts it on UDP or TCP sockets, falls back from UDP to TCP, and puts a `media_loop` peer behind
//!   the allocation;
//! - [`echo`]: the tone probe that measures the echo over any media path;
//! - [`signaling`]: the call start and the callbacks as the bundle shapes them, and the incoming-call policy (ignored,
//!   never answered). Unconfirmed — needs the Task 18 capture;
//! - [`call`]: the flow up to the skypetoken, with the call start through the fence.
//!
//! What doesn't: the live call. The lab accounts have no Teams license, so there is no skypetoken, no capture of a
//! real echo call (Task 18) and no confirmed signaling; `docs/spikes/echo-call.md` has the details.

use std::io;

pub mod call;
pub mod echo;
pub mod relay;
pub mod signaling;
pub mod stun;
pub mod turn;

/// Everything the media side of the spike can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("socket: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Media(#[from] media_loop::Error),
    #[error("TURN: {0}")]
    Turn(turn::TurnFailure),
    #[error("TURN client: {0}")]
    TurnUse(#[from] turn::TurnError),
    #[error("STUN: {0}")]
    Stun(#[from] stun::StunError),
    #[error("no relay server allocated: {}", describe(.0))]
    NoRelay(Vec<(relay::RelayServer, String)>),
    #[error("bad settings: {0}")]
    Config(String),
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
