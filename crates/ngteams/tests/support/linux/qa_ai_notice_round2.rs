//! QA of task 579, round 2: the AI notice as a modal dialog in the app's window. The first Tab goes to the notice, Tab
//! and Shift+Tab stay in it, Enter and Space answer it on each start of the app, no key and no click reaches the window
//! behind it, the font keys still work, the body scrolls at the largest font, and after the answer the shell's keys,
//! the compose box and the other dialogs work as before.

use std::thread;
use std::time::Instant;

use super::{
    LAB_B, QUIET, Rect, SETTLE, SandboxResult, User, changed_box, changed_in, dialog_of, ensure, notice_shown,
    primary_button, say,
};

/// The notice as it shows before the first key: no focus ring yet.
pub(super) struct Plain {
    shot: image::RgbImage,
    dialog: Rect,
    buttons: Rect,
    keep_x: u32,
}

/// The notice on the screen now, with the pointer parked.
fn plain_notice(user: &mut User<'_>) -> SandboxResult<Plain> {
    user.park()?;
    thread::sleep(SETTLE);
    let shot = user.look()?;
    let dialog = dialog_of(&shot).ok_or("no dialog on the screen")?;
    let keep = primary_button(&shot, dialog).ok_or("no primary button")?;
    let buttons = Rect {
        x: dialog.x,
        y: keep.y.saturating_sub(8),
        w: dialog.w,
        h: keep.h + 16,
    };
    Ok(Plain {
        shot,
        dialog,
        buttons,
        keep_x: keep.x,
    })
}

/// Which button shows the focus ring (`true`: "Keep AI features on"), or `None`.
fn ring_now(user: &mut User<'_>, plain: &Plain) -> SandboxResult<Option<bool>> {
    user.park()?;
    thread::sleep(SETTLE);
    let now = user.look()?;
    Ok(changed_in(&plain.shot, &now, plain.buttons).map(|ring| ring.center().0 >= plain.keep_x))
}

fn button_name(keep: bool) -> &'static str {
    if keep {
        "Keep AI features on"
    } else {
        "Turn AI features off"
    }
}

/// The first change outside `dialog` (with a margin for its shadow) between two shots.
fn changed_outside(before: &image::RgbImage, after: &image::RgbImage, dialog: Rect) -> Option<Rect> {
    let (width, height) = (before.width(), before.height());
    let (x0, y0) = (dialog.x.saturating_sub(12), dialog.y.saturating_sub(12));
    let (x1, y1) = (
        (dialog.x + dialog.w + 12).min(width),
        (dialog.y + dialog.h + 12).min(height),
    );
    let strips = [
        Rect {
            x: 0,
            y: 0,
            w: width,
            h: y0,
        },
        Rect {
            x: 0,
            y: y1,
            w: width,
            h: height - y1,
        },
        Rect {
            x: 0,
            y: y0,
            w: x0,
            h: y1 - y0,
        },
        Rect {
            x: x1,
            y: y0,
            w: width - x1,
            h: y1 - y0,
        },
    ];
    strips
        .into_iter()
        .filter(|strip| strip.w > 0 && strip.h > 0)
        .find_map(|strip| changed_in(before, after, strip))
}

/// The notice still waits with no answer, and nothing behind it opened or moved.
fn still_waits(user: &mut User<'_>, main: &str, what: &str) -> SandboxResult<Option<String>> {
    let state = user.state()?;
    let held = notice_shown(&state)
        && state["ai_features"].is_null()
        && state["main"] == main
        && state["switcher"] == false
        && state["status_menu"] == false;
    Ok((!held).then(|| {
        format!(
            "{what}: notice {} ai_features {} main {} switcher {} status menu {}",
            state["notice"], state["ai_features"], state["main"], state["switcher"], state["status_menu"]
        )
    }))
}

fn main_now(user: &mut User<'_>) -> SandboxResult<String> {
    Ok(user.state()?["main"].as_str().unwrap_or_default().to_owned())
}

// --- the first run's screen, then the notice ---

