//! The calendar's grids: the time grid of a day or a week (the days' headers, the all-day strip, the hours with their
//! half-hour lines, the events' blocks side by side where they overlap, the current time's line) and the month's.

use chrono::{Datelike as _, NaiveDate, NaiveDateTime, Timelike as _};
use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, point, prelude::FluentBuilder as _, px, relative, rgb,
};

use super::{CalendarView, GUTTER, HOUR_HEIGHT, MIN_MINUTES, MONTH_LISTED, Toggle, hour_label, mix, time_text};
use crate::calendar::{self, Look, Mode, Shown, Slot};
use crate::clock::{self, HourCycle};
use crate::icons::Glyph;
use crate::theme::Palette;

impl CalendarView {
    /// The time grid of the days shown: their headers, the all-day strip, and the hours with the events.
    pub(super) fn time_grid(&mut self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let span = self.span();
        let events = self.visible();
        let now = calendar::now().naive_local();
        let cycle = clock::cycle(cx);
        if self.toggles.on(Toggle::ScrollDue) {
            self.toggles.set(Toggle::ScrollDue, false);
            let hour = if span.contains(now.date()) {
                f32::from(u8::try_from(now.hour()).unwrap_or(8)) - 1.5
            } else {
                7.5
            };
            self.scroll
                .set_offset(point(px(0.0), px(-hour.clamp(0.0, 16.0) * HOUR_HEIGHT)));
        }
        let headers = div()
            .flex()
            .flex_none()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(div().w(px(GUTTER)).flex_none())
            .children(span.dates().map(|date| self.day_header(date, now.date(), palette, cx)));
        let all_day: Vec<Vec<Shown>> = span
            .dates()
            .map(|date| {
                events
                    .iter()
                    .filter(|shown| shown.all_day() && shown.on(date))
                    .cloned()
                    .collect()
            })
            .collect();
        let strip = all_day.iter().any(|day| !day.is_empty()).then(|| {
            div()
                .debug_selector(|| "calendar-all-day".to_owned())
                .flex()
                .flex_none()
                .border_b_1()
                .border_color(rgb(palette.border))
                .child(div().w(px(GUTTER)).flex_none())
                .children(all_day.into_iter().map(|day| {
                    div()
                        .flex_1()
                        .w_0()
                        .min_w_0()
                        .p_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .border_l_1()
                        .border_color(rgb(palette.border))
                        .children(day.into_iter().map(|shown| self.chip(shown, true, cycle, palette, cx)))
                }))
        });
        let gutter = div()
            .w(px(GUTTER))
            .flex_none()
            .relative()
            .h(px(24.0 * HOUR_HEIGHT))
            .children((1..24).map(|hour| {
                div()
                    .absolute()
                    .top(px(units(hour) * HOUR_HEIGHT + 4.0))
                    .left(px(8.0))
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(hour_label(hour, cycle))
            }));
        let columns: Vec<AnyElement> = span
            .dates()
            .map(|date| self.day_column(date, &events, now, cycle, palette, cx))
            .collect();
        let body = div()
            .id("calendar-hours")
            .debug_selector(|| "calendar-hours".to_owned())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(div().flex().child(gutter).children(columns));
        div()
            .debug_selector(|| "calendar-grid".to_owned())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(headers)
            .children(strip)
            .child(body)
    }

