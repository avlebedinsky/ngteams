//! QA of task 579, round 3: the scroll bar of the AI notice's body, at the largest font (24). It shows when the body is
//! cut (a 520 px wide window, a 1280 × 480 window) and not when the body fits; a click on its track and a drag of its
//! thumb scroll the body, the title and the buttons stay; Tab after it still moves the focus inside the notice.

use std::thread;
use std::time::Duration;

use ui_sandbox::input::keys;

use super::{
    Rect, SETTLE, SandboxResult, User, changed_box, changed_in, dialog_of, ensure, notice_shown, primary_button, say,
};

/// The notice's parts on a shot in Teams Light: the dialog, its title, its body and its buttons.
struct Parts {
    dialog: Rect,
    title: Rect,
    body: Rect,
    buttons: Rect,
}

fn parts(shot: &image::RgbImage) -> SandboxResult<Parts> {
    let dialog = dialog_of(shot).ok_or("no dialog")?;
    let keep = primary_button(shot, dialog).ok_or("no primary button")?;
    let title = Rect {
        x: dialog.x + 8,
        y: dialog.y + 8,
        w: dialog.w - 16,
        h: 80,
    };
    let buttons = Rect {
        x: dialog.x + 8,
        y: keep.y.saturating_sub(12),
        w: dialog.w - 16,
        h: keep.h + 20,
    };
    // the body without the scroll bar in the dialog's right padding
    let body = Rect {
        x: dialog.x + 8,
        y: title.y + title.h + 4,
        w: dialog.w.saturating_sub(64),
        h: buttons.y.saturating_sub(title.y + title.h + 8),
    };
    Ok(Parts {
        dialog,
        title,
        body,
        buttons,
    })
}

/// The scroll bar's thumb: grey columns in the dialog's right padding, each a run of at least 40 px.
fn thumb(shot: &image::RgbImage, dialog: Rect) -> Option<Rect> {
    let grey = |x: u32, y: u32| {
        let [r, g, b] = shot.get_pixel(x, y).0;
        r.abs_diff(g) < 14 && g.abs_diff(b) < 14 && (90..215).contains(&r)
    };
    let right = (dialog.x + dialog.w).min(shot.width());
    let mut found: Option<Rect> = None;
    for x in right.saturating_sub(48)..right.saturating_sub(4) {
        let (mut run, mut best) = ((0_u32, 0_u32), (0_u32, 0_u32));
        for y in dialog.y..(dialog.y + dialog.h).min(shot.height()) {
            if grey(x, y) {
                run = if run.1 == 0 { (y, 1) } else { (run.0, run.1 + 1) };
                if run.1 > best.1 {
                    best = run;
                }
            } else {
                run = (0, 0);
            }
        }
        if best.1 >= 40 {
            let column = Rect {
                x,
                y: best.0,
                w: 1,
                h: best.1,
            };
            found = Some(found.map_or(column, |seen| {
                let (x0, y0) = (seen.x.min(column.x), seen.y.min(column.y));
                let x1 = (seen.x + seen.w).max(column.x + 1);
                let y1 = (seen.y + seen.h).max(column.y + column.h);
                Rect {
                    x: x0,
                    y: y0,
                    w: x1 - x0,
                    h: y1 - y0,
                }
            }));
        }
    }
    found
}

/// The notice and its thumb on the screen now, with the pointer parked.
fn now(user: &mut User<'_>) -> SandboxResult<(image::RgbImage, Parts, Option<Rect>)> {
    user.park()?;
    thread::sleep(SETTLE);
    let shot = user.look()?;
    let found = parts(&shot)?;
    let thumb = thumb(&shot, found.dialog);
    Ok((shot, found, thumb))
}

/// Drags the pointer from `from` to `to` with the left button held, in small steps.
fn drag(user: &mut User<'_>, from: (u32, u32), to: (u32, u32)) -> SandboxResult {
    user.input.move_to(from.0, from.1)?;
    thread::sleep(Duration::from_millis(150));
    user.input.button_down(keys::BTN_LEFT)?;
    thread::sleep(Duration::from_millis(150));
    let steps = 12_i64;
    for step in 1..=steps {
        let at = |a: u32, b: u32| {
            let (a, b) = (i64::from(a), i64::from(b));
            u32::try_from(a + (b - a) * step / steps).unwrap_or(0)
        };
        user.input.move_to(at(from.0, to.0), at(from.1, to.1))?;
        thread::sleep(Duration::from_millis(40));
    }
    thread::sleep(Duration::from_millis(150));
    user.input.button_up(keys::BTN_LEFT)?;
    thread::sleep(Duration::from_millis(250));
    Ok(())
}

