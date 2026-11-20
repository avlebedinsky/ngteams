//! The call toolbar's Chat carries Teams' red unread dot while its pane is closed and the call's chat has unread
//! messages, and each new message of someone else's shows in Teams' chat bubble at the stage's top for a few seconds:
//! in a group call and a 1:1 call alike; neither while the pane shows the chat, the dot gone once the pane read it,
//! neither for the account's own messages. Every id and name is made up.

use std::sync::Arc;

use gpui::VisualTestContext;
use teams_core::{CallStatus, Event};
use teams_ui::call_stage::BUBBLE_FOR;

use super::{AB, LAB_A, LAB_B, Rig, TRIO, call_windows, click, draw, drawn, in_group_call, registered, shell_window};
use super::{stage, status, trace};

/// The store counts `count` unread in `chat`, and the engine tells the chat changed.
fn unread(rig: &Rig, chat: &str, count: u64) {
    rig.core.unread.lock().unwrap().insert(chat.to_owned(), count);
    rig.send(Event::ConversationChanged {
        conversation: chat.to_owned(),
    });
    rig.cx.run_until_parked();
}

/// `from` wrote `text` in `chat` (its `n`th message), the store counting `count` unread.
fn wrote(rig: &Rig, chat: &str, (from, n, text): (&str, i64, &str), count: u64) {
    let id = (1_760_000_000_000 + n).to_string();
    let message = super::message(chat, &id, from, &format!("<p>{text}</p>"), 1_760_000_000_000 + n);
    rig.core
        .messages
        .lock()
        .unwrap()
        .entry(chat.to_owned())
        .or_default()
        .push(message);
    unread(rig, chat, count);
}

fn dot(stage: &mut VisualTestContext) -> Option<gpui::Bounds<gpui::Pixels>> {
    draw(stage);
    stage.debug_bounds("call-chat-unread")
}

fn later(rig: &Rig, stage: &mut VisualTestContext, by: std::time::Duration) {
    stage.executor().advance_clock(by);
    rig.cx.run_until_parked();
    draw(stage);
}

#[test]
fn a_new_message_dots_chat_while_its_pane_is_closed_and_opening_it_clears_the_dot() {
    let rig = super::calling();
    let (trace, _) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    assert!(dot(stage).is_none(), "nothing unread: no dot");
    unread(&rig, TRIO, 2);
    let shown = dot(stage).expect("the dot on Chat");
    let chat = stage.debug_bounds("call-chat").unwrap();
    assert!(
        chat.contains(&shown.center()) && shown.center().x > chat.center().x && shown.center().y < chat.center().y,
        "at Chat's icon's top right: {shown:?} on {chat:?}"
    );
    assert!(
        (f32::from(shown.size.width) - 10.0).abs() < 0.5,
        "Teams' 10 px dot: {shown:?}"
    );
    // open: no dot while the pane shows the chat, a message coming meanwhile too
    click(stage, "call-chat");
    assert!(drawn(stage, "call-chat-pane"));
    assert!(dot(stage).is_none());
    unread(&rig, TRIO, 3);
    assert!(dot(stage).is_none(), "the pane shows it");
    // the pane read it: closed again, no dot
    unread(&rig, TRIO, 0);
    click(stage, "call-chat-close");
    assert!(dot(stage).is_none(), "cleared once read");
    // another message with the pane closed: the dot again
    unread(&rig, TRIO, 1);
    assert!(dot(stage).is_some());
}

#[test]
fn a_new_message_shows_in_a_bubble_for_a_few_seconds_and_a_click_on_it_opens_the_chat() {
    let rig = super::calling();
    // what was unread before the call isn't bubbled
    wrote(&rig, TRIO, (LAB_B, 1, "before the call"), 1);
    let (trace, _) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    assert!(dot(stage).is_some());
    assert!(!drawn(stage, "call-chat-bubble"));
    wrote(&rig, TRIO, (LAB_B, 2, "Can you see my screen?"), 2);
    assert!(drawn(stage, "call-chat-bubble"), "the bubble");
    let bubble = stage.debug_bounds("call-chat-bubble").unwrap();
    let area = stage.debug_bounds("call-stage").unwrap();
    assert!(
        (bubble.center().x - area.center().x).abs() < gpui::px(2.0) && bubble.origin.y < area.origin.y + gpui::px(30.0),
        "at the stage's top middle: {bubble:?} in {area:?}"
    );
    assert!(drawn(stage, "call-chat-bubble-mute") && drawn(stage, "call-chat-bubble-close"));
    later(&rig, stage, BUBBLE_FOR);
    assert!(!drawn(stage, "call-chat-bubble"), "gone after a few seconds");
    assert!(dot(stage).is_some(), "the dot stays");
    // a newer one; a click on it opens the chat
    wrote(&rig, TRIO, (LAB_B, 3, "Over here"), 3);
    click(stage, "call-chat-bubble-open");
    assert!(drawn(stage, "call-chat-pane"));
    assert!(!drawn(stage, "call-chat-bubble"));
    // none while the pane is open
    wrote(&rig, TRIO, (LAB_B, 4, "And here"), 4);
    assert!(!drawn(stage, "call-chat-bubble"));
}

