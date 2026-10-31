//! The calendar's dates and layout, apart from drawing ([`crate::calendar_view`] draws them): which days a view shows
//! ([`Mode`], [`Span`]), what its title says, where each event goes in the time grid (side by side where they
//! overlap, as Teams lays them out) and which events a day of the month grid lists.
//!
//! Every date is the viewer's own, in the local time zone; an event's start and end are placed there whatever form
//! the service gave them in ([`local`]). Weeks start on Monday, as the user's Teams shows them.

use std::rc::Rc;

use chrono::{
    DateTime, Datelike as _, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike as _, Weekday,
};
use teams_core::{CalendarEvent, EventResponse, EventTime, ShowAs};

/// How many days a view shows, as Teams' view switcher offers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Day,
    WorkWeek,
    Week,
    Month,
}

impl Mode {
    /// Every mode, in the switcher's order.
    pub const ALL: [Self; 4] = [Self::Day, Self::WorkWeek, Self::Week, Self::Month];

    /// Its name in the switcher.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Day => "Day",
            Self::WorkWeek => "Work week",
            Self::Week => "Week",
            Self::Month => "Month",
        }
    }

    /// Its key, for selectors, the trace and the settings.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::WorkWeek => "work-week",
            Self::Week => "week",
            Self::Month => "month",
        }
    }

    /// What the previous and next buttons step by, in their tooltips' words.
    #[must_use]
    pub fn unit(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::WorkWeek | Self::Week => "week",
            Self::Month => "month",
        }
    }
}

/// The days a view shows: from `first` for `days` days.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub first: NaiveDate,
    pub days: u32,
}

impl Span {
    /// The days `mode` shows around `anchor`: the day; Monday to Friday or Sunday of its week; the six weeks from the
    /// Monday on or before the first of its month.
    #[must_use]
    pub fn of(anchor: NaiveDate, mode: Mode) -> Self {
        match mode {
            Mode::Day => Self { first: anchor, days: 1 },
            Mode::WorkWeek => Self {
                first: monday(anchor),
                days: 5,
            },
            Mode::Week => Self {
                first: monday(anchor),
                days: 7,
            },
            Mode::Month => Self {
                first: monday(anchor.with_day(1).unwrap_or(anchor)),
                days: 42,
            },
        }
    }

    /// Each day, in order.
    pub fn dates(self) -> impl Iterator<Item = NaiveDate> {
        (0..self.days).filter_map(move |day| self.first.checked_add_signed(Duration::days(i64::from(day))))
    }

    /// The day after the last.
    #[must_use]
    pub fn end(self) -> NaiveDate {
        self.first + Duration::days(i64::from(self.days))
    }

    /// Whether `date` is one of its days.
    #[must_use]
    pub fn contains(self, date: NaiveDate) -> bool {
        date >= self.first && date < self.end()
    }

    /// The window a read of it asks for, in ms since the epoch: local midnight of the first day to local midnight
    /// after the last.
    #[must_use]
    pub fn window_ms(self) -> (i64, i64) {
        (midnight_ms(self.first), midnight_ms(self.end()))
    }
}

/// The Monday on or before `date`.
#[must_use]
pub fn monday(date: NaiveDate) -> NaiveDate {
    date - Duration::days(i64::from(date.weekday().num_days_from_monday()))
}

/// The day `mode`'s previous (`forward` false) or next button goes to from `anchor`.
#[must_use]
pub fn step(anchor: NaiveDate, mode: Mode, forward: bool) -> NaiveDate {
    let sign = if forward { 1 } else { -1 };
    match mode {
        Mode::Day => anchor + Duration::days(sign),
        Mode::WorkWeek | Mode::Week => anchor + Duration::days(7 * sign),
        Mode::Month => add_months(anchor.with_day(1).unwrap_or(anchor), sign),
    }
}

/// The first of the month `months` after `first`'s.
#[must_use]
pub fn add_months(first: NaiveDate, months: i64) -> NaiveDate {
    let index = i64::from(first.year()) * 12 + i64::from(first.month0()) + months;
    let year = i32::try_from(index.div_euclid(12)).unwrap_or(first.year());
    let month = u32::try_from(index.rem_euclid(12)).unwrap_or(0) + 1;
    NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(first)
}