/// The first run's screen shows first; a click on its Confirm shows the notice, and the first Tab goes to the notice's
/// first button with no click on the notice.
pub(super) fn first_run_then_first_tab(user: &mut User<'_>) -> SandboxResult {
    let state = user.state()?;
    ensure(state["first_run"] == true && state["notice"] == false, || {
        format!("the first run's screen first: {state}")
    })?;
    user.shot("r2-first-run-screen")?;
    let shot = user.look()?;
    let panel = dialog_of(&shot).ok_or("no first run's screen")?;
    let confirm = primary_button(&shot, panel).ok_or("no Confirm button")?;
    user.click(confirm.center())?;
    user.until("the notice after Confirm", notice_shown)?;
    let plain = plain_notice(user)?;
    user.shot("r2-notice-after-confirm")?;
    user.press("Tab")?;
    let ring = ring_now(user, &plain)?;
    user.shot("r2-first-tab")?;
    user.plain = Some(plain);
    say(&format!("  the first Tab: ring on keep {ring:?}"));
    ensure(ring == Some(false), || {
        format!("the first Tab does not put the focus ring on \"Turn AI features off\": {ring:?}")
    })
}

/// Tab and Shift+Tab move the ring between the two buttons and wrap around, never out of the notice.
pub(super) fn tab_and_shift_tab_cycle(user: &mut User<'_>) -> SandboxResult {
    let plain = user.plain.take().ok_or("no plain notice from the check before")?;
    let steps = [
        ("Tab", true),
        ("Tab", false),
        ("Tab", true),
        ("shift+Tab", false),
        ("shift+Tab", true),
        ("shift+Tab", false),
    ];
    let mut wrong = Vec::new();
    for (number, (key, keep)) in steps.into_iter().enumerate() {
        user.press(key)?;
        let ring = ring_now(user, &plain)?;
        say(&format!("  {key}: ring on keep {ring:?}"));
        if number < 2 {
            user.shot(&format!("r2-cycle-{}", number + 1))?;
        }
        if ring != Some(keep) {
            wrong.push(format!(
                "{key} (step {}): ring {ring:?}, not on {}",
                number + 1,
                button_name(keep)
            ));
        }
    }
    user.plain = Some(plain);
    ensure(wrong.is_empty(), || wrong.join("; "))
}

// --- nothing behind the notice reacts ---

/// The shell's keys, the call keys, the rail's keys and some plain keys: none of them reaches the window behind.
const KEYS_BEHIND: [&str; 31] = [
    "ctrl+k",
    "ctrl+n",
    "ctrl+e",
    "ctrl+shift+f",
    "ctrl+alt+g",
    "alt+Left",
    "alt+Right",
    "ctrl+1",
    "ctrl+2",
    "ctrl+4",
    "ctrl+alt+Down",
    "ctrl+alt+Up",
    "ctrl+shift+c",
    "ctrl+shift+j",
    "ctrl+shift+o",
    "ctrl+shift+e",
    "ctrl+shift+k",
    "alt+shift+c",
    "ctrl+,",
    "F6",
    "Escape",
    "Up",
    "Down",
    "Left",
    "Right",
    "Home",
    "End",
    "PageDown",
    "ctrl+Tab",
    "a",
    "t",
];

/// Each key of [`KEYS_BEHIND`]: the screen outside the notice stays the same and the notice still waits; the next Tab
/// still moves the ring in the notice.
pub(super) fn keys_behind_do_nothing(user: &mut User<'_>) -> SandboxResult {
    let plain = user.plain.take().ok_or("no plain notice")?;
    let main = main_now(user)?;
    let mut reacted = Vec::new();
    for key in KEYS_BEHIND {
        let before = user.look()?;
        user.press(key)?;
        thread::sleep(SETTLE);
        let after = user.look()?;
        if let Some(changed) = changed_outside(&before, &after, plain.dialog) {
            user.shot(&format!("r2-key-reacted-{}", key.replace('+', "-")))?;
            reacted.push(format!("{key}: the window behind changed at {changed:?}"));
        }
        if let Some(why) = still_waits(user, &main, key)? {
            reacted.push(why);
        }
    }
    say(&format!(
        "  {} keys pressed, {} reacted",
        KEYS_BEHIND.len(),
        reacted.len()
    ));
    user.shot("r2-after-keys-behind")?;
    let ring = ring_after_tab(user, &plain)?;
    user.plain = Some(plain);
    ensure(ring.is_some(), || {
        "after the keys, Tab moves no ring on the notice".to_owned()
    })?;
    ensure(reacted.is_empty(), || reacted.join("; "))
}

/// Tab, then which button shows the ring.
fn ring_after_tab(user: &mut User<'_>, plain: &Plain) -> SandboxResult<Option<bool>> {
    user.press("Tab")?;
    ring_now(user, plain)
}