#[test]
fn close_puts_the_bubble_away_and_mute_stops_the_bubbles_but_not_the_dot() {
    let rig = super::calling();
    let (trace, _) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    unread(&rig, TRIO, 0);
    wrote(&rig, TRIO, (LAB_B, 1, "one"), 1);
    click(stage, "call-chat-bubble-close");
    assert!(!drawn(stage, "call-chat-bubble"));
    wrote(&rig, TRIO, (LAB_B, 2, "two"), 2);
    assert!(drawn(stage, "call-chat-bubble"));
    click(stage, "call-chat-bubble-mute");
    assert!(!drawn(stage, "call-chat-bubble"));
    wrote(&rig, TRIO, (LAB_B, 3, "three"), 3);
    assert!(!drawn(stage, "call-chat-bubble"), "muted for the call");
    assert!(dot(stage).is_some());
}

#[test]
fn the_account_s_own_message_shows_no_dot_and_no_bubble() {
    let rig = super::calling();
    let (trace, _) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    unread(&rig, TRIO, 0);
    // the store counts nothing unread after the account's own message
    wrote(&rig, TRIO, (LAB_A, 1, "mine"), 0);
    assert!(dot(stage).is_none());
    assert!(!drawn(stage, "call-chat-bubble"));
    // nor bubbles it should a count still rise
    unread(&rig, TRIO, 1);
    assert!(!drawn(stage, "call-chat-bubble"));
}

#[test]
fn a_one_to_one_call_s_chat_is_dotted_too() {
    let rig = super::calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let stage = stage(&rig, &windows);
    unread(&rig, AB, 0);
    wrote(&rig, AB, (LAB_B, 1, "hi"), 1);
    assert!(dot(stage).is_some(), "a 1:1 call's chat");
    assert!(drawn(stage, "call-chat-bubble"));
}

#[test]
fn more_s_settings_turn_the_chat_bubbles_off_and_on_again() {
    let rig = super::calling();
    let (trace, _) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    unread(&rig, TRIO, 0);
    super::health::open_settings(stage);
    click(stage, "call-more-chat-bubbles");
    wrote(&rig, TRIO, (LAB_B, 1, "one"), 1);
    assert!(!drawn(stage, "call-chat-bubble"), "Don't show chat bubbles");
    assert!(dot(stage).is_some(), "the dot still says it");
    super::health::open_settings(stage);
    click(stage, "call-more-chat-bubbles");
    wrote(&rig, TRIO, (LAB_B, 2, "two"), 2);
    assert!(drawn(stage, "call-chat-bubble"), "Show chat bubbles");
}

#[test]
fn older_history_stored_during_the_call_shows_no_bubble() {
    let rig = super::calling();
    wrote(&rig, TRIO, (LAB_B, 50, "before the call"), 1);
    let (trace, _) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    assert!(!drawn(stage, "call-chat-bubble"));
    // a load of older history stores earlier unread messages: the count rises, but nothing new came
    wrote(&rig, TRIO, (LAB_B, 10, "from yesterday"), 2);
    assert!(!drawn(stage, "call-chat-bubble"), "no bubble for older history");
    wrote(&rig, TRIO, (LAB_B, 9, "from the day before"), 3);
    assert!(!drawn(stage, "call-chat-bubble"), "nor for its next page");
    assert!(dot(stage).is_some(), "the dot counts them");
    // a new message still bubbles
    wrote(&rig, TRIO, (LAB_B, 60, "new"), 4);
    assert!(drawn(stage, "call-chat-bubble"));
}

#[test]
fn the_chat_kept_behind_another_pane_does_not_read_its_messages() {
    let rig = super::calling();
    let (trace, _) = trace();
    let (windows, stage) = in_group_call(&rig, &trace);
    let shown = || {
        let handle = rig.cx.read(|cx| windows.read(cx).window()).unwrap();
        rig.cx.read(|cx| {
            let stage = handle
                .read(cx)
                .unwrap()
                .view()
                .clone()
                .downcast::<teams_ui::call_stage::CallStage>()
                .unwrap();
            stage.read(cx).chat_view().map(|chat| chat.read(cx).is_shown())
        })
    };
    click(stage, "call-chat");
    assert_eq!(shown(), Some(true));
    // People takes the Chat pane's place: the chat kept for later is not shown, so it sends no read receipt
    click(stage, "call-roster");
    assert!(!drawn(stage, "call-chat-pane"));
    assert_eq!(shown(), Some(false), "hidden behind People");
    click(stage, "call-chat");
    assert_eq!(shown(), Some(true), "shown again with its pane");
}
