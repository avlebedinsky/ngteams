//! The window's cards of the account's meeting notices, at its bottom right over everything, the newest at the bottom:
//! a started meeting's ([`crate::meeting_alert`]: Join, Message but for a channel's, and Dismiss; it goes after
//! Teams' banner time) and a calendar meeting's reminder ([`crate::meeting_reminder`]: Join, Snooze with its choices,
//! and Dismiss; it stays until it is acted on, or the meeting moves, ends or is joined). The desktop's notice of each
//! shows as well; acting on either closes both.

use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, App, Context, Entity, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, Styled as _, Subscription, div, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};

use super::Shell;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::meeting_alert::{MeetingAlert, StartedCard};
use crate::meeting_reminder::{self, MeetingReminders, ReminderCard, SNOOZE_CHOICES, Stage};
use crate::notify::{DEFAULT_ACTION, JOIN_ACTION};
use crate::theme::Palette;

/// How wide a card is.
const CARD_WIDTH: f32 = 360.0;

/// The account's meeting notices the shell shows as cards.
pub struct MeetingNotices {
    pub alert: Entity<MeetingAlert>,
    pub reminders: Entity<MeetingReminders>,
    _observing: [Subscription; 2],
}

impl Shell {
    /// Hands in the account's started meetings' banners and its meeting reminders: the window shows them as cards.
    pub fn set_meeting_notices(
        &mut self,
        alert: Entity<MeetingAlert>,
        reminders: Entity<MeetingReminders>,
        cx: &mut Context<Self>,
    ) {
        let observing = [
            cx.observe(&alert, |_, _, cx| cx.notify()),
            cx.observe(&reminders, |_, _, cx| cx.notify()),
        ];
        self.meeting_notices = Some(MeetingNotices {
            alert,
            reminders,
            _observing: observing,
        });
        cx.notify();
    }

    /// The account's meeting reminders, once the desktop handed them in.
    #[must_use]
    pub fn meeting_reminders(&self) -> Option<&Entity<MeetingReminders>> {
        self.meeting_notices.as_ref().map(|notices| &notices.reminders)
    }

    /// The cards, when any shows.
    pub(super) fn meeting_cards_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notices = self.meeting_notices.as_ref()?;
        let started = notices.alert.read(cx).cards();
        let reminders = notices.reminders.read(cx).cards(cx);
        if started.is_empty() && reminders.is_empty() {
            return None;
        }
        let now = meeting_reminder::wall_ms(cx);
        let (alert, reminder_model) = (notices.alert.clone(), notices.reminders.clone());
        let cards = reminders
            .into_iter()
            .zip(0_u64..)
            .map(|(card, key)| reminder_card((key, card), &reminder_model, now, palette))
            .chain(started.into_iter().map(|card| started_card(card, &alert, palette)));
        Some(
            gpui::deferred(
                div()
                    .debug_selector(|| "meeting-cards".to_owned())
                    .absolute()
                    .right(px(16.0))
                    .bottom(px(16.0))
                    .w(px(CARD_WIDTH))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(cards),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

/// What a card shows: its test selector, its mark, its title and lines, its actions and its close button.
struct Face {
    selector: &'static str,
    /// Tells cards of one kind apart, for the state of their controls.
    key: u64,
    mark: Glyph,
    title: String,
    lines: Vec<AnyElement>,
    actions: Vec<AnyElement>,
    close: Button,
}

/// A card's frame: a raised surface with its mark, its title and its close button over its lines and actions.
fn frame(face: Face, palette: Palette) -> AnyElement {
    let Face {
        selector,
        key,
        mark,
        title,
        lines,
        actions,
        close,
    } = face;
    let mark = div()
        .flex_none()
        .size(px(32.0))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(crate::theme::mix(palette.surface, palette.brand, 0.16)))
        .text_color(rgb(palette.brand))
        .child(mark.icon().size(px(16.0)));
    let head = div()
        .flex()
        .items_start()
        .gap_3()
        .child(mark)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(title)),
                )
                .children(lines),
        )
        .child(close);
    div()
        .debug_selector(move || selector.to_owned())
        .id(gpui::ElementId::NamedInteger(selector.into(), key))
        .occlude()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .text_color(rgb(palette.foreground))
        .shadow_lg()
        .flex()
        .flex_col()
        .gap_3()
        .child(head)
        .child(div().flex().justify_end().gap_2().children(actions))
        .into_any_element()
}

/// A muted line of a card.
fn line(text: String, palette: Palette) -> AnyElement {
    div()
        .text_sm()
        .text_color(rgb(palette.muted_foreground))
        .child(SharedString::from(text))
        .into_any_element()
}

/// The card's primary Join: an icon and the word, in the brand's colour.
fn join_button(id: &'static str, on_click: impl Fn(&mut App) + 'static) -> AnyElement {
    selected_by(
        id,
        Button::new(id)
            .primary()
            .small()
            .icon(Glyph::VIDEO.icon())
            .label("Join")
            .on_click(move |_, _, cx| on_click(cx)),
    )
    .into_any_element()
}

