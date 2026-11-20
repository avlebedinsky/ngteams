//! The message box's frame in the tour's screenshots, as Teams draws it: a thin border, no shadow or glow around it,
//! and, while the box has the focus, a line of the brand colour just inside its bottom border, its ends inside the
//! box's round corners. The child tells where the box is ([`line`]); the parent reads the pixels ([`check`]).

use std::path::Path;

use gpui::{App, Entity, Window};
use image::{Rgb, RgbImage};
use teams_ui::shell::{Main, Shell};
use ui_sandbox::SandboxResult;

/// The steps whose screenshots show the chat's box: focused and not, with a reply, an edit, an attachment, expanded,
/// in a larger text size, in Teams Light and Dark.
pub(super) const SHOTS: [&str; 12] = [
    "chat-actions",
    "compose-unfocused",
    "chat-reply",
    "chat-compose",
    "chat-compose-expanded",
    "compose-picture",
    "compose-picture-dark",
    "compose-unfocused-dark",
    "compose-edit-dark",
    "compose-font-18-dark",
    "chat-dark-compose",
    "chat-dark-compose-expanded",
];

/// The start of the line the child says before a shot of [`SHOTS`].
pub(super) const PREFIX: &str = "compose-edge ";

/// What the child says of the chat's box: where it is inside its border (logical px), the window's scale, the rem,
/// whether the box has the focus, and the brand colour.
pub(super) fn line(shell: &Entity<Shell>, window: &Window, cx: &App) -> String {
    let shell = shell.read(cx);
    // a chat kept behind another page still holds where its box was last drawn
    let (Main::Chat(_), Some(chat)) = (shell.main(), shell.chat()) else {
        return format!("{PREFIX}none");
    };
    let compose = chat.read(cx).compose().read(cx);
    let Some(bounds) = compose.box_bounds() else {
        return format!("{PREFIX}none");
    };
    format!(
        "{PREFIX}{} {} {} {} {} {} {} {:06x}",
        f32::from(bounds.origin.x),
        f32::from(bounds.origin.y),
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
        window.scale_factor(),
        f32::from(window.rem_size()),
        compose.focused(window, cx),
        teams_ui::theme::current(cx).brand,
    )
}

/// The box as the shot has it, in device pixels.
struct Edge {
    /// The border's outer columns and rows; `right` and `bottom` are one past them.
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
    /// The border's width, the focus line's height and the corners' outer radius.
    border: u32,
    line: u32,
    radius: u32,
    focused: bool,
    brand: Rgb<u8>,
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a window's coordinates are small and positive"
)]
fn device(logical: f32, scale: f32) -> u32 {
    (logical * scale).round().max(0.0) as u32
}

fn parse(said: &str) -> SandboxResult<Option<Edge>> {
    let rest = said.strip_prefix(PREFIX).ok_or("not a compose-edge line")?;
    if rest == "none" {
        return Ok(None);
    }
    let words: Vec<&str> = rest.split(' ').collect();
    let [x, y, width, height, scale, rem, focused, brand] = words[..] else {
        return Err(format!("a compose-edge line of {} words", words.len()).into());
    };
    let [x, y, width, height, scale, rem] = [x, y, width, height, scale, rem].map(str::parse::<f32>);
    let (x, y, width, height, scale, rem) = (x?, y?, width?, height?, scale?, rem?);
    let brand = u32::from_str_radix(brand, 16)?.to_be_bytes();
    Ok(Some(Edge {
        left: device(x - 1.0, scale),
        top: device(y - 1.0, scale),
        right: device(x + width + 1.0, scale),
        bottom: device(y + height + 1.0, scale),
        border: device(1.0, scale).max(1),
        line: device(2.0, scale).max(1),
        // the box's `rounded_lg`
        radius: device(rem * 0.5, scale),
        focused: focused.parse()?,
        brand: Rgb([brand[1], brand[2], brand[3]]),
    }))
}

/// The sum of the channels' differences.
fn apart(a: Rgb<u8>, b: Rgb<u8>) -> u32 {
    a.0.iter().zip(b.0).map(|(a, b)| u32::from(a.abs_diff(b))).sum()
}

/// How much bluer than grey a pixel is: the brand colour's tint.
fn blue(pixel: Rgb<u8>) -> i32 {
    let [red, green, blue] = pixel.0.map(i32::from);
    blue - i32::midpoint(red, green)
}

