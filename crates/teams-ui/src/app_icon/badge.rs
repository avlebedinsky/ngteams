//! Teams' unread badge over the tray icon: a red pill in a white ring at the icon's bottom right corner (the top right
//! holds the gear), the count in white within it, "99+" past 99 as Teams' own badge says ([`label`]).
//!
//! The digits are pixel fonts (3×5 and 5×7) drawn at a whole scale, so each stroke falls on whole pixels: at a panel's
//! 16 to 24 pixels a scaled outline font blurs into a smudge, while these stay sharp. Each icon size takes the largest
//! digits that fit "99+" across it in a pill at most 70% of its height ([`Fit::of`]), the same for every count, so the
//! number keeps its size as it changes; the pill's edge is anti-aliased.

use image::{Rgba, RgbaImage};

use crate::theme;

/// The most the badge counts; past it, it says "99+".
pub const MOST: u64 = 99;

/// The widest thing the badge says, which each size's digits are chosen to fit.
const WIDEST: &str = "99+";

const WHITE: [u8; 3] = [255, 255, 255];

/// What the badge says for an unread count: nothing at 0, the count up to [`MOST`], "99+" past it.
#[must_use]
pub fn label(unread: u64) -> Option<String> {
    match unread {
        0 => None,
        1..=MOST => Some(unread.to_string()),
        _ => Some(format!("{MOST}+")),
    }
}

/// The 3×5 digits 0 to 9, then "+": each row's pixels are its low 3 bits, the leftmost the highest.
const SMALL: [[u8; 5]; 11] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
    [0b000, 0b010, 0b111, 0b010, 0b000],
];

