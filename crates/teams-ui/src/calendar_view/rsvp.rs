//! An invitation's answer on its card, as Teams' calendar peek offers it to an attendee (seen in the lab's web client,
//! 2026-11-06): "RSVP to this event" with "Email organizer" ticked, an optional message, and Accept, Decline and
//! Tentative. For an occurrence of a series a drop-down picks this event or all events in the series (the series'
//! master answered). Unticked, the answer isn't sent to the organizer and the message goes nowhere. The account's
//! answer is the one marked; a declined meeting leaves the calendar, as in Teams, any other answer stays on the card.

use std::rc::Rc;

use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Styled as _, WeakEntity, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, Selectable as _, Sizable as _};
use teams_core::{CalendarEvent, EventResponse, EventWrite, RsvpAnswer};

use super::CalendarView;
use crate::icons::{Glyph, selected_by};
use crate::theme::Palette;

/// What the card says when an answer didn't go through.
pub const RSVP_NOT_SENT: &str = "Your response couldn't be sent. Check the connection, then try again.";

/// The card's answer as it is being given: for the whole series or not, told to the organizer or not, its message.
pub(super) struct RsvpDraft {
    /// The event whose card it is.
    event: String,
    /// For the whole series (its master answered).
    series: bool,
    /// "Email organizer": the answer is sent to the organizer, with the message.
    email: bool,
    pub(super) note: Entity<InputState>,
    sending: bool,
}

/// Whether `event` is an invitation the account answers: someone else's meeting, standing, that asks for an answer.
pub(super) fn answerable(event: &CalendarEvent, me: &str) -> bool {
    !event.is_organizer
        && !event.cancelled
        && !super::own_channel_meeting(event, me)
        && !matches!(event.response, EventResponse::Organizer | EventResponse::NoResponse)
}

/// The answer the account gave, as the buttons mark it.
fn given(event: &CalendarEvent) -> Option<RsvpAnswer> {
    match event.response {
        EventResponse::Accepted => Some(RsvpAnswer::Accept),
        EventResponse::Tentative => Some(RsvpAnswer::Tentative),
        EventResponse::Declined => Some(RsvpAnswer::Decline),
        _ => None,
    }
}

/// Each answer's selector key, glyph and words, in the peek's order.
const ANSWERS: [(RsvpAnswer, &str, Glyph, &str); 3] = [
    (RsvpAnswer::Accept, "accept", Glyph::CHECK, "Accept"),
    (RsvpAnswer::Decline, "decline", Glyph::X, "Decline"),
    (RsvpAnswer::Tentative, "tentative", Glyph::CIRCLE_QUESTION, "Tentative"),
];

fn key(answer: RsvpAnswer) -> &'static str {
    ANSWERS
        .iter()
        .find(|(each, ..)| *each == answer)
        .map_or("", |(_, key, ..)| key)
}

