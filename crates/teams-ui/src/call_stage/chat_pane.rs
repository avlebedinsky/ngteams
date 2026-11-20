//! The call window's Chat pane, as Teams shows it beside a call: the call's chat (a meeting's chat, a group chat, or
//! the 1:1 chat with whoever the call is with), its messages and its compose box, the same view as the main window's.
//! While the pane is closed and the chat has unread messages, the toolbar's Chat carries Teams' red dot (no count, as
//! Teams has it): the store's count (the account's own messages are never unread), counted again each time the chat
//! changes; gone while the pane shows the chat, which reads it. Each new message of someone else's that comes while
//! the pane is closed shows for [`BUBBLE_FOR`] in Teams' chat bubble at the stage's top middle: the sender's avatar
//! with their presence, their name over the message's first line, "Mute these notifications" (no more bubbles in this
//! call, as More → Settings' "Don't show chat bubbles") and Close; a click on it opens the pane. One bubble at a time:
//! a newer message takes its place.
use super::*;
use crate::chat_view::ChatView;

/// How long a chat bubble shows.
pub const BUBBLE_FOR: Duration = Duration::from_secs(4);
/// The bubble's size, px.
const BUBBLE: (f32, f32) = (320.0, 70.0);
/// The sender's avatar's side in the bubble, px.
const BUBBLE_AVATAR: f32 = 32.0;

/// The call's chat's unread count, as last read from the store, and the bubble of its newest message.
#[derive(Default)]
pub(super) struct Unread {
    /// The chat counted, and the revision of its last change when it was counted.
    of: Option<(String, u64)>,
    /// The count, once read for the chat.
    count: Option<u64>,
    counting: Option<Task<()>>,
    /// The arrival time of the newest message known in the chat, once read: only a message newer than it gets a
    /// bubble, so older history stored during the call (it raises the count too) shows none.
    newest: Option<i64>,
    /// The newest message's bubble while it shows.
    bubble: Option<Bubble>,
    bubbling: Option<Task<()>>,
    /// "Mute these notifications": no bubble for the rest of the call.
    muted: bool,
}

