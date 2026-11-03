//! An event's card: its title, when, its series, where, the organizer and the account's answer, who is invited and
//! its text; for a Teams meeting the split Join (any time, started or not; its menu has Join and Copy join link, as in
//! Teams) and Chat with participants.

use std::fmt::Write as _;
use std::rc::Rc;

use gpui::{
    App, ClipboardItem, Context, InteractiveElement as _, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, div, px, relative, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, Sizable as _};
use teams_core::{CalendarEvent, EventResponse};

use super::{CalendarView, CalendarViewEvent, Removal, Shown, mix, time_text};
use crate::calls::MeetingToJoin;
use crate::clock::{self, HourCycle};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::{self, Palette};

impl CalendarView {
    /// The open event's card, over the grid.
    pub(super) fn card_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let shown = self.card.clone()?;
        let event = Rc::clone(&shown.event);
        let cycle = clock::cycle(cx);
        let row = |glyph: Glyph, text: String| card_row(glyph, text, palette);
        let when = when_text(&shown, cycle);
        let organizer = organizer_text(&event);
        let invited = invited_text(&event);
        let actions = self.card_actions(&shown, palette, cx);
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
        // a channel's meeting the account scheduled is its own, though its calendar holds an attendee's copy of it
        let in_channel = super::own_channel_meeting(event, self.core.me());
        let own = event.is_organizer || in_channel;
        let meeting = own && (in_channel || !event.attendees.is_empty()) && !event.cancelled;
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

    /// What the card's Join goes into: the meeting's call when its event names its chat and organizer, else its link,
    /// opened in the browser (a short `/meet/` link names neither); nothing in a build or a mode that doesn't go into
    /// calls.
    fn join_target(&self, shown: &Shown, cx: &Context<Self>) -> Option<JoinTarget> {
        let event = &shown.event;
        if self.core.read_only() || !self.calls.read(cx).shows() {
            return None;
        }
        match (&event.meeting_chat, &event.meeting) {
            (Some(chat), Some(meeting)) => Some(JoinTarget::Call(MeetingToJoin {
                chat: chat.clone(),
                meeting: meeting.clone(),
                title: shown.title(),
            })),
            // a short `/meet/` link names no organizer: the meeting's chat does
            (Some(chat), None) => Some(JoinTarget::Chat {
                chat: chat.clone(),
                title: shown.title(),
                link: event.join_url.clone(),
            }),
            _ => event.join_url.clone().map(JoinTarget::Link),
        }
    }

    /// The split Join: the meeting's join, and its menu's Join and Copy join link, as in Teams.
    fn join_button(target: JoinTarget, link: Option<String>, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let view = cx.entity().downgrade();
        let join = move |view: &gpui::WeakEntity<Self>, target: &JoinTarget, cx: &mut App| {
            let target = target.clone();
            let _ = view.update(cx, |this, cx| this.join(target, cx));
        };
        let (face_view, face_target) = (view.clone(), target.clone());
        let face = div()
            .debug_selector(|| "calendar-card-join".to_owned())
            .id("calendar-card-join")
            .h(px(32.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded_l_md()
            .bg(rgb(palette.brand))
            .text_color(rgb(palette.on_brand))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .cursor_pointer()
            .hover(move |part| part.bg(rgb(palette.brand_hover)))
            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Join Teams meeting").build(window, cx))
            .child(Glyph::VIDEO.icon().size(px(16.0)))
            .child("Join")
            .on_click(move |_, _, cx: &mut App| join(&face_view, &face_target, cx));
        let menu = selected_by(
            "calendar-card-join-more",
            Button::new("calendar-card-join-more")
                .h(px(32.0))
                .px_2()
                .rounded_r_md()
                .bg(rgb(palette.brand))
                .text_color(rgb(palette.on_brand))
                .tooltip("More join options")
                .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0)))
                .dropdown_menu(move |menu, _, _| {
                    let (view, target) = (view.clone(), target.clone());
                    let menu = menu.min_w(px(200.0)).item(
                        PopupMenuItem::element(|_, _| {
                            menu_row("calendar-card-join-now", Glyph::VIDEO, "Join Teams meeting")
                        })
                        .on_click(move |_, _, cx: &mut App| join(&view, &target, cx)),
                    );
                    match link.clone() {
                        Some(link) => menu.item(
                            PopupMenuItem::element(|_, _| {
                                menu_row("calendar-card-copy-link", Glyph::LINK, "Copy join link")
                            })
                            .on_click(move |_, _, cx: &mut App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                            }),
                        ),
                        None => menu,
                    }
                }),
        );
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(1.0))
            .child(face)
            .child(menu)
    }

    /// Goes into the meeting: its pre-join, or its link in the browser.
    fn join(&mut self, target: JoinTarget, cx: &mut Context<Self>) {
        self.card = None;
        match target {
            JoinTarget::Call(to_join) => cx.emit(CalendarViewEvent::Join(to_join)),
            JoinTarget::Chat { chat, title, link } => cx.emit(CalendarViewEvent::JoinChat { chat, title, link }),
            JoinTarget::Link(link) => cx.open_url(&link),
        }
        cx.notify();
    }

    /// A meeting's card's actions: the split Join (Copy join link is in its menu, or beside Chat where no Join is
    /// offered: the read-only mode) and Chat with participants.
    pub(super) fn card_actions(&self, shown: &Shown, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let event = &shown.event;
        if !event.online || event.cancelled {
            return None;
        }
        let chat = event.meeting_chat.clone();
        let target = self.join_target(shown, cx);
        let offers_join = target.is_some();
        let join = target.map(|target| Self::join_button(target, event.join_url.clone(), palette, cx));
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
        let copy = event.join_url.clone().filter(|_| !offers_join).map(|link| {
            selected_by(
                "calendar-card-copy-link",
                icon_button("calendar-card-copy-link", Glyph::LINK, "Copy join link")
                    .on_click(move |_, _, cx: &mut App| cx.write_to_clipboard(ClipboardItem::new_string(link.clone()))),
            )
        });
        Some(
            div()
                .pt_2()
                .flex()
                .items_center()
                .gap_2()
                .children(join)
                .children(open_chat)
                .child(div().flex_1())
                .children(copy),
        )
    }
}

/// What the card's Join goes into.
#[derive(Clone)]
enum JoinTarget {
    /// The meeting's call, by its pre-join.
    Call(MeetingToJoin),
    /// The meeting's call by its pre-join, once its chat named its organizer; its link otherwise.
    Chat {
        chat: String,
        title: String,
        link: Option<String>,
    },
    /// Its link, opened in the browser: the event doesn't name what going into its call takes.
    Link(String),
}

/// A row of Join's menu: its glyph and words.
fn menu_row(selector: &'static str, glyph: Glyph, words: &'static str) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .flex()
        .items_center()
        .gap_2()
        .child(glyph.icon().size(px(14.0)))
        .child(words)
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