impl CalendarView {
    /// Makes the card's answer ready when its event is an invitation: its message box, "Email organizer" ticked.
    pub(super) fn ready_rsvp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(event) = self.card.as_ref().map(|shown| Rc::clone(&shown.event)) else {
            self.rsvp = None;
            return;
        };
        if self.core.read_only() || !answerable(&event, self.core.me()) {
            self.rsvp = None;
            return;
        }
        if self.rsvp.as_ref().is_some_and(|draft| draft.event == event.id) {
            return;
        }
        let note = cx.new(|cx| InputState::new(window, cx).placeholder("Add a message (optional)"));
        self.rsvp = Some(RsvpDraft {
            event: event.id.clone(),
            series: false,
            email: true,
            note,
            sending: false,
        });
    }

    /// Whether the card's message box (the answer's or the cancellation's) has the keyboard.
    pub(super) fn writing(&self, window: &Window, cx: &App) -> bool {
        let answer = self.rsvp.as_ref().map(|draft| &draft.note);
        answer
            .into_iter()
            .chain(self.removal_note.as_ref())
            .chain(
                self.join_id
                    .as_ref()
                    .into_iter()
                    .flat_map(super::join_id::JoinIdForm::inputs),
            )
            .chain(self.meet_now.as_ref().map(super::meet_now::MeetNowForm::input))
            .chain(self.others.adding.as_ref().map(super::people::AddCalendar::input))
            .any(|note| note.read(cx).focus_handle(cx).is_focused(window))
    }

    /// The card's answer to its invitation (see the module docs), once [`Self::ready_rsvp`] made it ready.
    pub(super) fn rsvp_view(
        &self,
        event: &CalendarEvent,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let draft = self.rsvp.as_ref().filter(|draft| draft.event == event.id)?;
        let occurrence = event.kind.recurring() && event.series_master_id.is_some();
        let email = draft.email;
        let heading = if occurrence {
            Self::rsvp_scope(draft.series, cx)
        } else {
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("RSVP to this event")
                .into_any_element()
        };
        let tick = selected_by(
            "calendar-card-rsvp-email",
            Checkbox::new("calendar-card-rsvp-email")
                .label("Email organizer")
                .checked(email)
                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                    if let Some(draft) = this.rsvp.as_mut() {
                        draft.email = *checked;
                    }
                    cx.notify();
                })),
        );
        let buttons = ANSWERS.map(|(answer, key, glyph, words)| {
            let chosen = given(event) == Some(answer);
            selected_by(
                format!("calendar-card-rsvp-{key}"),
                Button::new(gpui::SharedString::from(format!("calendar-card-rsvp-{key}")))
                    .icon(glyph.icon())
                    .label(words)
                    .outline()
                    .selected(chosen)
                    .when(chosen, |button| button.text_color(rgb(palette.brand)))
                    .disabled(draft.sending)
                    .on_click(cx.listener(move |this, _, _, cx| this.answer_carded(answer, cx))),
            )
        });
        Some(
            div()
                .debug_selector(|| "calendar-card-rsvp".to_owned())
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().min_w_0().flex().child(heading))
                        .child(tick),
                )
                .child(
                    div()
                        .debug_selector(|| "calendar-card-rsvp-note".to_owned())
                        .when(!email, |note| note.opacity(0.5))
                        .child(Input::new(&draft.note).disabled(!email)),
                )
                .child(div().flex().items_center().gap_2().children(buttons)),
        )
    }

    /// For an occurrence: what the answer is to, this event or all events in the series, as a drop-down.
    fn rsvp_scope(series: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().downgrade();
        let words = if series {
            "RSVP to all events in the series"
        } else {
            "RSVP to this event"
        };
        selected_by(
            "calendar-card-rsvp-scope",
            Button::new("calendar-card-rsvp-scope")
                .ghost()
                .small()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(words)
                        .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0))),
                )
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu.min_w(px(220.0));
                    for (whole, selector, label) in [
                        (false, "calendar-rsvp-scope-event", "This event"),
                        (true, "calendar-rsvp-scope-series", "All events in the series"),
                    ] {
                        let view = view.clone();
                        menu = menu.item(
                            PopupMenuItem::element(move |_, _| {
                                div().debug_selector(move || selector.to_owned()).child(label)
                            })
                            .checked(whole == series)
                            .on_click(move |_, _, cx: &mut App| {
                                let _ = view.update(cx, |view, cx| view.rsvp_for_series(whole, cx));
                            }),
                        );
                    }
                    menu
                }),
        )
        .into_any_element()
    }

    /// The answer is to the whole series (`true`) or to this event.
    pub fn rsvp_for_series(&mut self, series: bool, cx: &mut Context<Self>) {
        if let Some(draft) = self.rsvp.as_mut() {
            draft.series = series;
        }
        cx.notify();
    }

    /// Gives `answer` to the carded invitation: to the event or its series as picked, told to the organizer with the
    /// message while "Email organizer" is ticked.
    pub fn answer_carded(&mut self, answer: RsvpAnswer, cx: &mut Context<Self>) {
        let Some(draft) = self.rsvp.as_ref() else { return };
        let comment = draft
            .email
            .then(|| draft.note.read(cx).value().trim().to_owned())
            .filter(|comment| !comment.is_empty());
        let (series, email) = (draft.series, draft.email);
        self.send_rsvp(answer, series, email, comment, cx);
    }

    /// Sends the card's answer `answer` to its invitation, for the whole series when `series`; told to the organizer
    /// when `send`, with `comment`. Once it went through the card shows it (a declined meeting closes the card and
    /// leaves the calendar) and the days shown are read again.
    pub fn send_rsvp(
        &mut self,
        answer: RsvpAnswer,
        series: bool,
        send: bool,
        comment: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(event) = self.card.as_ref().map(|shown| Rc::clone(&shown.event)) else {
            return;
        };
        let id = if series {
            event.series_master_id.clone().unwrap_or_else(|| event.id.clone())
        } else {
            event.id.clone()
        };
        if let Some(draft) = self.rsvp.as_mut() {
            draft.sending = true;
        }
        self.trace
            .record(&crate::trace::TraceLine::new("calendar-event", "answered").kind(key(answer)));
        let write = EventWrite::Respond {
            id,
            answer,
            send,
            comment,
        };
        let touched = write.touched();
        let reply = self.core.write_event(write);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let written = reply.await.ok().map(|_| touched);
            let _ = this.update(cx, |view, cx| view.rsvp_sent(&event, answer, written.as_ref(), cx));
        })
        .detach();
        cx.notify();
    }

    /// The answer to `event` went through (`written`: what it touched) or didn't.
    fn rsvp_sent(
        &mut self,
        event: &CalendarEvent,
        answer: RsvpAnswer,
        written: Option<&teams_core::Touched>,
        cx: &mut Context<Self>,
    ) {
        if let Some(draft) = self.rsvp.as_mut() {
            draft.sending = false;
        }
        let Some(touched) = written else {
            self.say(RSVP_NOT_SENT, cx);
            return;
        };
        // the message went with this answer: the next one starts afresh
        self.rsvp = None;
        let open = self.card.as_ref().is_some_and(|shown| shown.event.id == event.id);
        if open {
            if answer == RsvpAnswer::Decline {
                self.close_card(cx);
            } else if let Some(shown) = self.card.as_mut() {
                let mut answered = (*shown.event).clone();
                answered.response = match answer {
                    RsvpAnswer::Accept => EventResponse::Accepted,
                    RsvpAnswer::Tentative | RsvpAnswer::Decline => EventResponse::Tentative,
                };
                shown.event = Rc::new(answered);
            }
        }
        self.written(touched, cx);
        cx.notify();
    }
}
