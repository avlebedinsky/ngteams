//! QA of task 666 (round 1): the call's Chat button and its chat bubble, as a user sees them in the app's own call
//! window on the mock. b (whom the mock plays) writes in the call's chat ([`ChatServiceMock::post_as`]); the account
//! writes its own over the control socket, as `ngteamsctl send` does.
//!
//! - b writes while the Chat pane is closed: Teams' red dot on Chat's icon, and a bubble about 320×70 at the stage's
//!   top middle for about 4 s; the dot stays after the bubble goes.
//! - A click on the bubble's words opens the pane, and the dot goes. With the pane open, b's message gives no dot and
//!   no bubble; the pane closed again shows no dot.
//! - The account's own message gives no dot and no bubble.
//! - The bubble's Close puts it away at once (the dot stays); its "Mute these notifications" stops the next bubbles
//!   (the dot still comes); More → Settings → "Show chat bubbles" brings them back.
//! - Shots in Teams Dark, at a font of 18 px and in a small window.
//! - The step `qa-chat-calls`: the bubble in a 1:1 call (not the dot: the mock's a-b chat counts nothing unread), and
//!   the dot and the bubble in a Meet now.
//!
//! [`ChatServiceMock::post_as`]: teams_mock::ChatServiceMock::post_as

use std::thread;
use std::time::{Duration, Instant};

use image::RgbImage;
use teams_ctl::protocol::{OpenTarget, Request, Response};
use teams_mock::{AB_CHAT, ABC_CHAT, LAB_B, TIMED_OUT};
// how long the bubble shows, as the app has it
use teams_ui::call_stage::BUBBLE_FOR;
use ui_sandbox::SandboxResult;

use super::meet_now as meet;
use super::pixels::{self, Rect};
use super::qa_order::{qa_call, speak};
use super::{Rig, SETTLE, TOOLBAR_BAND, View, say};

/// The width of the call's Chat pane (360 px) and the room around it, at the window's right.
const PANE_BESIDE: i32 = 380;
/// The bubble's size, as Teams draws it.
const BUBBLE: (i32, i32) = (320, 70);

/// The step (see the module docs).
pub(super) fn qa_chat(rig: &mut Rig<'_>) -> SandboxResult {
    qa_call(rig)?;
    speak(rig, &[])?;
    rig.show(View::Gallery)?;
    rig.resize((1280, 800))?;
    let chat = Chat::locate(rig, ABC_CHAT)?;
    let bubble = chat.dot_and_bubble(rig)?;
    chat.bubble_opens_pane(rig, bubble)?;
    chat.own_message(rig)?;
    chat.close_and_mute(rig)?;
    chat.looks(rig)
}

/// The step `qa-chat-calls`: the bubble in a 1:1 call; the dot and the bubble in a Meet now.
pub(super) fn other_calls(rig: &mut Rig<'_>) -> SandboxResult {
    one_to_one(rig)?;
    meet_now(rig)
}

/// One shot of the stage's top: when it started and ended, and whether the bubble showed.
type Sample = (Duration, Duration, bool);

/// The Chat button and the call's chat.
struct Chat {
    button: Rect,
    /// The toolbar's More.
    more: Rect,
    /// The call's chat.
    conversation: &'static str,
    /// The window as it is with no bubble, no pane and no dot.
    calm: RgbImage,
}

impl Chat {
    fn locate(rig: &mut Rig<'_>, chat: &'static str) -> SandboxResult<Self> {
        let bar = rig.toolbar("qa-chat")?;
        rig.park()?;
        thread::sleep(SETTLE);
        let calm = rig.shot()?;
        Ok(Self {
            button: bar.chat,
            more: bar.more,
            conversation: chat,
            calm,
        })
    }

    /// Where Teams' dot sits: Chat's icon and a little room above and on its right.
    fn dot_area(&self) -> Rect {
        Rect {
            x: self.button.x - 2,
            y: self.button.y - 6,
            w: self.button.w + 10,
            h: self.button.h / 2 + 8,
        }
    }