/// What the toolbar says a view shows: "Friday, October 30, 2026", "October 26–30, 2026", "October 26 – November 1,
/// 2026", "December 28, 2026 – January 3, 2027", "October 2026".
#[must_use]
pub fn title(anchor: NaiveDate, mode: Mode) -> String {
    let span = Span::of(anchor, mode);
    match mode {
        Mode::Day => anchor.format("%A, %B %-d, %Y").to_string(),
        Mode::Month => anchor.format("%B %Y").to_string(),
        Mode::WorkWeek | Mode::Week => {
            let (first, last) = (span.first, span.end() - Duration::days(1));
            if first.year() != last.year() {
                format!("{} – {}", first.format("%B %-d, %Y"), last.format("%B %-d, %Y"))
            } else if first.month() != last.month() {
                format!("{} – {}", first.format("%B %-d"), last.format("%B %-d, %Y"))
            } else {
                format!("{}–{}", first.format("%B %-d"), last.format("%-d, %Y"))
            }
        }
    }
}

/// Local midnight of `date` in ms since the epoch (the first instant of the day where a change of clock skips it).
fn midnight_ms(date: NaiveDate) -> i64 {
    let midnight = date.and_time(NaiveTime::MIN);
    Local
        .from_local_datetime(&midnight)
        .earliest()
        .map_or_else(|| midnight.and_utc().timestamp_millis(), |time| time.timestamp_millis())
}

/// An event's start or end in local time.
#[must_use]
pub fn local(time: EventTime) -> Option<NaiveDateTime> {
    match time {
        EventTime::Instant(ms) => Local.timestamp_millis_opt(ms).single().map(|time| time.naive_local()),
        EventTime::Floating(time) => NaiveDate::from_ymd_opt(time.year, u32::from(time.month), u32::from(time.day))?
            .and_hms_opt(u32::from(time.hour), u32::from(time.minute), u32::from(time.second)),
    }
}

/// The midnight nearest `time` (noon and later round to the next day's).
fn nearest_midnight(time: NaiveDateTime) -> NaiveDateTime {
    let day = time.date();
    let date = if time.hour() >= 12 {
        day + Duration::days(1)
    } else {
        day
    };
    date.and_time(NaiveTime::MIN)
}

/// How an event's block is drawn, by the account's answer and what the event is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// Accepted, organized, or an appointment: a filled block with a solid edge.
    Busy,
    /// Tentative, not answered, or showing the time as free: a filled block with a striped edge.
    Tentative,
    /// Cancelled: an outlined block.
    Cancelled,
}

/// An event placed in the viewer's time.
#[derive(Clone, Debug)]
pub struct Shown {
    pub event: Rc<CalendarEvent>,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
}

impl Shown {
    /// `event` in local time; `None` when its times can't be placed. An event that ends before it starts is shown
    /// ending as it starts.
    #[must_use]
    pub fn of(event: Rc<CalendarEvent>) -> Option<Self> {
        let mut start = local(event.start)?;
        let mut end = local(event.end)?.max(start);
        if event.all_day {
            // an all-day event's ends are midnights, whatever zone they were written in: the nearest local ones
            start = nearest_midnight(start);
            end = nearest_midnight(end).max(start + Duration::days(1));
        }
        Some(Self { event, start, end })
    }

    /// How its block is drawn.
    #[must_use]
    pub fn look(&self) -> Look {
        let event = &self.event;
        if event.cancelled {
            Look::Cancelled
        } else if matches!(event.response, EventResponse::Tentative | EventResponse::NotResponded)
            || matches!(event.show_as, ShowAs::Tentative | ShowAs::Free)
        {
            Look::Tentative
        } else {
            Look::Busy
        }
    }

    /// Its title as Teams shows it: a cancelled event's says so (once: the service usually renames it already).
    #[must_use]
    pub fn title(&self) -> String {
        let subject = if self.event.subject.is_empty() {
            "(No title)"
        } else {
            self.event.subject.as_str()
        };
        if self.event.cancelled && !subject.to_lowercase().starts_with("canceled") {
            format!("Canceled: {subject}")
        } else {
            subject.to_owned()
        }
    }

    /// Whether it is drawn in the all-day strip: an all-day event, or one that lasts a whole day or more.
    #[must_use]
    pub fn all_day(&self) -> bool {
        self.event.all_day || self.end - self.start >= Duration::hours(24)
    }