/// The body scrolled (it changed), and the title and the buttons did not move.
fn scrolled(before: &image::RgbImage, after: &image::RgbImage, at: &Parts, what: &str) -> SandboxResult {
    let moved = changed_in(before, after, at.body);
    say(&format!("  {what}: the body changed in {moved:?}"));
    ensure(moved.is_some(), || format!("{what}: the body does not scroll"))?;
    ensure(changed_in(before, after, at.title).is_none(), || {
        format!("{what}: the title moved")
    })?;
    ensure(changed_in(before, after, at.buttons).is_none(), || {
        format!("{what}: the buttons moved")
    })
}

/// After a pointer action on the scroll bar, a Tab moves the focus ring inside the notice only.
fn tab_stays_inside(user: &mut User<'_>, dialog: Rect, what: &str) -> SandboxResult {
    let changed = user.key_and_change("Tab")?;
    say(&format!("  Tab after {what}: changed {changed:?}"));
    ensure(
        changed.is_some_and(|ring| {
            ring.x >= dialog.x
                && ring.y >= dialog.y
                && ring.x + ring.w <= dialog.x + dialog.w
                && ring.y + ring.h <= dialog.y + dialog.h
        }),
        || format!("Tab after {what} did not move the focus ring inside the notice: {changed:?} (notice {dialog:?})"),
    )
}

/// At 1280 × 800 the body fits at font 24 and at font 15 (at 1280 and 520 px wide): no scroll bar.
pub(super) fn no_scrollbar_when_it_fits(user: &mut User<'_>) -> SandboxResult {
    user.until("the notice", notice_shown)?;
    let mut shown = Vec::new();
    let (_, at, found) = now(user)?;
    say(&format!(
        "  font 24, 1280 x 800: dialog {:?}, thumb {found:?}",
        at.dialog
    ));
    user.shots("r3-no-scrollbar-font-24-1280")?;
    if found.is_some() {
        shown.push(format!("font 24 at 1280 x 800: {found:?}"));
    }
    user.ok("font 15")?;
    thread::sleep(SETTLE);
    for width in [1280, 520] {
        if width != 1280 {
            user.resize(width)?;
        }
        let (_, at, found) = now(user)?;
        say(&format!(
            "  font 15, {width} px: dialog {:?}, thumb {found:?}",
            at.dialog
        ));
        user.shots(&format!("r3-no-scrollbar-font-15-{width}"))?;
        if found.is_some() {
            shown.push(format!("font 15 at {width} px: {found:?}"));
        }
    }
    user.resize(1280)?;
    user.ok("font 24")?;
    thread::sleep(SETTLE);
    ensure(shown.is_empty(), || {
        format!("a scroll bar shows when the body fits: {}", shown.join("; "))
    })
}

/// At 520 px wide: the bar shows; a click on its track low down scrolls down, a drag of the thumb to the top scrolls
/// back to the start; then Tab stays in the notice.
pub(super) fn scrollbar_at_520(user: &mut User<'_>) -> SandboxResult {
    user.resize(520)?;
    let outcome = bar_at_520(user);
    user.input.scroll(270, 300, -40)?;
    user.resize(1280)?;
    outcome
}

