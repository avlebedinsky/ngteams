//! QA of task 447 (round 1): each tile shows its raised hand with its place in the line of hands, as Teams does, in
//! the app's own call window on the mock. The mock's made-up people (and b, whom the mock plays) raise and lower
//! their hands as their `publishState` and `removeState` do ([`ChatServiceMock::hand_made_up`]); a raises and lowers
//! its own with Raise and with Ctrl+Shift+K. The app tells no positions: a chip is found where a user sees it, Teams'
//! gold at a tile's top left, and its number is compared with the chips of known numbers seen before.
//!
//! - A group call of six (five made-up people, a): hands go up one after another, each tile shows its number in
//!   that order, a's own tile too; the People pane beside them. A hand in the middle goes down: the later numbers
//!   move up. a's hand down and up again from Raise and from the key. a's camera on: its video tile keeps its chip.
//! - Teams Dark, the speaker view, a shared screen with its strip, small windows (shots).
//! - All hands down: no chip. A call of 26 on pages of 9: a hand on a page not shown, then shown.
//! - A Meet now (a meeting of the a-b-c chat) with made-up people: the hands there too, light and dark.
//!
//! [`ChatServiceMock::hand_made_up`]: teams_mock::ChatServiceMock::hand_made_up

use std::collections::BTreeMap;
use std::thread;

use image::RgbImage;
use teams_mock::{ABC_CHAT, LAB_B, TIMED_OUT};
use ui_sandbox::{SandboxResult, wait_until};

use super::pixels::{self, Rect};
use super::{Rig, SCREEN, SETTLE, View, WAIT, i32_of, say};

/// Teams' raised hand's gold in a light theme.
const GOLD_LIGHT: [u8; 3] = [0xea, 0xa3, 0x00];
/// Teams' raised hand's gold in a dark theme.
const GOLD_DARK: [u8; 3] = [0xef, 0xb8, 0x39];
/// A tile's corner where the chip sits holds at least this many gold px (the chip's hand) while the hand is up.
const CHIP_GOLD: usize = 12;
/// Two shots of one number differ by less than this (mean per channel).
const SAME: f64 = 3.0;
/// Two shots of different numbers differ by more than this.
const DIFFERENT: f64 = 5.0;

/// The step (see the module docs).
pub(super) fn qa_hands(rig: &mut Rig<'_>) -> SandboxResult {
    let connected = rig.calls()?.iter().any(|row| row.state == "connected");
    if !connected {
        rig.people.clear();
        rig.sharing = false;
        rig.group_call()?;
    }
    rig.join(5)?;
    rig.show(View::Gallery)?;
    rig.resize((1280, 800))?;
    let raise = rig.locate_raise("qa-hands")?;
    let people_button = people_button(rig, raise)?;
    let mut qa = Hands::read(rig, 6)?;
    turns(rig, &mut qa, raise, people_button)?;
    themes_views_sizes(rig, &mut qa, people_button)?;
    all_down(rig, &mut qa)?;
    off_page(rig)?;
    let leave = rig.leave_button("qa-hands")?;
    rig.leave(leave)?;
    meeting(rig)
}

/// People: left of Raise among the toolbar's white icons.
fn people_button(rig: &mut Rig<'_>, raise: Rect) -> SandboxResult<Rect> {
    let groups = rig.white_groups()?;
    let at = groups
        .iter()
        .position(|group| group.x <= raise.center().0 && raise.center().0 < group.right())
        .ok_or("qa-hands: Raise isn't among the toolbar's icons")?;
    at.checked_sub(1)
        .and_then(|index| groups.get(index).copied())
        .ok_or_else(|| "qa-hands: no People left of Raise".into())
}

