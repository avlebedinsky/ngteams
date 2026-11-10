//! New message (Ctrl+N, the list header's New message), on the fake core in a window of its own: the To: line and its
//! suggestions, the chat the people already are, the group name, and the first message that makes a chat through the
//! outbox. Synthetic accounts and chats only.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

pub mod common;
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;

use common::{LAB_A, LAB_B, Rig};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_core::{Action, ChatListing};
use teams_store::{Conversation, ConversationKind, Member, Message, OutboxKind, OutboxState};
use teams_ui::ViewTrace;
use teams_ui::new_chat::{NAMED_NEEDS_TWO, NOT_STARTED, NewChat, Suggested};
use teams_ui::settings::Settings;
use teams_ui::shell::{Main, Shell};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const AC: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
/// The a–b–c chat everyone wrote in.
const TALK: &str = "19:0000000000000000000000000000talk@thread.v2";
/// A newer a–b–c chat holding only a call (the service calls it empty): the one New message finds for b and c.
const CALL: &str = "19:0000000000000000000000000000call@thread.v2";
/// The chat the first message makes.
const MADE: &str = "19:new-synthetic-group@thread.v2";
/// A named a–b–c chat, the newest of all: never the one New message finds for b and c.
const NAMED: &str = "19:000000000000000000000000000named@thread.v2";
/// 2026-01-01T12:00:00Z, ms.
const NOON: i64 = 1_767_268_800_000;
const MINUTE: i64 = 60_000;

fn chat(id: &str, kind: ConversationKind, last: i64) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: None,
        version: 1,
        last_activity: Some(last),
    }
}

fn message(conversation: &str, id: &str, sender: &str, kind: &str, content: &str, at: i64) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: kind.to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: at,
        compose_time: Some(at),
        edit_time: None,
        delete_time: None,
    }
}

/// a's store: the a–b and a–c 1:1 chats, the a–b–c chat with text and, listed after it, the call-only a–b–c chat.
fn seeded() -> Rig {
    let rig = Rig::start();
    let core = &rig.core;
    core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, NOON + 3 * MINUTE),
        chat(AC, ConversationKind::OneToOne, NOON + 2 * MINUTE),
        chat(TALK, ConversationKind::Group, NOON + 4 * MINUTE),
        chat(CALL, ConversationKind::Group, NOON + MINUTE),
        Conversation {
            topic: Some("Lab trio".to_owned()),
            ..chat(NAMED, ConversationKind::Group, NOON + 5 * MINUTE)
        },
    ]);
    let mut messages = core.messages.lock().unwrap();
    messages.insert(
        AB.to_owned(),
        vec![message(AB, "1", LAB_B, "RichText/Html", "<p>hi a</p>", NOON)],
    );
    messages.insert(
        AC.to_owned(),
        vec![message(AC, "2", LAB_C, "RichText/Html", "<p>hi</p>", NOON)],
    );
    messages.insert(
        TALK.to_owned(),
        vec![message(TALK, "3", LAB_C, "RichText/Html", "<p>all three</p>", NOON)],
    );
    messages.insert(
        CALL.to_owned(),
        vec![message(CALL, "4", LAB_A, "Event/Call", "<ended/>", NOON + MINUTE)],
    );
    drop(messages);
    let mut stored = core.members.lock().unwrap();
    stored.insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    stored.insert(AC.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_C, None)]);
    for group in [TALK, CALL, NAMED] {
        stored.insert(
            group.to_owned(),
            [LAB_A, LAB_B, LAB_C].iter().map(|mri| Member::new(mri, None)).collect(),
        );
    }
    drop(stored);
    *core.listing.lock().unwrap() = ChatListing {
        favorites: None,
        empty: vec![CALL.to_owned()],
    };
    let mut names = core.names.lock().unwrap();
    names.insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    names.insert(LAB_C.to_owned(), "Lab Sea".to_owned());
    drop(names);
    rig.cx.update(teams_ui::window::init);
    rig
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

fn shell_window(rig: &Rig) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.simulate_resize(size(px(1000.0), px(700.0)));
    draw(cx);
    (shell, cx)
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let drawn = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(drawn.center(), Modifiers::none());
    draw(cx);
}

