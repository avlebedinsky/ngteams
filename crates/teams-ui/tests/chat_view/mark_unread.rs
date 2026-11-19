//! A message's "Mark as unread", as Teams has it in its more-options menu (`docs/protocol/chat.md`, "Mark as unread"),
//! on GPUI's test context over the fake engine: the entry after a hairline under copy and delete, on the account's own
//! messages too, and on key U; the write names the message; the "New messages" divider moves before it; no read
//! receipt goes while the chat stays shown; a mark the chat had when it opened ends once it is shown, and so does one
//! made here once the chat is shown again. The messages and ids are made up.

use std::sync::Arc;

use gpui::{Entity, Modifiers, VisualTestContext};
use teams_core::Action;
use teams_store::Message;
use teams_ui::ViewTrace;
use teams_ui::chat_view::{ChatView, Item, MessageAction, READ_DEBOUNCE};

use super::common::{LAB_A, LAB_B, Rig};
use super::{AB, MINUTE, NOON, arrive, chat_window, draw, from_menu, message, rig_with};

/// A message from `sender` that carries the `clientmessageid` a receipt and a mark name.
fn sent(id: &str, sender: &str, at: i64) -> Message {
    Message {
        client_message_id: Some(format!("{id}0")),
        ..message(id, sender, &format!("<p>number {id}</p>"), at)
    }
}

fn marks(rig: &Rig) -> Vec<Action> {
    rig.core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter(|action| {
            matches!(
                action,
                Action::MarkUnread { .. } | Action::EndUnreadMark { .. } | Action::MarkRead { .. }
            )
        })
        .cloned()
        .collect()
}

fn receipts(rig: &Rig) -> usize {
    rig.core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter(|action| matches!(action, Action::Read { .. }))
        .count()
}

/// The message the "New messages" divider stands before, if any.
fn divider_before(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| {
        let items = view.read(cx).items();
        let at = items.iter().position(|item| *item == Item::NewMessages)?;
        match items.get(at + 1) {
            Some(Item::Message(message)) => Some(message.id.clone()),
            _ => None,
        }
    })
}

#[test]
fn the_menu_offers_mark_as_unread_after_a_hairline_on_every_message() {
    let rig = rig_with(vec![sent("1", LAB_B, NOON), sent("2", LAB_A, NOON + MINUTE)]);
    let (_view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    for (row, last_above) in [("message-1", "action-copy"), ("message-2", "action-delete")] {
        let bounds = cx.debug_bounds(row).unwrap();
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
        draw(cx);
        let more = cx.debug_bounds("message-more").unwrap();
        cx.simulate_click(more.center(), Modifiers::none());
        draw(cx);
        let unread = cx
            .debug_bounds("action-mark-unread")
            .expect("Mark as unread in the menu");
        let above = cx.debug_bounds(last_above).unwrap();
        assert!(unread.origin.y > above.bottom(), "{row}: under {last_above}");
        cx.simulate_click(more.center(), Modifiers::none());
        draw(cx);
    }
    assert_eq!(MessageAction::MarkUnread.label(), "Mark as unread");
}

#[test]
fn mark_as_unread_names_the_message_moves_the_divider_and_holds_the_receipts() {
    let rig = rig_with(vec![
        sent("1", LAB_B, NOON),
        sent("2", LAB_B, NOON + MINUTE),
        sent("3", LAB_A, NOON + 2 * MINUTE),
    ]);
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    cx.update(|window, _| window.activate_window());
    draw(cx);
    assert_eq!(divider_before(&view, cx), None);

    from_menu(cx, "message-2", "action-mark-unread");
    assert_eq!(
        marks(&rig),
        [Action::MarkUnread {
            conversation: AB.to_owned(),
            message_id: Some("2".to_owned()),
        }]
    );
    assert_eq!(divider_before(&view, cx).as_deref(), Some("2"), "the divider before it");
    // a new message while the chat stays shown: still unread, no receipt
    let before = receipts(&rig);
    arrive(&rig, cx, &sent("4", LAB_B, NOON + 3 * MINUTE));
    cx.executor().advance_clock(READ_DEBOUNCE);
    draw(cx);
    assert_eq!(receipts(&rig), before, "no receipt while it stays marked here");

    // shown again (another pane was over it): opened again, so the mark ends and receipts go again
    rig.core
        .unread_marks
        .lock()
        .unwrap()
        .insert(AB.to_owned(), NOON + MINUTE - 1);
    cx.update(|_, cx| view.update(cx, |view, cx| view.set_shown(false, cx)));
    cx.update(|_, cx| view.update(cx, |view, cx| view.set_shown(true, cx)));
    draw(cx);
    assert_eq!(
        marks(&rig).last(),
        Some(&Action::EndUnreadMark {
            conversation: AB.to_owned(),
        })
    );
    cx.executor().advance_clock(READ_DEBOUNCE);
    draw(cx);
    assert!(receipts(&rig) > before, "receipts go again");
}

#[test]
fn key_u_marks_the_selected_message_unread() {
    let rig = rig_with(vec![sent("1", LAB_B, NOON), sent("2", LAB_B, NOON + MINUTE)]);
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    cx.update(|window, cx| view.read(cx).focus_handle().clone().focus(window, cx));
    cx.update(|_, cx| view.update(cx, |view, cx| view.select(Some("1".to_owned()), cx)));
    draw(cx);
    cx.simulate_keystrokes("u");
    draw(cx);
    assert_eq!(
        marks(&rig),
        [Action::MarkUnread {
            conversation: AB.to_owned(),
            message_id: Some("1".to_owned()),
        }]
    );
}

#[test]
fn a_chat_marked_unread_opens_with_the_divider_at_the_mark_and_ends_it() {
    let rig = rig_with(vec![
        sent("1", LAB_B, NOON),
        sent("2", LAB_A, NOON + MINUTE),
        sent("3", LAB_A, NOON + 2 * MINUTE),
    ]);
    // marked from the account's own message, as the chat list's "Mark as unread" marks a chat whose newest is its own
    rig.core
        .unread_marks
        .lock()
        .unwrap()
        .insert(AB.to_owned(), NOON + MINUTE - 1);
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    assert_eq!(divider_before(&view, cx).as_deref(), Some("2"));
    assert_eq!(
        marks(&rig),
        [Action::EndUnreadMark {
            conversation: AB.to_owned(),
        }],
        "once, on opening"
    );
}

#[test]
fn a_read_only_app_offers_no_mark() {
    let rig = rig_with(vec![sent("1", LAB_B, NOON)]);
    *rig.core.read_only.lock().unwrap() = true;
    rig.core.unread_marks.lock().unwrap().insert(AB.to_owned(), NOON - 1);
    let (_view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let row = cx.debug_bounds("message-1").unwrap();
    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    draw(cx);
    if let Some(more) = cx.debug_bounds("message-more") {
        cx.simulate_click(more.center(), Modifiers::none());
        draw(cx);
    }
    assert!(cx.debug_bounds("action-mark-unread").is_none());
    assert!(marks(&rig).is_empty(), "a read-only app ends no mark");
}