/// A started meeting's card.
fn started_card(card: StartedCard, alert: &Entity<MeetingAlert>, palette: Palette) -> AnyElement {
    let id = card.id;
    let act = |action: &'static str| {
        let alert = alert.clone();
        move |cx: &mut App| {
            alert.update(cx, |alert, cx| {
                alert.activated(id, action, cx);
            });
        }
    };
    let mut actions = vec![join_button("meeting-card-join", act(JOIN_ACTION))];
    if card.opens_chat {
        let open = act(DEFAULT_ACTION);
        actions.insert(
            0,
            selected_by(
                "meeting-card-message",
                Button::new("meeting-card-message")
                    .outline()
                    .small()
                    .icon(Glyph::MESSAGE.icon())
                    .label("Message")
                    .on_click(move |_, _, cx| open(cx)),
            )
            .into_any_element(),
        );
    }
    let dismissed = alert.clone();
    let close = icon_button("meeting-card-dismiss", Glyph::X, "Dismiss")
        .xsmall()
        .on_click(move |_, _, cx| dismissed.update(cx, |alert, cx| alert.dismiss(id, cx)));
    frame(
        Face {
            selector: "meeting-card-started",
            key: u64::from(card.id),
            mark: Glyph::VIDEO,
            title: card.title,
            lines: vec![line(card.body, palette)],
            actions,
            close,
        },
        palette,
    )
}

/// A meeting reminder's card.
fn reminder_card(
    (key, card): (u64, ReminderCard),
    reminders: &Entity<MeetingReminders>,
    now: i64,
    palette: Palette,
) -> AnyElement {
    let occasion = Rc::new(card.occasion.clone());
    let when = if card.stage == Stage::Start {
        meeting_reminder::when_text(card.start, card.start)
    } else {
        meeting_reminder::when_text(card.start, now)
    };
    let mut lines = vec![line(when, palette)];
    lines.extend(card.times.map(|times| line(times, palette)));
    let mut actions = vec![snooze_button(&occasion, reminders, card.stage)];
    let dismiss = {
        let (reminders, occasion) = (reminders.clone(), Rc::clone(&occasion));
        selected_by(
            "meeting-reminder-dismiss",
            Button::new("meeting-reminder-dismiss")
                .outline()
                .small()
                .label("Dismiss")
                .on_click(move |_, _, cx| reminders.update(cx, |reminders, cx| reminders.dismiss(&occasion, cx))),
        )
        .into_any_element()
    };
    actions.push(dismiss);
    if card.joinable {
        let (joined, occasion) = (reminders.clone(), Rc::clone(&occasion));
        actions.push(join_button("meeting-reminder-join", move |cx| {
            joined.update(cx, |reminders, cx| reminders.join(&occasion, cx));
        }));
    }
    let close = {
        let (reminders, occasion) = (reminders.clone(), Rc::clone(&occasion));
        icon_button("meeting-reminder-close", Glyph::X, "Dismiss")
            .xsmall()
            .on_click(move |_, _, cx| reminders.update(cx, |reminders, cx| reminders.dismiss(&occasion, cx)))
    };
    frame(
        Face {
            selector: "meeting-card-reminder",
            key,
            mark: Glyph::CALENDAR_CLOCK,
            title: card.title,
            lines,
            actions,
            close,
        },
        palette,
    )
}

/// Snooze and its choices; "Until it starts" only before the meeting starts.
fn snooze_button(
    occasion: &Rc<meeting_reminder::Occasion>,
    reminders: &Entity<MeetingReminders>,
    stage: Stage,
) -> AnyElement {
    let choices: Vec<u32> = SNOOZE_CHOICES
        .into_iter()
        .filter(|minutes| *minutes != 0 || stage == Stage::Before)
        .collect();
    let (occasion, reminders) = (Rc::clone(occasion), reminders.clone());
    selected_by(
        "meeting-reminder-snooze",
        Button::new("meeting-reminder-snooze")
            .outline()
            .small()
            .label("Snooze")
            .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0)))
            .dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, _| {
                choices.iter().fold(menu.min_w(px(160.0)), |menu, minutes| {
                    let (minutes, occasion, reminders) = (*minutes, Rc::clone(&occasion), reminders.clone());
                    let selector = format!("meeting-reminder-snooze-{minutes}");
                    let label = SharedString::from(meeting_reminder::snooze_label(minutes));
                    menu.item(
                        PopupMenuItem::element(move |_, _| {
                            let selector = selector.clone();
                            div().debug_selector(move || selector.clone()).child(label.clone())
                        })
                        .on_click(move |_, _, cx| {
                            reminders.update(cx, |reminders, cx| reminders.snooze(&occasion, minutes, cx));
                        }),
                    )
                })
            }),
    )
    .into_any_element()
}
