//! What a profile card says of a person's presence, their calendar and their clock, in Teams' words (the persona
//! card's own strings, seen 2026-11-11 with lab accounts): "Last seen 4 h ago • Free on 11/12", their work hours and
//! "10:02 PM - Same time zone as you". Pure: the time now, the viewer's offset from UTC and the clock's format are
//! given, so every form is tested without a clock.
//!
//! - **Presence**: someone offline with a time they were last active is "Last seen {when}" ("4 hours ago" on the full
//!   card, "4 h ago" on the compact one), offline with none just "Offline"; any other state its word.
//! - **Their calendar** (Loki's schedule, Graph's free/busy words): outside their working hours (in their own time
//!   zone, on their working days) "Free on {the next working day}"; inside them "Free until {time}" (their next change,
//!   else the end of their day), "Busy until …", "Tentative until …", "Working elsewhere until …"; out of office "Out of
//!   office until {date}". A time on another day than today is its date.
//! - **Their clock**: their time now and how it stands to the viewer's: "Same time zone as you", "You're 2 hours
//!   ahead", "You're 30 minutes behind"; the account's own card "Your local time".

use chrono::{DateTime, Datelike as _, Duration, FixedOffset, NaiveDate, NaiveTime, Offset as _, TimeZone as _, Utc};
use teams_core::{Availability, CardPresence};
use teams_proto::{ClockTime, FreeBusyStatus, Schedule, Weekday};

/// Between the presence and the calendar in the availability line, as Teams joins them.
pub const JOINED: &str = " \u{2022} ";
/// The own card's clock.
pub const YOUR_TIME: &str = "Your local time";
/// The clock of someone in the viewer's time zone.
pub const SAME_ZONE: &str = "Same time zone as you";
/// Someone offline UPS never saw active.
pub const OFFLINE: &str = "Offline";
/// Out of office, alone.
pub const OUT_OF_OFFICE: &str = "Out of office";

/// How long a "… ago" is written: the full card's words or the compact card's short ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Length {
    /// "4 hours ago".
    Long,
    /// "4 h ago".
    Short,
}

/// How long ago `since_ms` was at `now_ms`, in Teams' words.
#[must_use]
pub fn ago(since_ms: i64, now_ms: i64, length: Length) -> String {
    let minutes = (now_ms - since_ms).max(0) / 60_000;
    let hours = minutes / 60;
    let days = hours / 24;
    match (length, minutes, hours, days) {
        (_, 0, _, _) => "just now".to_owned(),
        (Length::Long, 1, _, _) => "1 minute ago".to_owned(),
        (Length::Long, minutes, 0, _) => format!("{minutes} minutes ago"),
        (Length::Short, minutes, 0, _) => format!("{minutes} min ago"),
        (Length::Long, _, 1, _) => "1 hour ago".to_owned(),
        (Length::Long, _, hours, 0) => format!("{hours} hours ago"),
        (Length::Short, _, hours, 0) => format!("{hours} h ago"),
        (_, _, _, 1) => "yesterday".to_owned(),
        (Length::Long, _, _, days) => format!("{days} days ago"),
        (Length::Short, _, _, days) => format!("{days} d ago"),
    }
}

/// What the availability line says of their presence: `live` (the presence model's) when it is known, else the card's
/// read; `None` when neither knows it.
#[must_use]
pub fn presence(card: Option<&CardPresence>, live: &Availability, now_ms: i64, length: Length) -> Option<String> {
    let availability = if *live == Availability::Unknown {
        card.map_or(&Availability::Unknown, |card| &card.availability)
    } else {
        live
    };
    let last_seen = card.and_then(|card| card.last_active_ms);
    match (availability, last_seen) {
        (Availability::Offline | Availability::Unknown, Some(at)) => {
            Some(format!("Last seen {}", ago(at, now_ms, length)))
        }
        (Availability::Offline, None) => Some(OFFLINE.to_owned()),
        (Availability::Unknown, None) => None,
        (known, _) => Some(crate::presence::label(known).to_string()),
    }
}