/// Fails when the box in `png` has a glow or a shadow around it, or its focus line is out of place: not the brand
/// colour all along, not just inside the bottom border, out past its round corners, or drawn without the focus.
pub(super) fn check(png: &Path, said: &str) -> SandboxResult {
    let Some(edge) = parse(said)? else {
        return Err("the child found no chat box".into());
    };
    let shot = image::open(png)?.to_rgb8();
    let (width, height) = shot.dimensions();
    let gap = edge.radius + 2;
    if edge.left < gap + 1 || edge.right + gap >= width || edge.bottom + gap >= height || edge.top >= edge.bottom {
        return Err(format!(
            "the box {}..{} × {}..{} in a {width}×{height} shot",
            edge.left, edge.right, edge.top, edge.bottom
        )
        .into());
    }
    no_glow(&shot, &edge, gap)?;
    bottom_edge(&shot, &edge, gap)
}

/// The first pixel along `run` more than a few levels off `pane`, the pane's colour farther out on the same line.
fn off(shot: &RgbImage, run: impl Iterator<Item = (u32, u32)>, pane: Rgb<u8>) -> Option<(u32, u32, Rgb<u8>, Rgb<u8>)> {
    run.map(|(x, y)| (x, y, *shot.get_pixel(x, y), pane))
        .find(|(_, _, pixel, _)| pixel.0.iter().zip(pane.0).any(|(a, b)| a.abs_diff(b) > 3))
}

/// The pane under and beside the box keeps its own colour up to the box's border: no shadow, no glow. The pane may be
/// a gradient, so each pixel is held to the pane `gap` px farther out on its row or column.
fn no_glow(shot: &RgbImage, edge: &Edge, gap: u32) -> SandboxResult {
    let under = (edge.left + edge.radius..edge.right - edge.radius).find_map(|x| {
        let pane = *shot.get_pixel(x, edge.bottom + gap);
        off(shot, (edge.bottom..edge.bottom + gap).map(|y| (x, y)), pane)
    });
    let sides = edge.top + edge.radius..edge.bottom - edge.radius;
    let left = sides.clone().find_map(|y| {
        let pane = *shot.get_pixel(edge.left - gap - 1, y);
        off(shot, (edge.left - gap..edge.left).map(|x| (x, y)), pane)
    });
    let right = sides.clone().find_map(|y| {
        let pane = *shot.get_pixel(edge.right + gap, y);
        off(shot, (edge.right..edge.right + gap).map(|x| (x, y)), pane)
    });
    let found = under
        .map(|found| ("under", found))
        .or(left.map(|found| ("left of", found)))
        .or(right.map(|found| ("right of", found)));
    match found {
        None => Ok(()),
        Some((side, (x, y, pixel, pane))) => {
            Err(format!("a glow {side} the box: {pixel:?} at {x},{y}, the pane {pane:?}").into())
        }
    }
}

/// The bottom border is the sides' colour; focused, the brand line runs just inside it from corner to corner, and
/// stays inside the corners' curve.
fn bottom_edge(shot: &RgbImage, edge: &Edge, gap: u32) -> SandboxResult {
    let side = *shot.get_pixel(edge.left, u32::midpoint(edge.top, edge.bottom));
    let border_row = edge.bottom - edge.border;
    let line_rows = border_row - edge.line..border_row;
    for x in edge.left + edge.radius..edge.right - edge.radius {
        let border = *shot.get_pixel(x, border_row);
        if apart(border, side) > 12 {
            return Err(format!("the bottom border {border:?} at {x},{border_row}, the sides' {side:?}").into());
        }
        for y in line_rows.clone() {
            let pixel = *shot.get_pixel(x, y);
            let lit = apart(pixel, edge.brand) <= 30;
            if lit != edge.focused {
                let wanted = if edge.focused { "the brand's" } else { "not the brand's" };
                return Err(format!("the focus line {pixel:?} at {x},{y}, {wanted} {:?}", edge.brand).into());
            }
        }
        let over = *shot.get_pixel(x, line_rows.start - 1);
        if apart(over, edge.brand) <= 30 {
            return Err(format!(
                "the focus line is thicker than {} px: {over:?} at {x},{}",
                edge.line,
                line_rows.start - 1
            )
            .into());
        }
    }
    // just past the corners' curve, in the line's rows: the pane or the corner's own grey edge, never the line's tint
    let y = line_rows.start;
    for (x, beside) in [
        (edge.left + edge.border, edge.left - gap),
        (edge.right - 1 - edge.border, edge.right + gap),
    ] {
        let pixel = *shot.get_pixel(x, y);
        let pane = *shot.get_pixel(beside, y);
        if blue(pixel) > blue(pane) + 12 {
            return Err(format!("{pixel:?} at {x},{y} outside the box's round corner, the pane {pane:?}").into());
        }
    }
    Ok(())
}