/// What a chat bubble shows: who sent the message, by MRI and name, and its first line.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Bubble {
    from: String,
    name: SharedString,
    line: SharedString,
}

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

    /// Counts the call's chat's unread messages again when the chat changed since it was counted.
    pub(super) fn follow_chat_unread(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.live(cx).and_then(|active| self.call_chat(&active)) else {
            self.chat_unread = Unread::default();
            return;
        };
        let changed = self
            .shell
            .as_ref()
            .map_or(0, |shell| shell.models.chats.read(cx).changed_at(&chat));
        if self
            .chat_unread
            .of
            .as_ref()
            .is_some_and(|(of, seen)| *of == chat && *seen == changed)
        {
            return;
        }
        if self.chat_unread.of.as_ref().is_some_and(|(of, _)| *of != chat) {
            self.chat_unread = Unread {
                muted: self.chat_unread.muted,
                ..Unread::default()
            };
        }
        self.chat_unread.of = Some((chat.clone(), changed));
        let reply = self.core.unread_count(chat.clone());
        self.chat_unread.counting = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            // a count that failed leaves the dot as it was
            let Ok(count) = reply.await else { return };
            // a stage that is gone shows no dot
            let _ = this.update(cx, |this, cx| this.counted(chat, count, cx));
        }));
    }

    /// `chat` counts `count` unread: the dot follows, and a count that rose shows the newest message's bubble.
    fn counted(&mut self, chat: String, count: u64, cx: &mut Context<Self>) {
        if self.chat_unread.of.as_ref().is_none_or(|(of, _)| *of != chat) {
            return;
        }
        // the first count of a call's chat is what came before the call: no bubble for it, but its newest message is
        // where the new ones start
        let first = self.chat_unread.count.is_none();
        let more = self.chat_unread.count.is_some_and(|before| count > before);
        self.chat_unread.count = Some(count);
        if first || more {
            self.read_newest(chat, more, cx);
        }
        cx.notify();
    }

    /// Whether the toolbar's Chat shows its unread dot: something unread, and the pane not showing the chat.
    pub(super) fn chat_unread_shown(&self) -> bool {
        self.pane != Pane::Chat && self.chat_unread.count.unwrap_or(0) > 0
    }

    /// Reads `chat`'s newest message. While `show`, it goes in the bubble when it is newer than all known before,
    /// someone else sent it, the pane is closed and bubbles aren't muted.
    fn read_newest(&mut self, chat: String, show: bool, cx: &mut Context<Self>) {
        let reply = self.core.messages(chat.clone(), None, 1);
        let me = self.core.me().to_owned();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let Ok(page) = reply.await else { return };
            let Some(message) = page.messages.into_iter().next() else {
                return;
            };
            // a stage that is gone shows no bubble
            let _ = this.update(cx, |this, cx| {
                let unread = &mut this.chat_unread;
                if unread.of.as_ref().is_none_or(|(of, _)| *of != chat) {
                    return;
                }
                let newer = unread.newest.is_none_or(|known| message.arrival_time > known);
                unread.newest = unread.newest.max(Some(message.arrival_time));
                let Some(from) = message.sender.clone().filter(|from| *from != me) else {
                    return;
                };
                if show && newer && crate::chat_list::is_text(&message) {
                    let line = SharedString::from(crate::chat_list::message_line(&message));
                    this.bubble(message, (from, line), cx);
                }
            });
        })
        .detach();
    }

    /// Shows `message` (sent by `from`, its first line `line`) in the bubble for [`BUBBLE_FOR`], unless the pane
    /// opened or bubbles were muted meanwhile.
    fn bubble(&mut self, message: teams_store::Message, (from, line): (String, SharedString), cx: &mut Context<Self>) {
        if self.pane == Pane::Chat || self.chat_unread.muted {
            return;
        }
        let name = message
            .sender_name
            .filter(|name| !name.trim().is_empty())
            .map(SharedString::from)
            .or_else(|| {
                self.people
                    .of(self.call)
                    .iter()
                    .find(|(mri, _)| *mri == from)
                    .map(|(_, name)| name.clone())
            })
            .unwrap_or_else(|| SharedString::from(SOMEONE));
        self.chat_unread.bubble = Some(Bubble { from, name, line });
        self.chat_unread.bubbling = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(BUBBLE_FOR).await;
            // a stage that is gone has no bubble to put away
            let _ = this.update(cx, Self::close_bubble);
        }));
        cx.notify();
    }

    /// Whether the call's chat bubbles are off (the bubble's "Mute these notifications", More → Settings' "Don't show
    /// chat bubbles").
    pub(super) fn chat_bubbles_muted(&self) -> bool {
        self.chat_unread.muted
    }

    /// Turns the call's chat bubbles off (`true`, the one showing put away) or on again.
    pub(super) fn mute_chat_bubbles(&mut self, muted: bool, cx: &mut Context<Self>) {
        self.chat_unread.muted = muted;
        if muted {
            self.close_bubble(cx);
        }
        cx.notify();
    }

    /// Puts the chat bubble away.
    pub(super) fn close_bubble(&mut self, cx: &mut Context<Self>) {
        self.chat_unread.bubble = None;
        self.chat_unread.bubbling = None;
        cx.notify();
    }

    /// The bubble's sender's avatar with their presence.
    fn bubble_avatar(&self, bubble: &Bubble, palette: Palette, cx: &App) -> gpui::Div {
        let picture = self.faces.picture(&bubble.from);
        let presence = self
            .shell
            .as_ref()
            .map(|shell| shell.models.presence.read(cx).of(&bubble.from))
            .and_then(|availability| {
                crate::presence_badge::badge(&availability, crate::presence_badge::MESSAGE, palette)
            })
            .map(|badge| badge.absolute().right(px(-2.0)).bottom(px(-2.0)));
        div()
            .relative()
            .flex_none()
            .size(px(BUBBLE_AVATAR))
            .child(people::avatar(
                Some(&bubble.name),
                picture,
                Size::Size(px(BUBBLE_AVATAR)),
            ))
            .children(presence)
    }

    /// The chat bubble while it shows, at the stage's top middle (see the module docs).
    pub(super) fn chat_bubble(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let bubble = self.chat_unread.bubble.clone().filter(|_| self.pane != Pane::Chat)?;
        let avatar = self.bubble_avatar(&bubble, palette, cx);
        let words = bubble_words(&bubble, palette, cx);
        let mute = selected_by(
            "call-chat-bubble-mute",
            icon_button("call-chat-bubble-mute", Glyph::BELL_OFF, "Mute these notifications")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.mute_chat_bubbles(true, cx))),
        );
        let close = selected_by(
            "call-chat-bubble-close",
            icon_button("call-chat-bubble-close", Glyph::X, "Close")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.close_bubble(cx))),
        );
        let bubble = div()
            .debug_selector(|| "call-chat-bubble".to_owned())
            .id("call-chat-bubble")
            .occlude()
            .w(px(BUBBLE.0))
            .max_w(relative(0.9))
            .h(px(BUBBLE.1))
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .rounded_lg()
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .border_1()
            .border_color(rgb(palette.border))
            .shadow(theme::glow(palette, 0x0000_0000, 0.18, 12.0, 2.0))
            .child(avatar)
            .child(words)
            .child(div().flex().items_center().gap_0p5().child(mute).child(close));
        Some(
            div()
                .absolute()
                .top(px(8.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(crate::motion::fade(bubble, "call-chat-bubble-in", cx))
                .into_any_element(),
        )
    }

    /// Shows `pane` beside the stage in place of the one open. The Chat pane's view, kept while another pane shows,
    /// stops reading its chat: no read receipt goes for a message the user did not see.
    pub(super) fn open_pane(&mut self, pane: Pane, cx: &mut App) {
        if self.pane == Pane::Chat
            && pane != Pane::Chat
            && let Some(chat) = &self.chat
        {
            chat.update(cx, |chat, cx| chat.set_shown(false, cx));
        }
        self.pane = pane;
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
        self.chat_unread.bubble = None;
        self.chat_unread.bubbling = None;
        cx.notify();
    }

    /// The chat of the Chat pane, once it was opened.
    #[must_use]
    pub fn chat_view(&self) -> Option<&Entity<ChatView>> {
        self.chat.as_ref()
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

/// The bubble's sender's name over the message's first line: a click opens the Chat pane.
fn bubble_words(bubble: &Bubble, palette: Palette, cx: &mut Context<CallStage>) -> gpui::Stateful<gpui::Div> {
    div()
        .debug_selector(|| "call-chat-bubble-open".to_owned())
        .id("call-chat-bubble-open")
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .cursor_pointer()
        .on_click(cx.listener(|this, _, window, cx| this.show_chat(true, window, cx)))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .truncate()
                .child(bubble.name.clone()),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .truncate()
                .child(bubble.line.clone()),
        )
}