/// The availability line: the presence's words and the calendar's, joined as Teams joins them; `None` when neither
/// says anything.
#[must_use]
pub fn availability_line(presence: Option<String>, calendar: Option<String>) -> Option<String> {
    match (presence, calendar) {
        (Some(presence), Some(calendar)) => Some(format!("{presence}{JOINED}{calendar}")),
        (one, other) => one.or(other),
    }
}

/// When the clock is read and how it is written: the time now, the viewer's offset east of UTC in minutes and the
/// time of day's `chrono` format (`HourCycle::time_of_day`).
#[derive(Clone, Copy, Debug)]
pub struct Now<'a> {
    pub ms: i64,
    pub viewer_offset: i32,
    pub clock: &'a str,
}

impl Now<'_> {
    fn zone(minutes: i32) -> FixedOffset {
        FixedOffset::east_opt(minutes.saturating_mul(60)).unwrap_or_else(|| Utc.fix())
    }

    /// The time now in the zone `minutes` east of UTC.
    fn in_zone(&self, minutes: i32) -> DateTime<FixedOffset> {
        let zone = Self::zone(minutes);
        zone.timestamp_millis_opt(self.ms)
            .single()
            .unwrap_or_else(|| zone.timestamp_nanos(0))
    }

    fn viewer(&self) -> DateTime<FixedOffset> {
        self.in_zone(self.viewer_offset)
    }

    /// `at_ms` on the viewer's clock: its time when it is today, else its date ("11/12").
    #[must_use]
    pub fn when(&self, at_ms: i64) -> String {
        let zone = Self::zone(self.viewer_offset);
        let Some(at) = zone.timestamp_millis_opt(at_ms).single() else {
            return String::new();
        };
        if at.date_naive() == self.viewer().date_naive() {
            at.format(self.clock).to_string()
        } else {
            date(at.date_naive())
        }
    }
}

/// A date as Teams writes it on the card: "11/12".
fn date(day: NaiveDate) -> String {
    format!("{}/{}", day.month(), day.day())
}

fn clock_time(time: ClockTime) -> Option<NaiveTime> {
    NaiveTime::from_hms_opt(u32::from(time.hour), u32::from(time.minute), 0)
}

fn weekday(day: chrono::Weekday) -> Weekday {
    match day {
        chrono::Weekday::Sun => Weekday::Sunday,
        chrono::Weekday::Mon => Weekday::Monday,
        chrono::Weekday::Tue => Weekday::Tuesday,
        chrono::Weekday::Wed => Weekday::Wednesday,
        chrono::Weekday::Thu => Weekday::Thursday,
        chrono::Weekday::Fri => Weekday::Friday,
        chrono::Weekday::Sat => Weekday::Saturday,
    }
}

/// Their working hours on their clock, when the schedule gives both ends.
fn hours(schedule: &Schedule) -> Option<(NaiveTime, NaiveTime)> {
    Some((clock_time(schedule.work_start?)?, clock_time(schedule.work_end?)?))
}

/// Whether `day` is one of their working days (every day when the schedule names none).
fn works_on(schedule: &Schedule, day: NaiveDate) -> bool {
    schedule.work_days.is_empty() || schedule.work_days.contains(&weekday(day.weekday()))
}

/// Whether `at` (on their clock) is inside their working hours; a schedule without hours is always inside them.
fn at_work(schedule: &Schedule, at: DateTime<FixedOffset>) -> bool {
    let Some((start, end)) = hours(schedule) else {
        return true;
    };
    let time = at.time();
    works_on(schedule, at.date_naive()) && start <= time && time < end
}

/// Their next working day from `at` (on their clock): today before their day starts, else the first working day after.
fn next_working_day(schedule: &Schedule, at: DateTime<FixedOffset>) -> NaiveDate {
    let today = at.date_naive();
    let before_start = hours(schedule).is_some_and(|(start, _)| at.time() < start);
    if before_start && works_on(schedule, today) {
        return today;
    }
    (1..=7)
        .filter_map(|days| today.checked_add_signed(Duration::days(days)))
        .find(|day| works_on(schedule, *day))
        .unwrap_or(today)
}