/// Points of the window at 1280 × 800 outside the notice: the title bar, the rail, the chat list and the empty pane.
const POINTS_BEHIND: [(&str, (u32, u32)); 19] = [
    ("the apps grid", (37, 23)),
    ("the sidebar toggle", (90, 23)),
    ("Back", (405, 23)),
    ("the search box", (800, 23)),
    ("Settings and more", (1218, 23)),
    ("the own picture", (1255, 23)),
    ("the rail's Activity", (37, 80)),
    ("the rail's Calendar", (37, 190)),
    ("the rail's More apps", (37, 245)),
    ("the rail's account", (37, 660)),
    ("the rail's add account", (37, 713)),
    ("the rail's Settings", (37, 765)),
    ("New chat", (429, 70)),
    ("the channel's row", (200, 160)),
    ("the channel's More options", (433, 160)),
    ("Lab B's row", (200, 255)),
    ("the team's row", (165, 335)),
    ("the pane below the notice", (900, 690)),
    ("the list beside the notice", (250, 600)),
];

/// A click on each point of [`POINTS_BEHIND`], a right click on a chat and the wheel on the list: the screen outside
/// the notice stays the same and the notice still waits and keeps the keyboard.
pub(super) fn pointer_behind_does_nothing(user: &mut User<'_>) -> SandboxResult {
    let plain = user.plain.take().ok_or("no plain notice")?;
    let main = main_now(user)?;
    let mut reacted = Vec::new();
    for (name, at) in POINTS_BEHIND {
        let before = user.look()?;
        user.click(at)?;
        reacted.extend(after_pointer(user, &before, &plain, &main, name)?);
    }
    let before = user.look()?;
    user.input.click(200, 255, ui_sandbox::input::keys::BTN_RIGHT)?;
    reacted.extend(after_pointer(
        user,
        &before,
        &plain,
        &main,
        "a right click on Lab B's row",
    )?);
    let before = user.look()?;
    user.input.scroll(250, 400, 3)?;
    reacted.extend(after_pointer(
        user,
        &before,
        &plain,
        &main,
        "the wheel on the chat list",
    )?);
    say(&format!(
        "  {} pointer actions, {} reacted",
        POINTS_BEHIND.len() + 2,
        reacted.len()
    ));
    user.shot("r2-after-pointer-behind")?;
    let ring = ring_after_tab(user, &plain)?;
    user.plain = Some(plain);
    ensure(ring.is_some(), || {
        "after the clicks, Tab moves no ring on the notice".to_owned()
    })?;
    ensure(reacted.is_empty(), || reacted.join("; "))
}

fn after_pointer(
    user: &mut User<'_>,
    before: &image::RgbImage,
    plain: &Plain,
    main: &str,
    name: &str,
) -> SandboxResult<Vec<String>> {
    user.park()?;
    thread::sleep(SETTLE);
    let after = user.look()?;
    let mut reacted = Vec::new();
    if let Some(changed) = changed_outside(before, &after, plain.dialog) {
        user.shot(&format!("r2-click-reacted-{}", name.replace([' ', '\''], "-")))?;
        reacted.push(format!("{name}: the window behind changed at {changed:?}"));
    }
    if let Some(why) = still_waits(user, main, name)? {
        reacted.push(why);
    }
    Ok(reacted)
}

// --- the font keys ---

/// Ctrl+= twice, Ctrl+- and Ctrl+0 change the font while the notice waits; the notice grows, fits and keeps the
/// keyboard.
pub(super) fn font_keys_while_waiting(user: &mut User<'_>) -> SandboxResult {
    let plain = user.plain.take().ok_or("no plain notice")?;
    let outcome = font_keys_steps(user, &plain);
    user.plain = Some(plain);
    outcome
}