/// Hands go up in turn, one goes down, a's own goes down and up from Raise and the key, a's camera goes on.
fn turns(rig: &mut Rig<'_>, qa: &mut Hands, raise: Rect, people_button: Rect) -> SandboxResult {
    let p = rig.people.clone();
    // the tiles in reading order: the five made-up people as they joined, a's own last (b, whom the mock plays, has
    // no tile)
    qa.expect(rig, &[None; 6], "nobody's hand up")?;
    rig.save("qa447-gallery-no-hand")?;
    made_up(rig, &p[2], true)?;
    qa.expect(rig, &[None, None, Some(1), None, None, None], "the first hand up")?;
    made_up(rig, &p[0], true)?;
    qa.expect(rig, &[Some(2), None, Some(1), None, None, None], "the second hand up")?;
    made_up(rig, &p[1], true)?;
    qa.expect(rig, &[Some(2), Some(3), Some(1), None, None, None], "the third hand up")?;
    rig.hand(true)?;
    qa.expect(
        rig,
        &[Some(2), Some(3), Some(1), None, None, Some(4)],
        "a's own hand by the key",
    )?;
    made_up(rig, &p[4], true)?;
    let five = [Some(2), Some(3), Some(1), None, Some(5), Some(4)];
    qa.expect(rig, &five, "the fifth hand")?;
    rig.save("qa447-gallery-5-hands")?;
    pane(rig, people_button, "qa447-people-5-hands")?;
    made_up(rig, &p[0], false)?;
    let four = [None, Some(2), Some(1), None, Some(4), Some(3)];
    qa.expect(rig, &four, "the second hand went down: the later ones move up")?;
    rig.save("qa447-gallery-second-down")?;
    pane(rig, people_button, "qa447-people-second-down")?;
    let down = rig.sent("removeState") + 1;
    rig.click(raise)?;
    rig.wait_sent("removeState", down, "Raise's click")?;
    let without_a = [None, Some(2), Some(1), None, Some(3), None];
    qa.expect(rig, &without_a, "a's own hand down from Raise")?;
    let up = rig.sent("publishState") + 1;
    rig.click(raise)?;
    rig.wait_sent("publishState", up, "Raise's click")?;
    // a's hand goes to the end of the line again
    let back = [None, Some(2), Some(1), None, Some(3), Some(4)];
    qa.expect(rig, &back, "a's own hand up from Raise, at the end of the line")?;
    rig.save("qa447-gallery-own-from-raise")?;
    rig.hand(false)?;
    qa.expect(rig, &without_a, "a's own hand down by the key")?;
    rig.hand(true)?;
    qa.expect(rig, &back, "a's own hand up by the key")?;
    rig.camera(true)?;
    thread::sleep(SETTLE * 2);
    qa.expect(rig, &back, "a's camera on: its video tile")?;
    rig.save("qa447-gallery-own-video")
}

/// Teams Dark, the speaker view, a share's strip, small windows: shots, the chips checked where the gallery shows.
fn themes_views_sizes(rig: &mut Rig<'_>, qa: &mut Hands, people_button: Rect) -> SandboxResult {
    let four = [None, Some(2), Some(1), None, Some(3), Some(4)];
    rig.theme(teams_ui::theme::DARK_KEY)?;
    qa.dark = true;
    qa.expect(rig, &four, "Teams Dark")?;
    rig.save("qa447-gallery-dark")?;
    pane(rig, people_button, "qa447-people-dark")?;
    // View's menu finds Raise by its key, which wants a's hand down at the start: a's hand goes down for the
    // views (three hands stay up) and up again after them, at the end of the line (4) as before. The light share
    // is found left of a's own tile, white with the camera off: the camera goes off first.
    rig.camera(false)?;
    thread::sleep(SETTLE * 2);
    rig.hand(false)?;
    for (dark, theme) in [(true, teams_ui::theme::DARK_KEY), (false, teams_ui::theme::LIGHT_KEY)] {
        rig.theme(theme)?;
        qa.dark = dark;
        let suffix = if dark { "-dark" } else { "" };
        for view in [View::Speaker, View::Share] {
            rig.show(view)?;
            thread::sleep(SETTLE);
            let chips = qa.chips_anywhere(rig)?;
            say(&format!("qa-hands: {view:?}{suffix}: {chips} chips"));
            if chips == 0 {
                rig.save(&format!("qa447-failed-{}{suffix}", view.name()))?;
                return Err(format!("qa-hands: {view:?}{suffix}: no hand's chip shows").into());
            }
            rig.save(&format!("qa447-{}{suffix}", view.name()))?;
            // a's own tile (the floating one in the speaker view, the strip's last beside a share) gets its chip
            rig.hand(true)?;
            thread::sleep(SETTLE);
            let with_own = qa.chips_anywhere(rig)?;
            say(&format!("qa-hands: {view:?}{suffix}, a's hand up: {with_own} chips"));
            if with_own != chips + 1 {
                rig.save(&format!("qa447-failed-{}-own{suffix}", view.name()))?;
                return Err(format!(
                    "qa-hands: {view:?}{suffix}: a's hand up, {with_own} chips, not {}",
                    chips + 1
                )
                .into());
            }
            rig.save(&format!("qa447-{}-own{suffix}", view.name()))?;
            rig.hand(false)?;
        }
        rig.show(View::Gallery)?;
    }
    rig.hand(true)?;
    for size in [(560, 400), (800, 560)] {
        rig.resize(size)?;
        for (dark, theme) in [(false, teams_ui::theme::LIGHT_KEY), (true, teams_ui::theme::DARK_KEY)] {
            rig.theme(theme)?;
            qa.dark = dark;
            let chips = qa.chips_anywhere(rig)?;
            let suffix = if dark { "-dark" } else { "" };
            say(&format!("qa-hands: {size:?}{suffix}: {chips} chips"));
            rig.save(&format!("qa447-gallery-{}x{}{suffix}", size.0, size.1))?;
            if chips == 0 {
                return Err(format!("qa-hands: {size:?}{suffix}: no hand's chip shows").into());
            }
        }
        rig.theme(teams_ui::theme::LIGHT_KEY)?;
        qa.dark = false;
    }
    rig.resize((1280, 800))?;
    qa.expect(rig, &four, "back at 1280×800, the camera off")
}

