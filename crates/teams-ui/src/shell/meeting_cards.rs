//! The window's cards of the account's meeting notices, the newest at the bottom: at the bottom right of the open chat's
//! messages, clear of its header and its compose box, else of the window, over everything else:
//! a started meeting's ([`crate::meeting_alert`]: Join, Message but for a channel's, and Dismiss; it goes after
//! Teams' banner time) and a calendar meeting's reminder ([`crate::meeting_reminder`]: Join, Snooze with its choices,
//! and Dismiss; it stays until it is acted on, or the meeting moves, ends or is joined). The desktop's notice of each
//! shows as well; acting on either closes both.
//!
//! At most [`MAX_CARDS`] show at once, as Teams' own notifications stack: the started meetings' and the soonest
//! reminders; "+N more" shows the rest, and the stack scrolls when the room is too short for it. A card is as narrow
//! as the room asks.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, App, Bounds, Context, Entity, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    anchored, canvas, div, point, px, rgb,
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

/// How wide a card is, room allowing.
const CARD_WIDTH: f32 = 360.0;
/// How narrow a card gets, past the messages' width when they are narrower.
const MIN_CARD_WIDTH: f32 = 260.0;
/// The room kept between the cards and the edges of the place they show in.
const MARGIN: f32 = 16.0;
/// How many cards show at once before "+N more".
pub const MAX_CARDS: usize = 3;

/// The account's meeting notices the shell shows as cards.
pub struct MeetingNotices {
    pub alert: Entity<MeetingAlert>,
    pub reminders: Entity<MeetingReminders>,
    /// Whether "+N more" was clicked: every card shows.
    all_shown: bool,
    /// The chat's messages area the cards were placed in, of the frame before: a frame that drew it elsewhere (or
    /// none) draws them again.
    placed_in: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Where the stack and how many cards were drawn in the last frame, for a test to read.
    drawn: Rc<Cell<Option<CardsDrawn>>>,
    _observing: [Subscription; 2],
}

/// Where the cards were drawn: the stack's bounds, the room it had, how many cards it showed and how many "+N more"
/// held back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardsDrawn {
    pub stack: Bounds<Pixels>,
    /// The chat's messages area it was placed in; `None`: the window's corner.
    pub area: Option<Bounds<Pixels>>,
    pub shown: usize,
    pub more: usize,
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
            all_shown: false,
            placed_in: Rc::default(),
            drawn: Rc::default(),
            _observing: observing,
        });
        cx.notify();
    }

    /// The account's meeting reminders, once the desktop handed them in.
    #[must_use]
    pub fn meeting_reminders(&self) -> Option<&Entity<MeetingReminders>> {
        self.meeting_notices.as_ref().map(|notices| &notices.reminders)
    }

    /// Where the cards were drawn in the last frame, while any shows.
    #[must_use]
    pub fn meeting_cards_drawn(&self) -> Option<CardsDrawn> {
        self.meeting_notices.as_ref()?.drawn.get()
    }

    /// The cards, when any shows, and what draws them again once the chat's messages area they go in moved.
    pub(super) fn meeting_cards_view(
        &mut self,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let area_drawn = self.chat.as_ref().map(|chat| chat.read(cx).messages_area());
        let Some(notices) = self.meeting_notices.as_mut() else {
            return Vec::new();
        };
        // what the chat drew last frame; a frame that doesn't draw it leaves none
        let area = area_drawn.as_ref().and_then(|drawn| drawn.take());
        notices.placed_in.set(area);
        let mut started = notices.alert.read(cx).cards();
        let mut reminders = notices.reminders.read(cx).cards(cx);
        let total = started.len() + reminders.len();
        if total <= MAX_CARDS {
            notices.all_shown = false;
        }
        if total == 0 {
            notices.drawn.set(None);
            return Vec::new();
        }
        if !notices.all_shown {
            started.drain(..started.len().saturating_sub(MAX_CARDS));
            reminders.truncate(MAX_CARDS - started.len());
        }
        let shown = started.len() + reminders.len();
        let now = meeting_reminder::wall_ms(cx);
        let (alert, reminder_model) = (notices.alert.clone(), notices.reminders.clone());
        let more = more_button(total - shown, notices.all_shown, cx);
        let cards = reminders
            .into_iter()
            .zip(0_u64..)
            .map(|(card, key)| reminder_card((key, card), &reminder_model, now, palette))
            .chain(started.into_iter().map(|card| started_card(card, &alert, palette)));
        let drawn = Rc::clone(&notices.drawn);
        let record = move |stack| {
            drawn.set(Some(CardsDrawn {
                stack,
                area,
                shown,
                more: total - shown,
            }));
        };
        let stack = div()
            .id("meeting-cards")
            .debug_selector(|| "meeting-cards".to_owned())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                canvas(move |bounds, _, _| record(bounds), |_, (), _, _| {})
                    .absolute()
                    .size_full(),
            )
            .children(more)
            .children(cards);
        let follow = follow_area(Rc::clone(&notices.placed_in), area_drawn);
        vec![Room::of(area, window).place(stack), follow]
    }
}

