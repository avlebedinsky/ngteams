//! The shell's New message ([`crate::new_chat`]): the list header's button and Ctrl+N open it in the conversation
//! pane, with its row at the top of the chats; a chat it names (chosen from its suggestions, or one a message was just
//! sent to) opens in its place. Left while its first message is on its way, it is kept out of sight until that is
//! settled ([`NewChat::keeps_going`]): a chat made then isn't opened over the place shown, and a message given back is
//! there when New message opens again. The read-only mode opens none: it writes nothing.

use gpui::{AnyElement, AppContext as _, Context, Entity, IntoElement as _, ParentElement as _, Subscription, Window};

use std::sync::Arc;

use super::{Main, Shell};
use crate::chat_list::ChatList;
use crate::models::Models;
use crate::new_chat::{NewChat, NewChatEvent};
use crate::sidebar::{Sidebar, SidebarEvent};
use crate::teams_list::TeamsList;
use crate::theme::Palette;
use crate::trace::ViewTrace;

/// New message, while it is open, and what it tells.
pub(super) type Open = Option<(Entity<NewChat>, Subscription)>;

impl Shell {
    /// The sidebar over the lists, and the subscription by which its New message opens New message.
    pub(super) fn make_sidebar(
        list: &Entity<ChatList>,
        teams: &Entity<TeamsList>,
        models: &Models,
        trace: &Arc<ViewTrace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<Sidebar>, Subscription) {
        let (list, teams, models, trace) = (list.clone(), teams.clone(), models.clone(), Arc::clone(trace));
        let sidebar = cx.new(|cx| Sidebar::new(list, teams, models, trace, cx));
        let asked = cx.subscribe_in(&sidebar, window, |this: &mut Self, _, event, window, cx| match event {
            SidebarEvent::NewMessage => this.open_new_chat(window, cx),
        });
        (sidebar, asked)
    }

    /// Opens New message in the conversation pane, its To: box taking the keyboard; nothing in the read-only mode.
    pub fn open_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read_only() {
            return;
        }
        if self.main == Main::NewChat
            && let Some((view, _)) = &self.new_chat
        {
            view.update(cx, |view, cx| view.focus(window, cx));
            return;
        }
        self.hide_calendar(window, cx);
        self.put_chat_away(cx);
        self.keep_draft(cx);
        // one kept out of sight (its first message on its way, or given back) is shown again
        let (view, events) = if let Some(kept) = self.new_chat.take() {
            kept
        } else {
            let (parts, list) = (self.parts(), self.list.clone());
            let view = cx.new(|cx| NewChat::new(parts, list, window, cx));
            let events = cx.subscribe_in(&view, window, |this: &mut Self, _, event, window, cx| match event {
                // a chat made while New message is out of sight isn't opened over the place shown
                NewChatEvent::Open(id) if this.main == Main::NewChat => this.open_chat(id.clone(), window, cx),
                NewChatEvent::Open(_) => cx.notify(),
            });
            (view, events)
        };
        view.update(cx, |view, cx| view.focus(window, cx));
        self.new_chat = Some((view, events));
        self.list.update(cx, |list, cx| list.select(None, cx));
        self.teams.update(cx, |teams, cx| teams.select(None, cx));
        self.main = Main::NewChat;
        self.close_switcher(window, cx);
        self.close_search(cx);
        self.sync_new_message_row(cx);
        cx.notify();
    }

    /// New message, while it is the place shown.
    #[must_use]
    pub fn new_chat(&self) -> Option<&Entity<NewChat>> {
        self.new_chat
            .as_ref()
            .filter(|_| self.main == Main::NewChat)
            .map(|(view, _)| view)
    }

    /// The conversation pane while New message is the place shown.
    pub(super) fn new_chat_pane(&self, pane: gpui::Div, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        match &self.new_chat {
            Some((view, _)) => pane.child(view.clone()).into_any_element(),
            None => pane.child(super::no_chat(palette, cx)).into_any_element(),
        }
    }

    /// The sidebar's New message row follows the place shown: there while New message is, gone with it. New message
    /// itself goes with it unless it is kept (see the module docs).
    pub(super) fn sync_new_message_row(&mut self, cx: &mut Context<Self>) {
        let open = self.main == Main::NewChat;
        if !open
            && self
                .new_chat
                .as_ref()
                .is_some_and(|(view, _)| !view.read(cx).keeps_going())
        {
            self.new_chat = None;
        }
        if self.sidebar.read(cx).new_message_open() != open {
            self.sidebar.update(cx, |sidebar, cx| sidebar.set_new_message(open, cx));
        }
    }
}
