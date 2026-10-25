//! Spike 3b: a native echo call. What exists and is tested offline:
//!
//! - [`relay`]: the media paths the probe runs over, `teams-media`'s peer behind a TURN allocation (the peer, the TURN
//!   client, its relay link and the UDP-to-TCP fallback are `teams-media`'s, moved there in M3) or a plain UDP peer;
//! - [`echo`]: the tone probe that measures the echo over any media path;
//! - [`signaling`]: the web client's test call as the Task 18 capture shows it (the call start with no callee, the
//!   conversation it opens, the acceptance with the remote SDP, the `add` that brings the echo in, the TURN
//!   credentials from `trap/tokens`), and the incoming-call policy (ignored, never answered);
//! - [`call`]: the flow up to the skypetoken, with the call start through the fence.
//!
//! What doesn't: the live native call. [`call::start`] refuses to send, because the echo joins only through writes on
//! the started call (the `add`, the acknowledgement, the `leave`) that the fence can't permit until it can read a
//! call's members in full; the web client's own test call gets no echo for the same reason. TURN over TLS is not
//! built; the SDP conversion is `teams_media::sdp`. `docs/spikes/echo-call.md` has the details.

use std::io;

pub mod call;
pub mod echo;
pub mod relay;
pub mod signaling;

/// Everything the media side of the spike can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("socket: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Media(#[from] media_loop::Error),
    #[error(transparent)]
    Relay(#[from] teams_media::Error),
    #[error("bad settings: {0}")]
    Config(String),
    #[error("timed out: {0}")]
    Timeout(&'static str),
}
