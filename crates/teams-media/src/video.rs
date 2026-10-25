//! A call's video (M3 Task 18): H.264 as the web client's offers and answers select it, through OpenH264.
//!
//! - [`codec`]: the H.264 encoder and decoder, on the OpenH264 library the build may load ([`codec::H264Library`]):
//!   Cisco's prebuilt library in every normal build, the bundled source only under `test-support`.
//! - [`convert`]: a camera's frame turned into the I420 the encoder takes (scaled down to the size the call sends),
//!   and a decoded I420 frame turned into the premultiplied BGRA a window draws.
//!
//! The session ([`crate::session`]) runs them on threads of their own: the camera's frames through the encoder to the
//! peer, the peer's frames through the decoder to the sink the caller gave.

pub mod codec;
pub mod convert;

/// H.264's RTP clock, in Hz.
pub const VIDEO_CLOCK: u32 = 90_000;
