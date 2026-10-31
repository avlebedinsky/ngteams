//! A repeating event's recurrence (`docs/protocol/calendar.md`, "A series"): how the web client's scheduling form
//! writes it on the event it saves, and how an event read whole gives it back.
//!
//! The event carries two fields beside its own (the bundle's model, the one its scheduling form and its other callers
//! build):
//!
//! - `eventRecurrencePattern`: `{"patternType": <n>, "<kind>": {…}}`, the kind's object holding what repeats it:
//!   `daily: {interval}`, `weekly: {interval, daysOfTheWeek: [<day>…]}`, `monthly: {interval, dayOfMonth}`,
//!   `relativeMonthly: {interval, dayOfTheWeek, dayOfTheWeekIndex}`, `yearly: {month, dayOfTheMonth}`,
//!   `relativeYearly: {month, dayOfTheWeek, dayOfTheWeekIndex}`. Its numbers are the model's: the pattern types 1 to 6
//!   (daily, weekly, monthly, relative monthly, yearly, relative yearly), the days 0 (Sunday) to 6, the week of the
//!   month 0 (first) to 4 (last), the months 1 to 12.
//! - `eventRecurrenceRange`: `{"startDate", "endDate"?}`, the series' first day as its local midnight with its offset
//!   (`2026-10-27T00:00:00+03:00`, as the form writes it), and its last day the same way when it ends.
//!
//! An answer may give the same values by name (`"Weekly"`, `"Tuesday"`, `"Last"`) or as numbers in strings: each reads.

use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::write::ZonedTime;

/// A day of the week, as a pattern names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Weekday {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl Weekday {
    /// Every day, Sunday first (the model's numbering).
    pub const ALL: [Self; 7] = [
        Self::Sunday,
        Self::Monday,
        Self::Tuesday,
        Self::Wednesday,
        Self::Thursday,
        Self::Friday,
        Self::Saturday,
    ];

    /// Monday to Friday.
    pub const WORK_WEEK: [Self; 5] = [
        Self::Monday,
        Self::Tuesday,
        Self::Wednesday,
        Self::Thursday,
        Self::Friday,
    ];

    /// Its number: 0 for Sunday to 6 for Saturday.
    #[must_use]
    pub fn number(self) -> u8 {
        match self {
            Self::Sunday => 0,
            Self::Monday => 1,
            Self::Tuesday => 2,
            Self::Wednesday => 3,
            Self::Thursday => 4,
            Self::Friday => 5,
            Self::Saturday => 6,
        }
    }

    /// Its name in English.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Sunday => "Sunday",
            Self::Monday => "Monday",
            Self::Tuesday => "Tuesday",
            Self::Wednesday => "Wednesday",
            Self::Thursday => "Thursday",
            Self::Friday => "Friday",
            Self::Saturday => "Saturday",
        }
    }

    /// The day a pattern's value names: its number, a number in a string, or its name.
    fn read(value: &Value) -> Option<Self> {
        let number = number(value);
        Self::ALL.into_iter().find(|day| {
            number == Some(u64::from(day.number()))
                || value.as_str().is_some_and(|name| name.eq_ignore_ascii_case(day.name()))
        })
    }
}

/// Which week of the month a relative pattern means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WeekIndex {
    First,
    Second,
    Third,
    Fourth,
    Last,
}

impl WeekIndex {
    pub const ALL: [Self; 5] = [Self::First, Self::Second, Self::Third, Self::Fourth, Self::Last];

    /// Its number: 0 (first) to 4 (last).
    #[must_use]
    pub fn number(self) -> u8 {
        match self {
            Self::First => 0,
            Self::Second => 1,
            Self::Third => 2,
            Self::Fourth => 3,
            Self::Last => 4,
        }
    }