fn font_keys_steps(user: &mut User<'_>, plain: &Plain) -> SandboxResult {
    let base = u64::from(teams_ui::theme::FONT_SIZE);
    user.press("ctrl+=")?;
    user.press("ctrl+=")?;
    user.until("the font two steps larger", |state| state["font"] == base + 2)?;
    user.park()?;
    thread::sleep(SETTLE);
    user.shot("r2-notice-font-keys-larger")?;
    let shot = user.look()?;
    let dialog = dialog_of(&shot).ok_or("no dialog at the larger font")?;
    say(&format!(
        "  font {}: the dialog {dialog:?} (was {:?})",
        base + 2,
        plain.dialog
    ));
    ensure(dialog.w > plain.dialog.w, || {
        format!("the notice did not grow: {dialog:?}")
    })?;
    ensure(dialog.y > 0 && dialog.y + dialog.h < shot.height(), || {
        format!("the notice does not fit at font {}: {dialog:?}", base + 2)
    })?;
    let before = user.look()?;
    user.press("Tab")?;
    user.park()?;
    thread::sleep(SETTLE);
    let after = user.look()?;
    let lower = Rect {
        x: dialog.x,
        y: dialog.y + dialog.h * 3 / 4,
        w: dialog.w,
        h: dialog.h / 4,
    };
    ensure(changed_in(&before, &after, lower).is_some(), || {
        "at the larger font Tab moves no ring on the notice".to_owned()
    })?;
    ensure(changed_outside(&before, &after, dialog).is_none(), || {
        "at the larger font Tab changed the window behind the notice".to_owned()
    })?;
    user.press("ctrl+-")?;
    user.until("the font one step smaller", |state| state["font"] == base + 1)?;
    user.press("ctrl+0")?;
    user.until("the font reset", |state| state["font"] == base)?;
    let state = user.state()?;
    ensure(notice_shown(&state) && state["ai_features"].is_null(), || {
        format!("the font keys answered the notice: {state}")
    })
}

// --- the answer, then the window's keys ---

/// Space on "Turn AI features off" answers it; then Ctrl+K, Ctrl+1, Ctrl+2, the font keys, a click on a chat and
/// typing in its box work as before.
pub(super) fn space_answers_then_keys_work(user: &mut User<'_>) -> SandboxResult {
    let plain = user.plain.take().ok_or("no plain notice")?;
    let mut ring = ring_now(user, &plain)?;
    for _ in 0..2 {
        if ring == Some(false) {
            break;
        }
        ring = ring_after_tab(user, &plain)?;
    }
    ensure(ring == Some(false), || {
        format!("Tab never put the ring on off: {ring:?}")
    })?;
    user.press("space")?;
    let state = answered_within_quiet(user)?;
    ensure(state["ai_features"] == false, || format!("Space on off: {state}"))?;
    user.shot("r2-after-space-off")?;
    let mut broken = Vec::new();
    shell_keys_after(user, &mut broken)?;
    typing_after(user, &mut broken)?;
    ensure(broken.is_empty(), || broken.join("; "))
}

/// Waits [`QUIET`] at most for the notice to go; the state after.
fn answered_within_quiet(user: &mut User<'_>) -> SandboxResult<serde_json::Value> {
    let start = Instant::now();
    loop {
        let state = user.state()?;
        if state["notice"] == false {
            return Ok(state);
        }
        if start.elapsed() > QUIET {
            return Err(format!("the key did not answer the notice in {QUIET:?}: {state}").into());
        }
        thread::sleep(super::POLL);
    }
}

fn shell_keys_after(user: &mut User<'_>, broken: &mut Vec<String>) -> SandboxResult {
    user.press("ctrl+k")?;
    if user.until("the switcher", |state| state["switcher"] == true).is_err() {
        broken.push("Ctrl+K opens no switcher after the answer".to_owned());
    } else {
        user.shot("r2-after-answer-ctrl-k")?;
        user.press("Escape")?;
        if user
            .until("the switcher closed", |state| state["switcher"] == false)
            .is_err()
        {
            broken.push("Escape does not close the switcher".to_owned());
        }
    }
    // the Activity pane takes the list pane's place: the screen tells it, not `main`
    let chats = user.look()?;
    user.press("ctrl+1")?;
    user.park()?;
    thread::sleep(SETTLE);
    let activity = user.look()?;
    user.shot("r2-after-answer-ctrl-1")?;
    user.press("ctrl+2")?;
    user.park()?;
    thread::sleep(SETTLE);
    let back = user.look()?;
    let (to_activity, to_chats) = (changed_box(&chats, &activity), changed_box(&activity, &back));
    say(&format!(
        "  Ctrl+1 changed {to_activity:?}, Ctrl+2 changed {to_chats:?}"
    ));
    if to_activity.is_none() || to_chats.is_none() {
        broken.push(format!(
            "Ctrl+1 and Ctrl+2 after the answer: {to_activity:?}, {to_chats:?}"
        ));
    }
    let base = u64::from(teams_ui::theme::FONT_SIZE);
    user.press("ctrl+=")?;
    if user
        .until("the font larger", |state| state["font"] == base + 1)
        .is_err()
    {
        broken.push("Ctrl+= does not change the font after the answer".to_owned());
    }
    user.press("ctrl+0")?;
    user.until("the font reset", |state| state["font"] == base)?;
    Ok(())
}

