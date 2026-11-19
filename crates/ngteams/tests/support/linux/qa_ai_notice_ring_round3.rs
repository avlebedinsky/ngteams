//! Round 3 of task 579's ring checks: the banner of a call that rings shows over the AI notice and takes Answer and
//! Decline clicks; the strip of a call in progress stays behind the notice; Tab stays in the notice after the calls.
//! After the notice, the call bar looks and works as before: a ring, a call in progress (Leave), a ring
//! while another dialog (Create team) is open.
//!
//! `QA579_COMPARE_WITH` names the evidence directory of another run (for example the code before the fix, whose call
//! bar is the call bar of main): each round-3 shot is compared with the shot of the same name there, in the call
//! bar's band, and the count of pixels that differ is said.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use teams_mock::{ChatServiceMock, LAB_B};
use ui_sandbox::SandboxResult;
use ui_sandbox::input::keys;

use super::{Callee, SETTLE, WAIT, answer_button, end_left, ensure, evidence_dir, in_call_with, is, ring, say};

const COMPARE_ENV: &str = "QA579_COMPARE_WITH";

/// The call bar's band at 1280 × 800: the banner, then the strip of a call in progress under it.
const BAR_BAND: (u32, u32, u32, u32) = (76, 44, 1276, 160);
/// The banner's buttons, right of the banner's strip below the title bar.
const BANNER_BUTTONS: (u32, u32, u32, u32) = (980, 50, 1270, 94);
/// The buttons of the strip of a call in progress, under the title bar.
const STRIP_BUTTONS: (u32, u32, u32, u32) = (900, 48, 1272, 160);
/// How long the parent watches for something that must not happen.
const QUIET: Duration = Duration::from_secs(4);

/// The centre of the first red fill from the top (Teams' danger red: Decline, Leave) in `region`; `dimmed` when it is
/// behind the notice's backdrop.
fn red_button(shot: &image::RgbImage, region: (u32, u32, u32, u32), dimmed: bool) -> Option<(u32, u32)> {
    let (left, top, right, bottom) = region;
    let red = |[r, g, b]: [u8; 3]| {
        let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
        if dimmed {
            r > g + 25 && r > b + 15
        } else {
            r > 150 && r > g + 70 && r > b + 50
        }
    };
    // the band of rows that hold red, then its columns: the first button from the top
    let mut rows = (top..bottom.min(shot.height())).filter(|&y| {
        (left..right.min(shot.width()))
            .filter(|&x| red(shot.get_pixel(x, y).0))
            .count()
            > 20
    });
    let first = rows.next()?;
    let last = rows
        .take_while(|&y| y > first)
        .fold(first, |end, y| if y == end + 1 { y } else { end });
    let y = first.midpoint(last);
    let xs: Vec<u32> = (left..right.min(shot.width()))
        .filter(|&x| red(shot.get_pixel(x, y).0))
        .collect();
    let (x0, x1) = (*xs.first()?, *xs.last()?);
    (x1 - x0 > 30 && last - first > 10).then(|| (x0.midpoint(x1), y))
}

