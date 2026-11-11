//! The shell's New message ([`crate::new_chat`]): the list header's button and Ctrl+N open it in the conversation
//! pane, with its row at the top of the chats; a chat it names (chosen from its suggestions, or one a message was just
//! sent to) opens in its place. Left while its first message is on its way, it is kept out of sight until that is
//! settled ([`NewChat::keeps_going`]): a chat made then isn't opened over the place shown, and a message given back is
//! there when New message opens again. A 1:1 chat's "Start a group chat" opens it as a new group chat of its people
//! ([`Shell::open_group_draft`]), its row named after them. The read-only mode opens none: it writes nothing.

use gpui::{AnyElement, AppContext as _, Context, Entity, IntoElement as _, ParentElement as _, Subscription, Window};

use std::sync::Arc;

use super::{Main, Shell};
use crate::chat_list::ChatList;
use crate::models::Models;
use crate::new_chat::{NewChat, NewChatEvent, Recipient};
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
            && !view.read(cx).drafting()
        {
            view.update(cx, |view, cx| view.focus(window, cx));
            return;
        }
        // a new group chat not made yet gives way to New message, unless its first message is on its way
        if self
            .new_chat
            .as_ref()
            .is_some_and(|(view, _)| view.read(cx).drafting() && !view.read(cx).keeps_going())
        {
            self.new_chat = None;
        }
        let view = self.show_new_chat(window, cx);
        view.update(cx, |view, cx| view.focus(window, cx));
        self.finish_opening_new_chat(window, cx);
    }

    /// A new group chat of `people` not made yet, from a 1:1 chat's "Start a group chat" ([`NewChat::start_draft`]),
    /// in New message's place; one whose first message is on its way is shown as it is instead, and nothing opens in
    /// the read-only mode.
    pub fn open_group_draft(&mut self, people: Vec<Recipient>, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read_only() {
            return;
        }
        if self
            .new_chat
            .as_ref()
            .is_some_and(|(view, _)| !view.read(cx).keeps_going())
        {
            self.new_chat = None;
        }
        let fresh = self.new_chat.is_none();
        let view = self.show_new_chat(window, cx);
        if fresh {
            view.update(cx, |view, cx| view.start_draft(people, window, cx));
        }
        self.finish_opening_new_chat(window, cx);
    }

    /// New message to `person` alone (a profile card's Chat, or a chat's people, with someone the account has no 1:1
    /// chat with): its To: line holding them, its box taking the keyboard ([`NewChat::start_with`]); one whose first
    /// message is on its way is shown as it is instead, and nothing opens in the read-only mode.
    pub fn open_new_chat_with(&mut self, person: Recipient, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read_only() {
            return;
        }
        if self
            .new_chat
            .as_ref()
            .is_some_and(|(view, _)| !view.read(cx).keeps_going())
        {
            self.new_chat = None;
        }
        let fresh = self.new_chat.is_none();
        let view = self.show_new_chat(window, cx);
        if fresh {
            view.update(cx, |view, cx| view.start_with(person, window, cx));
        }
        self.finish_opening_new_chat(window, cx);
    }

    /// New message made or kept, put in the conversation pane's place.
    fn show_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<NewChat> {
        self.hide_calendar(window, cx);
        self.put_chat_away(cx);
        self.keep_draft(cx);
        self.people.open = None;
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
                NewChatEvent::DraftChanged => this.sync_new_message_row(cx),
            });
            (view, events)
        };
        self.new_chat = Some((view.clone(), events));
        view
    }

    fn finish_opening_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        // a new group chat's row is named after its people
        let draft = self
            .new_chat
            .as_ref()
            .filter(|_| open)
            .and_then(|(view, _)| view.read(cx).draft_row());
        if self.sidebar.read(cx).new_message_draft() != draft.as_ref() {
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.set_new_message_draft(draft, cx));
        }
    }
}