    /// A day's header: its number and weekday, today's in the brand colour under a brand line; clicked, the day.
    pub(super) fn day_header(
        &self,
        date: NaiveDate,
        today: NaiveDate,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_today = date == today;
        let color = if is_today { palette.brand } else { palette.foreground };
        let selector = format!("calendar-day-{}", date.format("%Y-%m-%d"));
        div()
            .debug_selector(move || selector.clone())
            .id(SharedString::from(format!("calendar-day-{}", date.format("%Y%m%d"))))
            .flex_1()
            .w_0()
            .min_w_0()
            .relative()
            .h(px(68.0))
            .px_4()
            .flex()
            .flex_col()
            .justify_center()
            .border_l_1()
            .border_color(rgb(palette.border))
            // Teams' brand line over today
            .when(is_today, |header| {
                header.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(3.0))
                        .bg(rgb(palette.brand)),
                )
            })
            .cursor_pointer()
            .hover(move |header| header.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.go_to(date, cx);
                this.set_mode(Mode::Day, cx);
            }))
            .child(
                div()
                    .text_2xl()
                    .text_color(rgb(color))
                    .when(is_today, |number| number.font_weight(gpui::FontWeight::SEMIBOLD))
                    .child(date.day().to_string()),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(if is_today {
                        palette.brand
                    } else {
                        palette.muted_foreground
                    }))
                    .child(date.format("%a").to_string()),
            )
            .into_any_element()
    }

    /// A day's column of hours: its hour and half-hour lines, its events and, today, the current time's line.
    pub(super) fn day_column(
        &self,
        date: NaiveDate,
        events: &[Shown],
        now: NaiveDateTime,
        cycle: HourCycle,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let lines = (0..24).flat_map(|hour| {
            let top = units(hour) * HOUR_HEIGHT;
            [
                div()
                    .absolute()
                    .top(px(top))
                    .left_0()
                    .right_0()
                    .h(px(1.0))
                    .bg(rgb(palette.border)),
                div()
                    .absolute()
                    .top(px(top + HOUR_HEIGHT / 2.0))
                    .left_0()
                    .right_0()
                    .h(px(1.0))
                    // the renderer draws no dashes on a line this thin: a fainter line stands for Teams' dotted one
                    .bg(rgb(mix(palette.background, palette.border, 550))),
            ]
        });
        // the half hours, behind the events: a double click on an empty one schedules an event there
        let cells = (0..48u16)
            .map(|half| {
                let start = date.and_time(chrono::NaiveTime::MIN) + chrono::Duration::minutes(i64::from(half) * 30);
                let selector = format!("calendar-slot-{}-{half}", date.format("%Y-%m-%d"));
                div()
                    .debug_selector(move || selector.clone())
                    .id(SharedString::from(format!(
                        "calendar-slot-{}-{half}",
                        date.format("%Y%m%d")
                    )))
                    .absolute()
                    .top(px(f32::from(half) * HOUR_HEIGHT / 2.0))
                    .left_0()
                    .right_0()
                    .h(px(HOUR_HEIGHT / 2.0))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.click_count() >= 2 {
                            let slot =
                                crate::event_form::Slot::new(start, start + crate::event_form::model::DEFAULT_LENGTH);
                            this.new_event(slot, false, cx);
                        }
                    }))
            })
            .collect::<Vec<_>>();
        let slots = calendar::day_slots(events, date, MIN_MINUTES);
        let blocks: Vec<AnyElement> = slots
            .into_iter()
            .map(|slot| self.block(slot, cycle, palette, cx))
            .collect();
        let now_line = self.now_line(date, now, palette);
        div()
            .flex_1()
            .w_0()
            .min_w_0()
            .relative()
            .h(px(24.0 * HOUR_HEIGHT))
            .border_l_1()
            .border_color(rgb(palette.border))
            .when(calendar::weekend(date), |column| {
                column.bg(rgb(mix(palette.background, palette.surface, 500)))
            })
            .children(lines)
            .children(cells)
            .children(blocks)
            .children(now_line)
            .into_any_element()
    }

    /// The current time's line: in today's column the brand line with its dot, across the other days a faint one.
    pub(super) fn now_line(&self, date: NaiveDate, now: NaiveDateTime, palette: Palette) -> Option<gpui::Div> {
        if !self.span().contains(now.date()) {
            return None;
        }
        let minutes = now.hour() * 60 + now.minute();
        let top = units(minutes) / 60.0 * HOUR_HEIGHT;
        if date == now.date() {
            Some(
                div()
                    .debug_selector(|| "calendar-now".to_owned())
                    .absolute()
                    .top(px(top - 5.0))
                    .left(px(-5.0))
                    .right_0()
                    .h(px(10.0))
                    .flex()
                    .items_center()
                    .child(div().size(px(10.0)).rounded_full().bg(rgb(palette.brand)))
                    .child(div().flex_1().h(px(2.0)).bg(rgb(palette.brand))),
            )
        } else {
            Some(div().absolute().top(px(top)).left_0().right_0().h(px(1.0)).bg(rgb(mix(
                palette.background,
                palette.brand,
                450,
            ))))
        }
    }

    /// An event's block in the time grid.
    pub(super) fn block(&self, slot: Slot, cycle: HourCycle, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let top = units(slot.top) / 60.0 * HOUR_HEIGHT;
        let height = units(slot.length) / 60.0 * HOUR_HEIGHT;
        let width = 1.0 / units(slot.columns);
        let left = units(slot.column) * width;
        let shown = slot.shown;
        let look = shown.look();
        let one_line = height < 40.0;
        let title = shown.title();
        let organizer = shown.event.organizer_name.clone().filter(|_| !shown.event.is_organizer);
        let mark = mark(&shown, palette);
        let text = div()
            .flex_1()
            .min_w_0()
            .pl_2()
            .pr_1()
            .py_1()
            .overflow_hidden()
            .text_sm()
            .when(one_line, |text| {
                text.flex()
                    .gap_1()
                    .whitespace_nowrap()
                    .child(
                        div()
                            .flex_none()
                            .max_w_full()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(title.clone()),
                    )
                    .children(organizer.clone().map(|name| {
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(rgb(palette.muted_foreground))
                            .child(name)
                    }))
            })
            .when(!one_line, |text| {
                let time = (height >= 72.0)
                    .then(|| format!("{} – {}", time_text(shown.start, cycle), time_text(shown.end, cycle)));
                text.flex()
                    .flex_col()
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(title.clone()),
                    )
                    .children(organizer.clone().map(|name| {
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(rgb(palette.muted_foreground))
                            .child(name)
                    }))
                    .children(time.map(|time| {
                        div()
                            .text_xs()
                            .whitespace_nowrap()
                            .text_color(rgb(palette.muted_foreground))
                            .child(time)
                    }))
            });
        let selector = format!("calendar-event-{}", shown.event.id);
        let id = SharedString::from(format!("calendar-event-{}-{}", shown.event.id, slot.top));
        div()
            .absolute()
            .top(px(top + 1.0))
            .left(relative(left))
            .w(relative(width))
            .h(px((height - 2.0).max(14.0)))
            .pr(px(4.0))
            .pl(px(2.0))
            .child(
                event_face(look, palette)
                    .debug_selector(move || selector.clone())
                    .id(id)
                    .size_full()
                    .relative()
                    .flex()
                    .overflow_hidden()
                    .cursor_pointer()
                    .child(edge(look, palette))
                    .child(text)
                    .children(mark.map(|mark| div().absolute().right(px(4.0)).bottom(px(3.0)).child(mark)))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        // a double click opens the account's own event to change, as Teams does
                        if event.click_count() >= 2 && shown.event.is_organizer && !shown.event.cancelled {
                            this.edit_event(shown.event.id.clone(), cx);
                        } else {
                            this.open_card(shown.clone(), cx);
                        }
                    })),
            )
            .into_any_element()
    }

    /// An event as one line (the all-day strip's, a month day's): its time unless all day, its title, its mark.
    pub(super) fn chip(
        &self,
        shown: Shown,
        all_day: bool,
        cycle: HourCycle,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let look = shown.look();
        let selector = format!("calendar-event-{}", shown.event.id);
        let id = SharedString::from(format!(
            "calendar-chip-{}-{}",
            shown.event.id,
            shown.start.and_utc().timestamp()
        ));
        let time = (!all_day && !shown.all_day()).then(|| time_text(shown.start, cycle));
        let mark = mark(&shown, palette);
        event_face(look, palette)
            .debug_selector(move || selector.clone())
            .id(id)
            .h(px(24.0))
            .flex()
            .items_center()
            .gap_1()
            .overflow_hidden()
            .cursor_pointer()
            .text_sm()
            .child(edge(look, palette))
            .children(time.map(|time| {
                div()
                    .pl_1()
                    .flex_none()
                    .text_color(rgb(palette.muted_foreground))
                    .child(time)
            }))
            .child(
                div()
                    .pl_1()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(shown.title()),
            )
            .children(mark.map(|mark| div().pr_1().child(mark)))
            .on_click(cx.listener(move |this, _, _, cx| this.open_card(shown.clone(), cx)))
            .into_any_element()
    }

    /// The month view: the weekday names over six weeks of days, each listing its events.
    pub(super) fn month_view(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let span = self.span();
        let events = self.visible();
        let today = calendar::now().date_naive();
        let cycle = clock::cycle(cx);
        let names = div()
            .flex()
            .flex_none()
            .border_b_1()
            .border_color(rgb(palette.border))
            .children(
                [
                    "Monday",
                    "Tuesday",
                    "Wednesday",
                    "Thursday",
                    "Friday",
                    "Saturday",
                    "Sunday",
                ]
                .map(|name| {
                    div()
                        .flex_1()
                        .w_0()
                        .px_2()
                        .py_2()
                        .text_sm()
                        .text_color(rgb(palette.muted_foreground))
                        .child(name)
                }),
            );
        let dates: Vec<NaiveDate> = span.dates().collect();
        let weeks = dates.chunks(7).map(|week| {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .border_b_1()
                .border_color(rgb(palette.border))
                .children(
                    week.iter()
                        .map(|date| self.month_day(*date, &events, today, cycle, palette, cx)),
                )
        });
        div()
            .debug_selector(|| "calendar-month".to_owned())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(names)
            .children(weeks)
    }

    pub(super) fn month_day(
        &self,
        date: NaiveDate,
        events: &[Shown],
        today: NaiveDate,
        cycle: HourCycle,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let listed = calendar::day_list(events, date);
        let more = listed.len().saturating_sub(MONTH_LISTED);
        let in_month = date.month() == self.anchor.month();
        let is_today = date == today;
        let number = div()
            .size(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .text_sm()
            .when(is_today, |number| {
                number
                    .bg(rgb(palette.brand))
                    .text_color(rgb(palette.on_brand))
                    .font_weight(gpui::FontWeight::BOLD)
            })
            .when(!is_today && !in_month, |number| {
                number.text_color(rgb(palette.muted_foreground))
            })
            .child(if date.day() == 1 {
                date.format("%b %-d").to_string()
            } else {
                date.day().to_string()
            });
        let more = (more > 0).then(|| {
            div()
                .id(SharedString::from(format!("calendar-more-{}", date.format("%Y%m%d"))))
                .px_1()
                .text_xs()
                .cursor_pointer()
                .text_color(rgb(palette.brand))
                .child(format!("+{more} more"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.go_to(date, cx);
                    this.set_mode(Mode::Day, cx);
                }))
        });
        div()
            .flex_1()
            .w_0()
            .min_w_0()
            .p_1()
            .flex()
            .flex_col()
            .gap_0p5()
            .overflow_hidden()
            .border_l_1()
            .border_color(rgb(palette.border))
            .when(!in_month, |cell| {
                cell.bg(rgb(mix(palette.background, palette.surface, 500)))
            })
            .child(number)
            .children(
                listed
                    .into_iter()
                    .take(MONTH_LISTED)
                    .map(|shown| self.chip(shown, false, cycle, palette, cx)),
            )
            .children(more)
            .into_any_element()
    }
}