/// Lab B's row in the chat list at 1280 × 800.
const LAB_B_ROW: (u32, u32) = (200, 255);
const TYPED: &str = "after the notice";

fn typing_after(user: &mut User<'_>, broken: &mut Vec<String>) -> SandboxResult {
    user.click(LAB_B_ROW)?;
    let opened = user.until("the chat with Lab B and its box focused", |state| {
        state["box"]["focused"] == true
    });
    if let Err(error) = opened {
        broken.push(format!("a click on Lab B's chat: {error}"));
        return Ok(());
    }
    user.type_text(TYPED)?;
    if user
        .until("the typed text", |state| state["box"]["text"] == TYPED)
        .is_err()
    {
        broken.push("the typed text is not in the chat's box".to_owned());
    }
    user.shots("r2-after-answer-typing")?;
    let rows = user.state()?["rows"].clone();
    ensure(
        rows.as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["sender"] == LAB_B)),
        || format!("the chat with Lab B does not show b's messages: {rows}"),
    )
}

// --- another dialog ---

/// The Create team dialog still keeps Tab and Shift+Tab in itself: the window behind it never changes.
pub(super) fn other_dialog_keeps_tab(user: &mut User<'_>) -> SandboxResult {
    user.ok("open-create-team")?;
    user.park()?;
    thread::sleep(SETTLE);
    let base = user.look()?;
    let dialog = dialog_of(&base).ok_or("no Create team dialog on the screen")?;
    say(&format!("  the Create team dialog: {dialog:?}"));
    ensure(dialog.w + 100 < base.width(), || {
        format!("no dialog over a dimmed window: {dialog:?}")
    })?;
    user.shot("r2-create-team")?;
    let mut escaped = Vec::new();
    let mut moved = 0;
    let keys = ["Tab"; 12].into_iter().chain(["shift+Tab"; 4]);
    for (number, key) in keys.enumerate() {
        let before = user.look()?;
        user.press(key)?;
        user.park()?;
        thread::sleep(SETTLE);
        let after = user.look()?;
        if changed_in(&before, &after, dialog).is_some() {
            moved += 1;
        }
        if let Some(changed) = changed_outside(&before, &after, dialog) {
            escaped.push(format!(
                "{key} {}: the window behind changed at {changed:?}",
                number + 1
            ));
        }
        if number == 2 {
            user.shot("r2-create-team-tab-3")?;
        }
    }
    say(&format!(
        "  16 keys: the dialog changed {moved} times, the window behind {} times",
        escaped.len()
    ));
    user.press("Escape")?;
    ensure(moved >= 8, || {
        format!("Tab moved nothing in the dialog: {moved} changes")
    })?;
    ensure(escaped.is_empty(), || escaped.join("; "))
}

// --- Enter and Space on each start ---

/// Waits for the notice, Tabs to the button, presses `key` once: the answer is that button's, nothing behind moved.
fn answer_with(user: &mut User<'_>, key: &str, keep: bool) -> SandboxResult {
    user.until("the notice", notice_shown)?;
    let plain = plain_notice(user)?;
    let main = main_now(user)?;
    let first = ring_after_tab(user, &plain)?;
    ensure(first == Some(false), || {
        format!("the first Tab: ring on keep {first:?}")
    })?;
    if keep {
        let second = ring_after_tab(user, &plain)?;
        ensure(second == Some(true), || {
            format!("the second Tab: ring on keep {second:?}")
        })?;
    }
    user.press(key)?;
    let state = answered_within_quiet(user)?;
    user.park()?;
    thread::sleep(SETTLE);
    user.shot(&format!("{}-after-{key}", user.run))?;
    say(&format!(
        "  {key} on {}: ai_features {} main {}",
        button_name(keep),
        state["ai_features"],
        state["main"]
    ));
    ensure(state["ai_features"] == keep, || {
        format!("{key} on {}: {state}", button_name(keep))
    })?;
    ensure(
        state["main"] == main.as_str() && state["switcher"] == false && state["status_menu"] == false,
        || format!("{key} also reached the window behind: {state}"),
    )?;
    let file = user.settings_text();
    ensure(file.contains(&format!("ai_features = {keep}")), || file)
}