    /// Whether it falls on `date` (an all-day event's last day is the one before its end).
    #[must_use]
    pub fn on(&self, date: NaiveDate) -> bool {
        let first = self.start.date();
        let last = if self.end > self.start && self.end.time() == NaiveTime::MIN {
            (self.end - Duration::days(1)).date()
        } else {
            self.end.date()
        };
        date >= first && date <= last.max(first)
    }
}

/// The events Teams shows: a meeting the account declined isn't one (it organizes none it declined).
#[must_use]
pub fn shows(event: &CalendarEvent) -> bool {
    event.response != EventResponse::Declined
}

/// One event's place in a day's time grid: its top and height in minutes of the day, and its column among the
/// events it overlaps (`column` of `columns`).
#[derive(Clone, Debug)]
pub struct Slot {
    pub shown: Shown,
    pub top: i64,
    pub length: i64,
    pub column: usize,
    pub columns: usize,
}

/// The timed events of `date` (all-day ones left out) placed in its grid: each cut to the day, at least
/// `min_length` minutes tall, and side by side with the events it overlaps, as few columns as they need.
#[must_use]
pub fn day_slots(shown: &[Shown], date: NaiveDate, min_length: i64) -> Vec<Slot> {
    let day_start = date.and_time(NaiveTime::MIN);
    let day_end = day_start + Duration::days(1);
    let mut slots: Vec<Slot> = shown
        .iter()
        .filter(|shown| {
            !shown.all_day() && shown.start < day_end && (shown.end > day_start || shown.start >= day_start)
        })
        .map(|shown| {
            let start = shown.start.max(day_start);
            let end = shown.end.min(day_end);
            let top = (start - day_start).num_minutes();
            let length = (end - start).num_minutes().max(min_length).min(24 * 60 - top).max(1);
            Slot {
                shown: shown.clone(),
                top,
                length,
                column: 0,
                columns: 1,
            }
        })
        .collect();
    slots.sort_by(|a, b| a.top.cmp(&b.top).then(b.length.cmp(&a.length)));
    lay_out(&mut slots);
    slots
}

/// Gives overlapping slots columns side by side: each group of slots that overlap one another (directly or through
/// others) shares one column count, each slot in the first column free at its top.
fn lay_out(slots: &mut [Slot]) {
    let mut group_start = 0;
    let mut group_end = i64::MIN;
    let mut column_ends: Vec<i64> = Vec::new();
    for index in 0..slots.len() {
        let (top, bottom) = (slots[index].top, slots[index].top + slots[index].length);
        if top >= group_end && index > group_start {
            close_group(&mut slots[group_start..index], column_ends.len());
            group_start = index;
            column_ends.clear();
        }
        let column = column_ends
            .iter()
            .position(|end| *end <= top)
            .unwrap_or(column_ends.len());
        if column == column_ends.len() {
            column_ends.push(bottom);
        } else {
            column_ends[column] = bottom;
        }
        slots[index].column = column;
        group_end = if index == group_start {
            bottom
        } else {
            group_end.max(bottom)
        };
    }
    let columns = column_ends.len();
    if let Some(group) = slots.get_mut(group_start..) {
        close_group(group, columns);
    }
}

fn close_group(group: &mut [Slot], columns: usize) {
    for slot in group {
        slot.columns = columns.max(1);
    }
}

/// The events a day of the month grid lists, all-day ones first, then by start.
#[must_use]
pub fn day_list(shown: &[Shown], date: NaiveDate) -> Vec<Shown> {
    let mut listed: Vec<Shown> = shown.iter().filter(|shown| shown.on(date)).cloned().collect();
    listed.sort_by(|a, b| b.all_day().cmp(&a.all_day()).then(a.start.cmp(&b.start)));
    listed
}

/// The viewer's now, as a local date and time.
#[must_use]
pub fn now() -> DateTime<Local> {
    Local::now()
}

/// The weekday letters of the mini month, Monday first.
pub const WEEKDAY_LETTERS: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];

/// Whether `date` falls on a weekend.
#[must_use]
pub fn weekend(date: NaiveDate) -> bool {
    matches!(date.weekday(), Weekday::Sat | Weekday::Sun)
}