fn new_chat(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<NewChat> {
    cx.update(|_, cx| shell.read(cx).new_chat().cloned())
        .expect("New message is open")
}

/// Types `query` in the To: box and takes the first suggestion.
fn add(cx: &mut VisualTestContext, query: &str) {
    cx.simulate_input(query);
    draw(cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
}

fn main(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Main {
    cx.update(|_, cx| shell.read(cx).main().clone())
}

fn created(state: OutboxState) -> teams_store::OutboxEntry {
    let mut row = common::outbox_row(41, OutboxKind::CreateChat, state);
    row.message_id = Some(MADE.to_owned());
    row
}

#[test]
fn ctrl_n_opens_new_message_with_its_row_and_offers_the_latest_chats_people() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    cx.simulate_keystrokes("ctrl-n");
    draw(cx);
    assert_eq!(main(&shell, cx), Main::NewChat);
    assert!(
        cx.debug_bounds("chat-row-new-message").is_some(),
        "its row heads the chats"
    );
    assert!(
        cx.debug_bounds("new-chat-empty").is_some(),
        "no one yet: a new conversation"
    );
    let view = new_chat(&shell, cx);
    // with nothing typed: the people of the latest 1:1 chats, the latest first
    let offered: Vec<String> = cx.update(|_, cx| {
        view.read(cx)
            .suggestions()
            .iter()
            .filter_map(|suggested| match suggested {
                Suggested::Person { mri, .. } => Some(mri.clone()),
                Suggested::Chat(_) => None,
            })
            .collect()
    });
    assert_eq!(offered, [LAB_B, LAB_C]);
    assert!(cx.debug_bounds("new-chat-suggestions").is_some());
    // the header's button opens it too; another chat opened takes the row away
    click(cx, "chat-row-0");
    assert!(cx.debug_bounds("chat-row-new-message").is_none());
    click(cx, "new-message");
    assert_eq!(main(&shell, cx), Main::NewChat);
    assert!(rig.core.actions.lock().unwrap().is_empty(), "opening it writes nothing");
}

#[test]
fn the_people_find_their_chat_newest_roster_first_and_a_group_name_makes_it_new() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    cx.simulate_keystrokes("ctrl-n");
    draw(cx);
    let view = new_chat(&shell, cx);
    add(cx, "Bee");
    assert_eq!(cx.update(|_, cx| view.read(cx).people().len()), 1);
    // one person: their 1:1 chat, shown under the line
    assert_eq!(
        cx.update(|_, cx| view.read(cx).found().map(str::to_owned)),
        Some(AB.to_owned())
    );
    assert!(cx.debug_bounds("new-chat-empty").is_none());
    add(cx, "Sea");
    // b and c: the a–b–c chat whose roster came last, the call-only one, not the one with the latest message
    assert_eq!(
        cx.update(|_, cx| view.read(cx).found().map(str::to_owned)),
        Some(CALL.to_owned())
    );
    // a group name: always a new chat
    click(cx, "new-chat-group-name-toggle");
    cx.simulate_input("Lab trio");
    draw(cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).found().map(str::to_owned)), None);
    assert!(cx.debug_bounds("new-chat-empty").is_some());
    // Backspace in the empty To: box takes the last person off; the chip's remove the other
    click(cx, "new-chat-chip-remove-1");
    let left: Vec<String> = cx.update(|_, cx| view.read(cx).people().into_iter().map(str::to_owned).collect());
    assert_eq!(left, [LAB_B]);
    assert!(rig.core.actions.lock().unwrap().is_empty(), "finding writes nothing");
}

#[test]
fn the_first_message_makes_the_named_group_chat_then_sends_to_it_and_opens_it() {
    let rig = seeded();
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .push_back(created(OutboxState::Sent));
    let (shell, cx) = shell_window(&rig);
    cx.simulate_keystrokes("ctrl-n");
    draw(cx);
    let view = new_chat(&shell, cx);
    add(cx, "Bee");
    add(cx, "Sea");
    click(cx, "new-chat-group-name-toggle");
    cx.simulate_input("Lab trio");
    draw(cx);
    let compose = cx.update(|_, cx| view.read(cx).compose(cx));
    compose.update_in(cx, |compose, window, cx| compose.focus(window, cx));
    cx.simulate_input("hello all");
    cx.simulate_keystrokes("enter");
    draw(cx);
    let actions = rig.core.actions.lock().unwrap().clone();
    let [Action::CreateChat { others, topic }, Action::Send { conversation, .. }] = actions.as_slice() else {
        panic!("a chat made, then the message sent: {actions:?}");
    };
    let others: Vec<&str> = others.iter().map(teams_proto::Mri::as_str).collect();
    assert_eq!(others, [LAB_B, LAB_C]);
    assert_eq!(topic.as_deref(), Some("Lab trio"));
    assert_eq!(conversation, MADE);
    assert_eq!(main(&shell, cx), Main::Chat(MADE.to_owned()));
}