/// The end of their working day today, ms, when the schedule gives it.
fn end_of_day(schedule: &Schedule, at: DateTime<FixedOffset>) -> Option<i64> {
    let (_, end) = hours(schedule)?;
    let ended = at
        .date_naive()
        .and_time(end)
        .and_local_timezone(*at.offset())
        .single()?;
    Some(ended.timestamp_millis())
}

/// What the availability line says of their calendar (see the module docs); `None` when the schedule says nothing.
#[must_use]
pub fn calendar(schedule: &Schedule, now: &Now<'_>) -> Option<String> {
    let theirs = now.in_zone(schedule.utc_offset_minutes.unwrap_or(now.viewer_offset));
    let until = |word: &str, at: Option<i64>| match at {
        Some(at) => format!("{word} until {}", now.when(at)),
        None => word.to_owned(),
    };
    match schedule.current.as_ref()? {
        FreeBusyStatus::OutOfOffice => Some(until(OUT_OF_OFFICE, schedule.until_ms)),
        FreeBusyStatus::Unknown | FreeBusyStatus::Other(_) => None,
        _ if !at_work(schedule, theirs) => Some(format!("Free on {}", date(next_working_day(schedule, theirs)))),
        FreeBusyStatus::Free => Some(until(
            "Free",
            schedule.until_ms.or_else(|| end_of_day(schedule, theirs)),
        )),
        FreeBusyStatus::Busy => Some(until("Busy", schedule.until_ms)),
        FreeBusyStatus::Tentative => Some(until("Tentative", schedule.until_ms)),
        FreeBusyStatus::WorkingElsewhere => Some(until("Working elsewhere", schedule.until_ms)),
    }
}

/// "Work hours: 8:00 AM - 5:00 PM", their hours as the schedule gives them; `None` without both ends.
#[must_use]
pub fn work_hours(schedule: &Schedule, clock: &str) -> Option<String> {
    let (start, end) = hours(schedule)?;
    Some(format!("Work hours: {} - {}", start.format(clock), end.format(clock)))
}

/// How far apart two offsets are, in words: "2 hours", "1 hour", "30 minutes", "5 hours 30 minutes".
fn span(minutes: i32) -> String {
    let (hours, minutes) = (minutes / 60, minutes % 60);
    let hours_word = match hours {
        1 => "1 hour".to_owned(),
        hours => format!("{hours} hours"),
    };
    let minutes_word = match minutes {
        1 => "1 minute".to_owned(),
        minutes => format!("{minutes} minutes"),
    };
    match (hours, minutes) {
        (0, _) => minutes_word,
        (_, 0) => hours_word,
        _ => format!("{hours_word} {minutes_word}"),
    }
}

/// How the viewer's time zone stands to theirs, in Teams' words.
#[must_use]
pub fn zone_difference(viewer_offset: i32, their_offset: i32) -> String {
    let ahead = viewer_offset - their_offset;
    match ahead.signum() {
        0 => SAME_ZONE.to_owned(),
        1 => format!("You're {} ahead", span(ahead)),
        _ => format!("You're {} behind", span(-ahead)),
    }
}

/// The clock row: their time now and how it stands to the viewer's ("Your local time" on the account's own card);
/// `None` when their time zone isn't known.
#[must_use]
pub fn clock_line(their_offset: Option<i32>, own: bool, now: &Now<'_>) -> Option<String> {
    if own {
        return Some(format!("{} - {YOUR_TIME}", now.viewer().format(now.clock)));
    }
    let theirs = their_offset?;
    Some(format!(
        "{} - {}",
        now.in_zone(theirs).format(now.clock),
        zone_difference(now.viewer_offset, theirs)
    ))
}

/// The viewer's offset east of UTC now, minutes.
#[must_use]
pub fn local_offset() -> i32 {
    chrono::Local::now().offset().local_minus_utc() / 60
}

/// The time now, ms since the epoch.
#[must_use]
pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[cfg(test)]
#[path = "../../tests/support/profile_card_units.rs"]
mod tests;
