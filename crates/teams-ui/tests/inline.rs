//! A message's pasted GIF decoded as `inline::decode` keeps it for the run: every frame of a normal one, the frames
//! before the first that doesn't decode (a truncated file ends at once, never spins), at most `MAX_GIF_FRAMES` and
//! `MAX_GIF_BYTES` of them, each scaled down to `MAX_DECODED_SIDE`, and none for a canvas past `MAX_GIF_SIDE`. Every
//! GIF here is made up in the test. A picture is shaped as Teams sizes one (`inline::Shape`).

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use gpui::{DevicePixels, RenderImage, size};
use image::codecs::gif::GifEncoder;
use image::{Delay, Frame, Rgba, RgbaImage};
use teams_proto::{PastedFormat, PastedPicture};
use teams_ui::inline::{
    self, Kind, MAX_DECODED_SIDE, MAX_GIF_BYTES, MAX_GIF_FRAMES, MAX_GIF_SIDE, PICTURE_BOX, Shape, WIDE_MAX_WIDTH,
};

/// How long a decode may take before the test calls it stuck.
const STUCK: Duration = Duration::from_secs(20);

/// A GIF of `frames` frames, `width` by `height`, each of one colour (frame `i`'s red is `i`).
fn gif(width: u32, height: u32, frames: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut bytes);
        let frames = (0..frames).map(|index| {
            let red = u8::try_from(index % 256).unwrap();
            let image = RgbaImage::from_pixel(width, height, Rgba([red, 20, 30, 255]));
            Frame::from_parts(image, 0, 0, Delay::from_numer_denom_ms(100, 1))
        });
        encoder.encode_frames(frames).unwrap();
    }
    bytes
}

/// `bytes` with the logical screen (the canvas) said to be `width` by `height`.
fn with_canvas(mut bytes: Vec<u8>, width: u16, height: u16) -> Vec<u8> {
    bytes[6..8].copy_from_slice(&width.to_le_bytes());
    bytes[8..10].copy_from_slice(&height.to_le_bytes());
    bytes
}

/// `bytes` decoded as a pasted GIF on a thread of its own, failing when it takes longer than [`STUCK`].
fn decode(bytes: Vec<u8>) -> Option<(RenderImage, (u32, u32))> {
    let picture = Arc::new(PastedPicture {
        format: PastedFormat::Gif,
        bytes,
    });
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(inline::decode(&picture));
    });
    receiver.recv_timeout(STUCK).expect("the decode is stuck")
}

#[test]
fn every_frame_of_a_gif_is_kept_in_bgra_at_its_size() {
    let (image, natural) = decode(gif(16, 8, 3)).expect("decoded");
    assert_eq!(natural, (16, 8));
    assert_eq!(image.frame_count(), 3);
    for index in 0..3 {
        assert_eq!(image.size(index), size(DevicePixels(16), DevicePixels(8)));
        let pixel = &image.as_bytes(index).unwrap()[..4];
        assert_eq!(pixel, [30, 20, u8::try_from(index).unwrap(), 255], "frame {index}");
    }
}

#[test]
fn a_truncated_gif_keeps_the_frames_before_the_cut_and_never_spins() {
    let whole = gif(16, 8, 4);
    // without its trailer, then cut inside its last frame, then inside its first: whatever was decoded, at once
    for cut in [1, 6, whole.len() / 2, whole.len() - 30] {
        let bytes = whole[..whole.len() - cut].to_vec();
        if let Some((image, natural)) = decode(bytes) {
            assert_eq!(natural, (16, 8));
            assert!(
                (1..=4).contains(&image.frame_count()),
                "cut {cut}: {}",
                image.frame_count()
            );
        }
    }
    // the trailer alone missing loses no frame
    let (image, _) = decode(whole[..whole.len() - 1].to_vec()).expect("decoded");
    assert_eq!(image.frame_count(), 4);
    // cut inside the header: nothing
    assert!(decode(whole[..12].to_vec()).is_none());
}

#[test]
fn past_the_frame_cap_the_frames_before_are_kept() {
    let (image, _) = decode(gif(1, 1, MAX_GIF_FRAMES + 40)).expect("decoded");
    assert_eq!(image.frame_count(), MAX_GIF_FRAMES);
}