    /// Its name in English, lower case (`first`, …, `last`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Second => "second",
            Self::Third => "third",
            Self::Fourth => "fourth",
            Self::Last => "last",
        }
    }

    fn read(value: &Value) -> Option<Self> {
        let number = number(value);
        Self::ALL.into_iter().find(|index| {
            number == Some(u64::from(index.number()))
                || value
                    .as_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(index.name()))
        })
    }
}

/// How an event repeats.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Pattern {
    /// Every `interval` days.
    Daily { interval: u32 },
    /// Every `interval` weeks, on each of `days`.
    Weekly { interval: u32, days: Vec<Weekday> },
    /// Every `interval` months, on day `day` of the month.
    Monthly { interval: u32, day: u8 },
    /// Every `interval` months, on the `index` `day` (the fourth Tuesday).
    RelativeMonthly {
        interval: u32,
        index: WeekIndex,
        day: Weekday,
    },
    /// Every year, on `month` (1 to 12) `day`.
    Yearly { month: u8, day: u8 },
    /// Every year, on the `index` `day` of `month`.
    RelativeYearly { month: u8, index: WeekIndex, day: Weekday },
}

impl Pattern {
    /// Its `patternType` number.
    #[must_use]
    pub fn type_number(&self) -> u8 {
        match self {
            Self::Daily { .. } => 1,
            Self::Weekly { .. } => 2,
            Self::Monthly { .. } => 3,
            Self::RelativeMonthly { .. } => 4,
            Self::Yearly { .. } => 5,
            Self::RelativeYearly { .. } => 6,
        }
    }

    /// `eventRecurrencePattern` as the form writes it.
    #[must_use]
    pub fn wire(&self) -> Value {
        let (key, inner) = match self {
            Self::Daily { interval } => ("daily", json!({ "interval": interval })),
            Self::Weekly { interval, days } => {
                let mut days: Vec<u8> = days.iter().map(|day| day.number()).collect();
                days.sort_unstable();
                days.dedup();
                ("weekly", json!({ "interval": interval, "daysOfTheWeek": days }))
            }
            Self::Monthly { interval, day } => ("monthly", json!({ "interval": interval, "dayOfMonth": day })),
            Self::RelativeMonthly { interval, index, day } => (
                "relativeMonthly",
                json!({ "interval": interval, "dayOfTheWeek": day.number(), "dayOfTheWeekIndex": index.number() }),
            ),
            Self::Yearly { month, day } => ("yearly", json!({ "month": month, "dayOfTheMonth": day })),
            Self::RelativeYearly { month, index, day } => (
                "relativeYearly",
                json!({ "month": month, "dayOfTheWeek": day.number(), "dayOfTheWeekIndex": index.number() }),
            ),
        };
        let mut pattern = Map::new();
        pattern.insert("patternType".to_owned(), json!(self.type_number()));
        pattern.insert(key.to_owned(), inner);
        Value::Object(pattern)
    }

    /// The pattern `value` (`eventRecurrencePattern`) describes; `None` for one without a type this reads, or whose
    /// type's object is missing or out of range.
    #[must_use]
    pub fn read(value: &Value) -> Option<Self> {
        let type_number = pattern_type(value.get("patternType")?)?;
        Some(match type_number {
            1 => Self::Daily {
                interval: interval(value.get("daily")?)?,
            },
            2 => weekly(value.get("weekly")?)?,
            3 => {
                let inner = value.get("monthly")?;
                Self::Monthly {
                    interval: interval(inner)?,
                    day: small(inner, "dayOfMonth", 1..=31)?,
                }
            }
            4 => {
                let inner = value.get("relativeMonthly")?;
                Self::RelativeMonthly {
                    interval: interval(inner)?,
                    index: WeekIndex::read(inner.get("dayOfTheWeekIndex")?)?,
                    day: Weekday::read(inner.get("dayOfTheWeek")?)?,
                }
            }
            5 => {
                let inner = value.get("yearly")?;
                Self::Yearly {
                    month: small(inner, "month", 1..=12)?,
                    day: small(inner, "dayOfTheMonth", 1..=31)?,
                }
            }
            6 => {
                let inner = value.get("relativeYearly")?;
                Self::RelativeYearly {
                    month: small(inner, "month", 1..=12)?,
                    index: WeekIndex::read(inner.get("dayOfTheWeekIndex")?)?,
                    day: Weekday::read(inner.get("dayOfTheWeek")?)?,
                }
            }
            _ => return None,
        })
    }
}