    /// The red dot on Chat, if any: a small blob of Teams' red (Leave's red is far larger).
    fn dot(&self, shot: &RgbImage) -> Option<Rect> {
        let red = |x: i32, y: i32| {
            let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
                return false;
            };
            x < shot.width() && y < shot.height() && {
                let colour = shot.get_pixel(x, y).0;
                colour[0] > 170 && colour[1] < 90 && colour[2] < 110
            }
        };
        pixels::blobs(self.dot_area(), red)
            .into_iter()
            .map(|blob| blob.bounds)
            .find(|dot| (5..=14).contains(&dot.w) && (5..=14).contains(&dot.h))
    }

    /// The call's chat in a call whose toolbar has no Raise to find the buttons by (a 1:1 call): the dot is looked
    /// for along the whole toolbar.
    fn locate_band(rig: &mut Rig<'_>, chat: &'static str) -> SandboxResult<Self> {
        let window = rig.window()?;
        rig.park()?;
        thread::sleep(SETTLE);
        let calm = rig.shot()?;
        let band = Rect {
            x: window.x + window.w / 2,
            y: window.y + 4,
            w: window.w / 2 - 4,
            h: TOOLBAR_BAND - 8,
        };
        Ok(Self {
            button: band,
            more: band,
            conversation: chat,
            calm,
        })
    }

    /// The pane on the window's right while it is open: the right 300 px under the toolbar changed from the calm shot.
    fn pane_open(&self, rig: &Rig<'_>, shot: &RgbImage) -> SandboxResult<bool> {
        let window = rig.window()?;
        let right = Rect {
            x: window.right() - 300,
            y: window.y + TOOLBAR_BAND + 4,
            w: 290,
            h: window.h - TOOLBAR_BAND - 14,
        };
        Ok(pixels::changed_surface_pixels(&self.calm, shot, right) >= 3_000)
    }

    /// The bubble in `shot`, if any: what changed from the calm shot in the band at the stage's top.
    fn bubble(&self, rig: &Rig<'_>, shot: &RgbImage) -> SandboxResult<Option<Rect>> {
        Self::bubble_from(rig, &self.calm, shot, 0)
    }

    /// The bubble in `shot`, if any: what changed from `reference` in the band at the stage's top, left of the
    /// window's right `beside` px (a pane's).
    fn bubble_from(rig: &Rig<'_>, reference: &RgbImage, shot: &RgbImage, beside: i32) -> SandboxResult<Option<Rect>> {
        let window = rig.window()?;
        let band = Rect {
            x: window.x + 4,
            y: window.y + TOOLBAR_BAND,
            w: window.w - 8 - beside,
            h: 130,
        };
        let changed = pixels::changes(reference, shot, band)
            .into_iter()
            .filter(|blob| blob.pixels >= 30)
            .map(|blob| blob.bounds)
            .reduce(Rect::union);
        Ok(changed.filter(|bubble| bubble.w >= 200 && bubble.h >= 40))
    }

    /// Waits for the bubble: where it is, and when it came: after the start of the last shot without it, before the
    /// end of the first with it.
    fn wait_bubble(&self, rig: &mut Rig<'_>, what: &str) -> SandboxResult<(Rect, (Instant, Instant))> {
        let start = Instant::now();
        let mut after = start;
        while start.elapsed() < Duration::from_secs(12) {
            let taken = Instant::now();
            let shot = rig.shot()?;
            if let Some(bubble) = self.bubble(rig, &shot)? {
                return Ok((bubble, (after, Instant::now())));
            }
            after = taken;
        }
        rig.save("failed-qa666-no-bubble")?;
        Err(format!("{what}: no bubble in 12 s").into())
    }

    /// Nothing shows at the stage's top for `time` (no bubble).
    fn no_bubble_for(&self, rig: &mut Rig<'_>, time: Duration, what: &str) -> SandboxResult {
        let start = Instant::now();
        while start.elapsed() < time {
            let shot = rig.shot()?;
            if let Some(bubble) = self.bubble(rig, &shot)? {
                rig.save("failed-qa666-bubble")?;
                return Err(format!("{what}: a bubble showed at {bubble:?}").into());
            }
            thread::sleep(Duration::from_millis(200));
        }
        Ok(())
    }

    /// Waits for the dot to show (`shown`) or go.
    fn wait_dot(&self, rig: &mut Rig<'_>, shown: bool, what: &str) -> SandboxResult<RgbImage> {
        let shot = rig.shot_until(|shot| self.dot(shot).is_some() == shown)?;
        if self.dot(&shot).is_some() != shown {
            rig.save("failed-qa666-dot")?;
            let state = if shown { "no dot came" } else { "the dot stayed" };
            return Err(format!("{what}: {state} on Chat").into());
        }
        Ok(shot)
    }

    fn post(&self, rig: &Rig<'_>, text: &str) -> SandboxResult {
        rig.mock
            .post_as(LAB_B, self.conversation, &format!("<p>{text}</p>"))
            .map(|_| ())
            .ok_or_else(|| format!("b's post to {} was refused", self.conversation).into())
    }

    fn toggle_pane(&self, rig: &mut Rig<'_>, open: bool) -> SandboxResult {
        rig.click(self.button)?;
        let start = Instant::now();
        loop {
            let shot = rig.shot()?;
            if self.pane_open(rig, &shot)? == open {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(8) {
                rig.save("failed-qa666-pane")?;
                return Err(format!("Chat's click: the pane is not {}", if open { "open" } else { "closed" }).into());
            }
        }
    }

    /// Shots of the stage's top from the bubble's coming for 12 s: each shot's start and end (from `came_after`) and
    /// whether the bubble showed; and the first shot that showed it. Taken at once, so it is seen as it goes, and
    /// that it does not come back.
    fn bubble_samples(
        &self,
        rig: &mut Rig<'_>,
        (came_after, seen_by): (Instant, Instant),
    ) -> SandboxResult<(Vec<Sample>, Option<RgbImage>)> {
        let mut samples: Vec<Sample> = Vec::new();
        let mut shown_shot = None;
        while came_after.elapsed() < Duration::from_secs(12) {
            let taken = came_after.elapsed();
            let shot = rig.shot()?;
            let shown = self.bubble(rig, &shot)?.is_some();
            if shown && shown_shot.is_none() {
                shown_shot = Some(shot);
            }
            samples.push((taken, came_after.elapsed(), shown));
        }
        say(&format!(
            "qa666: the bubble seen {:?} after the shot before it; then {}",
            seen_by.saturating_duration_since(came_after),
            samples
                .iter()
                .map(|(from, to, shown)| format!(
                    "{:.1}-{:.1}s:{}",
                    from.as_secs_f32(),
                    to.as_secs_f32(),
                    if *shown { "on" } else { "off" }
                ))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        Ok((samples, shown_shot))
    }

    /// b writes with the pane closed: the dot, and the bubble for about 4 s at the stage's top middle; the bubble's
    /// place, for the clicks after.
    fn dot_and_bubble(&self, rig: &mut Rig<'_>) -> SandboxResult<Rect> {
        if self.dot(&self.calm).is_some() {
            return Err("a dot on Chat before any message".into());
        }
        self.post(
            rig,
            "Lunch at noon? The new place on the corner has a long table for all of us",
        )?;
        let (bubble, (came_after, seen_by)) = self.wait_bubble(rig, "b's message")?;
        let (samples, shown_shot) = self.bubble_samples(rig, (came_after, seen_by))?;
        if let Some(shot) = &shown_shot {
            super::save_shot(rig, shot, "qa666-chat-dot-and-bubble")?;
        }
        let first_off = samples
            .iter()
            .position(|(_, _, shown)| !shown)
            .ok_or("the bubble didn't go in 12 s")?;
        if samples[first_off..].iter().any(|(_, _, shown)| *shown) {
            rig.save("failed-qa666-bubble-back")?;
            return Err("the bubble went and came back for the same message".into());
        }
        let least = first_off
            .checked_sub(1)
            .map_or(Duration::ZERO, |last| samples[last].0)
            .saturating_sub(seen_by.saturating_duration_since(came_after));
        let most = samples[first_off].1;
        say(&format!("qa666: the bubble showed between {least:?} and {most:?}"));
        if least > BUBBLE_FOR + Duration::from_millis(500)
            || most < BUBBLE_FOR.saturating_sub(Duration::from_millis(500))
        {
            return Err(format!("the bubble showed {least:?} to {most:?}: Teams' shows about 4 s").into());
        }
        let shot = self.wait_dot(rig, true, "after the bubble went")?;
        rig.save("qa666-chat-dot-after-bubble")?;
        let dot = self.dot(&shot).ok_or("no dot")?;
        say(&format!(
            "qa666: the bubble at {bubble:?}, the dot at {dot:?}, Chat at {:?}",
            self.button
        ));
        let window = rig.window()?;
        let stage = rig.stage()?;
        let middle = stage.panel.x + stage.panel.w / 2;
        let (w, h) = BUBBLE;
        if (bubble.w - w).abs() > 24 || (bubble.h - h).abs() > 24 {
            return Err(format!("the bubble is {}×{}: Teams' is {w}×{h}", bubble.w, bubble.h).into());
        }
        if (bubble.center().0 - middle).abs() > 12
            || bubble.y - stage.panel.y > 30
            || bubble.y < window.y + TOOLBAR_BAND
        {
            return Err(format!(
                "the bubble at {bubble:?} isn't at the stage's top middle ({:?})",
                stage.panel
            )
            .into());
        }
        // the dot at the icon's top right
        if dot.center().0 < self.button.center().0 || dot.center().1 > self.button.y + self.button.h / 2 {
            return Err(format!("the dot at {dot:?} isn't at Chat's top right ({:?})", self.button).into());
        }
        Ok(bubble)
    }

    /// A click on the bubble's words opens the pane and the dot goes; with the pane open nothing dots or bubbles.
    fn bubble_opens_pane(&self, rig: &mut Rig<'_>, bubble: Rect) -> SandboxResult {
        // the words: right of the avatar, left of the bell and Close. The bubble comes where the first one was; it is
        // clicked soon after the message, a shot or two being slow on a busy machine (a later click may find it gone)
        let words = Rect {
            x: bubble.x + 60,
            y: bubble.y + 10,
            w: 120,
            h: bubble.h - 20,
        };
        let mut opened = false;
        for (attempt, wait) in [1_500_u64, 1_000, 2_200].into_iter().enumerate() {
            self.post(rig, &format!("Are you there? ({})", attempt + 1))?;
            thread::sleep(Duration::from_millis(wait));
            rig.click(words)?;
            let after = rig.shot()?;
            super::save_shot(rig, &after, "qa666-chat-bubble-after-click")?;
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(4) {
                let shot = rig.shot()?;
                if self.pane_open(rig, &shot)? && self.dot(&shot).is_none() {
                    opened = true;
                    break;
                }
            }
            say(&format!(
                "qa666: click {} on the bubble {wait} ms after the post: the pane opened: {opened}",
                attempt + 1
            ));
            if opened {
                break;
            }
            // the message stays unread: the next bubble needs this one gone
            thread::sleep(BUBBLE_FOR);
        }
        if !opened {
            rig.save("failed-qa666-bubble-click")?;
            return Err("three clicks on the bubble didn't open the pane and clear the dot".into());
        }
        rig.save("qa666-chat-pane-from-bubble")?;
        say("qa666: a click on the bubble opened the pane; the dot went");
        // the stage left of the pane before b's next message: a bubble would change its top
        rig.park()?;
        thread::sleep(SETTLE);
        let open = rig.shot()?;
        self.post(rig, "Here is the agenda")?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            let shot = rig.shot()?;
            if self.dot(&shot).is_some() {
                rig.save("failed-qa666-dot-pane-open")?;
                return Err("a dot on Chat while its pane is open".into());
            }
            if let Some(bubble) = Self::bubble_from(rig, &open, &shot, PANE_BESIDE)? {
                rig.save("failed-qa666-bubble-pane-open")?;
                return Err(format!("a bubble at {bubble:?} while the Chat pane is open").into());
            }
            thread::sleep(Duration::from_millis(200));
        }
        rig.save("qa666-chat-pane-open-new-message")?;
        self.toggle_pane(rig, false)?;
        thread::sleep(SETTLE);
        let shot = rig.shot()?;
        if self.dot(&shot).is_some() {
            rig.save("failed-qa666-dot-after-close")?;
            return Err("a dot on Chat after the pane read the messages and closed".into());
        }
        say("qa666: no dot and no bubble with the pane open, no dot after it closed");
        Ok(())
    }

    /// The account's own message: no dot, no bubble.
    fn own_message(&self, rig: &mut Rig<'_>) -> SandboxResult {
        let answer = rig.ctl.request(
            None,
            Request::Send {
                chat: self.conversation.to_owned(),
                text: "On my way".to_owned(),
                wait: true,
            },
        )?;
        match answer {
            Response::Outbox(row) => say(&format!("qa666: the own message went: {:?}", row.state)),
            other => return Err(format!("the own message: {other:?}").into()),
        }
        self.no_bubble_for(rig, Duration::from_secs(5), "the account's own message")?;
        if self.dot(&rig.shot()?).is_some() {
            rig.save("failed-qa666-own-dot")?;
            return Err("the account's own message dotted Chat".into());
        }
        say("qa666: the account's own message gave no dot and no bubble");
        Ok(())
    }

    /// The bubble's icons on its right: Close the rightmost, the bell left of it.
    fn bubble_icons(rig: &mut Rig<'_>, bubble: Rect) -> SandboxResult<(Rect, Rect)> {
        let shot = rig.shot()?;
        let right = Rect {
            x: bubble.right() - 90,
            y: bubble.y + 4,
            w: 86,
            h: bubble.h - 8,
        };
        let mut icons: Vec<Rect> = pixels::blobs(right, |x, y| {
            let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
                return false;
            };
            x < shot.width() && y < shot.height() && {
                let p = shot.get_pixel(x, y).0;
                let light = (u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114) / 1000;
                light < 140
            }
        })
        .into_iter()
        .map(|blob| blob.bounds)
        .filter(|bounds| bounds.w >= 6 && bounds.h >= 6)
        .collect();
        icons.sort_by_key(|icon| icon.x);
        let close = *icons.last().ok_or("no icons on the bubble's right")?;
        let bell = icons
            .iter()
            .rev()
            .find(|icon| icon.right() < close.x - 2)
            .copied()
            .ok_or("no bell left of Close")?;
        Ok((bell, close))
    }

    /// Close puts the bubble away at once; Mute stops the next bubbles (the dot comes); More → Settings brings them back.
    fn close_and_mute(&self, rig: &mut Rig<'_>) -> SandboxResult {
        self.post(rig, "One more thing")?;
        let (bubble, _) = self.wait_bubble(rig, "a message for Close")?;
        let (bell, close) = Self::bubble_icons(rig, bubble)?;
        say(&format!("qa666: the bubble's bell at {bell:?}, Close at {close:?}"));
        let clicked = Instant::now();
        rig.click(close)?;
        let shot = rig.shot()?;
        if self.bubble(rig, &shot)?.is_some() {
            rig.save("failed-qa666-close")?;
            return Err(format!(
                "Close didn't put the bubble away ({:?} after the click)",
                clicked.elapsed()
            )
            .into());
        }
        self.wait_dot(rig, true, "after Close")?;
        say("qa666: Close put the bubble away; the dot stayed");
        // the bell where the last bubble had it, clicked soon after the message (a shot can take seconds on a busy
        // machine); what counts is that the next message brings no bubble
        self.post(rig, "And another one")?;
        thread::sleep(Duration::from_millis(1_500));
        rig.click(bell)?;
        let shot = rig.shot()?;
        if self.bubble(rig, &shot)?.is_some() {
            rig.save("failed-qa666-mute")?;
            return Err("Mute these notifications didn't put the bubble away".into());
        }
        self.post(rig, "Muted, I hope")?;
        self.no_bubble_for(rig, Duration::from_secs(5), "a message after Mute")?;
        self.wait_dot(rig, true, "a message after Mute")?;
        say("qa666: after Mute these notifications no bubble came; the dot did");
        self.bubbles_from_more(rig, "Show chat bubbles")?;
        self.post(rig, "Bubbles back?")?;
        self.wait_bubble(rig, "a message after Show chat bubbles")?;
        say("qa666: More > Settings > Show chat bubbles brought the bubble back");
        self.bubbles_from_more(rig, "Don't show chat bubbles")?;
        self.post(rig, "No bubble now")?;
        self.no_bubble_for(rig, Duration::from_secs(5), "a message after Don't show chat bubbles")?;
        say("qa666: More > Settings > Don't show chat bubbles stopped them");
        self.bubbles_from_more(rig, "Show chat bubbles")?;
        // read them all: the pane open and closed
        self.toggle_pane(rig, true)?;
        self.toggle_pane(rig, false)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// More → Settings → its chat bubbles' entry (`label` names it, for the shots): the More menu opened by a click,
    /// Settings found from the menu's end (Help is last), its submenu's second entry.
    fn bubbles_from_more(&self, rig: &mut Rig<'_>, label: &str) -> SandboxResult {
        rig.click(self.more)?;
        thread::sleep(SETTLE);
        rig.key("up")?;
        rig.key("up")?;
        rig.key("right")?;
        thread::sleep(SETTLE);
        rig.save(&format!(
            "qa666-more-settings-{}",
            label.replace(' ', "-").replace('\'', "").to_lowercase()
        ))?;
        rig.key("down")?;
        rig.key("enter")?;
        thread::sleep(SETTLE);
        rig.key("escape")?;
        rig.park()?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Shots of the dot and the bubble in Teams Dark, at a font of 18 px, in a small window.
    fn looks(&self, rig: &mut Rig<'_>) -> SandboxResult {
        for (name, theme, font, size) in [
            (
                "dark",
                teams_ui::theme::DARK_KEY,
                teams_ui::theme::FONT_SIZE,
                (1280, 800),
            ),
            ("font-18", teams_ui::theme::LIGHT_KEY, 18, (1280, 800)),
            ("560x400-font-18", teams_ui::theme::LIGHT_KEY, 18, (560, 400)),
            (
                "800x560-dark",
                teams_ui::theme::DARK_KEY,
                teams_ui::theme::FONT_SIZE,
                (800, 560),
            ),
        ] {
            rig.theme(theme)?;
            rig.theme(&format!("{}{font}", super::FONT_PREFIX))?;
            rig.resize(size)?;
            rig.park()?;
            thread::sleep(SETTLE);
            self.post(
                rig,
                "A message with a long first line that has to stop at the end of the bubble",
            )?;
            // the bubble in a theme the calm shot was not taken in: wait for the words, then shoot
            thread::sleep(Duration::from_millis(1_800));
            rig.save(&format!("qa666-chat-bubble-{name}"))?;
            thread::sleep(BUBBLE_FOR);
        }
        rig.theme(teams_ui::theme::LIGHT_KEY)?;
        rig.theme(&format!("{}{}", super::FONT_PREFIX, teams_ui::theme::FONT_SIZE))?;
        rig.resize((1280, 800))?;
        self.toggle_pane(rig, true)?;
        self.toggle_pane(rig, false)?;
        Ok(())
    }
}

/// A 1:1 call: b's message bubbles (the dot is not checked here, see the comment in the body).
fn one_to_one(rig: &mut Rig<'_>) -> SandboxResult {
    let leave = rig.leave_button("qa-chat group call")?;
    rig.leave(leave)?;
    rig.people.clear();
    rig.sharing = false;
    // the main window shows the a-b chat (the harness's settings): it would read b's message at once. Another chat
    // there, as a user in a call has whatever chat open.
    rig.ctl.request(
        None,
        Request::Open {
            target: OpenTarget::Chat {
                chat: ABC_CHAT.to_owned(),
            },
        },
    )?;
    thread::sleep(SETTLE);
    let shown = rig.banners_shown();
    let _ = rig.mock.ring(LAB_B, super::LAB_A);
    rig.answer(shown)?;
    rig.resize((1280, 800))?;
    let chat = Chat::locate_band(rig, AB_CHAT)?;
    chat.post(rig, "Can you hear me?")?;
    let (bubble, _) = chat.wait_bubble(rig, "b's message in the 1:1 call")?;
    rig.save("qa666-chat-one-to-one-bubble")?;
    // only the bubble: on this mock the app's store counts b's message in the a-b chat as read (it sends no read
    // receipt for it), so no dot comes (docs/backlog/1to1-call-chat-dot-on-mock.md)
    say(&format!("qa666: in the 1:1 call the bubble at {bubble:?}"));
    rig.save("qa666-chat-one-to-one")?;
    let leave = rig.leave_button("qa-chat 1:1 call")?;
    rig.leave(leave)
}

/// A Meet now (a meeting of the a-b-c chat) that a joins: b's message dots Chat and bubbles.
fn meet_now(rig: &mut Rig<'_>) -> SandboxResult {
    // the main window off the meeting's chat, as in the 1:1 call
    rig.ctl.request(
        None,
        Request::Open {
            target: OpenTarget::Chat {
                chat: AB_CHAT.to_owned(),
            },
        },
    )?;
    thread::sleep(SETTLE);
    let call = rig.mock.ring_group(LAB_B, ABC_CHAT, &[]);
    if !rig.mock.end_ring(&call, 0, TIMED_OUT) {
        return Err("no Meet now on the mock".into());
    }
    let conversation = meet::conversation_of(rig, &call)?;
    meet::post(rig, ABC_CHAT, &meet::start(&conversation, &call, true))?;
    thread::sleep(SETTLE * 2);
    rig.call.clone_from(&call);
    rig.expect_call_window()?;
    rig.ctl.request(
        None,
        Request::Join {
            chat: ABC_CHAT.to_owned(),
        },
    )?;
    rig.wait_call("connected in the Meet now", |row| row.state == "connected")?;
    rig.wait_window(|size| size.0 == super::i32_of(super::SCREEN.0))?;
    rig.park()?;
    thread::sleep(SETTLE * 2);
    rig.resize((1280, 800))?;
    let chat = Chat::locate(rig, ABC_CHAT)?;
    chat.post(rig, "Welcome to the meeting")?;
    let (bubble, _) = chat.wait_bubble(rig, "b's message in the Meet now")?;
    chat.wait_dot(rig, true, "b's message in the Meet now")?;
    say(&format!("qa666: in the Meet now the bubble at {bubble:?} and the dot"));
    rig.save("qa666-chat-meet-now")?;
    let leave = rig.leave_button("qa-chat Meet now")?;
    rig.leave(leave)?;
    if !rig.mock.hang_up_call(&call) {
        say("qa666: the Meet now ended with a's leave");
    }
    meet::post(rig, ABC_CHAT, &meet::end(&conversation, &call, true))
}
