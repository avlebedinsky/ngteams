//! The day beside the scheduling form: the date's hours, the new event's block in green with its two handles, and
//! the day's other events; the previous and next days, today, and the date's own picker above them.

use chrono::{Duration, NaiveTime, Timelike as _};
use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};

use super::model::{self, STEP_MINUTES};
use super::{Drag, EventForm};
use crate::calendar_view::mix;
use crate::clock;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::{self, Palette};

/// One hour's height in the day, px.
pub(super) const HOUR: f32 = 64.0;
/// The time gutter's width, px.
pub(super) const GUTTER: f32 = 60.0;

/// What a drag of the new event's block moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Part {
    /// The whole block.
    Whole,
    /// Its top: the start.
    Start,
    /// Its bottom: the end.
    End,
}

/// A time of day's offset from the top of the day, px.
pub(super) fn top_of(time: NaiveTime) -> f32 {
    let minutes = u16::try_from(time.hour() * 60 + time.minute()).unwrap_or(0);
    f32::from(minutes) / 60.0 * HOUR
}

impl EventForm {
    /// The day beside the form.
    pub(super) fn day_view(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let body = div()
            .id("event-day-hours")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.day_scroll)
            .child(
                div()
                    .flex()
                    .child(self.gutter(palette, cx))
                    .child(self.day_column(palette, cx)),
            );
        div()
            .debug_selector(|| "event-day".to_owned())
            .w(px(440.0))
            .flex_none()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .overflow_hidden()
            .child(self.day_header(palette, cx))
            .child(body)
    }

    /// The day's header (the day and the Scheduler share it): the previous and next days, the event's day, and the
    /// date's own picker.
    pub(super) fn day_header(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let day = self.day;
        let form = cx.entity().downgrade();
        let date_menu = Button::new("event-day-date")
            .ghost()
            .small()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(model::day_title(day))
                    .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0))),
            )
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.min_w(px(220.0)).scrollable(true).max_h(px(420.0));
                (0..42).fold(menu, |menu, offset| {
                    let date = chrono::Local::now().date_naive() + Duration::days(offset - 7);
                    let form = form.clone();
                    menu.item(
                        PopupMenuItem::new(model::day_title(date))
                            .checked(date == day)
                            .on_click(move |_, _, cx: &mut App| {
                                let _ = form.update(cx, |form, cx| form.show_day(date, cx));
                            }),
                    )
                })
            });
        div()
            .h(px(52.0))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(selected_by(
                "event-day-previous",
                icon_button("event-day-previous", Glyph::CHEVRON_LEFT, "Previous day")
                    .small()
                    .on_click(cx.listener(move |form, _, _, cx| form.show_day(day - Duration::days(1), cx))),
            ))
            .child(selected_by(
                "event-day-today",
                icon_button("event-day-today", Glyph::CALENDAR_TODAY, "Go to the event's day")
                    .small()
                    .on_click(cx.listener(|form, _, _, cx| {
                        let date = form.slot.start.date();
                        form.show_day(date, cx);
                    })),
            ))
            .child(selected_by(
                "event-day-next",
                icon_button("event-day-next", Glyph::CHEVRON_RIGHT, "Next day")
                    .small()
                    .on_click(cx.listener(move |form, _, _, cx| form.show_day(day + Duration::days(1), cx))),
            ))
            .child(selected_by("event-day-date", date_menu))
    }

    pub(super) fn gutter(&self, palette: Palette, cx: &App) -> gpui::Div {
        let cycle = clock::cycle(cx);
        div()
            .w(px(GUTTER))
            .flex_none()
            .relative()
            .h(px(24.0 * HOUR))
            .border_r_1()
            .border_color(rgb(palette.border))
            .children((1..24).map(|hour| {
                let label = crate::calendar_view::hour_label(hour, cycle);
                div()
                    .absolute()
                    .top(px(f32::from(u8::try_from(hour).unwrap_or(0)) * HOUR + 4.0))
                    .left(px(8.0))
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(label)
            }))
    }

    fn day_column(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let day = self.day;
        let cells = (0..48u32).map(|half| {
            let top = f32::from(u16::try_from(half).unwrap_or(0)) * HOUR / 2.0;
            let start = day.and_time(NaiveTime::MIN) + Duration::minutes(i64::from(half) * 30);
            div()
                .id(SharedString::from(format!("event-day-slot-{half}")))
                .absolute()
                .top(px(top))
                .left_0()
                .right_0()
                .h(px(HOUR / 2.0))
                .border_t_1()
                .border_color(rgb(if half % 2 == 0 {
                    palette.border
                } else {
                    mix(palette.surface, palette.border, 500)
                }))
                .on_click(cx.listener(move |form, _, _, cx| {
                    if form.drag.is_none() {
                        let slot = form.slot.starting(start);
                        form.set_slot(slot, cx);
                    }
                }))
        });
        let others: Vec<AnyElement> = self
            .day_events
            .iter()
            .filter(|shown| !shown.all_day() && shown.on(day))
            .map(|shown| {
                let start = if shown.start.date() < day {
                    NaiveTime::MIN
                } else {
                    shown.start.time()
                };
                let end_top = if shown.end.date() > day {
                    24.0 * HOUR
                } else {
                    top_of(shown.end.time())
                };
                let top = top_of(start);
                div()
                    .absolute()
                    .top(px(top))
                    .left(px(4.0))
                    .right(px(4.0))
                    .h(px((end_top - top).max(22.0)))
                    .px_2()
                    .py(px(2.0))
                    .overflow_hidden()
                    .rounded_sm()
                    .border_l_2()
                    .border_color(rgb(palette.brand))
                    .bg(rgb(mix(palette.surface, palette.foreground, 120)))
                    .text_sm()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(shown.title())
                    .into_any_element()
            })
            .collect();
        div()
            .flex_1()
            .relative()
            .h(px(24.0 * HOUR))
            .children(cells)
            .children(others)
            .children(self.draft_block(cx))
    }

    /// The new event's block, on the day shown: green, its times and title, a handle at its top and its bottom.
    fn draft_block(&self, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let slot = self.slot;
        if slot.all_day || slot.start.date() != self.day {
            return None;
        }
        let cycle = clock::cycle(cx);
        let top = top_of(slot.start.time());
        let end_top = if slot.end.date() > self.day {
            24.0 * HOUR
        } else {
            top_of(slot.end.time())
        };
        let green = mix(theme::SUCCESS, 0x00ff_ffff, 280);
        let ink = 0x0011_1111;
        let title = self.title.read(cx).value().to_string();
        let times = format!(
            "{} - {}",
            slot.start.format(cycle.time_of_day()),
            slot.end.format(cycle.time_of_day())
        );
        let handle = |id: &'static str, part: Part| {
            div()
                .id(id)
                .absolute()
                .size(px(10.0))
                .rounded_full()
                .border_2()
                .border_color(rgb(green))
                .bg(rgb(ink))
                .cursor_ns_resize()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |form, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        form.start_drag(part, event, cx);
                    }),
                )
        };
        let label = if title.trim().is_empty() {
            times
        } else {
            format!("{} · {times}", title.trim())
        };
        Some(
            div()
                .debug_selector(|| "event-day-draft".to_owned())
                .id("event-day-draft")
                .absolute()
                .top(px(top))
                .left(px(0.0))
                .right(px(0.0))
                .h(px((end_top - top).max(20.0)))
                .px_3()
                .bg(rgb(green))
                .text_color(rgb(ink))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .shadow_md()
                .cursor_move()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|form, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        form.start_drag(Part::Whole, event, cx);
                    }),
                )
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .pt(px(2.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(label),
                )
                .child(
                    handle("event-day-draft-start", Part::Start)
                        .top(px(-5.0))
                        .right(px(24.0)),
                )
                .child(handle("event-day-draft-end", Part::End).bottom(px(-5.0)).left(px(24.0))),
        )
    }

    fn start_drag(&mut self, part: Part, event: &MouseDownEvent, cx: &mut Context<Self>) {
        self.drag = Some(Drag {
            part,
            from: event.position.y,
            slot: self.slot,
        });
        cx.notify();
    }

    /// The pointer moved: a drag of the new event's block moves it (snapped to a quarter hour).
    pub(super) fn dragged(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        let moved = f32::from(event.position.y - drag.from) / HOUR * 60.0;
        #[expect(clippy::cast_possible_truncation, reason = "a drag moves by minutes of a day")]
        let minutes = (moved / STEP_MINUTES as f32).round() as i64 * STEP_MINUTES;
        let slot = match drag.part {
            Part::Whole => drag.slot.moved(minutes),
            Part::Start => drag.slot.start_moved(minutes),
            Part::End => drag.slot.end_moved(minutes),
        };
        if slot != self.slot {
            self.slot = slot;
            cx.notify();
        }
    }

    /// The drag ended: where it left the event is set as a slot picked anywhere else is (the series moved with it,
    /// the free and busy times read again for the new times).
    pub(super) fn end_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        let slot = self.slot;
        if slot == drag.slot {
            cx.notify();
            return;
        }
        self.slot = drag.slot;
        self.set_slot(slot, cx);
    }
}

/// The offset of the day's hours that shows the new event a little below the top.
pub(super) fn scroll_to(slot_start: NaiveTime) -> f32 {
    (top_of(slot_start) - 2.0 * HOUR).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_s_offset_is_its_share_of_the_day() {
        assert!((top_of(NaiveTime::from_hms_opt(15, 0, 0).unwrap_or_default()) - 15.0 * HOUR).abs() < 0.01);
        assert!((top_of(NaiveTime::from_hms_opt(0, 30, 0).unwrap_or_default()) - HOUR / 2.0).abs() < 0.01);
    }
}