/// Every hand goes down: no chip, no gold border.
fn all_down(rig: &mut Rig<'_>, qa: &mut Hands) -> SandboxResult {
    let p = rig.people.clone();
    made_up(rig, &p[2], false)?;
    qa.expect(
        rig,
        &[None, Some(1), None, None, Some(2), Some(3)],
        "the first hand down",
    )?;
    made_up(rig, &p[1], false)?;
    made_up(rig, &p[4], false)?;
    qa.expect(rig, &[None, None, None, None, None, Some(1)], "a's hand alone")?;
    rig.hand(false)?;
    qa.expect(rig, &[None; 6], "all hands down")?;
    let shot = rig.shot()?;
    let gold = qa
        .tiles
        .iter()
        .map(|tile| gold_in(&shot, *tile, GOLD_LIGHT))
        .sum::<usize>();
    rig.save("qa447-gallery-all-down")?;
    if gold > 0 {
        return Err(format!("qa-hands: all hands down, yet {gold} gold px on the tiles").into());
    }
    Ok(())
}

/// A call of 26 on pages of 9: a hand up on a page not shown shows on that page.
fn off_page(rig: &mut Rig<'_>) -> SandboxResult {
    rig.join(24)?;
    let others = rig.people.len();
    rig.gallery_size(9, others)?;
    let last = rig.people[others - 1].clone();
    made_up(rig, &last, true)?;
    let first = rig.people[1].clone();
    made_up(rig, &first, true)?;
    thread::sleep(SETTLE);
    let stage = rig.stage()?;
    let pager = stage
        .pager
        .ok_or("qa-hands: a call of 26 on pages of 9 shows no pager")?;
    let chips_on = |rig: &mut Rig<'_>| -> SandboxResult<usize> {
        let stage = rig.stage()?;
        let shot = rig.shot()?;
        Ok(stage
            .tiles
            .iter()
            .filter(|tile| chip_gold(&shot, **tile, GOLD_LIGHT) >= CHIP_GOLD)
            .count())
    };
    let on_first = chips_on(rig)?;
    rig.save("qa447-large-page-1")?;
    let mut counts = vec![on_first];
    let panel = stage.panel;
    loop {
        rig.park()?;
        let before = rig.shot()?;
        rig.click(pager.next)?;
        let after = rig.shot()?;
        if pixels::changed_pixels(&before, &after, panel) < 2_000 || counts.len() >= 8 {
            break;
        }
        let chips = chips_on(rig)?;
        rig.save(&format!("qa447-large-page-{}", counts.len() + 1))?;
        counts.push(chips);
    }
    say(&format!("qa-hands: chips on each page of 26: {counts:?}"));
    if counts.iter().sum::<usize>() != 2 || on_first != 1 {
        return Err(format!("qa-hands: two hands up in a call of 26, the pages show {counts:?} chips").into());
    }
    made_up(rig, &last, false)?;
    made_up(rig, &first, false)?;
    rig.gallery_size(49, others)?;
    rig.join(5)
}