#[test]
fn a_chat_that_could_not_be_made_gives_the_message_back_and_a_named_one_needs_two_people() {
    let rig = seeded();
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .push_back(created(OutboxState::Failed));
    let (shell, cx) = shell_window(&rig);
    cx.simulate_keystrokes("ctrl-n");
    draw(cx);
    let view = new_chat(&shell, cx);
    add(cx, "Bee");
    click(cx, "new-chat-group-name-toggle");
    cx.simulate_input("Pair");
    draw(cx);
    let compose = cx.update(|_, cx| view.read(cx).compose(cx));
    compose.update_in(cx, |compose, window, cx| compose.focus(window, cx));
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    draw(cx);
    // one other person and a name: nothing goes out, the message stays
    assert!(rig.core.actions.lock().unwrap().is_empty());
    assert_eq!(
        cx.update(|_, cx| compose.read(cx).problem().cloned()),
        Some(NAMED_NEEDS_TWO.into())
    );
    assert_eq!(cx.update(|_, cx| compose.read(cx).value(cx)).trim(), "hi");
    // a second person: the chat is asked for, fails, and the message comes back
    let to = cx.debug_bounds("new-chat-to-input").expect("the To: box");
    cx.simulate_click(to.center(), Modifiers::none());
    add(cx, "Sea");
    compose.update_in(cx, |compose, window, cx| compose.focus(window, cx));
    cx.simulate_keystrokes("enter");
    draw(cx);
    let actions = rig.core.actions.lock().unwrap().clone();
    assert!(matches!(actions.as_slice(), [Action::CreateChat { .. }]), "{actions:?}");
    assert_eq!(
        cx.update(|_, cx| compose.read(cx).problem().cloned()),
        Some(NOT_STARTED.into())
    );
    assert_eq!(cx.update(|_, cx| compose.read(cx).value(cx)).trim(), "hi");
    assert_eq!(main(&shell, cx), Main::NewChat);
}

#[test]
fn a_call_only_group_chat_is_found_by_to_opened_and_then_listed_after_the_others() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    let listed = |cx: &mut VisualTestContext| -> Vec<String> {
        cx.update(|_, cx| {
            shell
                .read(cx)
                .list()
                .read(cx)
                .rows()
                .iter()
                .map(|row| row.id.clone())
                .collect()
        })
    };
    assert!(
        !listed(cx).contains(&CALL.to_owned()),
        "the list leaves the call-only chat out"
    );
    cx.simulate_keystrokes("ctrl-n");
    draw(cx);
    let view = new_chat(&shell, cx);
    cx.simulate_input("Sea");
    draw(cx);
    let chats: Vec<String> = cx.update(|_, cx| {
        view.read(cx)
            .suggestions()
            .iter()
            .filter_map(|suggested| match suggested {
                Suggested::Chat(row) => Some(row.id.clone()),
                Suggested::Person { .. } => None,
            })
            .collect()
    });
    assert!(
        chats.contains(&CALL.to_owned()) && chats.contains(&TALK.to_owned()),
        "{chats:?}"
    );
    let at = cx.update(|_, cx| {
        view.read(cx)
            .suggestions()
            .iter()
            .position(|suggested| matches!(suggested, Suggested::Chat(row) if row.id == CALL))
    });
    view.update_in(cx, |view, window, cx| view.choose(at.unwrap(), window, cx));
    draw(cx);
    assert_eq!(main(&shell, cx), Main::Chat(CALL.to_owned()));
    assert_eq!(
        listed(cx).last(),
        Some(&CALL.to_owned()),
        "opened: listed after the others"
    );
    // and it stays listed once another chat is opened
    click(cx, "chat-row-0");
    assert!(listed(cx).contains(&CALL.to_owned()));
}

/// New message with b and c named "Lab trio", its first message sent while the outbox still holds the new chat's row
/// (`settled` is what the outbox says of it next); then another chat opened before that row settles.
fn first_message_then_away(
    rig: &Rig,
    settled: OutboxState,
) -> (Entity<Shell>, Entity<NewChat>, &'static mut VisualTestContext) {
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .push_back(created(OutboxState::Pending));
    rig.core.outbox_answers.lock().unwrap().push_back(created(settled));
    let (shell, cx) = shell_window(rig);
    cx.simulate_keystrokes("ctrl-n");
    draw(cx);
    let view = new_chat(&shell, cx);
    add(cx, "Bee");
    add(cx, "Sea");
    click(cx, "new-chat-group-name-toggle");
    cx.simulate_input("Lab trio");
    draw(cx);
    let compose = cx.update(|_, cx| view.read(cx).compose(cx));
    compose.update_in(cx, |compose, window, cx| compose.focus(window, cx));
    cx.simulate_input("hello all");
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(cx.update(|_, cx| view.read(cx).is_starting()));
    assert!(
        cx.update(|_, cx| compose.read(cx).editor().read(cx).is_empty()),
        "the editor was cleared"
    );
    assert!(
        cx.update(|_, cx| shell.read(cx).has_unsent(cx)),
        "the pending first message holds a restart"
    );
    click(cx, "chat-row-0");
    assert!(matches!(main(&shell, cx), Main::Chat(_)));
    // the outbox settles the row
    rig.cx.executor().advance_clock(std::time::Duration::from_secs(1));
    draw(cx);
    (shell, view, cx)
}