fn bar_at_520(user: &mut User<'_>) -> SandboxResult {
    user.until("the notice", notice_shown)?;
    let (start, at, found) = now(user)?;
    user.shots("r3-scrollbar-520")?;
    say(&format!(
        "  520 px: dialog {:?}, body {:?}, thumb {found:?}",
        at.dialog, at.body
    ));
    let thumb_at = found.ok_or("no scroll bar at 520 px wide, font 24, where the body is cut")?;
    ensure(
        thumb_at.x > at.body.x + at.body.w / 2 && thumb_at.y >= at.body.y.saturating_sub(16),
        || format!("the thumb {thumb_at:?} is not right of the body {:?}", at.body),
    )?;
    // the track under the thumb, near its end (where its tint stops; the buttons can wrap above Keep)
    let column = thumb_at.center().0;
    let track_end = (thumb_at.y + thumb_at.h..start.height())
        .find(|&y| start.get_pixel(column, y).0.iter().all(|&channel| channel >= 250))
        .unwrap_or(thumb_at.y + thumb_at.h);
    say(&format!("  the track ends at y {track_end}"));
    ensure(track_end > thumb_at.y + thumb_at.h + 30, || {
        format!("no track under the thumb {thumb_at:?}: it ends at {track_end}")
    })?;
    let click = (column, track_end - 12);
    say(&format!("  a click on the track at {click:?}"));
    user.click(click)?;
    let (clicked, _, moved) = now(user)?;
    user.shots("r3-scrollbar-520-track-click")?;
    say(&format!("  after the track click: thumb {moved:?}"));
    scrolled(&start, &clicked, &at, "a click on the track")?;
    let moved = moved.ok_or("no thumb after the track click")?;
    ensure(moved.y > thumb_at.y, || {
        format!("the thumb did not go down: {thumb_at:?} -> {moved:?}")
    })?;
    // the thumb dragged back to the top: the body as at the start
    let (x, y) = moved.center();
    drag(user, (x, y), (x, at.dialog.y.saturating_sub(40).max(5)))?;
    let (dragged, _, back) = now(user)?;
    user.shots("r3-scrollbar-520-dragged-top")?;
    say(&format!("  after the drag to the top: thumb {back:?}"));
    let off_start = changed_in(&start, &dragged, at.body);
    ensure(off_start.is_none(), || {
        format!("a drag of the thumb to the top did not scroll back to the start: {off_start:?}")
    })?;
    ensure(changed_in(&clicked, &dragged, at.body).is_some(), || {
        "a drag of the thumb did not scroll the body".to_owned()
    })?;
    tab_stays_inside(user, at.dialog, "the drag")
}

/// At 1280 × 480: the bar shows; a drag of the thumb to the end shows the last line; a click high on the track scrolls
/// back to the start; then Tab stays in the notice.
pub(super) fn scrollbar_at_1280x480(user: &mut User<'_>) -> SandboxResult {
    user.sandbox.niri_msg(&["action", "set-window-height", "480"])?;
    let outcome = user
        .until("the window 480 px high", |state| {
            state["height"]
                .as_f64()
                .is_some_and(|drawn| (drawn - 480.0).abs() < 2.0)
        })
        .and_then(|_| bar_at_480(user));
    user.input.scroll(640, 240, -40)?;
    user.sandbox.niri_msg(&["action", "set-window-height", "100%"])?;
    user.until("the window high again", |state| {
        state["height"].as_f64().is_some_and(|drawn| drawn > 700.0)
    })?;
    thread::sleep(SETTLE);
    outcome
}

fn bar_at_480(user: &mut User<'_>) -> SandboxResult {
    thread::sleep(SETTLE);
    let (start, at, found) = now(user)?;
    user.shots("r3-scrollbar-1280x480")?;
    say(&format!(
        "  1280 x 480: dialog {:?}, body {:?}, thumb {found:?}",
        at.dialog, at.body
    ));
    let thumb_at = found.ok_or("no scroll bar at 1280 x 480, font 24, where the body is cut")?;
    let (x, y) = thumb_at.center();
    drag(user, (x, y), (x, 470))?;
    let (dragged, _, end) = now(user)?;
    user.shots("r3-scrollbar-1280x480-dragged-end")?;
    say(&format!("  after the drag to the end: thumb {end:?}"));
    scrolled(&start, &dragged, &at, "a drag of the thumb")?;
    let end = end.ok_or("no thumb after the drag")?;
    ensure(end.y + end.h + 30 >= at.buttons.y, || {
        format!(
            "the thumb is not at the end of the track: {end:?}, buttons at {}",
            at.buttons.y
        )
    })?;
    // the end: one more wheel step down changes nothing
    user.input.scroll(at.body.center().0, at.body.center().1, 3)?;
    let (wheeled, _, _) = now(user)?;
    let more = changed_in(&dragged, &wheeled, at.body);
    say(&format!("  the wheel down at the end: changed {more:?}"));
    ensure(more.is_none(), || {
        format!("the drag to the end did not reach the end of the body: the wheel scrolled more in {more:?}")
    })?;
    // a click high on the track: back to the start
    user.click((end.center().0, thumb_at.y + 2))?;
    let (clicked, _, _) = now(user)?;
    user.shots("r3-scrollbar-1280x480-track-click-top")?;
    let off_start = changed_in(&start, &clicked, at.body);
    say(&format!(
        "  after a click at the track's top: off the start in {off_start:?}"
    ));
    ensure(off_start.is_none(), || {
        format!("a click at the top of the track did not scroll back to the start: {off_start:?}")
    })?;
    let whole = changed_box(&start, &clicked);
    say(&format!("  the screen against the start: {whole:?}"));
    tab_stays_inside(user, at.dialog, "the track click")
}
