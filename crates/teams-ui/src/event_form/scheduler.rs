//! The Scheduler beside the scheduling form, as Teams' form shows it in place of the day: the day's hours with a
//! column for the account and one for each attendee, each person's busy times in it (busy, tentative, out of office,
//! working elsewhere, as their calendars say), the new event's slot across them in green, and the times every attendee
//! is free above them ("Suggested times"). A click on a free half hour or a suggestion moves the event there.
//!
//! What it shows is read when it opens and again as the day, the attendees or the event's length change
//! (`Core::availability`: the scheduling form's two free/busy reads, which reach nobody); nothing is kept.

use std::rc::Rc;

use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone as _};
use gpui::{
    AnyElement, AsyncApp, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, div, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::Button;
use teams_core::MeetingAvailability;
use teams_proto::{BusyTime, FreeBusy};

use super::EventForm;
use super::day::{HOUR, top_of};
use crate::calendar_view::mix;
use crate::clock;
use crate::icons::selected_by;
use crate::theme::{self, Palette};

/// Out of office's purple, as Teams draws it.
const AWAY: u32 = 0x0087_64B8;

/// What the Scheduler knows of the day it shows.
#[derive(Clone, Debug, Default)]
pub(super) enum Read {
    /// Nothing asked yet.
    #[default]
    None,
    /// Asked; the answer to come.
    Reading,
    Read(Rc<MeetingAvailability>),
    /// The reads failed.
    Failed,
}

/// What a read was asked for: the day, the attendees' addresses and the event's length. Asked again when it changes.
pub(super) type ReadKey = (NaiveDate, Vec<String>, i64);

/// A column of the Scheduler: whose, and their busy times.
struct Column {
    name: SharedString,
    busy: Vec<BusyTime>,
}

impl EventForm {
    /// What the Scheduler would read now.
    pub(super) fn availability_key(&self) -> ReadKey {
        let mut addresses: Vec<String> = self
            .attendees
            .iter()
            .map(|attendee| attendee.address.to_lowercase())
            .collect();
        addresses.sort();
        (self.day, addresses, self.slot.length().num_minutes())
    }

    /// Reads the day's free and busy times, unless what was read is still what the form asks about.
    pub(super) fn read_availability(&mut self, cx: &mut Context<Self>) {
        let key = self.availability_key();
        if self.availability_key.as_ref() == Some(&key) {
            return;
        }
        self.availability_key = Some(key.clone());
        let start_ms = Local
            .from_local_datetime(&self.day.and_time(NaiveTime::MIN))
            .earliest()
            .map_or(0, |start| start.timestamp_millis());
        let minutes = u32::try_from(key.2.max(1)).unwrap_or(30);
        let reply = self.core.availability(self.attendees.clone(), start_ms, minutes);
        self.availability = Read::Reading;
        self.availability_read = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |form, cx| {
                // an answer for a day or attendees the form no longer shows is dropped
                if form.availability_key.as_ref() != Some(&key) {
                    return;
                }
                form.availability = match answer {
                    Ok(read) => Read::Read(Rc::new(read)),
                    Err(_) => Read::Failed,
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The suggested starts on the day shown, when they were read.
    pub(super) fn suggestions_on_day(&self) -> Vec<NaiveDateTime> {
        let Read::Read(read) = &self.availability else {
            return Vec::new();
        };
        read.suggestions
            .iter()
            .filter_map(|ms| DateTime::from_timestamp_millis(*ms))
            .map(|time| time.with_timezone(&Local).naive_local())
            .filter(|time| time.date() == self.day)
            .collect()
    }

    /// The Scheduler, in the day's place.
    pub(super) fn scheduler_view(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let columns = self.columns();
        let body = div()
            .id("event-scheduler-hours")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.day_scroll)
            .child(
                div()
                    .flex()
                    .child(self.gutter(palette, cx))
                    .child(self.people_columns(&columns, palette, cx)),
            );
        div()
            .debug_selector(|| "event-scheduler".to_owned())
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
            .child(self.suggestions_strip(palette, cx))
            .child(column_heads(&columns, palette))
            .child(body)
    }

    /// The account's column, then each attendee's, with what their calendars say.
    fn columns(&self) -> Vec<Column> {
        let people = match &self.availability {
            Read::Read(read) => read.people.clone(),
            _ => Vec::new(),
        };
        let invited: Vec<String> = self
            .attendees
            .iter()
            .map(|attendee| attendee.address.to_lowercase())
            .collect();
        let own: Vec<BusyTime> = people
            .iter()
            .filter(|person| !invited.contains(&person.address.to_lowercase()))
            .flat_map(|person| person.busy.clone())
            .collect();
        let mut columns = vec![Column {
            name: "You".into(),
            busy: own,
        }];
        columns.extend(self.attendees.iter().map(|attendee| {
            let address = attendee.address.to_lowercase();
            Column {
                name: attendee.name.clone().unwrap_or_else(|| attendee.address.clone()).into(),
                busy: people
                    .iter()
                    .filter(|person| person.address.to_lowercase() == address)
                    .flat_map(|person| person.busy.clone())
                    .collect(),
            }
        }));
        columns
    }

    /// "Suggested times" for the day: a button for each start every attendee is free at, or why there are none.
    fn suggestions_strip(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let cycle = clock::cycle(cx);
        let suggestions = self.suggestions_on_day();
        let note = match (&self.availability, self.attendees.is_empty(), suggestions.is_empty()) {
            (_, true, _) => Some("Add attendees to see when everyone is free"),
            (Read::Reading | Read::None, _, _) => Some("Looking for times everyone is free…"),
            (Read::Failed, _, _) => Some("Their calendars couldn't be read. Try another day."),
            (Read::Read(_), _, true) => Some("No time on this day suits everyone"),
            (Read::Read(_), _, false) => None,
        };
        let buttons = suggestions.into_iter().take(12).map(|start| {
            let id = SharedString::from(format!("event-suggested-{}", start.format("%H%M")));
            selected_by(
                id.clone(),
                Button::new(id)
                    .outline()
                    .xsmall()
                    .label(start.format(cycle.time_of_day()).to_string())
                    .on_click(cx.listener(move |form, _, _, cx| {
                        let slot = form.slot.starting(start);
                        form.set_slot(slot, cx);
                    })),
            )
        });
        div()
            .debug_selector(|| "event-scheduler-suggestions".to_owned())
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted_foreground))
                    .child("Suggested times"),
            )
            .children(note.map(|note| div().text_sm().text_color(rgb(palette.muted_foreground)).child(note)))
            .child(div().flex().flex_wrap().gap_1().children(buttons))
    }

    /// The day's half hours under every column, each person's busy times, and the event's slot across them.
    fn people_columns(&self, columns: &[Column], palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let day = self.day;
        let cells = (0..48u32).map(|half| {
            let top = f32::from(u16::try_from(half).unwrap_or(0)) * HOUR / 2.0;
            let start = day.and_time(NaiveTime::MIN) + Duration::minutes(i64::from(half) * 30);
            div()
                .id(SharedString::from(format!("event-scheduler-slot-{half}")))
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
                .hover(move |cell| cell.bg(rgb(palette.hover)))
                .on_click(cx.listener(move |form, _, _, cx| {
                    let slot = form.slot.starting(start);
                    form.set_slot(slot, cx);
                }))
        });
        let lanes = div().absolute().top_0().left_0().right_0().bottom_0().flex().children(
            columns
                .iter()
                .enumerate()
                .map(|(index, column)| lane(column, index, day, palette).into_any_element()),
        );
        div()
            .flex_1()
            .relative()
            .h(px(24.0 * HOUR))
            .children(cells)
            .children(self.slot_band(palette))
            .child(lanes)
    }

    /// The event's slot across every column, on the day shown.
    fn slot_band(&self, palette: Palette) -> Option<gpui::Div> {
        let slot = self.slot;
        if slot.all_day || slot.start.date() != self.day {
            return None;
        }
        let top = top_of(slot.start.time());
        let end_top = if slot.end.date() > self.day {
            24.0 * HOUR
        } else {
            top_of(slot.end.time())
        };
        Some(
            div()
                .debug_selector(|| "event-scheduler-slot".to_owned())
                .absolute()
                .top(px(top))
                .left_0()
                .right_0()
                .h(px((end_top - top).max(8.0)))
                .border_2()
                .border_color(rgb(theme::SUCCESS))
                .bg(rgb(mix(palette.surface, theme::SUCCESS, 260))),
        )
    }
}