#[test]
fn a_wide_canvas_is_scaled_down_frame_by_frame() {
    let side = u16::try_from(MAX_GIF_SIDE).unwrap();
    let (image, natural) = decode(with_canvas(gif(1, 1, 2), side, 8)).expect("decoded");
    assert_eq!(natural, (MAX_GIF_SIDE, 8));
    assert_eq!(image.frame_count(), 2);
    let width = i32::try_from(MAX_DECODED_SIDE).unwrap();
    for index in 0..2 {
        assert_eq!(image.size(index), size(DevicePixels(width), DevicePixels(4)));
    }
}

#[test]
fn a_canvas_past_the_largest_side_is_not_decoded_at_all() {
    let past = u16::try_from(MAX_GIF_SIDE + 1).unwrap();
    assert!(decode(with_canvas(gif(1, 1, 2), past, 8)).is_none());
    assert!(decode(with_canvas(gif(1, 1, 2), 8, past)).is_none());
    assert!(decode(with_canvas(gif(1, 1, 2), u16::MAX, u16::MAX)).is_none());
}

#[test]
fn past_the_byte_budget_the_frames_before_are_kept() {
    // each frame 2048 by 1024 in BGRA once decoded: the budget holds this many
    let side = u16::try_from(MAX_DECODED_SIDE).unwrap();
    let per_frame = usize::from(side) * usize::from(side / 2) * 4;
    let fit = MAX_GIF_BYTES / per_frame;
    let (image, _) = decode(with_canvas(gif(1, 1, fit + 4), side, side / 2)).expect("decoded");
    assert_eq!(image.frame_count(), fit);
}

/// `shape`'s frame and image, rounded to whole pixels: `((frame width, frame height), (image width, image height))`.
fn sides(shape: Shape) -> ((f32, f32), (f32, f32)) {
    let round = |side: gpui::Pixels| f32::from(side).round();
    (
        (round(shape.frame.width), round(shape.frame.height)),
        (round(shape.image.width), round(shape.image.height)),
    )
}

#[test]
fn a_picture_is_sized_by_its_kind_as_teams_sizes_it() {
    let any = None;
    // a phone's screenshot: fitted into the square frame, never stretched to the bubble
    assert_eq!(Kind::of(1080.0, 2520.0), Kind::Tall);
    assert_eq!(
        sides(Shape::of((1080, 2520), (1080, 2520), any)),
        ((PICTURE_BOX, PICTURE_BOX), (77.0, PICTURE_BOX))
    );
    // a small one: at its own size in the square frame, never upscaled
    assert_eq!(Kind::of(100.0, 100.0), Kind::Small);
    assert_eq!(
        sides(Shape::of((100, 100), (200, 200), any)),
        ((PICTURE_BOX, PICTURE_BOX), (100.0, 100.0))
    );
    // a photo: as high as the frame's side, its aspect ratio kept
    assert_eq!(Kind::of(800.0, 600.0), Kind::Standard);
    assert_eq!(
        sides(Shape::of((800, 600), (800, 600), any)),
        ((240.0, PICTURE_BOX), (240.0, PICTURE_BOX))
    );
    // a wide one: its own size while within Teams' bounds, else at most the widest and the frame's side high
    assert_eq!(Kind::of(500.0, 200.0), Kind::Wide);
    assert_eq!(
        sides(Shape::of((500, 200), (500, 200), any)),
        ((500.0, 200.0), (500.0, 200.0))
    );
    assert_eq!(
        sides(Shape::of((2000, 500), (2000, 500), any)),
        ((WIDE_MAX_WIDTH, 155.0), (WIDE_MAX_WIDTH, 155.0))
    );
    assert_eq!(
        sides(Shape::of((1000, 450), (1000, 450), any)),
        ((400.0, PICTURE_BOX), (400.0, PICTURE_BOX))
    );
}

#[test]
fn a_picture_is_never_wider_than_the_width_there_is() {
    let room = Some(gpui::px(102.0));
    // a photo's frame narrows, the picture contained in it
    assert_eq!(
        sides(Shape::of((800, 600), (800, 600), room)),
        ((100.0, PICTURE_BOX), (100.0, 75.0))
    );
    // a wide one keeps its aspect ratio
    assert_eq!(
        sides(Shape::of((500, 200), (500, 200), room)),
        ((100.0, 40.0), (100.0, 40.0))
    );
}
