#![cfg(target_os = "linux")]

#[expect(dead_code, reason = "the sandbox also uses the pill and change readers")]
#[path = "support/linux/reactors_pixels.rs"]
mod pixels;

use image::{Rgb, RgbImage};
use pixels::Rect;

fn fill(image: &mut RgbImage, rect: Rect, colour: [u8; 3]) {
    for y in rect.y0..rect.y1 {
        for x in rect.x0..rect.x1 {
            image.put_pixel(x, y, Rgb(colour));
        }
    }
}

#[test]
fn a_short_caption_does_not_shrink_the_card_to_its_text() {
    let pill = Rect {
        x0: 300,
        y0: 350,
        x1: 400,
        y1: 376,
    };
    for background in [[255; 3], [243, 244, 250]] {
        for card in [
            Rect {
                x0: 250,
                y0: 380,
                x1: 450,
                y1: 460,
            },
            Rect {
                x0: 250,
                y0: 266,
                x1: 450,
                y1: 346,
            },
        ] {
            let before = RgbImage::from_pixel(800, 600, Rgb(background));
            let mut after = before.clone();
            fill(&mut after, pill, [197, 203, 250]);
            fill(&mut after, card, [224; 3]);
            fill(
                &mut after,
                Rect {
                    x0: card.x0 + 1,
                    y0: card.y0 + 1,
                    x1: card.x1 - 1,
                    y1: card.y1 - 1,
                },
                [255; 3],
            );
            fill(
                &mut after,
                Rect {
                    x0: card.x0 + 16,
                    y0: card.y0 + 16,
                    x1: card.x0 + 124,
                    y1: card.y0 + 60,
                },
                [97; 3],
            );
            fill(
                &mut after,
                Rect {
                    x0: 0,
                    y0: 0,
                    x1: 100,
                    y1: 600,
                },
                [0; 3],
            );
            let dir = tempfile::tempdir().unwrap();
            let before_path = dir.path().join("before.png");
            let after_path = dir.path().join("after.png");
            before.save(&before_path).unwrap();
            after.save(&after_path).unwrap();
            assert_eq!(pixels::card_drawn(&before_path, &after_path, pill).unwrap(), card);
        }
    }
}

#[test]
fn the_pill_and_changes_outside_its_reach_do_not_make_a_card() {
    let pill = Rect {
        x0: 300,
        y0: 350,
        x1: 400,
        y1: 376,
    };
    let before = RgbImage::from_pixel(800, 600, Rgb([255; 3]));
    let mut after = before.clone();
    fill(&mut after, pill, [197, 203, 250]);
    fill(
        &mut after,
        Rect {
            x0: 0,
            y0: 0,
            x1: 100,
            y1: 100,
        },
        [0; 3],
    );
    let dir = tempfile::tempdir().unwrap();
    let before_path = dir.path().join("before.png");
    let after_path = dir.path().join("after.png");
    before.save(&before_path).unwrap();
    after.save(&after_path).unwrap();
    assert!(pixels::card_drawn(&before_path, &after_path, pill).is_err());
}
