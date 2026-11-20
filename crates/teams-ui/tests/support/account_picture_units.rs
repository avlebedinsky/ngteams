//! The store of the account pictures: save, replace, reset, the formats read, a file refused. Each picture is made
//! here (synthetic).

use super::*;

use image::{ImageFormat, Rgba, RgbaImage};

const A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";

const RED: Rgba<u8> = Rgba([220, 30, 30, 255]);
const GREEN: Rgba<u8> = Rgba([30, 200, 30, 255]);
const BLUE: Rgba<u8> = Rgba([30, 30, 220, 255]);

/// A picture `width` by `height`: red on the left third, green in the middle third, blue on the right third.
fn stripes(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, _| match x * 3 / width {
        0 => RED,
        1 => GREEN,
        _ => BLUE,
    })
}

fn written(dir: &Path, name: &str, picture: &RgbaImage, format: ImageFormat) -> PathBuf {
    let path = dir.join(name);
    DynamicImage::ImageRgba8(picture.clone())
        .to_rgb8()
        .save_with_format(&path, format)
        .unwrap();
    path
}

fn decoded(png: &[u8]) -> RgbaImage {
    image::load_from_memory_with_format(png, ImageFormat::Png)
        .unwrap()
        .to_rgba8()
}

/// [`AccountPictures::save`] in a turn of its own: the PNG kept.
fn kept(pictures: &AccountPictures, account: &str, source: &Path) -> Result<Vec<u8>, PictureError> {
    pictures
        .save(account, source, pictures.turn(account))
        .map(|png| png.expect("no later turn"))
}

fn near(pixel: Rgba<u8>, wanted: Rgba<u8>) -> bool {
    pixel.0.iter().zip(wanted.0).all(|(got, want)| got.abs_diff(want) < 40)
}

#[test]
fn a_saved_picture_is_the_centre_as_a_square_png_and_stays_when_its_file_goes() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let pictures = AccountPictures::new(data.path().join(DIR));
    let wide = written(source.path(), "wide.png", &stripes(900, 300), ImageFormat::Png);

    let png = kept(&pictures, A, &wide).unwrap();
    std::fs::remove_file(&wide).unwrap();

    let kept = decoded(&png);
    assert_eq!(kept.dimensions(), (SIDE, SIDE), "larger than SIDE: made SIDE across");
    // the centre third of a 3:1 picture is all of its square: green from edge to edge
    assert!(near(*kept.get_pixel(2, SIDE / 2), GREEN));
    assert!(near(*kept.get_pixel(SIDE - 3, SIDE / 2), GREEN));
    assert_eq!(pictures.load(A), Some(png), "the copy stays after the chosen file went");
    assert_eq!(pictures.load(B), None, "another account has none");
}

#[test]
fn a_small_picture_is_kept_at_its_own_size() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let pictures = AccountPictures::new(data.path().join(DIR));
    let tall = written(source.path(), "tall.png", &stripes(30, 90), ImageFormat::Png);

    let kept = decoded(&kept(&pictures, A, &tall).unwrap());

    assert_eq!(kept.dimensions(), (30, 30));
}

#[test]
fn a_second_picture_replaces_the_first() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let pictures = AccountPictures::new(data.path().join(DIR));
    let red = written(
        source.path(),
        "red.png",
        &RgbaImage::from_pixel(40, 40, RED),
        ImageFormat::Png,
    );
    let blue = written(
        source.path(),
        "blue.png",
        &RgbaImage::from_pixel(40, 40, BLUE),
        ImageFormat::Png,
    );

    kept(&pictures, A, &red).unwrap();
    kept(&pictures, A, &blue).unwrap();

    let kept = decoded(&pictures.load(A).unwrap());
    assert!(near(*kept.get_pixel(20, 20), BLUE));
    let files: Vec<_> = std::fs::read_dir(&pictures.dir).unwrap().collect();
    assert_eq!(files.len(), 1, "one file for the account, no part file left");
}

