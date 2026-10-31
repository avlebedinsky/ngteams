use std::time::{Duration, Instant};

use super::compose::Compositor;
use super::*;

/// An I420 frame `width` by `height`: luma a diagonal ramp, chroma grey.
fn ramp(width: u32, height: u32) -> VideoFrame {
    let (w, h) = (to_usize(width), to_usize(height));
    let mut data = vec![128_u8; w * h * 3 / 2];
    for (row, line) in data[..w * h].chunks_exact_mut(w).enumerate() {
        for (column, value) in line.iter_mut().enumerate() {
            *value = u8::try_from((column * 7 + row * 3) % 220 + 16).unwrap_or(0);
        }
    }
    VideoFrame {
        width,
        height,
        format: PixelFormat::I420,
        stride: width,
        data,
    }
}

/// A mask of the model's size with the person on its left half.
fn left_half_person() -> Vec<u8> {
    (0..MODEL_HEIGHT * MODEL_WIDTH)
        .map(|index| if index % MODEL_WIDTH < MODEL_WIDTH / 2 { 255 } else { 0 })
        .collect()
}

#[test]
fn no_effect_leaves_the_frame_as_it_is() {
    let frame = ramp(64, 36);
    let mut effected = frame.clone();
    Effects::new().apply(&mut effected, &Background::None);
    assert_eq!(effected, frame);
}

#[test]
fn a_picture_replaces_the_background_and_keeps_the_person() {
    let frame = ramp(320, 180);
    let mut picture = frame.clone();
    picture.data.fill(200);
    let mut effected = frame.clone();
    Compositor::default().replace(&mut effected, &left_half_person(), &picture);
    let width = 320;
    let (person, background) = (40 * width + 20, 40 * width + 300);
    assert_eq!(effected.data[person], frame.data[person], "the person's pixel kept");
    assert_eq!(effected.data[background], 200, "the background's pixel replaced");
}

#[test]
fn blur_smooths_the_background_and_keeps_the_person() {
    let mut frame = ramp(320, 180);
    // a sharp checkerboard: the blur flattens it where it is the background
    for (index, value) in frame.data[..320 * 180].iter_mut().enumerate() {
        *value = if (index % 320 / 4 + index / 320 / 4) % 2 == 0 {
            40
        } else {
            220
        };
    }
    let mut effected = frame.clone();
    Compositor::default().blur(&mut effected, &left_half_person());
    let spread = |data: &[u8], from: usize| {
        let row = &data[90 * 320 + from..90 * 320 + from + 16];
        row.iter().max().copied().unwrap_or(0) - row.iter().min().copied().unwrap_or(0)
    };
    assert_eq!(
        spread(&effected.data, 20),
        spread(&frame.data, 20),
        "the person's side untouched"
    );
    assert!(
        spread(&effected.data, 280) < 30,
        "the background flattened: {}",
        spread(&effected.data, 280)
    );
}

#[test]
fn a_stock_picture_is_prepared_at_the_frame_size() {
    let picture = compose::backdrop_frame(&Backdrop::Stock(&STOCK[0]), 640, 360).unwrap();
    assert_eq!((picture.width, picture.height), (640, 360));
    assert_eq!(picture.data.len(), 640 * 360 * 3 / 2);
    let tall = compose::backdrop_frame(&Backdrop::Stock(&STOCK[1]), 360, 640).unwrap();
    assert_eq!((tall.width, tall.height), (360, 640));
}

#[test]
fn every_stock_picture_decodes() {
    for stock in &STOCK {
        let decoded = image::load_from_memory(stock.jpeg).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (1280, 720), "{}", stock.key);
    }
    assert_eq!(super::stock("waves").map(|stock| stock.name), Some("Purple waves"));
    assert!(super::stock("nothing").is_none());
}

#[test]
fn a_missing_picture_blurs_instead() {
    let mut frame = ramp(160, 90);
    let before = frame.clone();
    let mut effects = Effects::new();
    effects.apply(
        &mut frame,
        &Background::Picture(Backdrop::File("/nonexistent/picture.jpg".into())),
    );
    assert_ne!(frame, before, "the frame was still changed: blurred");
}

#[test]
fn an_added_picture_is_scaled_down_and_listed() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("big.png");
    image::RgbImage::from_pixel(3000, 1000, image::Rgb([10, 200, 30]))
        .save(&source)
        .unwrap();
    let kept = import(&source, &dir.path().join("added")).unwrap();
    let decoded = image::open(&kept).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (1920, 640));
    assert_eq!(added(&dir.path().join("added")), vec![kept]);
    assert!(import(&dir.path().join("missing.png"), dir.path()).is_err());
}

/// What a 720p frame costs: the model's run (on its own thread) and the camera thread's part (the frame scaled for
/// the model and put together with each effect). Timed in a release build (`cargo test --release -p teams-media
/// effects -- --ignored --nocapture`): the debug build's numbers say nothing.
#[test]
#[ignore = "a timing: run in a release build"]
fn a_720p_frame_costs_milliseconds() {
    let source = ramp(1280, 720);
    let plan = segment::plan().unwrap();
    let input = segment::model_input(&source);
    let model = median(|| {
        segment::run(&plan, &input).unwrap();
    });
    println!("the model: median {model:?}");
    let scaled = median(|| {
        segment::model_input(&source);
    });
    println!("the frame scaled for the model: median {scaled:?}");
    let mut effects = Effects::new();
    for background in [Background::Blur, Background::Picture(Backdrop::Stock(&STOCK[2]))] {
        let mut frame = source.clone();
        effects.apply(&mut frame, &background);
        effects.segmenter.settle(&source, Duration::from_secs(5));
        let cost = median(|| {
            let mut frame = source.clone();
            effects.apply(&mut frame, &background);
        });
        println!("the camera thread with {background:?}: median {cost:?}");
        assert!(cost < Duration::from_millis(33), "a frame takes {cost:?}");
    }
    assert!(model < Duration::from_millis(66), "the model takes {model:?}");
}

/// The median of 60 runs of `run`, after 5 to warm up.
fn median(mut run: impl FnMut()) -> Duration {
    for _ in 0..5 {
        run();
    }
    let mut costs: Vec<Duration> = (0..60)
        .map(|_| {
            let start = Instant::now();
            run();
            start.elapsed()
        })
        .collect();
    costs.sort();
    costs[30]
}

/// The effects on a picture of a person (`TEAMS_EFFECTS_SAMPLE`, any picture), written as PNGs under
/// `TEAMS_EFFECTS_OUT`, for a look.
#[test]
#[ignore = "writes pictures for a look"]
fn effects_on_a_sample_picture() {
    let (Ok(sample), Ok(out)) = (
        std::env::var("TEAMS_EFFECTS_SAMPLE"),
        std::env::var("TEAMS_EFFECTS_OUT"),
    ) else {
        return;
    };
    let picture = compose::backdrop_frame(&Backdrop::File(sample.into()), 1280, 720).unwrap();
    let mut effects = Effects::new();
    for (name, background) in [
        ("blur", Background::Blur),
        ("picture", Background::Picture(Backdrop::Stock(&STOCK[0]))),
    ] {
        effects.segmenter.settle(&picture, Duration::from_secs(5));
        let mut frame = picture.clone();
        effects.apply(&mut frame, &background);
        let bgra = crate::video::convert::i420_to_bgra(&frame).unwrap();
        let rgba: Vec<u8> = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect();
        image::RgbaImage::from_raw(1280, 720, rgba)
            .unwrap()
            .save(std::path::Path::new(&out).join(format!("effects-{name}.png")))
            .unwrap();
    }
}