/// b's Meet now in the a-b-c chat, joined from its banner; made-up people join; hands go up, light and dark.
fn meeting(rig: &mut Rig<'_>) -> SandboxResult {
    wait_until(WAIT, || (!rig.sandbox.bus().tray_items().is_empty()).then_some(()))
        .ok_or("the desktop notification connection isn't ready")?;
    let seen = rig.sandbox.bus().notifications().len();
    let call = rig.mock.ring_group(LAB_B, ABC_CHAT, &[]);
    if !rig.mock.end_ring(&call, 0, TIMED_OUT) {
        return Err("no Meet now on the mock".into());
    }
    let conversation = rig
        .mock
        .calls()
        .into_iter()
        .find(|known| known.id == call)
        .map(|known| known.conversation)
        .ok_or("no Meet now's conversation")?;
    post(rig, &meet_now_event(&conversation, &call, true))?;
    let banner = wait_until(WAIT, || {
        rig.sandbox
            .bus()
            .notifications()
            .into_iter()
            .skip(seen)
            .find(|banner| banner.body.contains(" started "))
    })
    .ok_or("no banner for b's Meet now")?;
    let opened = |seen: &[serde_json::Value]| {
        seen.iter()
            .filter(|line| line["view"] == "call-window" && line["change"] == "opened")
            .count()
    };
    let before = opened(rig.app.trace.catch_up());
    rig.window_id = None;
    rig.sandbox.bus().invoke_action(banner.id, "join")?;
    rig.wait_call("connected in the Meet now", |row| row.state == "connected")?;
    rig.app
        .trace
        .wait_until("the call window opened", |seen| opened(seen) > before)?;
    rig.wait_window(|size| size.0 == i32_of(SCREEN.0))?;
    rig.call.clone_from(&call);
    rig.people.clear();
    rig.sharing = false;
    rig.join(3)?;
    rig.resize((1280, 800))?;
    let mut qa = Hands::read(rig, 4)?;
    let raise = rig.locate_raise("the meeting")?;
    let people_button = people_button(rig, raise)?;
    let p = rig.people.clone();
    made_up(rig, &p[1], true)?;
    made_up(rig, &p[2], true)?;
    rig.hand(true)?;
    made_up(rig, &p[0], true)?;
    let hands = [Some(4), Some(1), Some(2), Some(3)];
    qa.expect(rig, &hands, "the meeting's hands")?;
    rig.save("qa447-meeting")?;
    pane(rig, people_button, "qa447-meeting-people")?;
    made_up(rig, &p[1], false)?;
    let moved = [Some(3), None, Some(1), Some(2)];
    qa.expect(rig, &moved, "the meeting's first hand down")?;
    rig.theme(teams_ui::theme::DARK_KEY)?;
    qa.dark = true;
    qa.expect(rig, &moved, "the meeting in Teams Dark")?;
    rig.save("qa447-meeting-dark")?;
    rig.theme(teams_ui::theme::LIGHT_KEY)?;
    if !rig.mock.hang_up_call(&call) {
        return Err("the Meet now didn't end".into());
    }
    post(rig, &meet_now_event(&conversation, &call, false))?;
    Ok(())
}

/// `mri` raises (`up`) or lowers its hand on the mock, and the window gets the time to draw it.
fn made_up(rig: &mut Rig<'_>, mri: &str, up: bool) -> SandboxResult {
    if !rig.mock.hand_made_up(&rig.call, mri, up) {
        return Err(format!("qa-hands: {mri} isn't in the mock's call").into());
    }
    thread::sleep(SETTLE);
    Ok(())
}

/// The People pane opened beside the stage as `name`, then closed.
fn pane(rig: &mut Rig<'_>, people_button: Rect, name: &str) -> SandboxResult {
    rig.click(people_button)?;
    thread::sleep(SETTLE);
    rig.save(name)?;
    rig.click(people_button)?;
    thread::sleep(SETTLE);
    Ok(())
}

fn post(rig: &Rig<'_>, content: &str) -> SandboxResult {
    rig.mock
        .add_message(ABC_CHAT, (LAB_B, "Lab B"), "Event/Call", content)
        .map(|_| ())
        .ok_or_else(|| "no a-b-c chat".into())
}

