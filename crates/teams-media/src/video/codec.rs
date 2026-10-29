//! H.264 through OpenH264 (`docs/protocol/calling.md`, "The H.264 codec").
//!
//! **Which library**: the source the `openh264` binding bundles, compiled into every build (the lab's, the personal
//! build, the tests alike), so a call's video needs nothing downloaded or installed beside the program. The build uses
//! `nasm` for OpenH264's assembly when it finds one, plain C otherwise.
//!
//! **What it makes**: a camera's I420 frames encoded as constrained baseline H.264 (OpenH264's encoder makes only
//! that), Annex B access units with their parameter sets before every IDR, as the web client's offers select
//! (`profile-level-id=42e01f`, `packetization-mode=1`); the encoder's target bitrate follows the call's bandwidth
//! estimate ([`VideoEncoder::set_bitrate`]) between [`MIN_BITRATE`] and its maximum. Received access units decode to
//! I420 frames.

use std::time::Duration;

use openh264::decoder::{Decoder, DecoderConfig};
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode, UsageType,
};
use openh264::formats::YUVSource;
use openh264::{OpenH264API, Timestamp};

use crate::Error;
use crate::devices::{PixelFormat, VideoFrame};

/// The lowest bitrate the encoder is set to, whatever the estimate, in bits per second.
pub const MIN_BITRATE: u32 = 100_000;

/// A new handle on the bundled OpenH264 for each encoder and decoder (the binding's handles aren't shared).
fn api() -> OpenH264API {
    OpenH264API::from_source()
}

/// How the encoder runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EncoderSettings {
    /// The highest bitrate it is ever set to, in bits per second.
    pub max_bitrate: u32,
    /// The bitrate it starts at.
    pub start_bitrate: u32,
    /// The frame rate the camera runs at, for the rate control.
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

/// One encoded frame: an Annex B access unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedVideo {
    pub data: Vec<u8>,
    /// An IDR (with its parameter sets before it).
    pub keyframe: bool,
}

/// The H.264 encoder of a call's camera.
pub struct VideoEncoder {
    encoder: Encoder,
    settings: EncoderSettings,
    /// The target the rate control runs at.
    bitrate: u32,
    /// The size of the last frame encoded: the encoder starts again (at its maximum) when it changes.
    size: Option<(u32, u32)>,
    /// The encoder's target differs from [`Self::bitrate`] (it was set up again at its maximum since).
    stale: bool,
}

impl std::fmt::Debug for VideoEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoEncoder")
            .field("settings", &self.settings)
            .field("bitrate", &self.bitrate)
            .finish_non_exhaustive()
    }
}

impl VideoEncoder {
    /// An encoder with `settings`.
    ///
    /// # Errors
    /// [`Error::Video`] when OpenH264 refuses the setup.
    pub fn new(settings: EncoderSettings) -> Result<Self, Error> {
        let max = settings.max_bitrate.max(MIN_BITRATE);
        // the encoder is set up at its maximum (its rate control's ceiling), then set to its target
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(max))
            .max_frame_rate(FrameRate::from_hz(f32::from(settings.fps.max(1))))
            .usage_type(UsageType::CameraVideoRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .skip_frames(true)
            // an IDR every 4 s besides those asked for: a picture comes back after a loss nobody reported
            .intra_frame_period(IntraFramePeriod::from_num_frames(u32::from(settings.fps.max(1)) * 4))
            .num_threads(1);
        let encoder = Encoder::with_api_config(api(), config).map_err(video_error)?;
        Ok(Self {
            encoder,
            settings: EncoderSettings {
                max_bitrate: max,
                ..settings
            },
            bitrate: settings.start_bitrate.clamp(MIN_BITRATE, max),
            size: None,
            stale: true,
        })
    }

    /// The target bitrate now, in bits per second.
    #[must_use]
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Sets the target bitrate to `bps`, kept between [`MIN_BITRATE`] and the maximum; the target it took.
    pub fn set_bitrate(&mut self, bps: u64) -> u32 {
        let max = self.settings.max_bitrate;
        let bitrate = u32::try_from(bps).unwrap_or(u32::MAX).clamp(MIN_BITRATE, max);
        if bitrate != self.bitrate {
            self.bitrate = bitrate;
            self.stale = true;
        }
        self.bitrate
    }

    /// Encodes `frame` (I420, each plane's stride its width: [`super::convert::to_i420`]'s), captured `at` into the
    /// stream (the rate control paces itself by it), an IDR when `keyframe`; `None` when the rate control skipped it.
    ///
    /// # Errors
    /// [`Error::Video`] for a frame of another format or the encoder failing.
    pub fn encode(&mut self, frame: &VideoFrame, at: Duration, keyframe: bool) -> Result<Option<EncodedVideo>, Error> {
        let source = I420Source::new(frame)?;
        let size = (frame.width, frame.height);
        if self.size != Some(size) {
            // the binding sets the encoder up again (at its maximum) for a new size
            self.size = Some(size);
            self.stale = true;
        }
        if keyframe {
            self.encoder.force_intra_frame();
        }
        let millis = u64::try_from(at.as_millis()).unwrap_or(u64::MAX);
        let encoded = self
            .encoder
            .encode_at(&source, Timestamp::from_millis(millis))
            .map_err(video_error)?;
        let kind = encoded.frame_type();
        let data = encoded.to_vec();
        if self.stale {
            self.apply_bitrate()?;
        }
        Ok(match kind {
            FrameType::Skip | FrameType::Invalid => None,
            _ if data.is_empty() => None,
            kind => Some(EncodedVideo {
                data,
                keyframe: kind == FrameType::IDR,
            }),
        })
    }

