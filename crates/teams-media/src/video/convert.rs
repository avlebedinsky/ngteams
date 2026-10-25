//! Frames between the devices, the codec and a window: whatever raw format a camera hands over turned into the I420
//! the encoder takes ([`to_i420`], scaled down by a whole factor to fit the size the call sends), and a decoded I420
//! frame turned into the premultiplied BGRA a window draws ([`i420_to_bgra`]). Colours as the camera's pictures and
//! H.264's are written: BT.601, limited range.

use crate::devices::{PixelFormat, VideoFrame};

/// The largest picture the call sends: a bigger camera frame is scaled down by a whole factor until it fits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaxSize {
    pub width: u32,
    pub height: u32,
}

impl MaxSize {
    /// 1280 by 720.
    pub const HD: Self = Self {
        width: 1280,
        height: 720,
    };
}

/// One pixel as luma and chroma.
#[derive(Clone, Copy)]
struct Yuv {
    y: u8,
    u: u8,
    v: u8,
}

/// `frame` as I420 (even sides, each plane's stride its width), scaled down by the smallest whole factor that fits
/// `max`; `None` for a frame whose data is shorter than its format and size need, or that is empty.
#[must_use]
pub fn to_i420(frame: &VideoFrame, max: MaxSize) -> Option<VideoFrame> {
    if frame.width == 0 || frame.height == 0 || !holds(frame) {
        return None;
    }
    let factor = (1..=16)
        .find(|factor| frame.width / factor <= max.width && frame.height / factor <= max.height)
        .unwrap_or(16);
    let width = (frame.width / factor) & !1;
    let height = (frame.height / factor) & !1;
    if width == 0 || height == 0 {
        return None;
    }
    if factor == 1 && frame.format == PixelFormat::I420 && frame.stride == width && frame.width == width {
        let size = to_usize(width) * to_usize(height) * 3 / 2;
        return frame.data.get(..size).map(|data| VideoFrame {
            width,
            height,
            format: PixelFormat::I420,
            stride: width,
            data: data.to_vec(),
        });
    }
    let (w, h) = (to_usize(width), to_usize(height));
    let mut data = vec![0_u8; w * h * 3 / 2];
    let (luma, chroma) = data.split_at_mut(w * h);
    let (u_plane, v_plane) = chroma.split_at_mut(w * h / 4);
    for (row, line) in luma.chunks_exact_mut(w).enumerate() {
        for (column, out) in line.iter_mut().enumerate() {
            *out = pixel(frame, column * to_usize(factor), row * to_usize(factor)).y;
        }
    }
    for row in 0..h / 2 {
        for column in 0..w / 2 {
            let at = pixel(frame, column * 2 * to_usize(factor), row * 2 * to_usize(factor));
            u_plane[row * w / 2 + column] = at.u;
            v_plane[row * w / 2 + column] = at.v;
        }
    }
    Some(VideoFrame {
        width,
        height,
        format: PixelFormat::I420,
        stride: width,
        data,
    })
}

/// Whether `frame`'s data is as long as its format, stride and height need.
fn holds(frame: &VideoFrame) -> bool {
    let (stride, height) = (to_usize(frame.stride), to_usize(frame.height));
    let needed = match frame.format {
        PixelFormat::I420 | PixelFormat::Nv12 => stride * height + stride * height.div_ceil(2),
        _ => stride * height,
    };
    frame.stride >= frame.width.saturating_mul(bytes_per_pixel(frame.format)) && frame.data.len() >= needed
}

/// Bytes a pixel takes in the first plane (one for the planar formats' luma).
fn bytes_per_pixel(format: PixelFormat) -> u32 {
    match format {
        PixelFormat::I420 | PixelFormat::Nv12 => 1,
        PixelFormat::Yuy2 => 2,
        PixelFormat::Rgb | PixelFormat::Bgr => 3,
        PixelFormat::Rgbx | PixelFormat::Bgrx | PixelFormat::Rgba | PixelFormat::Bgra => 4,
    }
}

/// The pixel at `(column, row)` of `frame` (which [`holds`] its size).
fn pixel(frame: &VideoFrame, column: usize, row: usize) -> Yuv {
    let stride = to_usize(frame.stride);
    let data = &frame.data;
    let at = |index: usize| data.get(index).copied().unwrap_or(0);
    let luma_size = stride * to_usize(frame.height);
    match frame.format {
        PixelFormat::I420 => {
            let chroma = (row / 2) * (stride / 2) + column / 2;
            Yuv {
                y: at(row * stride + column),
                u: at(luma_size + chroma),
                v: at(luma_size + luma_size / 4 + chroma),
            }
        }
        PixelFormat::Nv12 => {
            let chroma = luma_size + (row / 2) * stride + (column / 2) * 2;
            Yuv {
                y: at(row * stride + column),
                u: at(chroma),
                v: at(chroma + 1),
            }
        }
        PixelFormat::Yuy2 => {
            let pair = row * stride + (column / 2) * 4;
            Yuv {
                y: at(pair + (column % 2) * 2),
                u: at(pair + 1),
                v: at(pair + 3),
            }
        }
        rgb => {
            let start = row * stride + column * to_usize(bytes_per_pixel(rgb));
            let (red, green, blue) = match rgb {
                PixelFormat::Rgbx | PixelFormat::Rgba | PixelFormat::Rgb => (at(start), at(start + 1), at(start + 2)),
                _ => (at(start + 2), at(start + 1), at(start)),
            };
            from_rgb(red, green, blue)
        }
    }
}