pub(super) fn enter_turns_off(user: &mut User<'_>) -> SandboxResult {
    answer_with(user, "Return", false)
}

pub(super) fn enter_keeps_on(user: &mut User<'_>) -> SandboxResult {
    answer_with(user, "Return", true)
}

pub(super) fn space_turns_off(user: &mut User<'_>) -> SandboxResult {
    answer_with(user, "space", false)
}

pub(super) fn space_keeps_on(user: &mut User<'_>) -> SandboxResult {
    answer_with(user, "space", true)
}

// --- the body scrolls ---

/// At font 24 in a 520 px wide window the points do not fit: the wheel scrolls them, the title and the buttons stay.
pub(super) fn body_scrolls_at_the_largest_font(user: &mut User<'_>) -> SandboxResult {
    user.resize(520)?;
    let outcome = body_scrolls(user);
    user.input.scroll(270, 300, -40)?;
    user.resize(1280)?;
    outcome
}

fn body_scrolls(user: &mut User<'_>) -> SandboxResult {
    user.park()?;
    thread::sleep(SETTLE);
    let before = user.look()?;
    let dialog = dialog_of(&before).ok_or("no dialog")?;
    let keep = primary_button(&before, dialog).ok_or("no primary button")?;
    let title = Rect {
        x: dialog.x + 8,
        y: dialog.y + 8,
        w: dialog.w - 16,
        h: 80,
    };
    let buttons = Rect {
        x: dialog.x + 8,
        y: keep.y.saturating_sub(72),
        w: dialog.w - 16,
        h: keep.h + 80,
    };
    let body = Rect {
        x: dialog.x + 8,
        y: title.y + title.h + 8,
        w: dialog.w - 16,
        h: buttons.y.saturating_sub(title.y + title.h + 24),
    };
    let (x, y) = body.center();
    user.input.scroll(x, y, 5)?;
    user.park()?;
    thread::sleep(SETTLE);
    let after = user.look()?;
    let moved = changed_in(&before, &after, body);
    say(&format!("  the body {body:?}: moved {moved:?}"));
    user.shots("r2-notice-font-24-520-scrolled")?;
    user.input.scroll(x, y, 40)?;
    user.shots("r2-notice-font-24-520-scrolled-end")?;
    ensure(moved.is_some(), || {
        "the wheel does not scroll the notice's points".to_owned()
    })?;
    ensure(changed_in(&before, &after, title).is_none(), || {
        "the title moved with the points".to_owned()
    })?;
    ensure(changed_in(&before, &after, buttons).is_none(), || {
        "the buttons moved with the points".to_owned()
    })
}

/// A window 480 px high at font 24: the whole notice and its buttons are inside it.
pub(super) fn notice_fits_a_low_window(user: &mut User<'_>) -> SandboxResult {
    user.sandbox.niri_msg(&["action", "set-window-height", "480"])?;
    let outcome = user
        .until("the window 480 px high", |state| {
            state["height"]
                .as_f64()
                .is_some_and(|drawn| (drawn - 480.0).abs() < 2.0)
        })
        .and_then(|_| low_window_fits(user));
    user.sandbox.niri_msg(&["action", "set-window-height", "100%"])?;
    user.until("the window high again", |state| {
        state["height"].as_f64().is_some_and(|drawn| drawn > 700.0)
    })?;
    thread::sleep(SETTLE);
    outcome
}

fn low_window_fits(user: &mut User<'_>) -> SandboxResult {
    thread::sleep(SETTLE);
    user.shots("r2-notice-font-24-1280x480")?;
    let shot = user.look()?;
    let dialog = dialog_of(&shot).ok_or("no dialog")?;
    let keep = primary_button(&shot, dialog);
    say(&format!("  1280 x 480: the dialog {dialog:?}, Keep {keep:?}"));
    ensure(dialog.y > 0 && dialog.y + dialog.h + 8 <= 480, || {
        format!("the dialog is not inside the 480 px high window: {dialog:?}")
    })?;
    ensure(
        keep.is_some_and(|keep| keep.y + keep.h + 8 < dialog.y + dialog.h),
        || format!("the Keep button is not inside the dialog: {keep:?} in {dialog:?}"),
    )?;
    Ok(())
}