/// A pattern's type number (`patternType`: its number, or its name in any case); `None` for anything else.
fn pattern_type(kind: &Value) -> Option<u64> {
    const NAMES: [&str; 6] = [
        "Daily",
        "Weekly",
        "Monthly",
        "RelativeMonthly",
        "Yearly",
        "RelativeYearly",
    ];
    number(kind).or_else(|| {
        let name = kind.as_str()?;
        NAMES
            .iter()
            .position(|known| known.eq_ignore_ascii_case(name))
            .and_then(|at| u64::try_from(at + 1).ok())
    })
}

/// A pattern's `interval` (1 when absent); `None` when it is out of range.
fn interval(inner: &Value) -> Option<u32> {
    inner
        .get("interval")
        .map_or(Some(1), number)
        .and_then(|interval| u32::try_from(interval).ok())
        .filter(|interval| (1..=999).contains(interval))
}

/// A small number at `key` within `range`; `None` when it is missing or out of range.
fn small(inner: &Value, key: &str, range: std::ops::RangeInclusive<u8>) -> Option<u8> {
    number(inner.get(key)?)
        .and_then(|value| u8::try_from(value).ok())
        .filter(|value| range.contains(value))
}

/// A weekly pattern's object (`weekly`); `None` without a day or with one that doesn't read.
fn weekly(inner: &Value) -> Option<Pattern> {
    let days: Vec<Weekday> = inner
        .get("daysOfTheWeek")?
        .as_array()?
        .iter()
        .map(Weekday::read)
        .collect::<Option<_>>()?;
    if days.is_empty() {
        return None;
    }
    Some(Pattern::Weekly {
        interval: interval(inner)?,
        days,
    })
}

/// A series' recurrence: how it repeats, from its first day, until its last day when it ends.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Recurrence {
    pub pattern: Pattern,
    /// The series' first day: its local midnight, with the offset then.
    pub start: ZonedTime,
    /// Its last day the same way, when it ends.
    pub end: Option<ZonedTime>,
}

impl Recurrence {
    /// `eventRecurrenceRange` as the form writes it; `None` for a day out of range.
    #[must_use]
    pub fn range_wire(&self) -> Option<Value> {
        let mut range = Map::new();
        range.insert("startDate".to_owned(), json!(self.start.iso()?));
        if let Some(end) = self.end {
            range.insert("endDate".to_owned(), json!(end.iso()?));
        }
        Some(Value::Object(range))
    }

    /// The recurrence `event` (an event as read) carries: its pattern and its range's first day both readable, its
    /// last day when it has one.
    #[must_use]
    pub fn read(event: &Map<String, Value>) -> Option<Self> {
        let pattern = Pattern::read(event.get("eventRecurrencePattern")?)?;
        let range = event.get("eventRecurrenceRange")?;
        let start = day(range.get("startDate")?.as_str()?)?;
        let end = match range.get("endDate") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) if text.trim().is_empty() => None,
            Some(value) => Some(day(value.as_str()?)?),
        };
        Some(Self { pattern, start, end })
    }
}

/// A number, or a number in a string.
fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