#[test]
fn a_first_message_still_goes_when_new_message_is_left_and_its_chat_opens_over_nothing() {
    let rig = seeded();
    let (shell, view, cx) = first_message_then_away(&rig, OutboxState::Sent);
    let went = main(&shell, cx);
    let actions = rig.core.actions.lock().unwrap().clone();
    let [Action::CreateChat { .. }, Action::Send { conversation, .. }] = actions.as_slice() else {
        panic!("the chat made, then the message sent: {actions:?}");
    };
    assert_eq!(conversation, MADE);
    assert_eq!(
        main(&shell, cx),
        went,
        "the chat made isn't opened over the place shown"
    );
    assert!(!cx.update(|_, cx| view.read(cx).is_starting()));
    // New message opened again is a new one
    cx.simulate_keystrokes("ctrl-n");
    draw(cx);
    assert_ne!(new_chat(&shell, cx).entity_id(), view.entity_id());
}

#[test]
fn a_first_message_given_back_while_new_message_was_left_is_there_when_it_opens_again() {
    let rig = seeded();
    let (shell, view, cx) = first_message_then_away(&rig, OutboxState::Failed);
    let actions = rig.core.actions.lock().unwrap().clone();
    assert!(matches!(actions.as_slice(), [Action::CreateChat { .. }]), "{actions:?}");
    assert!(matches!(main(&shell, cx), Main::Chat(_)));
    cx.simulate_keystrokes("ctrl-n");
    draw(cx);
    assert_eq!(new_chat(&shell, cx).entity_id(), view.entity_id());
    let compose = cx.update(|_, cx| view.read(cx).compose(cx));
    assert_eq!(cx.update(|_, cx| compose.read(cx).value(cx)).trim(), "hello all");
    assert_eq!(
        cx.update(|_, cx| compose.read(cx).problem().cloned()),
        Some(NOT_STARTED.into())
    );
}

/// A made-up picture, `width` by `height`, as PNG bytes.
fn png(width: u32, height: u32) -> Vec<u8> {
    let picture = image::RgbaImage::from_pixel(width, height, image::Rgba([40, 120, 200, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

#[test]
fn a_picture_pasted_for_one_chat_stays_behind_when_the_people_find_another() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    cx.simulate_keystrokes("ctrl-n");
    draw(cx);
    let view = new_chat(&shell, cx);
    add(cx, "Bee");
    let first = cx.update(|_, cx| view.read(cx).compose(cx));
    first.update_in(cx, |compose, window, cx| compose.focus(window, cx));
    cx.simulate_input("Look");
    let pasted = gpui::ClipboardEntry::Image(gpui::Image::from_bytes(gpui::ImageFormat::Png, png(30, 30)));
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem { entries: vec![pasted] }));
    cx.simulate_keystrokes("ctrl-v");
    draw(cx);
    assert!(
        cx.debug_bounds("editor-picture-1").is_some(),
        "pasted in the a–b chat's box"
    );
    let to = cx.debug_bounds("new-chat-to-input").expect("the To: box");
    cx.simulate_click(to.center(), Modifiers::none());
    add(cx, "Sea");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).found().map(str::to_owned)),
        Some(CALL.to_owned())
    );
    // the text goes on to the a–b–c chat's box; the picture, uploaded for the a–b chat alone, doesn't
    let compose = cx.update(|_, cx| view.read(cx).compose(cx));
    assert_eq!(cx.update(|_, cx| compose.read(cx).value(cx)).trim(), "Look");
    assert!(cx.debug_bounds("editor-picture-1").is_none());
    compose.update_in(cx, |compose, window, cx| compose.focus(window, cx));
    cx.simulate_keystrokes("enter");
    draw(cx);
    let actions = rig.core.actions.lock().unwrap().clone();
    let [Action::Send { conversation, text }] = actions.as_slice() else {
        panic!("the message sent, nothing held back: {actions:?}");
    };
    assert_eq!(conversation, CALL);
    assert!(text.pictures().is_empty());
}

#[test]
fn the_read_only_mode_offers_no_new_message() {
    let rig = seeded();
    *rig.core.read_only.lock().unwrap() = true;
    let (shell, cx) = shell_window(&rig);
    assert!(cx.debug_bounds("new-message").is_none());
    cx.simulate_keystrokes("ctrl-n");
    draw(cx);
    assert_ne!(main(&shell, cx), Main::NewChat);
}
