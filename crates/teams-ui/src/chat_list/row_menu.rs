//! A chat row's "More chat options" menu, as Teams' chat list has it: "Mark as read" while the chat is unread, else
//! "Mark as unread" (`docs/protocol/chat.md`, "Mark as unread"), first in the menu with Teams' glasses. The menu
//! offers nothing that writes in read-only mode, so it isn't drawn there; a channel's row has its own menu.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, DismissEvent, Focusable as _, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::Sizable as _;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use teams_core::Action;

use super::{ChatList, ChatRow, RowKind};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::{self, Palette};

/// The row menu's entry on an unread chat, as Teams words it.
pub const MARK_READ: &str = "Mark as read";
/// The row menu's entry on a read chat, as Teams words it.
pub const MARK_UNREAD: &str = "Mark as unread";
/// How much of the row's right end the "..." covers while shown, px: the time and the count under it, as Teams shows
/// the "..." in their place.
const COVER: f32 = 72.0;
/// The "..."'s tooltip and accessible name, as Teams words it.
pub const MORE_CHAT_OPTIONS: &str = "More chat options";

impl ChatList {
    /// The row's "...": over the row's right end on `cover` (the row's hovered colour), as Teams draws it over the
    /// time, while the row (its hover group `group`) is hovered or its menu is open; its menu under it while open.
    /// `None` for a row that offers none (a channel's, or any in read-only mode).
    pub(super) fn row_more(
        &self,
        row: &ChatRow,
        selector: &str,
        (group, cover): (SharedString, u32),
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.core.read_only() || row.kind == RowKind::Channel {
            return None;
        }
        let open = self
            .row_menu
            .as_ref()
            .filter(|(id, at, _)| *id == row.id && at == selector)
            .map(|(_, _, menu)| menu.clone());
        let (id, at, unread) = (row.id.clone(), selector.to_owned(), row.unread > 0);
        let button = icon_button(
            SharedString::from(format!("{selector}-more-{}", row.id)),
            Glyph::ELLIPSIS,
            MORE_CHAT_OPTIONS,
        )
        .xsmall()
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            let shown = this
                .row_menu
                .as_ref()
                .is_some_and(|(open, from, _)| *open == id && *from == at);
            if shown {
                this.row_menu = None;
                cx.notify();
            } else {
                this.open_row_menu(&id, &at, unread, window, cx);
            }
        }));
        let panel = format!("{selector}-menu");
        let shown = open.is_some();
        // the menu opens under the "...", its left edge the button's
        let anchor = div()
            .relative()
            .child(selected_by(format!("{selector}-more"), button))
            .children(open.map(|menu| {
                div().absolute().top(gpui::relative(1.0)).left_0().child(
                    gpui::deferred(
                        gpui::anchored()
                            .anchor(gpui::Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.0))
                            .child(div().debug_selector(move || panel.clone()).child(menu)),
                    )
                    .with_priority(1),
                )
            }));
        Some(
            div()
                .id(SharedString::from(format!("{selector}-more-wrap")))
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(COVER))
                .pr_3()
                .flex()
                .items_center()
                .justify_end()
                .bg(gpui::rgb(cover))
                .when(!shown, |more| {
                    more.invisible().group_hover(group, gpui::Styled::visible)
                })
                // the "..." takes its own clicks: the row under it doesn't open
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(anchor)
                .into_any_element(),
        )
    }

    /// Opens the menu of the chat `id`, whose row the test selector `selector` names (`chat-row-<index>`,
    /// `favorite-row-<index>`), focused so the keyboard moves in it; nothing in read-only mode.
    pub fn open_row_menu(
        &mut self,
        id: &str,
        selector: &str,
        unread: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.core.read_only() {
            return;
        }
        let weak = cx.entity().downgrade();
        let return_focus = window.focused(cx);
        let chosen = id.to_owned();
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            let (key, label, glyph) = if unread {
                ("mark-read", MARK_READ, Glyph::GLASSES)
            } else {
                ("mark-unread", MARK_UNREAD, Glyph::GLASSES_OFF)
            };
            let (weak, chosen) = (weak.clone(), chosen.clone());
            let menu = menu
                .min_w(px(200.0))
                .item(item(format!("chat-row-{key}"), label, glyph).on_click(move |_, _, cx| {
                    let _ = weak.update(cx, |this, cx| this.mark(&chosen, unread, cx));
                }));
            match return_focus.clone() {
                Some(focus) => menu.action_context(focus),
                None => menu,
            }
        });
        self.row_menu_dismiss = Some(cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            this.row_menu = None;
            cx.notify();
        }));
        menu.focus_handle(cx).focus(window, cx);
        self.row_menu = Some((id.to_owned(), selector.to_owned(), menu));
        cx.notify();
    }

    /// Whether a row's menu is open, and whose: the chat's id.
    #[must_use]
    pub fn row_menu_open(&self) -> Option<&str> {
        self.row_menu.as_ref().map(|(id, _, _)| id.as_str())
    }

    /// The row menu's choice on the chat `id`: "Mark as read" (`read`) or "Mark as unread"; the row changes as the
    /// engine tells it.
    pub fn mark(&mut self, id: &str, read: bool, cx: &mut Context<Self>) {
        self.row_menu = None;
        let conversation = id.to_owned();
        let action = if read {
            Action::MarkRead { conversation }
        } else {
            Action::MarkUnread {
                conversation,
                message_id: None,
            }
        };
        // the row comes back as the engine's events
        drop(self.core.write(action));
        cx.notify();
    }
}

/// A menu entry, drawn in the theme of the moment it is drawn.
fn item(id: String, label: &'static str, glyph: Glyph) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        let palette: Palette = theme::current(cx);
        let id = id.clone();
        div()
            .debug_selector(move || id.clone())
            .w_full()
            .flex()
            .items_center()
            .gap_3()
            .px_2()
            .py_1p5()
            .text_color(gpui::rgb(palette.foreground))
            .child(glyph.icon().size(px(16.0)))
            .child(label)
    })
}