/// A range's day: an RFC 3339 instant with its offset, else a date (with or without a time and no offset) as that
/// day's midnight in UTC.
fn day(text: &str) -> Option<ZonedTime> {
    let text = text.trim();
    if let Ok(time) = OffsetDateTime::parse(text, &Rfc3339) {
        return Some(ZonedTime {
            ms: i64::try_from(time.unix_timestamp_nanos() / 1_000_000).ok()?,
            offset_minutes: i32::from(time.offset().whole_minutes()),
        });
    }
    let date = text.split(['T', ' ']).next()?;
    let mut parts = date.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let month = time::Month::try_from(month).ok()?;
    let date = time::Date::from_calendar_date(year, month, day).ok()?;
    let ms = i64::try_from(date.midnight().assume_utc().unix_timestamp_nanos() / 1_000_000).ok()?;
    Some(ZonedTime { ms, offset_minutes: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_goes_out_in_the_model_s_numbers_and_reads_back() {
        let weekly = Pattern::Weekly {
            interval: 2,
            days: vec![Weekday::Thursday, Weekday::Tuesday],
        };
        assert_eq!(
            weekly.wire(),
            json!({"patternType": 2, "weekly": {"interval": 2, "daysOfTheWeek": [2, 4]}})
        );
        let monthly = Pattern::RelativeMonthly {
            interval: 1,
            index: WeekIndex::Last,
            day: Weekday::Friday,
        };
        assert_eq!(
            monthly.wire(),
            json!({"patternType": 4, "relativeMonthly": {"interval": 1, "dayOfTheWeek": 5, "dayOfTheWeekIndex": 4}})
        );
        for pattern in [
            Pattern::Daily { interval: 3 },
            Pattern::Weekly {
                interval: 1,
                days: vec![Weekday::Monday, Weekday::Wednesday],
            },
            Pattern::Monthly { interval: 1, day: 27 },
            monthly,
            Pattern::Yearly { month: 10, day: 27 },
            Pattern::RelativeYearly {
                month: 10,
                index: WeekIndex::Fourth,
                day: Weekday::Tuesday,
            },
        ] {
            assert_eq!(Pattern::read(&pattern.wire()), Some(pattern));
        }
    }

    #[test]
    fn a_pattern_given_by_name_reads_too() {
        let named =
            json!({"patternType": "Weekly", "weekly": {"interval": "1", "daysOfTheWeek": ["Tuesday", "friday"]}});
        assert_eq!(
            Pattern::read(&named),
            Some(Pattern::Weekly {
                interval: 1,
                days: vec![Weekday::Tuesday, Weekday::Friday]
            })
        );
        assert_eq!(Pattern::read(&json!({"patternType": 0})), None);
        assert_eq!(
            Pattern::read(&json!({"patternType": 2, "weekly": {"daysOfTheWeek": []}})),
            None
        );
        assert_eq!(
            Pattern::read(&json!({"patternType": 3, "monthly": {"dayOfMonth": 40}})),
            None
        );
    }

    #[test]
    fn a_range_is_written_as_local_midnights_and_reads_back() {
        let recurrence = Recurrence {
            pattern: Pattern::Daily { interval: 1 },
            start: ZonedTime {
                ms: 1_793_048_400_000,
                offset_minutes: 180,
            },
            end: None,
        };
        let range = recurrence.range_wire().expect("a range");
        assert_eq!(range, json!({"startDate": "2026-10-27T00:00:00+03:00"}));
        let mut event = Map::new();
        event.insert("eventRecurrencePattern".to_owned(), recurrence.pattern.wire());
        event.insert("eventRecurrenceRange".to_owned(), range);
        assert_eq!(Recurrence::read(&event), Some(recurrence));
        event.insert(
            "eventRecurrenceRange".to_owned(),
            json!({"startDate": "2026-10-27", "endDate": "2026-12-01T00:00:00"}),
        );
        let read = Recurrence::read(&event).expect("a recurrence");
        assert_eq!(read.start.iso().as_deref(), Some("2026-10-27T00:00:00+00:00"));
        assert_eq!(
            read.end.and_then(ZonedTime::iso).as_deref(),
            Some("2026-12-01T00:00:00+00:00")
        );
    }
}
