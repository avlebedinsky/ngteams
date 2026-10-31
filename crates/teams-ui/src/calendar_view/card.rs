//! An event's card: its title, when, its series, where, the organizer and the account's answer, who is invited and
//! its text; for a Teams meeting Join (once a call there is known), Chat with participants and Copy link.

use std::fmt::Write as _;
use std::rc::Rc;

use gpui::{
    App, ClipboardItem, Context, InteractiveElement as _, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, div, px, relative, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Sizable as _};
use teams_core::{CalendarEvent, EventResponse};

use super::{CalendarView, CalendarViewEvent, Removal, Shown, mix, time_text};
use crate::clock::{self, HourCycle};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::{self, Palette};

impl CalendarView {
    /// The open event's card, over the grid.
    pub(super) fn card_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let shown = self.card.clone()?;
        let event = Rc::clone(&shown.event);
        let cycle = clock::cycle(cx);
        let read_only = self.core.read_only();
        let calls = self.calls.read(cx);
        let joinable = event
            .meeting_chat
            .as_deref()
            .is_some_and(|chat| calls.shows() && !read_only && calls.joinable(chat));
        let row = |glyph: Glyph, text: String| card_row(glyph, text, palette);
        let when = when_text(&shown, cycle);
        let organizer = organizer_text(&event);
        let invited = invited_text(&event);
        let actions = self.card_actions(&event, joinable, palette, cx);
        let manage = self.manage_actions(&event, palette, cx);
        let card = div()
            .debug_selector(|| "calendar-card".to_owned())
            .id("calendar-card")
            .occlude()
            .w(px(480.0))
            .max_h(relative(0.85))
            .p_5()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(card_header(shown.title(), palette, cx))
            .children(event.cancelled.then(|| {
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(mix(palette.surface, theme::DANGER, 150)))
                    .child("This event was canceled.")
            }))
            .child(row(Glyph::CLOCK, when))
            .children(
                event
                    .kind
                    .recurring()
                    .then(|| row(Glyph::REPEAT, "Part of a series".to_owned())),
            )
            .children(event.location.clone().map(|location| row(Glyph::MAP_PIN, location)))
            .children(organizer.map(|organizer| row(Glyph::USER, organizer)))
            .children(invited.map(|invited| row(Glyph::USERS, invited)))
            .children(event.preview.clone().map(|preview| {
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .max_h(px(120.0))
                    .overflow_hidden()
                    .child(preview)
            }))
            .children(actions)
            .children(manage);
        Some(
            div()
                .id("calendar-card-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x0000_0066))
                .on_click(cx.listener(|this, _, _, cx| this.close_card(cx)))
                .child(card),
        )
    }

    /// The card's Edit and its removal (Cancel meeting for the account's own meeting, Delete for its appointment,
    /// Remove from calendar for a meeting its organizer cancelled), the removal confirmed first; none in the read-only
    /// mode, none for someone else's meeting that still stands.
    pub(super) fn manage_actions(
        &self,
        event: &Rc<CalendarEvent>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        if self.core.read_only() {
            return None;
        }
        let own = event.is_organizer;
        let meeting = own && !event.attendees.is_empty() && !event.cancelled;
        // an occurrence's card acts on that occurrence alone, as Teams' card says (its series is changed in the form)
        let occurrence = matches!(
            event.kind,
            teams_core::EventKind::Occurrence | teams_core::EventKind::Exception
        );
        let removal = if meeting && occurrence {
            "Cancel occurrence"
        } else if meeting {
            "Cancel meeting"
        } else if own && occurrence {
            "Delete occurrence"
        } else if own {
            "Delete"
        } else if event.cancelled {
            "Remove from calendar"
        } else {
            return None;
        };
        let edit = (own && !event.cancelled).then(|| {
            let id = event.id.clone();
            selected_by(
                "calendar-card-edit",
                Button::new("calendar-card-edit")
                    .icon(Glyph::PENCIL.icon())
                    .label("Edit")
                    .outline()
                    .on_click(cx.listener(move |this, _, _, cx| this.edit_event(id.clone(), cx))),
            )
        });
        let row = match self.removal {
            Removal::None => div()
                .flex()
                .items_center()
                .gap_2()
                .children(edit)
                .child(div().flex_1())
                .child(selected_by(
                    "calendar-card-remove",
                    Button::new("calendar-card-remove")
                        .icon(if meeting { Glyph::CALENDAR_CANCEL } else { Glyph::TRASH }.icon())
                        .label(removal)
                        .ghost()
                        .text_color(rgb(theme::DANGER))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.removal = Removal::Asked;
                            cx.notify();
                        })),
                )),
            Removal::Asked | Removal::Sending => {
                let question = if meeting && occurrence {
                    "Cancel this occurrence? Everyone invited gets a cancellation of it; the rest of the series stays."
                } else if meeting {
                    "Cancel this meeting? Everyone invited gets a cancellation."
                } else if occurrence && own {
                    "Delete this occurrence from your calendar? The rest of the series stays."
                } else {
                    "Delete this event from your calendar?"
                };
                self.removal_confirm(question, removal, palette, cx)
            }
        };
        Some(div().pt_2().border_t_1().border_color(rgb(palette.border)).child(row))
    }

    /// The removal's confirmation: its `question`, Don't, and the `removal` itself (off while it is being sent).
    fn removal_confirm(
        &self,
        question: &'static str,
        removal: &'static str,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .debug_selector(|| "calendar-card-confirm".to_owned())
            .p_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .bg(rgb(mix(palette.surface, theme::DANGER, 100)))
            .child(div().flex_1().text_sm().child(question))
            .child(selected_by(
                "calendar-card-keep",
                Button::new("calendar-card-keep")
                    .label("Don't")
                    .outline()
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.removal = Removal::None;
                        cx.notify();
                    })),
            ))
            .child(selected_by(
                "calendar-card-remove-confirm",
                Button::new("calendar-card-remove-confirm")
                    .label(removal)
                    .danger()
                    .small()
                    .disabled(self.removal == Removal::Sending)
                    .on_click(cx.listener(|this, _, _, cx| this.remove_carded(cx))),
            ))
    }

    /// A meeting's card's actions: Join, Chat with participants, Copy link; and why Join isn't offered yet.
    pub(super) fn card_actions(
        &self,
        event: &Rc<CalendarEvent>,
        joinable: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        if !event.online || event.cancelled {
            return None;
        }
        let chat = event.meeting_chat.clone();
        // Teams' Join on every meeting: usable once a call runs in its chat
        let join = chat.clone().map(|chat| {
            selected_by(
                "calendar-card-join",
                Button::new("calendar-card-join")
                    .icon(Glyph::VIDEO.icon())
                    .label("Join")
                    .primary()
                    .disabled(!joinable)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.card = None;
                        cx.emit(CalendarViewEvent::Join(chat.clone()));
                        cx.notify();
                    })),
            )
        });
        let open_chat = chat.clone().map(|chat| {
            selected_by(
                "calendar-card-chat",
                Button::new("calendar-card-chat")
                    .icon(Glyph::MESSAGE.icon())
                    .label("Chat with participants")
                    .outline()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.card = None;
                        cx.emit(CalendarViewEvent::OpenChat(chat.clone()));
                        cx.notify();
                    })),
            )
        });
        let copy = event.join_url.clone().map(|link| {
            selected_by(
                "calendar-card-copy-link",
                icon_button("calendar-card-copy-link", Glyph::LINK, "Copy the meeting link")
                    .on_click(move |_, _, cx: &mut App| cx.write_to_clipboard(ClipboardItem::new_string(link.clone()))),
            )
        });
        let waiting = (!joinable && chat.is_some()).then(|| {
            div()
                .debug_selector(|| "calendar-card-not-started".to_owned())
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(if self.core.read_only() {
                    "Read-only: joining is off."
                } else {
                    "Join shows here and in the meeting's chat once its call has started."
                })
        });
        Some(
            div()
                .pt_2()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .children(join)
                        .children(open_chat)
                        .child(div().flex_1())
                        .children(copy),
                )
                .children(waiting),
        )
    }
}