/// Compares `name` with the shot of the same name in [`COMPARE_ENV`]'s directory, in the call bar's band.
fn compare(name: &str) {
    let (Some(other), Some(here)) = (std::env::var_os(COMPARE_ENV).map(PathBuf::from), evidence_dir()) else {
        return;
    };
    let file = format!("qa579-{name}.png");
    let (Ok(a), Ok(b)) = (image::open(here.join(&file)), image::open(other.join(&file))) else {
        say(&format!("  compare {file}: no shot in one of the runs"));
        return;
    };
    let (a, b) = (a.to_rgb8(), b.to_rgb8());
    let (left, top, right, bottom) = BAR_BAND;
    let mut differ = 0_u32;
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for y in top..bottom.min(a.height()).min(b.height()) {
        for x in left..right.min(a.width()).min(b.width()) {
            let (p, q) = (a.get_pixel(x, y).0, b.get_pixel(x, y).0);
            if p.iter().zip(q).any(|(p, q)| p.abs_diff(q) > 8) {
                differ += 1;
                bounds = Some(bounds.map_or((x, y, x, y), |(x0, y0, x1, y1)| {
                    (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                }));
            }
        }
    }
    let line = format!("compare {file}: {differ} pixels differ in the call bar's band, in {bounds:?}");
    say(&format!("  {line}"));
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(here.join("compare.txt"))
        .and_then(|mut out| std::io::Write::write_all(&mut out, format!("{line}\n").as_bytes()));
}

/// Shots of the screen in Teams Light and Teams Dark, each compared; back in Light after.
fn shots(b: &mut Callee<'_>, name: &str) -> SandboxResult {
    b.park()?;
    thread::sleep(SETTLE);
    b.shot(&format!("{name}-light"))?;
    compare(&format!("{name}-light"));
    b.ask("theme-dark")?;
    thread::sleep(SETTLE);
    b.shot(&format!("{name}-dark"))?;
    compare(&format!("{name}-dark"));
    b.ask("theme-light")?;
    thread::sleep(SETTLE);
    Ok(())
}

/// The windows as niri sees them: id, title, focused.
fn windows(b: &Callee<'_>) -> SandboxResult<Vec<(u64, String, bool)>> {
    let text = b.sandbox.niri_msg(&["-j", "windows"])?;
    let list: serde_json::Value = serde_json::from_str(&text)?;
    Ok(list
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|window| {
                    Some((
                        window["id"].as_u64()?,
                        window["title"].as_str().unwrap_or_default().to_owned(),
                        window["is_focused"].as_bool().unwrap_or(false),
                    ))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// The main window in front, over the call window: niri's focus on the oldest window, tried a few times.
fn main_window_in_front(b: &mut Callee<'_>) -> SandboxResult {
    let before = windows(b)?;
    let first = before.iter().map(|(id, _, _)| *id).min().ok_or("no windows")?;
    say(&format!("  the windows before: {before:?}"));
    for attempt in 1..=3 {
        let mark = b.trace.mark();
        b.sandbox
            .niri_msg(&["action", "focus-window", "--id", &first.to_string()])?;
        thread::sleep(SETTLE);
        b.trace.mark();
        let lines: Vec<String> = b.trace.seen[mark..]
            .iter()
            .map(|line| format!("{} {}", line["view"], line["change"]))
            .collect();
        let now = windows(b)?;
        say(&format!(
            "  focus-window {first}, try {attempt}: {now:?}; trace: {}",
            lines.join("; ")
        ));
        if now.iter().any(|(id, _, focused)| *id == first && *focused) {
            return Ok(());
        }
    }
    Err(format!("niri does not keep the main window ({first}) in front").into())
}

/// Whether `by` sent `op` for the call `id`.
fn sent(mock: &ChatServiceMock, id: &str, op: &str, by: &str) -> bool {
    mock.calls()
        .iter()
        .any(|call| call.id == id && call.requests.iter().any(|(sent, who)| sent == op && who == by))
}

/// A click on the banner's red Decline while the notice waits: the call is declined, the notice still waits.
pub(super) fn banner_decline_click_while_the_notice_waits(b: &mut Callee<'_>, mock: &ChatServiceMock) -> SandboxResult {
    b.notice_waits("before the ring")?;
    let (id, mark) = ring(b, mock)?;
    let at = red_button(&b.look()?, BANNER_BUTTONS, false);
    say(&format!("  the banner's Decline at {at:?}"));
    let Some((x, y)) = at else {
        end_left(b, mock, &id);
        return Err("no red Decline button on the banner while the notice waits".into());
    };
    b.input.click(x, y, keys::BTN_LEFT)?;
    b.park()?;
    let hidden = b
        .trace
        .within(mark, "the banner hidden", Duration::from_secs(8), |line| {
            is(line, "call-banner", "hidden")
        });
    thread::sleep(SETTLE);
    b.shot("r3-ring-declined-by-click-under-notice")?;
    let rejected = sent(mock, &id, "reject", LAB_B);
    say(&format!(
        "  a click on the banner's Decline: banner hidden {}, b's reject sent {rejected}",
        hidden.is_ok()
    ));
    end_left(b, mock, &id);
    ensure(hidden.is_ok() && rejected, || {
        "a click on the banner's Decline while the notice waits does not decline the call".to_owned()
    })?;
    b.notice_waits("after the decline by a click")
}

/// A call answered while the notice waits: in the main window its strip stays behind the notice's backdrop, and a
/// click on its Leave does nothing. (A second call does not ring during a call: the engine takes one call at a time.)
pub(super) fn call_in_progress_stays_behind_the_notice(b: &mut Callee<'_>, mock: &ChatServiceMock) -> SandboxResult {
    let (id, mark) = ring(b, mock)?;
    b.press("ctrl+shift+s")?;
    if let Err(error) = b.trace.since(mark, "in-call connected", in_call_with("connected")) {
        end_left(b, mock, &id);
        return Err(format!("Ctrl+Shift+S did not answer: {error}").into());
    }
    let outcome = behind_the_notice_while_up(b);
    let mark = b.trace.mark();
    b.press("ctrl+shift+h")?;
    let _ = b.trace.since(mark, "in-call ended", in_call_with("ended"));
    end_left(b, mock, &id);
    let _ = b.trace.within(mark, "the call window closed", WAIT, |line| {
        is(line, "call-window", "closed")
    });
    thread::sleep(SETTLE);
    outcome?;
    b.notice_waits("after the call in progress")
}

fn behind_the_notice_while_up(b: &mut Callee<'_>) -> SandboxResult {
    main_window_in_front(b)?;
    shots(b, "r3-in-call-under-notice")?;
    b.notice_waits("during the call")?;
    let leave = red_button(&b.look()?, STRIP_BUTTONS, true);
    say(&format!("  the strip's Leave (dimmed) at {leave:?}"));
    let (x, y) = leave.ok_or("no Leave on the call strip behind the notice")?;
    let mark = b.trace.mark();
    b.input.click(x, y, keys::BTN_LEFT)?;
    b.park()?;
    let ended = b.trace.within(mark, "in-call ended", QUIET, in_call_with("ended"));
    say(&format!(
        "  a click on Leave behind the notice: the call ended {}",
        ended.is_ok()
    ));
    ensure(ended.is_err(), || {
        "a click on the strip's Leave behind the notice ended the call".to_owned()
    })?;
    b.notice_waits("after a click on Leave behind it")
}

/// The notice's dialog in Teams Light: the only bright surface over the dimmed window.
fn dialog_of(shot: &image::RgbImage) -> Option<(u32, u32, u32, u32)> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..shot.height() {
        for x in 0..shot.width() {
            let [r, g, b] = shot.get_pixel(x, y).0;
            if r > 235 && g > 235 && b > 235 {
                found = Some(found.map_or((x, y, x, y), |(x0, y0, x1, y1)| {
                    (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                }));
            }
        }
    }
    found
}

/// After the calls, Tab, Tab, Shift+Tab, Shift+Tab move the focus ring only inside the notice; the focus ends on
/// "Keep AI features on", so the next Tab puts it on "Turn AI features off" again.
pub(super) fn tab_stays_in_the_notice_after_calls(b: &mut Callee<'_>, _mock: &ChatServiceMock) -> SandboxResult {
    main_window_in_front(b)?;
    b.park()?;
    thread::sleep(SETTLE);
    let mut before = b.look()?;
    let (x0, y0, x1, y1) = dialog_of(&before).ok_or("no notice on the screen")?;
    say(&format!("  the notice at {x0},{y0} .. {x1},{y1}"));
    let mut outside = Vec::new();
    for key in ["Tab", "Tab", "shift+Tab", "shift+Tab"] {
        b.press(key)?;
        b.park()?;
        thread::sleep(SETTLE);
        let after = b.look()?;
        let mut changed: Option<(u32, u32, u32, u32)> = None;
        for y in 0..after.height().min(before.height()) {
            for x in 0..after.width().min(before.width()) {
                if after.get_pixel(x, y) != before.get_pixel(x, y) {
                    changed =
                        Some(changed.map_or((x, y, x, y), |(a, b, c, d)| (a.min(x), b.min(y), c.max(x), d.max(y))));
                }
            }
        }
        say(&format!("  {key}: changed {changed:?}"));
        let inside = changed.is_some_and(|(a, c, e, d)| a >= x0 && c >= y0 && e <= x1 && d <= y1);
        if !inside {
            outside.push(format!("{key}: {changed:?}"));
        }
        before = after;
    }
    b.shot("r3-tab-after-calls")?;
    ensure(outside.is_empty(), || {
        format!(
            "a Tab after the calls changed nothing or something outside the notice: {}",
            outside.join("; ")
        )
    })?;
    b.notice_waits("after the Tabs")
}

/// After the notice: the banner looks as before (compared), and a click on its Decline declines the call.
pub(super) fn banner_after_the_notice(b: &mut Callee<'_>, mock: &ChatServiceMock) -> SandboxResult {
    let (id, mark) = ring(b, mock)?;
    shots(b, "r3-ring-after-notice")?;
    let at = red_button(&b.look()?, BANNER_BUTTONS, false);
    let Some((x, y)) = at else {
        end_left(b, mock, &id);
        return Err("no red Decline on the banner".into());
    };
    b.input.click(x, y, keys::BTN_LEFT)?;
    b.park()?;
    let hidden = b
        .trace
        .within(mark, "the banner hidden", Duration::from_secs(8), |line| {
            is(line, "call-banner", "hidden")
        });
    thread::sleep(SETTLE);
    let rejected = sent(mock, &id, "reject", LAB_B);
    end_left(b, mock, &id);
    say(&format!(
        "  Decline after the notice: hidden {}, reject {rejected}",
        hidden.is_ok()
    ));
    ensure(hidden.is_ok() && rejected, || {
        "a click on the banner's Decline after the notice does not decline".to_owned()
    })
}

/// After the notice, a call in progress: its strip in the main window (compared), and a click on its Leave ends the
/// call.
pub(super) fn in_call_strip_after_the_notice(b: &mut Callee<'_>, mock: &ChatServiceMock) -> SandboxResult {
    let (id, mark) = ring(b, mock)?;
    b.press("ctrl+shift+s")?;
    if let Err(error) = b.trace.since(mark, "in-call connected", in_call_with("connected")) {
        end_left(b, mock, &id);
        return Err(format!("Ctrl+Shift+S did not answer: {error}").into());
    }
    let outcome = strip_after_the_notice(b);
    end_left(b, mock, &id);
    let _ = b.trace.within(mark, "the call window closed", WAIT, |line| {
        is(line, "call-window", "closed")
    });
    thread::sleep(SETTLE);
    outcome
}

fn strip_after_the_notice(b: &mut Callee<'_>) -> SandboxResult {
    main_window_in_front(b)?;
    shots(b, "r3-in-call-after-notice")?;
    let mark = b.trace.mark();
    main_window_in_front(b)?;
    b.park()?;
    thread::sleep(SETTLE);
    let leave = red_button(&b.look()?, STRIP_BUTTONS, false);
    say(&format!("  the strip's Leave at {leave:?}"));
    let (x, y) = leave.ok_or("no Leave on the call strip")?;
    b.input.click(x, y, keys::BTN_LEFT)?;
    b.park()?;
    let ended = b.trace.within(mark, "in-call ended", WAIT, in_call_with("ended"));
    thread::sleep(SETTLE);
    b.shot("r3-in-call-left-by-click")?;
    ended
        .map(|_| ())
        .map_err(|error| format!("a click on the strip's Leave did not end the call: {error}").into())
}

/// After the notice, the Create team dialog open: a call rings. The banner shows (compared), and what a click on its
/// Answer does is said; the call ends, the dialog closes.
pub(super) fn ring_with_another_dialog_open(b: &mut Callee<'_>, mock: &ChatServiceMock) -> SandboxResult {
    let opened = b.ask("open-create-team")?;
    ensure(opened == "ok", || format!("open-create-team: {opened}"))?;
    thread::sleep(SETTLE);
    let (id, mark) = ring(b, mock)?;
    shots(b, "r3-ring-over-create-team")?;
    let at = answer_button(&b.look()?);
    say(&format!("  the banner's Answer at {at:?} while Create team is open"));
    let answered = if let Some((x, y)) = at {
        b.input.click(x, y, keys::BTN_LEFT)?;
        b.park()?;
        b.trace
            .within(
                mark,
                "in-call connected",
                Duration::from_secs(8),
                in_call_with("connected"),
            )
            .is_ok()
    } else {
        false
    };
    thread::sleep(SETTLE);
    b.shot("r3-ring-over-create-team-after-click")?;
    let line = format!("a click on the banner's Answer while Create team is open: answered {answered}");
    say(&format!("  {line}"));
    if let Some(dir) = evidence_dir() {
        let _ = std::fs::write(dir.join("r3-create-team-click.txt"), format!("{line}\n"));
    }
    let end = b.trace.mark();
    if answered {
        b.press("ctrl+shift+h")?;
        let _ = b.trace.since(end, "in-call ended", in_call_with("ended"));
    }
    end_left(b, mock, &id);
    let _ = b
        .trace
        .within(end, "the call window closed", Duration::from_secs(10), |line| {
            is(line, "call-window", "closed")
        });
    main_window_in_front(b)?;
    b.press("Escape")?;
    thread::sleep(SETTLE);
    b.shot("r3-create-team-closed")?;
    Ok(())
}