/// The column heads: each person's initials and name, over their column.
fn column_heads(columns: &[Column], palette: Palette) -> gpui::Div {
    div()
        .h(px(40.0))
        .flex()
        .border_b_1()
        .border_color(rgb(palette.border))
        .child(div().w(px(super::day::GUTTER)).flex_none())
        .children(columns.iter().map(|column| {
            div()
                .flex_1()
                .min_w_0()
                .px_1()
                .flex()
                .items_center()
                .gap_1()
                .border_l_1()
                .border_color(rgb(palette.border))
                .child(
                    div()
                        .size(px(22.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(rgb(mix(palette.surface, palette.brand, 350)))
                        .text_xs()
                        .child(initials(&column.name)),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_xs()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(column.name.clone()),
                )
        }))
}

/// One person's column: their busy times on `day`, coloured by what they are.
fn lane(column: &Column, index: usize, day: NaiveDate, palette: Palette) -> gpui::Div {
    let blocks: Vec<AnyElement> = column
        .busy
        .iter()
        .filter_map(|busy| {
            let (fill, edge) = match busy.status {
                FreeBusy::Busy => (mix(palette.surface, palette.brand, 650), palette.brand),
                FreeBusy::Tentative => (mix(palette.surface, palette.brand, 250), palette.brand),
                FreeBusy::Away => (mix(palette.surface, AWAY, 650), AWAY),
                FreeBusy::WorkingElsewhere => (mix(palette.surface, palette.foreground, 200), palette.muted_foreground),
                FreeBusy::Free | FreeBusy::Other(_) => return None,
            };
            let (top, bottom) = span_on(busy, day)?;
            Some(
                div()
                    .absolute()
                    .top(px(top))
                    .left(px(2.0))
                    .right(px(2.0))
                    .h(px((bottom - top).max(6.0)))
                    .rounded_sm()
                    .border_l_2()
                    .border_color(rgb(edge))
                    .bg(rgb(fill))
                    .into_any_element(),
            )
        })
        .collect();
    div()
        .debug_selector(move || format!("event-scheduler-column-{index}"))
        .flex_1()
        .min_w_0()
        .relative()
        .border_l_1()
        .border_color(rgb(palette.border))
        .children(blocks)
}

/// Where `busy` lies in `day`'s column (its top and bottom, px); `None` when it isn't on that day.
fn span_on(busy: &BusyTime, day: NaiveDate) -> Option<(f32, f32)> {
    let start = DateTime::from_timestamp_millis(busy.start_ms)?
        .with_timezone(&Local)
        .naive_local();
    let end = DateTime::from_timestamp_millis(busy.end_ms)?
        .with_timezone(&Local)
        .naive_local();
    let day_start = day.and_time(NaiveTime::MIN);
    let day_end = day_start + Duration::days(1);
    if end <= day_start || start >= day_end {
        return None;
    }
    let top = if start <= day_start { 0.0 } else { top_of(start.time()) };
    let bottom = if end >= day_end {
        24.0 * HOUR
    } else {
        top_of(end.time())
    };
    Some((top, bottom))
}

/// Up to two initials of a name (its first letters), or of an address.
fn initials(name: &str) -> String {
    name.split(['@', ' ', '.'])
        .filter_map(|part| part.chars().next())
        .filter(|letter| letter.is_alphanumeric())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_s_initials_are_its_first_letters() {
        assert_eq!(initials("Bot B"), "BB");
        assert_eq!(initials("bot-c@lab.example.com"), "BL");
        assert_eq!(initials("You"), "Y");
    }

    #[test]
    fn a_busy_time_lies_in_its_day_s_column() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 27).expect("a date");
        let at = |hour: u32| {
            Local
                .from_local_datetime(&day.and_hms_opt(hour, 0, 0).expect("a time"))
                .earliest()
                .expect("a local time")
                .timestamp_millis()
        };
        let busy = BusyTime {
            status: FreeBusy::Busy,
            start_ms: at(9),
            end_ms: at(10),
            subject: None,
        };
        let (top, bottom) = span_on(&busy, day).expect("on the day");
        assert!((top - 9.0 * HOUR).abs() < 0.01 && (bottom - 10.0 * HOUR).abs() < 0.01);
        assert!(span_on(&busy, day + Duration::days(1)).is_none());
    }
}