/// BT.601 limited range from RGB.
fn from_rgb(r: u8, g: u8, b: u8) -> Yuv {
    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
    Yuv {
        y: clamp_u8(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16),
        u: clamp_u8(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128),
        v: clamp_u8(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128),
    }
}

/// An I420 `frame` (each plane's stride its width, as [`to_i420`] and the decoder make it) as premultiplied BGRA,
/// rows `width * 4` bytes apart, opaque; `None` for another format or a frame whose data is too short.
#[must_use]
pub fn i420_to_bgra(frame: &VideoFrame) -> Option<Vec<u8>> {
    if frame.format != PixelFormat::I420 || !holds(frame) {
        return None;
    }
    let (width, height, stride) = (to_usize(frame.width), to_usize(frame.height), to_usize(frame.stride));
    let luma_size = stride * height;
    let mut out = vec![0_u8; width * height * 4];
    for (row, line) in out.chunks_exact_mut(width * 4).enumerate() {
        let (pixels, _) = line.as_chunks_mut::<4>();
        for (column, bgra) in pixels.iter_mut().enumerate() {
            let chroma = (row / 2) * (stride / 2) + column / 2;
            let luma = frame.data[row * stride + column];
            let cb = frame.data.get(luma_size + chroma).copied().unwrap_or(128);
            let cr = frame
                .data
                .get(luma_size + luma_size / 4 + chroma)
                .copied()
                .unwrap_or(128);
            let (red, green, blue) = to_rgb(luma, cb, cr);
            *bgra = [blue, green, red, 255];
        }
    }
    Some(out)
}

/// RGB from BT.601 limited range.
fn to_rgb(luma: u8, cb: u8, cr: u8) -> (u8, u8, u8) {
    let scaled = 298 * (i32::from(luma) - 16);
    let (cb, cr) = (i32::from(cb) - 128, i32::from(cr) - 128);
    (
        clamp_u8((scaled + 409 * cr + 128) >> 8),
        clamp_u8((scaled - 100 * cb - 208 * cr + 128) >> 8),
        clamp_u8((scaled + 516 * cb + 128) >> 8),
    )
}

fn clamp_u8(value: i32) -> u8 {
    u8::try_from(value.clamp(0, 255)).unwrap_or(u8::MAX)
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(format: PixelFormat, width: u32, height: u32, bytes: &[u8]) -> VideoFrame {
        let stride = width * bytes_per_pixel(format);
        let size = to_usize(stride) * to_usize(height);
        VideoFrame {
            width,
            height,
            format,
            stride,
            data: bytes.iter().copied().cycle().take(size).collect(),
        }
    }

    #[test]
    fn white_and_black_rgb_are_limited_range_luma() {
        let white = to_i420(&solid(PixelFormat::Bgrx, 4, 4, &[255, 255, 255, 0]), MaxSize::HD).unwrap();
        assert_eq!(white.data[0], 235);
        let black = to_i420(&solid(PixelFormat::Rgb, 4, 4, &[0, 0, 0]), MaxSize::HD).unwrap();
        assert_eq!(black.data[0], 16);
        assert_eq!(black.data[16], 128, "grey chroma");
    }

    #[test]
    fn a_big_frame_is_scaled_down_by_a_whole_factor() {
        let frame = solid(PixelFormat::Yuy2, 1920, 1080, &[100, 128, 100, 128]);
        let scaled = to_i420(&frame, MaxSize::HD).unwrap();
        assert_eq!((scaled.width, scaled.height), (960, 540));
        assert_eq!(scaled.data.len(), 960 * 540 * 3 / 2);
        assert_eq!(scaled.data[0], 100);
    }

    #[test]
    fn i420_round_trips_through_bgra_near_its_colour() {
        let frame = solid(PixelFormat::Rgba, 8, 8, &[200, 40, 90, 255]);
        let i420 = to_i420(&frame, MaxSize::HD).unwrap();
        let bgra = i420_to_bgra(&i420).unwrap();
        assert_eq!(bgra.len(), 8 * 8 * 4);
        let (b, g, r, a) = (bgra[0], bgra[1], bgra[2], bgra[3]);
        assert!(
            r.abs_diff(200) <= 3 && g.abs_diff(40) <= 3 && b.abs_diff(90) <= 3,
            "{r} {g} {b}"
        );
        assert_eq!(a, 255);
    }

    #[test]
    fn a_short_frame_is_refused() {
        let mut frame = solid(PixelFormat::Rgb, 8, 8, &[1, 2, 3]);
        frame.data.truncate(10);
        assert!(to_i420(&frame, MaxSize::HD).is_none());
        assert!(i420_to_bgra(&frame).is_none());
    }
}
