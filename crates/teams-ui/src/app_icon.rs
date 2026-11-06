//! The app's icon (`packaging/icons/`, the pictures the install puts in the desktop's icon theme): the name the theme
//! knows it by ([`NAME`]), its pixels as the tray item carries them ([`pixmaps`], with Teams' unread badge and its
//! count while something is unread: [`badge`]), and the picture a view draws ([`image`]). The tray and the views carry
//! the pixels themselves, so the icon shows whether or not the theme holds it (the lab app's sandbox has no installed
//! icon).

pub mod badge;

use std::sync::{Arc, OnceLock};

use gpui::{Image, ImageFormat};
use image::{Rgba, RgbaImage};

/// The icon's name in the desktop's icon theme: the personal build's app id, which its desktop entry names as `Icon`.
pub const NAME: &str = crate::window::PERSONAL_APP_ID;

/// The sizes the tray item offers, small to large: the host scales the nearest one to its panel.
const TRAY: [&[u8]; 6] = [
    include_bytes!("../../../packaging/icons/hicolor/16x16/apps/com.nickalie.ngteams.png"),
    include_bytes!("../../../packaging/icons/hicolor/22x22/apps/com.nickalie.ngteams.png"),
    include_bytes!("../../../packaging/icons/hicolor/24x24/apps/com.nickalie.ngteams.png"),
    include_bytes!("../../../packaging/icons/hicolor/32x32/apps/com.nickalie.ngteams.png"),
    include_bytes!("../../../packaging/icons/hicolor/48x48/apps/com.nickalie.ngteams.png"),
    include_bytes!("../../../packaging/icons/hicolor/64x64/apps/com.nickalie.ngteams.png"),
];

/// The size a view draws (it scales it down to what it shows).
const VIEW: &[u8] = include_bytes!("../../../packaging/icons/hicolor/128x128/apps/com.nickalie.ngteams.png");

/// An icon's pixels as a `StatusNotifierItem` carries them: width, height, and ARGB32 in network byte order.
pub type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

/// The tray item's pictures of the icon, every size, badged with `unread` while it isn't 0 ([`badge::label`]).
#[must_use]
pub fn pixmaps(unread: u64) -> Pixmaps {
    pictures(unread).iter().filter_map(argb).collect()
}

/// The tray's sizes of the icon as RGBA pictures, badged with `unread` while it isn't 0.
#[must_use]
pub fn pictures(unread: u64) -> Vec<RgbaImage> {
    static PLAIN: OnceLock<Vec<RgbaImage>> = OnceLock::new();
    let plain = PLAIN.get_or_init(|| TRAY.iter().filter_map(|png| decode(png)).collect());
    let Some(label) = badge::label(unread) else {
        return plain.clone();
    };
    plain
        .iter()
        .map(|picture| {
            let mut picture = picture.clone();
            badge::draw(&mut picture, &label);
            picture
        })
        .collect()
}

/// The icon as a view draws it.
#[must_use]
pub fn image() -> Arc<Image> {
    static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
    Arc::clone(IMAGE.get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, VIEW.to_vec()))))
}

fn decode(png: &[u8]) -> Option<RgbaImage> {
    Some(
        image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .ok()?
            .into_rgba8(),
    )
}

fn argb(picture: &RgbaImage) -> Option<(i32, i32, Vec<u8>)> {
    let (width, height) = picture.dimensions();
    let argb = picture
        .pixels()
        .flat_map(|&Rgba([red, green, blue, alpha])| [alpha, red, green, blue])
        .collect();
    Some((i32::try_from(width).ok()?, i32::try_from(height).ok()?, argb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    #[test]
    fn every_tray_size_decodes_to_square_argb() {
        for unread in [0, 1, 100] {
            let pixmaps = pixmaps(unread);
            assert_eq!(pixmaps.len(), TRAY.len());
            for (width, height, argb) in &pixmaps {
                assert_eq!(width, height);
                assert_eq!(argb.len(), usize::try_from(width * height * 4).unwrap());
            }
            let sides: Vec<i32> = pixmaps.iter().map(|(width, _, _)| *width).collect();
            assert_eq!(sides, [16, 22, 24, 32, 48, 64]);
        }
    }

    #[test]
    fn the_badge_is_red_at_the_bottom_right_and_says_the_count() {
        let plain = pixmaps(0);
        let (side, _, plain) = &plain[5];
        let side = usize::try_from(*side).unwrap();
        let at = |argb: &Vec<u8>, column: usize, row: usize| {
            let offset = (row * side + column) * 4;
            [argb[offset], argb[offset + 1], argb[offset + 2], argb[offset + 3]]
        };
        let [_, red, green, blue] = theme::DANGER.to_be_bytes();
        let one = pixmaps(1);
        let (_, _, one) = &one[5];
        // just inside the badge's ring, left of its digit
        assert_eq!(at(one, side - 28, side - 16), [255, red, green, blue]);
        // the top right (the gear) and the bottom left are the icon itself, untouched
        assert_eq!(at(one, side - 8, side / 4), at(plain, side - 8, side / 4));
        assert_eq!(at(one, side / 4, side * 3 / 4), at(plain, side / 4, side * 3 / 4));
        // each count draws its own number
        assert_ne!(pixmaps(1), pixmaps(2));
        assert_ne!(pixmaps(99), pixmaps(100));
        assert_eq!(pixmaps(100), pixmaps(5000));
    }
}