#[test]
fn reset_deletes_the_picture_and_is_quiet_without_one() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let pictures = AccountPictures::new(data.path().join(DIR));
    let red = written(
        source.path(),
        "red.png",
        &RgbaImage::from_pixel(8, 8, RED),
        ImageFormat::Png,
    );
    kept(&pictures, A, &red).unwrap();
    kept(&pictures, B, &red).unwrap();

    pictures.reset(A).unwrap();

    assert_eq!(pictures.load(A), None);
    assert!(pictures.load(B).is_some(), "the other account keeps its picture");
    pictures.reset(A).unwrap();
}

#[test]
fn jpeg_gif_and_webp_are_read_whatever_the_file_is_named() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let pictures = AccountPictures::new(data.path().join(DIR));
    let green = RgbaImage::from_pixel(16, 16, GREEN);
    for (name, format) in [
        ("photo.jpg", ImageFormat::Jpeg),
        ("first-frame.gif", ImageFormat::Gif),
        ("lossless.webp", ImageFormat::WebP),
        ("no-extension", ImageFormat::Png),
    ] {
        let path = written(source.path(), name, &green, format);
        let kept = decoded(&kept(&pictures, A, &path).unwrap());
        assert!(near(*kept.get_pixel(8, 8), GREEN), "{name}");
    }
}

#[test]
fn a_file_that_is_no_picture_is_refused_and_the_picture_before_stays() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let pictures = AccountPictures::new(data.path().join(DIR));
    let red = written(
        source.path(),
        "red.png",
        &RgbaImage::from_pixel(8, 8, RED),
        ImageFormat::Png,
    );
    let before = kept(&pictures, A, &red).unwrap();
    let text = source.path().join("notes.png");
    std::fs::write(&text, "not a picture").unwrap();

    let refused = kept(&pictures, A, &text);

    assert!(matches!(refused, Err(PictureError::Unreadable(_))), "{refused:?}");
    let gone = kept(&pictures, A, &source.path().join("gone.png"));
    assert!(
        matches!(gone, Err(PictureError::Unopened(_))),
        "the chosen file, not the copy: {gone:?}"
    );
    assert_eq!(pictures.load(A), Some(before));
}

#[test]
fn a_save_of_an_earlier_turn_keeps_nothing_after_a_later_save_or_a_reset() {
    let source = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let pictures = AccountPictures::new(data.path().join(DIR));
    let red = written(
        source.path(),
        "red.png",
        &RgbaImage::from_pixel(8, 8, RED),
        ImageFormat::Png,
    );
    let blue = written(
        source.path(),
        "blue.png",
        &RgbaImage::from_pixel(8, 8, BLUE),
        ImageFormat::Png,
    );

    // red asked first, blue next; blue ends first
    let (first, second) = (pictures.turn(A), pictures.turn(A));
    assert!(pictures.save(A, &blue, second).unwrap().is_some());
    assert_eq!(
        pictures.save(A, &red, first).unwrap(),
        None,
        "the earlier turn keeps nothing"
    );
    assert!(!pictures.is_current(A, first) && pictures.is_current(A, second));
    let kept = decoded(&pictures.load(A).unwrap());
    assert!(near(*kept.get_pixel(4, 4), BLUE));

    // a save asked before a reset ends after it
    let before = pictures.turn(A);
    pictures.reset(A).unwrap();
    assert_eq!(pictures.save(A, &red, before).unwrap(), None);
    assert_eq!(pictures.load(A), None);
    assert!(!pictures.is_current(A, before));
    let files: Vec<_> = std::fs::read_dir(&pictures.dir).unwrap().collect();
    assert!(files.is_empty(), "no part file left: {files:?}");
}

#[test]
fn each_account_gets_its_own_file_name_without_a_colon() {
    let stems = [A, B, "8_orgid_x", "8:orgid:x", "8:orgid:X"].map(file_stem);

    assert!(stems.iter().all(|stem| !stem.contains(':') && !stem.contains('/')));
    for (at, stem) in stems.iter().enumerate() {
        assert!(!stems[at + 1..].contains(stem), "{stem} twice");
    }
    assert_eq!(file_stem("8:orgid:ab-1"), "8_3aorgid_3aab-1");
}