/// The 5×7 digits 0 to 9, then "+", as [`SMALL`]'s.
const LARGE: [[u8; 7]; 11] = [
    [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
    [0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Font {
    /// [`SMALL`].
    Small,
    /// [`LARGE`].
    Large,
}

impl Font {
    const fn width(self) -> u32 {
        match self {
            Self::Small => 3,
            Self::Large => 5,
        }
    }

    const fn height(self) -> u32 {
        match self {
            Self::Small => 5,
            Self::Large => 7,
        }
    }

    /// A glyph's rows: a digit's, or "+" for anything else.
    fn rows(self, glyph: char) -> &'static [u8] {
        let index = glyph
            .to_digit(10)
            .and_then(|digit| usize::try_from(digit).ok())
            .unwrap_or(10);
        match self {
            Self::Small => &SMALL[index],
            Self::Large => &LARGE[index],
        }
    }
}

/// How the badge is drawn on one icon size: its digits' font and scale, and its white ring's width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fit {
    font: Font,
    scale: u32,
    ring: u32,
}

impl Fit {
    /// The fit on an icon of `side` pixels: the largest digits that fit (the 5×7 ones unless the 3×5 ones come out 2
    /// pixels taller or more), none on an icon too small for any.
    fn of(side: u32) -> Option<Self> {
        let ring = (side / 20).max(1);
        let best = |font| {
            (1..=side)
                .map(|scale| Self { font, scale, ring })
                .take_while(|fit| fit.fits(side))
                .last()
        };
        match (best(Font::Large), best(Font::Small)) {
            (Some(large), Some(small)) if small.glyph_height() >= large.glyph_height() + 2 => Some(small),
            (Some(large), _) => Some(large),
            (None, small) => small,
        }
    }

    /// Whether "99+" fits across an icon of `side` pixels, in a pill at most 70% of its height.
    fn fits(self, side: u32) -> bool {
        self.width(WIDEST) <= side && self.height() * 10 <= side * 7
    }

    fn glyph_height(self) -> u32 {
        self.font.height() * self.scale
    }

    /// From the pill's edge to its text: the ring and a pixel of the scale's red.
    fn inset(self) -> u32 {
        self.ring + self.scale
    }

    fn height(self) -> u32 {
        self.glyph_height() + 2 * self.inset()
    }

    /// The text's width: its glyphs a pixel of the scale apart.
    fn text_width(self, label: &str) -> u32 {
        let glyphs = u32::try_from(label.chars().count()).unwrap_or(u32::MAX);
        (glyphs * (self.font.width() + 1) * self.scale).saturating_sub(self.scale)
    }

    /// The pill's width: the text's and the insets, never narrower than it is high (a circle for one digit).
    fn width(self, label: &str) -> u32 {
        (self.text_width(label) + 2 * self.inset()).max(self.height())
    }
}

/// Draws the badge saying `label` at `picture`'s bottom right corner; on an icon too small for any digits, a red dot
/// in a white ring.
pub fn draw(picture: &mut RgbaImage, label: &str) {
    let side = picture.width().min(picture.height());
    let [_, red, green, blue] = theme::DANGER.to_be_bytes();
    let Some(fit) = Fit::of(side) else {
        let pill = Pill::corner(picture, side / 2, side / 2);
        pill.paint(picture, 0, WHITE);
        pill.paint(picture, 1, [red, green, blue]);
        return;
    };
    let pill = Pill::corner(picture, fit.width(label).min(side), fit.height());
    pill.paint(picture, 0, WHITE);
    pill.paint(picture, fit.ring, [red, green, blue]);
    let mut left = pill.left + pill.width.saturating_sub(fit.text_width(label)) / 2;
    let top = pill.top + fit.inset();
    for glyph in label.chars() {
        stamp(picture, fit, fit.font.rows(glyph), (left, top));
        left += (fit.font.width() + 1) * fit.scale;
    }
}

/// Paints a glyph's `rows` white with its top left at `at`, each of its pixels a square of the fit's scale.
fn stamp(picture: &mut RgbaImage, fit: Fit, rows: &[u8], (left, top): (u32, u32)) {
    let width = fit.font.width();
    let lit = rows.iter().zip(0..).flat_map(|(&bits, row)| {
        (0..width)
            .filter(move |column| (bits >> (width - 1 - column)) & 1 == 1)
            .map(move |column| (column, row))
    });
    let square: Vec<(u32, u32)> = (0..fit.scale)
        .flat_map(|dx| (0..fit.scale).map(move |dy| (dx, dy)))
        .collect();
    for (column, row) in lit {
        for (dx, dy) in &square {
            let (x, y) = (left + column * fit.scale + dx, top + row * fit.scale + dy);
            if x < picture.width() && y < picture.height() {
                picture.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }
    }
}

/// The badge's outline: a rectangle whose short ends are half circles.
#[derive(Clone, Copy, Debug)]
struct Pill {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
}

impl Pill {
    /// A pill `width` by `height` in `picture`'s bottom right corner.
    fn corner(picture: &RgbaImage, width: u32, height: u32) -> Self {
        Self {
            left: picture.width().saturating_sub(width),
            top: picture.height().saturating_sub(height),
            width,
            height,
        }
    }

    /// Paints the pill, `inset` pixels in from its edge, in `colour` over `picture`, its edge anti-aliased.
    #[expect(clippy::cast_precision_loss, reason = "an icon's side is at most 64 pixels")]
    fn paint(self, picture: &mut RgbaImage, inset: u32, colour: [u8; 3]) {
        let half = self.height as f32 / 2.0;
        let radius = half - inset as f32;
        let middle = self.top as f32 + half;
        let (from, to) = (self.left as f32 + half, (self.left + self.width) as f32 - half);
        for (column, row, pixel) in picture.enumerate_pixels_mut() {
            let (x, y) = (column as f32 + 0.5, row as f32 + 0.5);
            let along = (from - x).max(x - to).max(0.0);
            let cover = (radius + 0.5 - along.hypot(y - middle)).clamp(0.0, 1.0);
            if cover > 0.0 {
                *pixel = over(*pixel, colour, cover);
            }
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
    fn the_label_is_the_count_up_to_99_then_99_plus() {
        assert_eq!(label(0), None);
        assert_eq!(label(1).as_deref(), Some("1"));
        assert_eq!(label(42).as_deref(), Some("42"));
        assert_eq!(label(99).as_deref(), Some("99"));
        assert_eq!(label(100).as_deref(), Some("99+"));
        assert_eq!(label(u64::MAX).as_deref(), Some("99+"));
    }

    #[test]
    fn each_tray_size_takes_the_largest_sharp_digits_that_fit() {
        // each side's font, scale and ring
        type Fits = Vec<(u32, Option<(Font, u32, u32)>)>;
        let fits: Fits = [8, 16, 22, 24, 32, 48, 64]
            .into_iter()
            .map(|side| (side, Fit::of(side).map(|fit| (fit.font, fit.scale, fit.ring))))
            .collect();
        assert_eq!(
            fits,
            [
                (8, None),
                (16, Some((Font::Small, 1, 1))),
                (22, Some((Font::Large, 1, 1))),
                (24, Some((Font::Large, 1, 1))),
                (32, Some((Font::Small, 2, 1))),
                (48, Some((Font::Large, 2, 2))),
                (64, Some((Font::Large, 3, 3))),
            ]
        );
        for side in [16, 22, 24, 32, 48, 64] {
            let fit = Fit::of(side).unwrap();
            assert!(fit.width(WIDEST) <= side, "{side}");
            // one digit is a circle, its text centred to the pixel
            assert_eq!(fit.width("7"), fit.height(), "{side}");
            assert_eq!((fit.width("7") - fit.text_width("7")) % 2, 0, "{side}");
            assert_eq!((fit.width("42") - fit.text_width("42")) % 2, 0, "{side}");
        }
    }

    /// A transparent icon of `side` with the badge saying `label`.
    fn drawn(side: u32, label: &str) -> RgbaImage {
        let mut picture = RgbaImage::new(side, side);
        draw(&mut picture, label);
        picture
    }

    /// The picture's rows from `from` as text: `#` white, `r` the badge's red, `.` anything else.
    fn ascii(picture: &RgbaImage, from: (u32, u32)) -> Vec<String> {
        let [_, red, green, blue] = theme::DANGER.to_be_bytes();
        (from.1..picture.height())
            .map(|row| {
                (from.0..picture.width())
                    .map(|column| match picture.get_pixel(column, row).0 {
                        [255, 255, 255, 255] => '#',
                        [r, g, b, 255] if [r, g, b] == [red, green, blue] => 'r',
                        _ => '.',
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_digit_sits_white_in_the_red_pill_at_the_bottom_right() {
        // a 9 pixel circle from (7, 7), its rim anti-aliased (so mostly `.`), its 3×5 "7" from (10, 9)
        assert_eq!(
            ascii(&drawn(16, "7"), (7, 7)),
            [
                "....#....",
                "....r....",
                "..r###r..",
                "..rrr#r..",
                "#rrrr#rr#",
                "..rrr#r..",
                "..rrr#r..",
                "....r....",
                "....#....",
            ]
        );
        // "99+" is a 15 pixel pill from (1, 7), its glyphs a pixel apart
        assert_eq!(
            ascii(&drawn(16, "99+"), (0, 7)),
            [
                ".....#######....",
                ".....rrrrrrr....",
                "...###r###rrrr..",
                "...#r#r#r#rr#r..",
                ".#r###r###r###r#",
                "...rr#rrr#rr#r..",
                "...###r###rrrr..",
                ".....rrrrrrr....",
                ".....#######....",
            ]
        );
    }

    #[test]
    fn the_badge_grows_with_the_count_and_stays_in_the_corner() {
        for side in [16, 22, 24, 32, 48, 64] {
            let fit = Fit::of(side).unwrap();
            let left = |label: &str| {
                let picture = drawn(side, label);
                // the leftmost column the badge reached on its middle row
                let middle = side - fit.height() / 2 - 1;
                (0..side).find(|&column| picture.get_pixel(column, middle).0[3] > 0)
            };
            let (one, two, most) = (left("9"), left("42"), left("99+"));
            assert!(one > two && two > most, "{side}: {one:?} {two:?} {most:?}");
            // the icon's top left and what is above the badge stay untouched
            let picture = drawn(side, "99+");
            assert_eq!(picture.get_pixel(0, 0).0, [0, 0, 0, 0]);
            assert_eq!(picture.get_pixel(side / 2, side - fit.height() - 1).0, [0, 0, 0, 0]);
        }
    }

    #[test]
    fn a_tiny_icon_gets_a_dot() {
        let picture = drawn(8, "5");
        // the dot's middle: opaque, red over the white ring's edge, and no digit
        let [red, green, blue, alpha] = picture.get_pixel(6, 6).0;
        assert_eq!(alpha, 255);
        assert!(red > green + 80 && red > blue + 80, "{red} {green} {blue}");
        assert_eq!(picture.get_pixel(1, 1).0, [0, 0, 0, 0]);
    }

    #[test]
    fn the_pill_blends_over_the_icon() {
        let mut picture = RgbaImage::from_pixel(16, 16, Rgba([0, 0, 255, 128]));
        draw(&mut picture, "1");
        // an anti-aliased edge pixel mixes the white ring over the half-clear blue and comes out more opaque
        let edge = picture.get_pixel(8, 14).0;
        assert!(edge[3] > 128 && edge[0] > 0 && edge[2] > edge[0], "{edge:?}");
    }
}