/// An event's block or chip by its look: filled with the brand colour's tint, a cancelled one only outlined.
fn event_face(look: Look, palette: Palette) -> gpui::Div {
    let face = div().rounded_md();
    match look {
        Look::Busy | Look::Tentative => face
            .bg(rgb(mix(palette.background, palette.brand, 220)))
            .hover(move |face| face.bg(rgb(mix(palette.background, palette.brand, 320)))),
        Look::Cancelled => face
            .bg(rgb(palette.background))
            .border_1()
            .border_color(rgb(mix(palette.background, palette.brand, 600)))
            .text_color(rgb(palette.muted_foreground))
            .hover(move |face| face.bg(rgb(palette.hover))),
    }
}

/// An event's coloured left edge: solid when accepted, striped while tentative or not answered, faint when cancelled.
fn edge(look: Look, palette: Palette) -> gpui::Div {
    let edge = div().w(px(4.0)).h_full().flex_none().rounded_l_md().overflow_hidden();
    match look {
        Look::Busy => edge.bg(rgb(palette.brand)),
        Look::Cancelled => edge.bg(rgb(mix(palette.background, palette.brand, 400))),
        Look::Tentative => edge
            .flex()
            .flex_col()
            .bg(rgb(mix(palette.background, palette.brand, 350)))
            .children((0..40).map(|stripe| {
                div()
                    .h(px(3.0))
                    .flex_none()
                    .when(stripe % 2 == 0, |stripe| stripe.bg(rgb(palette.brand)))
            })),
    }
}

/// The recurring mark of an event of a series, crossed out when it is cancelled.
fn mark(shown: &Shown, palette: Palette) -> Option<AnyElement> {
    if !shown.event.kind.recurring() && !shown.event.cancelled {
        return None;
    }
    let glyph = if shown.event.cancelled {
        Glyph::REPEAT_OFF
    } else {
        Glyph::REPEAT
    };
    Some(
        glyph
            .icon()
            .size(px(14.0))
            .text_color(rgb(palette.muted_foreground))
            .into_any_element(),
    )
}

/// A small count (hours, minutes of a day, columns) as a float for a length.
fn units(count: impl TryInto<u16>) -> f32 {
    f32::from(count.try_into().unwrap_or(u16::MAX))
}