/// b's Meet now's start (`started`) or end at `conversation`, as the service posts it in the chat.
fn meet_now_event(conversation: &str, call: &str, started: bool) -> String {
    let link = format!(
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/{conversation}?i=10-0-0-2&amp;e=1"
    );
    let kind = "<conversationType>\"huddleGroupCall\"</conversationType>";
    if started {
        format!(
            "<partlist type=\"started\" alt=\"\"><part identity=\"{LAB_B}\"><name>{LAB_B}</name></part></partlist>{kind}\
             <callEventType>callStarted</callEventType><conversationUrl>{link}</conversationUrl><callId>{call}</callId>"
        )
    } else {
        format!(
            "<ended/><partlist alt=\"\" count=\"0\"></partlist>{kind}<callEventType>callEnded</callEventType>\
             <conversationUrl>{link}</conversationUrl><callId>{call}</callId>"
        )
    }
}

/// The gallery's tiles in reading order as a user reads them, and the chips of each number seen so far.
struct Hands {
    tiles: Vec<Rect>,
    dark: bool,
    /// (dark, number): the shot and the tile where that number's chip was first seen.
    seen: BTreeMap<(bool, usize), (RgbImage, Rect)>,
}

impl Hands {
    /// The gallery of `count` tiles, a's own last (its camera off, white).
    fn read(rig: &mut Rig<'_>, count: usize) -> SandboxResult<Self> {
        let stage = rig.stage_with_own()?;
        let tiles = stage.reading_order();
        let own = stage.own.ok_or("qa-hands: no own tile")?;
        if tiles.len() != count || !tiles.last().is_some_and(|last| last.holds(own, 2)) {
            return Err(format!("qa-hands: {count} tiles with a's own last wanted, the stage read {stage:?}").into());
        }
        say(&format!("qa-hands: tiles {tiles:?}"));
        Ok(Self {
            tiles,
            dark: false,
            seen: BTreeMap::new(),
        })
    }

    fn gold(&self) -> [u8; 3] {
        if self.dark { GOLD_DARK } else { GOLD_LIGHT }
    }

    /// Which tiles show a chip in `shot`.
    fn chips(&self, shot: &RgbImage) -> Vec<bool> {
        self.tiles
            .iter()
            .map(|tile| chip_gold(shot, *tile, self.gold()) >= CHIP_GOLD)
            .collect()
    }

    /// How many tiles of the stage as it shows now (any view, Teams Light's stage or the dark one's tiles read
    /// light) show a chip: the gold of the chip's hand in a tile's top left corner.
    fn chips_anywhere(&self, rig: &mut Rig<'_>) -> SandboxResult<usize> {
        rig.park()?;
        let window = rig.window()?;
        let shot = rig.shot()?;
        let gold = self.gold();
        // the hand's gold, in blobs as large as the chip's hand
        let hands = pixels::blobs(window, |x, y| {
            u32::try_from(x)
                .ok()
                .zip(u32::try_from(y).ok())
                .and_then(|(x, y)| shot.get_pixel_checked(x, y))
                .is_some_and(|pixel| near(pixel.0, gold))
        });
        Ok(hands
            .iter()
            .filter(|blob| (8..=20).contains(&blob.bounds.w) && (8..=20).contains(&blob.bounds.h))
            .count())
    }

    /// The tiles show the chips `expected` (each tile's number, in reading order), the numbers as seen before; a
    /// tile with a hand up has the gold border.
    fn expect(&mut self, rig: &mut Rig<'_>, expected: &[Option<usize>], case: &str) -> SandboxResult {
        rig.park()?;
        let want: Vec<bool> = expected.iter().map(Option::is_some).collect();
        let shot = rig.shot_until(|shot| self.chips(shot) == want)?;
        let shown = self.chips(&shot);
        if shown != want {
            rig.save(&format!("qa447-failed-{}", slug(case)))?;
            return Err(format!("qa-hands: {case}: chips on {shown:?}, wanted on {want:?}").into());
        }
        for (tile, number) in self.tiles.clone().into_iter().zip(expected) {
            let Some(number) = *number else {
                continue;
            };
            let edge = Rect { h: 3, ..tile };
            if gold_in(&shot, edge, self.gold()) * 2 < usize::try_from(tile.w).unwrap_or(0) {
                rig.save(&format!("qa447-failed-{}", slug(case)))?;
                return Err(format!("qa-hands: {case}: the tile {tile:?} with a hand up has no gold border").into());
            }
            self.number_is(rig, (&shot, tile), number, case)?;
        }
        say(&format!("qa-hands: {case}: ok {expected:?}"));
        Ok(())
    }