#[cfg(test)]
mod tests {
    use super::*;
    use teams_core::{EventKind, FloatingTime};

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn at(day: u32, hour: u8, minute: u8) -> EventTime {
        EventTime::Floating(FloatingTime {
            year: 2026,
            month: 10,
            day: u8::try_from(day).unwrap(),
            hour,
            minute,
            second: 0,
        })
    }

    fn event(id: &str, start: EventTime, end: EventTime) -> Shown {
        Shown::of(Rc::new(CalendarEvent {
            id: id.to_owned(),
            subject: id.to_owned(),
            start,
            end,
            all_day: false,
            kind: EventKind::Single,
            cancelled: false,
            online: false,
            join_url: None,
            meeting_chat: None,
            location: None,
            organizer_name: None,
            organizer_address: None,
            is_organizer: false,
            response: EventResponse::Accepted,
            show_as: ShowAs::Busy,
            private: false,
            preview: None,
            conference_id: None,
            attendees: Vec::new(),
            series_master_id: None,
        }))
        .unwrap()
    }

    #[test]
    fn spans_start_on_monday() {
        let friday = date(2026, 10, 30);
        assert_eq!(
            Span::of(friday, Mode::WorkWeek),
            Span {
                first: date(2026, 10, 26),
                days: 5
            }
        );
        assert_eq!(Span::of(friday, Mode::Week).end(), date(2026, 11, 2));
        assert_eq!(Span::of(friday, Mode::Month).first, date(2026, 9, 28));
        assert_eq!(Span::of(friday, Mode::Day).days, 1);
    }

    #[test]
    fn titles_read_as_teams_writes_them() {
        assert_eq!(title(date(2026, 10, 30), Mode::WorkWeek), "October 26–30, 2026");
        assert_eq!(title(date(2026, 10, 30), Mode::Week), "October 26 – November 1, 2026");
        assert_eq!(
            title(date(2026, 12, 31), Mode::Week),
            "December 28, 2026 – January 3, 2027"
        );
        assert_eq!(title(date(2026, 10, 30), Mode::Day), "Friday, October 30, 2026");
        assert_eq!(title(date(2026, 10, 30), Mode::Month), "October 2026");
    }

    #[test]
    fn steps_move_by_the_view() {
        assert_eq!(step(date(2026, 10, 30), Mode::Week, true), date(2026, 11, 6));
        assert_eq!(step(date(2026, 1, 31), Mode::Month, false), date(2025, 12, 1));
        assert_eq!(step(date(2026, 10, 30), Mode::Day, false), date(2026, 10, 29));
    }

    #[test]
    fn overlapping_events_go_side_by_side() {
        let shown = [
            event("a", at(27, 17, 30), at(27, 18, 0)),
            event("b", at(27, 17, 30), at(27, 17, 45)),
            event("c", at(27, 20, 0), at(27, 21, 0)),
        ];
        let slots = day_slots(&shown, date(2026, 10, 27), 20);
        let by_id = |id: &str| slots.iter().find(|slot| slot.shown.event.id == id).unwrap();
        assert_eq!((by_id("a").column, by_id("a").columns), (0, 2));
        assert_eq!((by_id("b").column, by_id("b").columns), (1, 2));
        assert_eq!((by_id("c").column, by_id("c").columns), (0, 1));
        assert_eq!(by_id("a").top, 17 * 60 + 30);
        assert_eq!(by_id("b").length, 20);
    }

    #[test]
    fn an_all_day_event_lists_on_its_days_only() {
        let all_day = event("x", at(28, 0, 0), at(29, 0, 0));
        assert!(all_day.all_day());
        assert!(all_day.on(date(2026, 10, 28)));
        assert!(!all_day.on(date(2026, 10, 29)));
        assert!(day_slots(&[all_day], date(2026, 10, 28), 20).is_empty());
    }

    #[test]
    fn an_all_day_event_snaps_to_the_nearest_midnights() {
        // written as the midnights of a zone three hours ahead of this one
        let mut event = (*event("y", at(27, 21, 0), at(28, 21, 0)).event).clone();
        event.all_day = true;
        let shown = Shown::of(Rc::new(event)).unwrap();
        assert!(shown.on(date(2026, 10, 28)));
        assert!(!shown.on(date(2026, 10, 27)));
        assert!(!shown.on(date(2026, 10, 29)));
    }
}
