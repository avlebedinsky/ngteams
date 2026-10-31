//! A call's video: what the camera is sent with ([`codec`]), the frames' conversions ([`convert`]): a camera's frame
//! turned into the I420 libwebrtc takes (scaled down to the size the call sends), and a decoded I420 frame turned into
//! the premultiplied BGRA a window draws; and the camera's background effects ([`effects`]: blurred, or a picture
//! behind the person). libwebrtc encodes and decodes; the session ([`crate::session`]) hands it the camera's frames
//! (with the effect chosen applied) and the sinks what it decoded, on threads of their own.

pub mod codec;
pub mod convert;
pub mod effects;

/// H.264's RTP clock, in Hz.
pub const VIDEO_CLOCK: u32 = 90_000;