/// One of the card's lines: its glyph, then its text.
fn card_row(glyph: Glyph, text: String, palette: Palette) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .pt(px(2.0))
                .child(glyph.icon().size(px(16.0)).text_color(rgb(palette.muted_foreground))),
        )
        .child(div().flex_1().min_w_0().child(text))
}

/// The card's title line: the calendar's colour, the title and Close.
fn card_header(title: String, palette: Palette, cx: &mut Context<CalendarView>) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .gap_3()
        .child(div().mt(px(6.0)).size(px(12.0)).rounded_sm().bg(rgb(palette.brand)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xl()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(selected_by(
            "calendar-card-close",
            icon_button("calendar-card-close", Glyph::X, "Close")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.close_card(cx))),
        ))
}

/// When the event is: its day (or days) and times, or its day and "all day".
fn when_text(shown: &Shown, cycle: HourCycle) -> String {
    if shown.all_day() {
        format!("{} (all day)", shown.start.format("%A, %B %-d, %Y"))
    } else if shown.start.date() == shown.end.date() {
        format!(
            "{} {} – {}",
            shown.start.format("%A, %B %-d, %Y"),
            time_text(shown.start, cycle),
            time_text(shown.end, cycle)
        )
    } else {
        format!(
            "{} {} – {} {}",
            shown.start.format("%a %b %-d"),
            time_text(shown.start, cycle),
            shown.end.format("%a %b %-d"),
            time_text(shown.end, cycle)
        )
    }
}

/// Who organizes the event, with the account's answer beside it.
fn organizer_text(event: &CalendarEvent) -> Option<String> {
    let organizer = if event.is_organizer {
        "You're the organizer".to_owned()
    } else {
        format!("Organizer: {}", event.organizer_name.as_deref()?)
    };
    let answer = match &event.response {
        EventResponse::Accepted => Some("You accepted"),
        EventResponse::Tentative => Some("You tentatively accepted"),
        EventResponse::NotResponded => Some("You haven't responded"),
        EventResponse::Declined => Some("You declined"),
        EventResponse::Organizer | EventResponse::NoResponse | EventResponse::Other(_) => None,
    };
    Some(match answer {
        Some(answer) => format!("{organizer} · {answer}"),
        None => organizer,
    })
}

/// Who is invited: how many, the first six by name and how many more.
fn invited_text(event: &CalendarEvent) -> Option<String> {
    if event.attendees.is_empty() {
        return None;
    }
    let names: Vec<&str> = event
        .attendees
        .iter()
        .filter_map(|attendee| attendee.name.as_deref().or(attendee.address.as_deref()))
        .take(6)
        .collect();
    let rest = event.attendees.len().saturating_sub(names.len());
    let mut text = format!("{} invited: {}", event.attendees.len(), names.join(", "));
    if rest > 0 {
        let _ = write!(text, " and {rest} more");
    }
    Some(text)
}
