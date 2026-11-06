//! A 1:1 call's Transfer, as Teams' call window has it (seen 2026-11-06): it holds the call first and asks whom.
//! "Transfer the call" finds the person ("Type a name"), then, once picked, offers "Transfer to an audio
//! call" and Transfer; Cancel leaves the call held.
//!
//! Teams' dialog also offers "Transfer to their work voicemail" and "Include a call topic for the recipient"; neither
//! was captured, so neither is offered here.
use super::invite::{Finder, FinderOf, found_list};
use super::*;
use gpui_component::Disableable as _;
use gpui_component::input::Input;
use teams_core::Invitable;

/// Which dialog it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asking {
    Transfer,
}

/// The open dialog: which, its box, and the person picked.
pub(super) struct Dialog {
    pub(super) asking: Asking,
    pub(super) finder: Finder,
    pub(super) picked: Option<Invitable>,
}

impl CallStage {
    /// Whether the call takes Transfer: a 1:1 call's that is up, as Teams' toolbar offers them, never in the
    /// read-only mode.
    pub(super) fn takes_transfer(&self, active: &ActiveCall) -> bool {
        self.takes_hold(active)
    }

    /// Transfer asked: the call goes on hold (unless it is already), and its dialog opens.
    pub fn open_transfer(&mut self, asking: Asking, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.live(cx) else { return };
        if !self.takes_transfer(&active) {
            return;
        }
        if active.held.is_none() {
            self.ask(Ask::ToggleHold, cx);
        }
        let finder = Self::finder(FinderOf::Dialog, "Type a name", window, cx);
        finder.query.update(cx, |input, cx| input.focus(window, cx));
        self.trace.record(&TraceLine::new("call", "dialog").kind(asking.name()));
        self.dialog = Some(Dialog {
            asking,
            finder,
            picked: None,
        });
        cx.notify();
    }

    /// The dialog put away (Cancel): the call stays held, as Teams leaves it.
    pub fn close_transfer(&mut self, cx: &mut Context<Self>) {
        if self.dialog.take().is_some() {
            cx.notify();
        }
    }

    /// The dialog showing `found` under its box, as a search would leave it (the UI tour's).
    pub fn transfer_found(&mut self, found: Vec<Invitable>, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.dialog {
            dialog.finder.found = found;
        }
        cx.notify();
    }

    /// `person` picked in the dialog.
    pub fn pick_transfer(&mut self, person: Invitable, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.dialog else { return };
        dialog.finder.found.clear();
        dialog
            .finder
            .query
            .update(cx, |input, cx| input.set_value("", window, cx));
        dialog.picked = Some(person);
        cx.notify();
    }

    /// Transfer: the call's far end transferred to the person picked, the dialog put away.
    fn transfer_now(&mut self, cx: &mut Context<Self>) {
        let Some(picked) = self.dialog.as_ref().and_then(|dialog| dialog.picked.clone()) else {
            return;
        };
        self.dialog = None;
        self.ask(
            Ask::Transfer {
                target: Mri::new(&picked.mri),
            },
            cx,
        );
    }

    /// The open dialog over the whole window, as Teams words it.
    pub(super) fn transfer_dialog(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.dialog.as_ref()?;
        let (title, selector) = match dialog.asking {
            Asking::Transfer => ("Transfer the call", "call-transfer-dialog"),
        };
        let who = match &dialog.picked {
            Some(person) => picked_chip(person, palette, cx),
            None => div()
                .relative()
                .child(Input::new(&dialog.finder.query))
                .children(found_list(
                    &dialog.finder,
                    "call-transfer-found",
                    palette,
                    cx,
                    Self::pick_transfer,
                ))
                .into_any_element(),
        };
        let kind = (dialog.asking == Asking::Transfer && dialog.picked.is_some()).then(|| how(palette));
        let card = div()
            .debug_selector(move || selector.to_owned())
            .occlude()
            .w(px(600.0))
            .max_w_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .shadow_lg()
            .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child(title))
            .child(who)
            .children(kind)
            .child(Self::dialog_buttons(dialog, cx));
        Some(
            div()
                .id("call-transfer-scrim")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .p_6()
                .bg(gpui::black().opacity(0.4))
                .occlude()
                .child(card)
                .into_any_element(),
        )
    }

    /// The dialog's buttons: Cancel and Transfer (once someone is picked).
    fn dialog_buttons(dialog: &Dialog, cx: &mut Context<Self>) -> gpui::Div {
        let cancel = selected_by(
            "call-transfer-cancel",
            Button::new("call-transfer-cancel")
                .label("Cancel")
                .outline()
                .on_click(cx.listener(|this, _, _, cx| this.close_transfer(cx))),
        );
        let picked = dialog.picked.is_some();
        let row = div().flex().justify_end().gap_2().child(cancel);
        match dialog.asking {
            Asking::Transfer => row.child(selected_by(
                "call-transfer-go",
                Button::new("call-transfer-go")
                    .label("Transfer")
                    .primary()
                    .disabled(!picked)
                    .on_click(cx.listener(|this, _, _, cx| this.transfer_now(cx))),
            )),
        }
    }
}

impl Asking {
    /// The trace's name for it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Transfer => "transfer",
        }
    }
}

/// The person picked, as Teams shows them in the box: a chip with their initials, name and its remove.
fn picked_chip(person: &Invitable, palette: Palette, cx: &mut Context<CallStage>) -> AnyElement {
    let name = SharedString::from(person.name.clone());
    div()
        .debug_selector(|| "call-transfer-picked".to_owned())
        .h(px(32.0))
        .px_1()
        .flex()
        .items_center()
        .gap_2()
        .rounded_md()
        .bg(rgb(palette.hover))
        .child(people::avatar(Some(&name), None, Size::Small))
        .child(name)
        .child(selected_by(
            "call-transfer-unpick",
            icon_button("call-transfer-unpick", Glyph::X, "Remove")
                .xsmall()
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(dialog) = &mut this.dialog {
                        dialog.picked = None;
                    }
                    cx.notify();
                })),
        ))
        .into_any_element()
}

/// How the call goes to them: Teams' "Transfer to an audio call" (its voicemail choice wasn't captured: not offered).
fn how(palette: Palette) -> AnyElement {
    div()
        .debug_selector(|| "call-transfer-how".to_owned())
        .h(px(32.0))
        .px_3()
        .flex()
        .items_center()
        .justify_between()
        .rounded_md()
        .bg(rgb(palette.hover))
        .border_b_2()
        .border_color(rgb(palette.brand))
        .child("Transfer to an audio call")
        .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0)))
        .into_any_element()
}
