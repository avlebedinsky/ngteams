//! The app's icon (`packaging/icons/`, the pictures the install puts in the desktop's icon theme): the name the theme
//! knows it by ([`NAME`]), its pixels as the tray item carries them ([`pixmaps`], with Teams' unread badge while
//! something is unread), and the picture a view draws ([`image`]). The tray and the views carry the pixels themselves,
//! so the icon shows whether or not the theme holds it (the lab app's sandbox has no installed icon).

use std::sync::{Arc, OnceLock};

use gpui::{Image, ImageFormat};
use image::{Rgba, RgbaImage};

use crate::theme;

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

/// The tray item's pictures of the icon, every size, with the unread badge when `unread`.
#[must_use]
pub fn pixmaps(unread: bool) -> Pixmaps {
    static PLAIN: OnceLock<Pixmaps> = OnceLock::new();
    static BADGED: OnceLock<Pixmaps> = OnceLock::new();
    let cell = if unread { &BADGED } else { &PLAIN };
    cell.get_or_init(|| TRAY.iter().filter_map(|png| pixmap(png, unread)).collect())
        .clone()
}

/// The icon as a view draws it.
#[must_use]
pub fn image() -> Arc<Image> {
    static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
    Arc::clone(IMAGE.get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, VIEW.to_vec()))))
}

fn pixmap(png: &[u8], badged: bool) -> Option<(i32, i32, Vec<u8>)> {
    let mut picture = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    if badged {
        badge(&mut picture);
    }
    let (width, height) = picture.dimensions();
    let argb = picture
        .pixels()
        .flat_map(|&Rgba([red, green, blue, alpha])| [alpha, red, green, blue])
        .collect();
    Some((i32::try_from(width).ok()?, i32::try_from(height).ok()?, argb))
}

/// Teams' unread badge: a red dot in a white ring at the icon's bottom right corner (the top right holds the gear).
#[expect(clippy::cast_precision_loss, reason = "an icon's side is at most 64 pixels")]
fn badge(picture: &mut RgbaImage) {
    let side = picture.width().min(picture.height()) as f32;
    let radius = (side * 0.22).max(3.0);
    let ring = (side / 16.0).max(1.0);
    let centre = (picture.width() as f32 - radius, picture.height() as f32 - radius);
    let [_, red, green, blue] = theme::DANGER.to_be_bytes();
    disc(picture, centre, radius, [255, 255, 255]);
    disc(picture, centre, radius - ring, [red, green, blue]);
}

/// Paints a disc of `colour` over `picture`, its edge anti-aliased.
#[expect(clippy::cast_precision_loss, reason = "an icon's side is at most 64 pixels")]
fn disc(picture: &mut RgbaImage, (x, y): (f32, f32), radius: f32, colour: [u8; 3]) {
    for (column, row, pixel) in picture.enumerate_pixels_mut() {
        let distance = (column as f32 + 0.5 - x).hypot(row as f32 + 0.5 - y);
        let cover = (radius + 0.5 - distance).clamp(0.0, 1.0);
        if cover > 0.0 {
            *pixel = over(*pixel, colour, cover);
        }
    }
}

/// `colour` at `cover` over `under`, both unpremultiplied.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "each value is clamped to 0..=255 first"
)]
fn over(under: Rgba<u8>, colour: [u8; 3], cover: f32) -> Rgba<u8> {
    let below = f32::from(under[3]) / 255.0;
    let alpha = cover + below * (1.0 - cover);
    let mix = |top: u8, bottom: u8| {
        let value = (f32::from(top) * cover + f32::from(bottom) * below * (1.0 - cover)) / alpha;
        value.round().clamp(0.0, 255.0) as u8
    };
    Rgba([
        mix(colour[0], under[0]),
        mix(colour[1], under[1]),
        mix(colour[2], under[2]),
        (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tray_size_decodes_to_square_argb() {
        for unread in [false, true] {
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
    fn the_badge_is_a_red_dot_at_the_bottom_right() {
        let plain = pixmaps(false);
        let badged = pixmaps(true);
        let (side, _, plain) = &plain[5];
        let (_, _, badged) = &badged[5];
        let side = usize::try_from(*side).unwrap();
        // the badge's centre, a few pixels in from the corner
        let at = |argb: &Vec<u8>, column: usize, row: usize| {
            let offset = (row * side + column) * 4;
            [argb[offset], argb[offset + 1], argb[offset + 2], argb[offset + 3]]
        };
        let [_, red, green, blue] = theme::DANGER.to_be_bytes();
        let centre = side - side * 22 / 100;
        assert_eq!(at(badged, centre, centre), [255, red, green, blue]);
        // the top right (the gear) and the bottom left are the icon itself, untouched
        assert_eq!(at(badged, centre, side / 4), at(plain, centre, side / 4));
        assert_eq!(at(badged, side / 4, side * 3 / 4), at(plain, side / 4, side * 3 / 4));
    }
}
