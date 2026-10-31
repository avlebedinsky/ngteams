//! The event's text in the scheduling form: the app's rich-text editor ([`Editor`]) in Teams' text box, its tools
//! along the box's foot as Teams' form has them (Attach, Picture, Emoji, Format, Loop, the accessibility checker),
//! Format showing Teams' format bar ([`crate::editor::toolbar::FormatBar`]) along the box's top, Emoji the app's emoji
//! picker over the box.
//!
//! What it sends is the HTML the editor writes for a message (`teams-proto`'s message text: paragraphs, marks, lists,
//! quotes, code, links), the event's own HTML while the text isn't edited (so an invitation's join block goes back as
//! it came). An event's text read whole loads into the editor as the renderer reads a message's HTML; one the editor
//! can't hold (a picture) loads as its plain lines.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, anchored, deferred, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{Disableable as _, Selectable as _, Sizable as _};

use super::{EventForm, card, model};
use crate::calendar_view::mix;
use crate::chat_view::measured;
use crate::editor::doc::{Block, Doc};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::reaction_picker::{ReactionPicker, ReactionPickerEvent};
use crate::reactions;
use crate::theme::Palette;

impl EventForm {
    /// Loads an event's text (HTML) into the editor, kept to send back while it isn't edited.
    pub(super) fn load_body(&mut self, html: Option<&str>, cx: &mut Context<Self>) {
        self.body_seed = html.map(str::to_owned).filter(|html| !html.trim().is_empty());
        self.body_changed = false;
        let Some(html) = self.body_seed.clone() else {
            self.body.update(cx, |editor, cx| editor.set_doc(Doc::default(), cx));
            return;
        };
        let text = teams_proto::chat::parse_html(&html);
        self.body.update(cx, |editor, cx| {
            if editor.load(&text.nodes, &HashMap::<String, String>::new(), cx).is_err() {
                let plain = model::html_text(&html);
                editor.set_doc(Doc::new(plain.lines().map(Block::paragraph).collect()), cx);
            }
        });
    }

    /// The text as the HTML a save sends (none for an empty text): the editor's message HTML, or its plain lines
    /// when that can't be written (a text too long for a message).
    pub(super) fn body_html(&self, cx: &App) -> String {
        let editor = self.body.read(cx);
        match crate::compose::message_of(editor.doc(), &[]) {
            Ok(Some(message)) => message.html().to_owned(),
            Ok(None) => String::new(),
            Err(_) => model::text_html(&editor.text()),
        }
    }

    /// Format: shows or hides the format bar over the text, the text keeping the focus.
    pub fn toggle_formatting(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.formatting = !self.formatting;
        let focus = self.body.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Whether the format bar shows.
    #[must_use]
    pub fn formatting(&self) -> bool {
        self.formatting
    }

    /// Opens the emoji picker over the text, or closes it.
    fn toggle_emoji(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.emoji.take().is_some() || self.core.read_only() {
            cx.notify();
            return;
        }
        let picker = cx.new(|cx| ReactionPicker::for_emoji(Arc::clone(&self.core), window, cx));
        let events = cx.subscribe_in(&picker, window, |this: &mut Self, _, event, window, cx| {
            if let ReactionPickerEvent::Picked(key) = event {
                let emoji = reactions::glyph(key);
                this.body.update(cx, |editor, cx| editor.insert(&emoji, cx));
            }
            this.emoji = None;
            let focus = this.body.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
            cx.notify();
        });
        self.emoji = Some((picker, events));
        cx.notify();
    }

    /// The text's card: its icon, the box with the editor (the format bar over it while Format is on) and its tools.
    pub(super) fn body_card(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        card(palette)
            .flex_1()
            .min_h(px(200.0))
            .p_4()
            .flex()
            .gap_3()
            .child(
                div().w(px(40.0)).flex_none().flex().justify_center().pt_2().child(
                    Glyph::NOTES
                        .icon()
                        .size(px(18.0))
                        .text_color(rgb(palette.muted_foreground)),
                ),
            )
            .child(
                div()
                    .debug_selector(|| "event-body".to_owned())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .when(self.formatting, |text| {
                        text.child(
                            div()
                                .px_2()
                                .py_1()
                                .border_b_1()
                                .border_color(rgb(palette.border))
                                .child(self.format_bar.clone()),
                        )
                    })
                    .child(
                        div()
                            .id("event-body-text")
                            .flex_1()
                            .min_h_0()
                            .p_2()
                            .overflow_y_scroll()
                            .child(self.body.clone()),
                    )
                    .child(self.body_tools(palette, cx)),
            )
    }

    /// The tools along the text's foot, as Teams has them: those the app doesn't have yet disabled, saying so.
    fn body_tools(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let unavailable = |id: &'static str, glyph: Glyph, tip: &'static str| {
            selected_by(id, icon_button(id, glyph, tip).small().disabled(true)).into_any_element()
        };
        let format_tip = if self.formatting {
            "Hide formatting options"
        } else {
            "Show formatting options"
        };
        let format = selected_by(
            "event-format",
            icon_button("event-format", Glyph::TEXT_FORMAT, format_tip)
                .small()
                .selected(self.formatting)
                .on_click(cx.listener(|form, _, window, cx| form.toggle_formatting(window, cx))),
        )
        .into_any_element();
        let emoji = selected_by(
            "event-emoji",
            icon_button("event-emoji", Glyph::EMOJI, "Emoji")
                .small()
                .selected(self.emoji.is_some())
                .disabled(self.core.read_only())
                .on_click(cx.listener(|form, _, window, cx| form.toggle_emoji(window, cx))),
        )
        .relative()
        .child(measured(&self.emoji_at))
        .into_any_element();
        div()
            .h(px(44.0))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .border_t_1()
            .border_color(rgb(palette.border))
            .bg(rgb(mix(palette.surface, palette.background, 400)))
            .child(unavailable(
                "event-attach",
                Glyph::PAPERCLIP,
                "Attaching files isn't available yet",
            ))
            .child(unavailable(
                "event-picture",
                Glyph::IMAGE,
                "Pictures aren't available yet",
            ))
            .child(emoji)
            .child(format)
            .child(unavailable(
                "event-loop",
                Glyph::SUN,
                "Loop components aren't available yet",
            ))
            .child(unavailable(
                "event-accessibility",
                Glyph::ACCESSIBILITY,
                "The accessibility checker isn't available yet",
            ))
    }

    /// The emoji picker over the text, by its button as it was last drawn, opening upwards.
    pub(super) fn emoji_overlay(&self) -> Option<AnyElement> {
        let ((picker, _), at) = self.emoji.as_ref().zip(self.emoji_at.get())?;
        Some(
            deferred(
                anchored()
                    .anchor(gpui::Anchor::BottomLeft)
                    .position(at.origin)
                    .snap_to_window_with_margin(px(8.0))
                    .child(div().pb_1().child(picker.clone())),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}