    /// Hands the encoder its target (it is set up by its first frame, so only after one).
    fn apply_bitrate(&mut self) -> Result<(), Error> {
        let mut info = openh264_sys2::SBitrateInfo {
            iLayer: openh264_sys2::SPATIAL_LAYER_ALL,
            iBitrate: i32::try_from(self.bitrate).unwrap_or(i32::MAX),
        };
        // SAFETY: only the raw API's handle is taken; nothing is set through it here
        let raw = unsafe { self.encoder.raw_api() };
        // SAFETY: the encoder was set up by the frame just encoded; `info` is the `SBitrateInfo` this option reads,
        // alive for the call, and the encoder keeps no pointer to it
        let code = unsafe { raw.set_option(openh264_sys2::ENCODER_OPTION_BITRATE, (&raw mut info).cast()) };
        if code != 0 {
            return Err(Error::Video(format!("the encoder refused its bitrate ({code})")));
        }
        self.stale = false;
        Ok(())
    }
}

/// The H.264 decoder of a call's received video.
pub struct VideoDecoder {
    decoder: Decoder,
}

impl std::fmt::Debug for VideoDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoDecoder").finish_non_exhaustive()
    }
}

impl VideoDecoder {
    /// A decoder.
    ///
    /// # Errors
    /// [`Error::Video`] when OpenH264 refuses the setup.
    pub fn new() -> Result<Self, Error> {
        let decoder = Decoder::with_api_config(api(), DecoderConfig::new()).map_err(video_error)?;
        Ok(Self { decoder })
    }

    /// Decodes one Annex B access unit: the picture it completes (I420, each plane's stride its width), or `None`
    /// while it completes none (parameter sets alone, a picture that waits on its reference).
    ///
    /// # Errors
    /// [`Error::Video`] when the decoder can't decode it (a lost reference: a keyframe is wanted).
    pub fn decode(&mut self, unit: &[u8]) -> Result<Option<VideoFrame>, Error> {
        let Some(decoded) = self.decoder.decode(unit).map_err(video_error)? else {
            return Ok(None);
        };
        Ok(Some(packed(&decoded)))
    }
}

/// A decoded picture copied out of the decoder's buffers, its planes packed.
fn packed(decoded: &impl YUVSource) -> VideoFrame {
    let (width, height) = decoded.dimensions();
    let (y_stride, u_stride, v_stride) = decoded.strides();
    let (half_width, half_height) = (width.div_ceil(2), height.div_ceil(2));
    let mut data = Vec::with_capacity(width * height + 2 * half_width * half_height);
    copy_plane(decoded.y(), y_stride, width, height, &mut data);
    copy_plane(decoded.u(), u_stride, half_width, half_height, &mut data);
    copy_plane(decoded.v(), v_stride, half_width, half_height, &mut data);
    let width = u32::try_from(width).unwrap_or(u32::MAX);
    VideoFrame {
        width,
        height: u32::try_from(height).unwrap_or(u32::MAX),
        format: PixelFormat::I420,
        stride: width,
        data,
    }
}

fn copy_plane(plane: &[u8], stride: usize, width: usize, rows: usize, out: &mut Vec<u8>) {
    for row in plane.chunks(stride.max(1)).take(rows) {
        let row = row.get(..width).unwrap_or(row);
        out.extend_from_slice(row);
        out.resize(out.len() + (width - row.len()), 0);
    }
}

/// An I420 frame of ours as the encoder reads it.
struct I420Source<'a> {
    width: usize,
    height: usize,
    y: &'a [u8],
    u: &'a [u8],
    v: &'a [u8],
}

impl<'a> I420Source<'a> {
    fn new(frame: &'a VideoFrame) -> Result<Self, Error> {
        let (width, height) = (to_usize(frame.width), to_usize(frame.height));
        let luma = width * height;
        let chroma = luma / 4;
        let packed = frame.format == PixelFormat::I420
            && frame.stride == frame.width
            && width % 2 == 0
            && height % 2 == 0
            && frame.data.len() >= luma + 2 * chroma;
        if !packed || width == 0 || height == 0 {
            return Err(Error::Video(format!(
                "the encoder takes packed I420 of even sides, not {} {}x{} (stride {})",
                frame.format.name(),
                frame.width,
                frame.height,
                frame.stride
            )));
        }
        Ok(Self {
            width,
            height,
            y: &frame.data[..luma],
            u: &frame.data[luma..luma + chroma],
            v: &frame.data[luma + chroma..luma + 2 * chroma],
        })
    }
}

impl YUVSource for I420Source<'_> {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn strides(&self) -> (usize, usize, usize) {
        (self.width, self.width / 2, self.width / 2)
    }

    fn y(&self) -> &[u8] {
        self.y
    }

    fn u(&self) -> &[u8] {
        self.u
    }

    fn v(&self) -> &[u8] {
        self.v
    }
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn video_error(error: impl std::fmt::Display) -> Error {
    Error::Video(error.to_string())
}