/// Drawn after the chat: once its messages area moved (or went) since the cards were placed in it, they are placed
/// again.
fn follow_area(
    placed_in: Rc<Cell<Option<Bounds<Pixels>>>>,
    area_drawn: Option<Rc<Cell<Option<Bounds<Pixels>>>>>,
) -> AnyElement {
    canvas(
        move |_, window, _| {
            let now = area_drawn.as_ref().and_then(|drawn| drawn.get());
            if now != placed_in.get() {
                // a refresh in prepaint does nothing: the next frame asks for it
                window.on_next_frame(|window, _| window.refresh());
            }
        },
        |_, (), _, _| {},
    )
    .absolute()
    .size_0()
    .into_any_element()
}

/// The room the cards have: the corner they stand on (their bottom right), how wide and how tall they may be.
struct Room {
    corner: gpui::Point<Pixels>,
    width: Pixels,
    height: Pixels,
}

impl Room {
    /// The bottom right of the chat's messages `area`, else of the window below its title bar.
    fn of(area: Option<Bounds<Pixels>>, window: &Window) -> Self {
        let viewport = window.viewport_size();
        let (window_w, window_h) = (f32::from(viewport.width), f32::from(viewport.height));
        let (right, bottom, top, wide) = match area {
            Some(area) => (
                f32::from(area.right()) - MARGIN,
                f32::from(area.bottom()) - MARGIN,
                f32::from(area.top()) + MARGIN,
                f32::from(area.size.width) - 2.0 * MARGIN,
            ),
            None => (
                window_w - MARGIN,
                window_h - MARGIN,
                super::title_bar::TITLE_HEIGHT + MARGIN,
                window_w - 2.0 * MARGIN,
            ),
        };
        let width = CARD_WIDTH.min(wide.max(MIN_CARD_WIDTH)).min(window_w - MARGIN);
        Self {
            corner: point(px(right), px(bottom)),
            width: px(width.max(0.0)),
            height: px((bottom - top).max(0.0)),
        }
    }

    /// `stack` over everything, its bottom right on the corner, as wide as the room and scrolling past its height.
    fn place(&self, stack: gpui::Stateful<gpui::Div>) -> AnyElement {
        gpui::deferred(
            anchored()
                .anchor(Anchor::BottomRight)
                .position(self.corner)
                .snap_to_window_with_margin(px(MARGIN / 2.0))
                .child(stack.relative().w(self.width).max_h(self.height).overflow_y_scroll()),
        )
        .with_priority(1)
        .into_any_element()
    }
}

/// "+N more" while cards are held back; "Show fewer" once they all show.
fn more_button(held: usize, all_shown: bool, cx: &mut Context<Shell>) -> Option<AnyElement> {
    let (label, show_all) = match (held, all_shown) {
        (0, false) => return None,
        (_, true) => ("Show fewer".to_owned(), false),
        (held, false) => (format!("+{held} more"), true),
    };
    Some(
        div()
            .flex()
            .justify_end()
            .child(selected_by(
                "meeting-cards-more",
                Button::new("meeting-cards-more")
                    .outline()
                    .small()
                    .label(label)
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        if let Some(notices) = shell.meeting_notices.as_mut() {
                            notices.all_shown = show_all;
                            cx.notify();
                        }
                    })),
            ))
            .into_any_element(),
    )
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
