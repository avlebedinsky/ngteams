use std::path::Path;

use ui_sandbox::SandboxResult;

/// The most room a card takes beside its pill, either way: a little over its 320 px.
const CARD_REACH: u32 = 340;

/// A rectangle on the screen, in output pixels: `x0..x1`, `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Rect {
    pub fn center(self) -> (u32, u32) {
        (u32::midpoint(self.x0, self.x1), u32::midpoint(self.y0, self.y1))
    }

    pub fn contains(self, (x, y): (u32, u32)) -> bool {
        (self.x0..self.x1).contains(&x) && (self.y0..self.y1).contains(&y)
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }

    pub fn grown(self, by: u32) -> Self {
        Self {
            x0: self.x0.saturating_sub(by),
            y0: self.y0.saturating_sub(by),
            x1: self.x1 + by,
            y1: self.y1 + by,
        }
    }
}

/// The largest patch of pixels of `after` drawn in the palette's selection colour that weren't in `before`: the pill
/// under the keys. A smoothed pixel of a face's initials in the card can have that colour too.
pub fn highlighted(before: &Path, after: &Path) -> SandboxResult<Rect> {
    let wanted = teams_ui::theme::LIGHT_PALETTE.selection.to_be_bytes();
    let near = |pixel: &image::Rgb<u8>| {
        pixel
            .0
            .iter()
            .zip(&wanted[1..])
            .all(|(&got, &want)| got.abs_diff(want) <= 6)
    };
    let (before, after) = (image::open(before)?.to_rgb8(), image::open(after)?.to_rgb8());
    largest_patch(&after, |x, y, pixel| near(pixel) && !near(before.get_pixel(x, y)))
        .ok_or_else(|| "no pill drawn in the selection colour".into())
}

/// The bounds of the largest patch of pixels that `taken` takes, joined side by side or corner to corner, if any.
fn largest_patch(shot: &image::RgbImage, taken: impl Fn(u32, u32, &image::Rgb<u8>) -> bool) -> Option<Rect> {
    let (width, height) = shot.dimensions();
    let index = |x: u32, y: u32| y as usize * width as usize + x as usize;
    let mut seen = vec![false; width as usize * height as usize];
    let mut best: Option<(usize, Rect)> = None;
    for (x, y, pixel) in shot.enumerate_pixels() {
        if seen[index(x, y)] || !taken(x, y, pixel) {
            continue;
        }
        seen[index(x, y)] = true;
        let (mut count, mut patch) = (
            0,
            Rect {
                x0: x,
                y0: y,
                x1: x + 1,
                y1: y + 1,
            },
        );
        let mut stack = vec![(x, y)];
        while let Some((px, py)) = stack.pop() {
            count += 1;
            patch = Rect {
                x0: patch.x0.min(px),
                y0: patch.y0.min(py),
                x1: patch.x1.max(px + 1),
                y1: patch.y1.max(py + 1),
            };
            for (nx, ny) in neighbours((px, py), (width, height)) {
                if !seen[index(nx, ny)] && taken(nx, ny, shot.get_pixel(nx, ny)) {
                    seen[index(nx, ny)] = true;
                    stack.push((nx, ny));
                }
            }
        }
        if best.is_none_or(|(most, _)| count > most) {
            best = Some((count, patch));
        }
    }
    best.map(|(_, patch)| patch)
}

/// The pixels round `(x, y)` that are on a screen `width` by `height`.
fn neighbours((x, y): (u32, u32), (width, height): (u32, u32)) -> impl Iterator<Item = (u32, u32)> {
    (-1_i64..=1)
        .flat_map(|dy| (-1_i64..=1).map(move |dx| (dx, dy)))
        .filter(|&step| step != (0, 0))
        .filter_map(move |(dx, dy)| {
            let nx = u32::try_from(i64::from(x) + dx).ok()?;
            let ny = u32::try_from(i64::from(y) + dy).ok()?;
            (nx < width && ny < height).then_some((nx, ny))
        })
}

/// The changed pixels beside `pill`, outside its highlight, within the largest card's reach.
/// Count the light border's 31-level contrast so a short caption cannot stand in for the card.
pub fn card_drawn(before: &Path, after: &Path, pill: Rect) -> SandboxResult<Rect> {
    let (before, after) = (image::open(before)?.to_rgb8(), image::open(after)?.to_rgb8());
    let around = pill.grown(3);
    let (middle, _) = pill.center();
    let reach = Rect {
        x0: middle.saturating_sub(CARD_REACH / 2),
        y0: pill.y0.saturating_sub(CARD_REACH),
        x1: middle + CARD_REACH / 2,
        y1: pill.y1 + CARD_REACH,
    };
    bounds_of(&after, |x, y, pixel| {
        reach.contains((x, y))
            && !around.contains((x, y))
            && pixel
                .0
                .iter()
                .zip(before.get_pixel(x, y).0)
                .any(|(&got, was)| got.abs_diff(was) > 16)
    })
    .ok_or_else(|| "no card drawn".into())
}

/// The bounds of the pixels `taken` takes, if any.
fn bounds_of(shot: &image::RgbImage, taken: impl Fn(u32, u32, &image::Rgb<u8>) -> bool) -> Option<Rect> {
    let mut found: Option<Rect> = None;
    for (x, y, pixel) in shot.enumerate_pixels() {
        if !taken(x, y, pixel) {
            continue;
        }
        found = Some(found.map_or(
            Rect {
                x0: x,
                y0: y,
                x1: x + 1,
                y1: y + 1,
            },
            |rect| Rect {
                x0: rect.x0.min(x),
                y0: rect.y0.min(y),
                x1: rect.x1.max(x + 1),
                y1: rect.y1.max(y + 1),
            },
        ));
    }
    found
}

/// Whether the pixels of `rect` differ between two screenshots.
pub fn changed_within(before: &Path, after: &Path, rect: Rect) -> SandboxResult<bool> {
    let (before, after) = (image::open(before)?.to_rgb8(), image::open(after)?.to_rgb8());
    Ok(bounds_of(&after, |x, y, pixel| {
        rect.contains((x, y))
            && pixel
                .0
                .iter()
                .zip(before.get_pixel(x, y).0)
                .any(|(&got, was)| got.abs_diff(was) > 48)
    })
    .is_some())
}
