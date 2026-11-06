//! The call window's Chat pane, as Teams shows it beside a call: the call's chat (a meeting's chat, a group chat, or
//! the 1:1 chat with whoever the call is with), its messages and its compose box, the same view as the main window's.
use super::*;
use crate::chat_view::ChatView;

impl CallStage {
    /// The chat of the call up: its own (a chat's, a join's, a meeting's), or for a 1:1 invitation the chat of the
    /// account and its caller; none for the test call or without the parts a chat is drawn with.
    pub(super) fn call_chat(&self, active: &ActiveCall) -> Option<String> {
        self.shell.as_ref()?;
        if active.kind == CallKind::TestCall {
            return None;
        }
        active.chat.clone().or_else(|| {
            let caller = active.caller.as_ref()?;
            teams_proto::one_to_one_id(self.core.me(), caller.as_str())
        })
    }

    /// Opens or closes the Chat pane, as the toolbar's Chat does; its compose box takes the keyboard as it opens.
    pub fn show_chat(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !open {
            if self.pane == Pane::Chat {
                self.pane = Pane::None;
            }
            if let Some(chat) = &self.chat {
                chat.update(cx, |chat, cx| chat.set_shown(false, cx));
            }
            self.focus.focus(window, cx);
            cx.notify();
            return;
        }
        let Some(conversation) = self.live(cx).and_then(|active| self.call_chat(&active)) else {
            return;
        };
        let stale = self
            .chat
            .as_ref()
            .is_some_and(|chat| chat.read(cx).conversation() != conversation);
        if stale {
            self.chat = None;
        }
        if self.chat.is_none()
            && let Some(parts) = self.shell.clone()
        {
            let chat = cx.new(|cx| ChatView::new(parts, conversation, window, cx));
            cx.observe(&chat, |_, _, cx| cx.notify()).detach();
            self.chat = Some(chat);
        }
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.set_shown(true, cx));
            let compose = chat.read(cx).compose().clone();
            compose.update(cx, |compose, cx| compose.focus(window, cx));
        }
        self.pane = Pane::Chat;
        cx.notify();
    }

    /// Whether the Chat pane is open.
    #[must_use]
    pub fn chat_shown(&self) -> bool {
        self.pane == Pane::Chat && self.chat.is_some()
    }

    /// The Chat pane while it is open: "Meeting chat" for a meeting's call, else "Chat", over the chat itself.
    pub(super) fn chat_pane(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.pane != Pane::Chat {
            return None;
        }
        let chat = self.chat.clone()?;
        let close = selected_by(
            "call-chat-close",
            icon_button("call-chat-close", Glyph::X, "Close chat")
                .small()
                .on_click(cx.listener(|this, _, window, cx| this.show_chat(false, window, cx))),
        );
        let title = if active.kind == CallKind::Meeting {
            "Meeting chat"
        } else {
            "Chat"
        };
        Some(
            people_pane::side_pane("call-chat-pane", title, close, palette)
                .w(px(CHAT_WIDTH))
                .child(div().flex_1().min_h_0().flex().flex_col().child(chat))
                .into_any_element(),
        )
    }
}
