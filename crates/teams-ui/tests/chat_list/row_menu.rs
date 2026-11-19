//! A chat row's "More chat options" menu (`teams_ui::chat_list`): "Mark as unread" on a read chat, "Mark as read" on
//! an unread one, as Teams has them first in its menu; by pointer and by the menu key; none in read-only mode. The
//! chats are the made-up ones of the list's other tests.

use gpui::{Entity, Modifiers, VisualTestContext};
use teams_core::Action;
use teams_ui::chat_list::{MARK_READ, MARK_UNREAD};
use teams_ui::shell::Shell;

use super::common::Rig;
use super::{AB, GROUP, drawn, list_of, seeded, shell_window};

/// A selector made at run time, as GPUI's test lookups take one: kept for the test's run.
fn selector(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// The test selector of the "Chats" row of `id`.
fn row_of(shell: &Entity<Shell>, cx: &mut VisualTestContext, id: &str) -> &'static str {
    let list = list_of(shell, cx);
    let at = cx
        .update(|_, cx| list.read(cx).rows().iter().position(|row| row.id == id))
        .expect("the chat is listed");
    selector(format!("chat-row-{at}"))
}

fn marks(rig: &Rig) -> Vec<Action> {
    rig.core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter(|action| matches!(action, Action::MarkUnread { .. } | Action::MarkRead { .. }))
        .cloned()
        .collect()
}

/// Points at the row `row`, clicks its "..." and then the menu's `entry`.
fn choose(cx: &mut VisualTestContext, row: &'static str, entry: &'static str) {
    let bounds = cx.debug_bounds(row).unwrap_or_else(|| panic!("{row} drawn"));
    cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    cx.run_until_parked();
    let more = selector(format!("{row}-more"));
    assert!(drawn(cx, more), "{more} on hover");
    let more = cx.debug_bounds(more).unwrap();
    cx.simulate_click(more.center(), Modifiers::none());
    assert!(drawn(cx, entry), "{entry} in the menu");
    let entry = cx.debug_bounds(entry).unwrap();
    cx.simulate_click(entry.center(), Modifiers::none());
    cx.run_until_parked();
}

#[test]
fn a_read_chat_is_marked_unread_and_an_unread_one_read_from_its_rows_menu() {
    let rig = seeded();
    rig.core.unread.lock().unwrap().insert(AB.to_owned(), 0);
    rig.core.unread.lock().unwrap().insert(GROUP.to_owned(), 3);
    let (shell, cx) = shell_window(&rig);
    let (read, unread) = (row_of(&shell, cx, AB), row_of(&shell, cx, GROUP));
    choose(cx, read, "chat-row-mark-unread");
    assert_eq!(
        marks(&rig),
        [Action::MarkUnread {
            conversation: AB.to_owned(),
            message_id: None,
        }],
        "from its newest message"
    );
    let list = list_of(&shell, cx);
    assert_eq!(
        cx.update(|_, cx| list.read(cx).row_menu_open().map(str::to_owned)),
        None
    );
    choose(cx, unread, "chat-row-mark-read");
    assert_eq!(
        marks(&rig).last(),
        Some(&Action::MarkRead {
            conversation: GROUP.to_owned(),
        })
    );
    assert_eq!((MARK_READ, MARK_UNREAD), ("Mark as read", "Mark as unread"));
}

#[test]
fn the_menu_key_opens_the_rows_menu() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    let row = row_of(&shell, cx, AB);
    let bounds = cx.debug_bounds(row).unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
    let sidebar = cx.update(|_, cx| shell.read(cx).sidebar().clone());
    cx.update(|window, cx| sidebar.update(cx, |sidebar, cx| sidebar.focus_list(window, cx)));
    cx.run_until_parked();
    cx.simulate_keystrokes("shift-f10");
    cx.run_until_parked();
    let list = list_of(&shell, cx);
    assert_eq!(
        cx.update(|_, cx| list.read(cx).row_menu_open().map(str::to_owned)),
        Some(AB.to_owned())
    );
    assert!(drawn(cx, "chat-row-mark-unread"));
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert_eq!(
        marks(&rig),
        [Action::MarkUnread {
            conversation: AB.to_owned(),
            message_id: None,
        }]
    );
}

#[test]
fn a_read_only_app_draws_no_rows_menu() {
    let rig = seeded();
    *rig.core.read_only.lock().unwrap() = true;
    let (shell, cx) = shell_window(&rig);
    let row = row_of(&shell, cx, AB);
    let bounds = cx.debug_bounds(row).unwrap();
    cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    assert!(!drawn(cx, selector(format!("{row}-more"))));
}