    /// The chip on `tile` in `shot` shows `number`: it matches that number's chip seen before and none other.
    fn number_is(
        &mut self,
        rig: &mut Rig<'_>,
        (shot, tile): (&RgbImage, Rect),
        number: usize,
        case: &str,
    ) -> SandboxResult {
        let differences: Vec<(usize, f64)> = self
            .seen
            .iter()
            .filter(|((dark, _), _)| *dark == self.dark)
            .map(|((_, seen), (other, at))| (*seen, digit_difference((shot, tile), (other, *at))))
            .collect();
        say(&format!(
            "qa-hands: {case}: tile {tile:?} number {number}: differences {differences:?}"
        ));
        let wrong = differences.iter().find(|(seen, difference)| {
            if *seen == number {
                *difference >= SAME
            } else {
                *difference <= DIFFERENT
            }
        });
        if let Some((seen, difference)) = wrong {
            rig.save(&format!("qa447-failed-{}", slug(case)))?;
            return Err(format!(
                "qa-hands: {case}: the chip on {tile:?} should show {number}; against the chip of {seen} it differs by \
                 {difference:.1}"
            )
            .into());
        }
        self.seen
            .entry((self.dark, number))
            .or_insert_with(|| (shot.clone(), tile));
        Ok(())
    }
}

fn slug(case: &str) -> String {
    case.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

fn near(pixel: [u8; 3], gold: [u8; 3]) -> bool {
    pixel.iter().zip(gold).all(|(got, wanted)| got.abs_diff(wanted) <= 24)
}

/// How many px of `gold` lie in `within` of `shot`.
fn gold_in(shot: &RgbImage, within: Rect, gold: [u8; 3]) -> usize {
    let mut count = 0;
    for y in within.y..within.bottom() {
        for x in within.x..within.right() {
            let pixel = u32::try_from(x)
                .ok()
                .zip(u32::try_from(y).ok())
                .and_then(|(x, y)| shot.get_pixel_checked(x, y));
            if pixel.is_some_and(|pixel| near(pixel.0, gold)) {
                count += 1;
            }
        }
    }
    count
}

/// The gold px in `tile`'s top left corner, inside its border: the chip's hand.
fn chip_gold(shot: &RgbImage, tile: Rect, gold: [u8; 3]) -> usize {
    let corner = Rect {
        x: tile.x + 5,
        y: tile.y + 5,
        w: 60.min(tile.w - 10),
        h: 40.min(tile.h - 10),
    };
    gold_in(shot, corner, gold)
}

/// The part of a chip that holds its number: a box round the digit, clear of the pill's round edge (which blends
/// with the tile under it).
fn digit_box(tile: Rect) -> Rect {
    Rect {
        x: tile.x + 18,
        y: tile.y + 19,
        w: 11,
        h: 12,
    }
}

/// How much the numbers of two chips differ: the mean per channel at the best of small shifts.
fn digit_difference((a, a_tile): (&RgbImage, Rect), (b, b_tile): (&RgbImage, Rect)) -> f64 {
    let a_box = digit_box(a_tile);
    let b_box = digit_box(b_tile);
    let at = |image: &RgbImage, x: i32, y: i32| {
        u32::try_from(x)
            .ok()
            .zip(u32::try_from(y).ok())
            .and_then(|(x, y)| image.get_pixel_checked(x, y))
            .map_or([0, 0, 0], |pixel| pixel.0)
    };
    let mut best = f64::MAX;
    for dy in -3..=3 {
        for dx in -3..=3 {
            let mut sum = 0_u64;
            for y in 0..a_box.h {
                for x in 0..a_box.w {
                    let one = at(a, a_box.x + x, a_box.y + y);
                    let other = at(b, b_box.x + x + dx, b_box.y + y + dy);
                    sum += one
                        .iter()
                        .zip(other)
                        .map(|(p, q)| u64::from(p.abs_diff(q)))
                        .sum::<u64>();
                }
            }
            #[expect(clippy::cast_precision_loss, reason = "a few hundred px of three channels")]
            let mean = sum as f64 / f64::from(a_box.w * a_box.h * 3);
            best = best.min(mean);
        }
    }
    best
}
