//! The message of "Share this file": the app's rich-text editor ([`Editor`]) in the preview's box, as the compose box
//! has it. Format (Teams' "Show formatting options") shows the compose box's format bar over the text, the text
//! keeping the keyboard; Emoji ("Emoji, GIFs and Stickers") opens the app's emoji picker over the box, and the emoji
//! picked goes in at the cursor. Both sit at the box's bottom right, as Teams draws them.

use std::sync::Arc;

use gpui::{
    AnyElement, AppContext as _, Context, Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, anchored, deferred, div, px, rgb,
};
use gpui_component::{Disableable as _, Selectable as _, Sizable as _};
use teams_store::OutboxKind;

use super::ShareFileDialog;
use crate::chat_view::measured;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::reaction_picker::{ReactionPicker, ReactionPickerEvent};
use crate::reactions;
use crate::theme::Palette;

/// The format button's words, as Teams has them, while the bar is hidden and while it shows.
pub const SHOW_FORMATTING: &str = "Show formatting options";
pub const HIDE_FORMATTING: &str = "Hide formatting options";
/// The emoji button's words, as Teams has them.
pub const EMOJI: &str = "Emoji, GIFs and Stickers";

impl ShareFileDialog {
    pub(super) fn message_locked(&self) -> bool {
        self.step == super::Step::Sharing || self.retries.values().any(|row| row.kind == OutboxKind::Send)
    }

    pub(super) fn lock_message(&self, cx: &mut Context<Self>) {
        self.message
            .update(cx, |editor, cx| editor.set_read_only(self.message_locked(), cx));
    }

    /// Format: shows or hides the format bar over the message, the message keeping the keyboard.
    pub fn toggle_formatting(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.message_locked() {
            return;
        }
        self.formatting = !self.formatting;
        let focus = self.message.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Whether the format bar shows.
    #[must_use]
    pub fn formatting(&self) -> bool {
        self.formatting
    }

    /// Emoji: opens the emoji picker over the message, or closes it.
    pub fn toggle_emoji(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.message_locked() {
            return;
        }
        if self.emoji.take().is_some() {
            cx.notify();
            return;
        }
        self.more_open = false;
        let picker = cx.new(|cx| ReactionPicker::for_emoji(Arc::clone(&self.core), window, cx));
        let events = cx.subscribe_in(&picker, window, |this: &mut Self, _, event, window, cx| {
            if let ReactionPickerEvent::Picked(key) = event {
                let emoji = reactions::glyph(key);
                this.message.update(cx, |editor, cx| editor.insert(&emoji, cx));
            }
            this.emoji = None;
            let focus = this.message.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
            cx.notify();
        });
        self.emoji = Some((picker, events));
        cx.notify();
    }

    /// Whether the emoji picker is open.
    #[must_use]
    pub fn emoji_open(&self) -> bool {
        self.emoji.is_some()
    }

    /// The message's text as the editor holds it, plain.
    #[must_use]
    pub fn message_text(&self, cx: &gpui::App) -> String {
        self.message.read(cx).text()
    }

    /// The format bar over the message, while Format is on.
    pub(super) fn format_line(&self, palette: Palette) -> Option<AnyElement> {
        self.formatting.then(|| {
            div()
                .debug_selector(|| "share-file-format-bar".to_owned())
                .pb_1()
                .border_b_1()
                .border_color(rgb(palette.border))
                .child(self.format_bar.clone())
                .into_any_element()
        })
    }

    /// Format and Emoji, at the box's bottom right.
    pub(super) fn message_tools(&self, cx: &mut Context<Self>) -> AnyElement {
        let tip = if self.formatting {
            HIDE_FORMATTING
        } else {
            SHOW_FORMATTING
        };
        let format = selected_by(
            "share-file-format",
            icon_button("share-file-format", Glyph::TEXT_FORMAT, tip)
                .small()
                .disabled(self.message_locked())
                .selected(self.formatting)
                .on_click(cx.listener(|this, _, window, cx| this.toggle_formatting(window, cx))),
        );
        let emoji = selected_by(
            "share-file-emoji",
            icon_button("share-file-emoji", Glyph::EMOJI, EMOJI)
                .small()
                .disabled(self.message_locked())
                .selected(self.emoji.is_some())
                .on_click(cx.listener(|this, _, window, cx| this.toggle_emoji(window, cx))),
        )
        .relative()
        .child(measured(&self.emoji_at));
        div()
            .flex()
            .justify_end()
            .gap_1()
            .child(format)
            .child(emoji)
            .into_any_element()
    }

    /// The emoji picker over the message, by its button as it was last drawn, opening upwards.
    pub(super) fn emoji_overlay(&self) -> Option<AnyElement> {
        let ((picker, _), at) = self.emoji.as_ref().zip(self.emoji_at.get())?;
        Some(
            deferred(
                anchored()
                    .anchor(gpui::Anchor::BottomRight)
                    .position(gpui::point(at.right(), at.top()))
                    .snap_to_window_with_margin(px(8.0))
                    .child(div().pb_1().child(picker.clone())),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}
