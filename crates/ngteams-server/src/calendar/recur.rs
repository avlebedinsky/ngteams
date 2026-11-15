//! A series' recurrence, as the scheduling form writes it on an event (`eventRecurrencePattern`,
//! `eventRecurrenceRange`: `docs/protocol/calendar.md`, "A series"), and its occurrences. The rule runs on the
//! clock of the series' own zone, so an occurrence keeps its time of day across a daylight-time change.

use chrono::{Datelike as _, Month, NaiveDate, NaiveDateTime, TimeZone as _, Weekday};
use rrule::{Frequency, NWeekday, RRule, RRuleSet, Tz, Unvalidated};
use serde_json::{Map, Value};

use super::zones::Zone;

/// The most occurrences one read takes of a series.
const MOST: u16 = 1000;

/// A series: its rule on its zone's clock, and how long each occurrence lasts.
pub(super) struct Series {
    rule: RRuleSet,
    zone: Zone,
    duration_ms: i64,
    last_day: Option<NaiveDate>,
}

/// The series an event's fields describe, starting at `start_ms`: `Ok(None)` for an event that doesn't repeat.
///
/// # Errors
/// The pattern or the range isn't one the form writes, or one comes without the other.
pub(super) fn series(body: &Map<String, Value>, start_ms: i64, end_ms: i64, zone: Zone) -> Result<Option<Series>, ()> {
    let given = |key: &str| body.get(key).filter(|value| !value.is_null());
    let (pattern, range) = match (given("eventRecurrencePattern"), given("eventRecurrenceRange")) {
        (None, None) => return Ok(None),
        (Some(pattern), Some(range)) => (pattern, range),
        _ => return Err(()),
    };
    let first_day = day(range.get("startDate")).ok_or(())?;
    let last_day = match range.get("endDate") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text.trim().is_empty() => None,
        end => Some(day(end).ok_or(())?),
    };
    let start = zone.local(start_ms);
    let first = first_day.max(start.date()).and_time(start.time());
    let mut rule = rule(pattern).ok_or(())?;
    if let Some(last) = last_day {
        if last < first.date() {
            return Err(());
        }
        let until = last.and_hms_opt(23, 59, 59).ok_or(())?;
        rule = rule.until(Tz::UTC.from_utc_datetime(&until));
    }
    let rule = rule.build(Tz::UTC.from_utc_datetime(&first)).map_err(|_| ())?;
    Ok(Some(Series {
        rule,
        zone,
        duration_ms: end_ms - start_ms,
        last_day,
    }))
}

impl Series {
    /// The starts of the occurrences that overlap `[from_ms, to_ms)`, in order.
    pub(super) fn starts(&self, from_ms: i64, to_ms: i64) -> Vec<i64> {
        let margin = chrono::Duration::days(1);
        let after = self.zone.local(from_ms - self.duration_ms) - margin;
        let before = self.zone.local(to_ms) + margin;
        let found = self
            .rule
            .clone()
            .after(Tz::UTC.from_utc_datetime(&after))
            .before(Tz::UTC.from_utc_datetime(&before))
            .all(MOST);
        found
            .dates
            .iter()
            .map(|local| self.zone.instant(local.naive_utc()))
            .filter(|start| *start < to_ms && (start + self.duration_ms > from_ms || *start >= from_ms))
            .collect()
    }

    /// Whether an occurrence starts at `start_ms`.
    pub(super) fn has(&self, start_ms: i64) -> bool {
        self.starts(start_ms, start_ms + 1).contains(&start_ms)
    }

    /// When the last occurrence is over, for a series that ends.
    pub(super) fn until_ms(&self) -> Option<i64> {
        let end_of_day: NaiveDateTime = self.last_day?.succ_opt()?.and_hms_opt(0, 0, 0)?;
        Some(self.zone.instant(end_of_day) + self.duration_ms)
    }
}

/// The rule of an `eventRecurrencePattern`; the week starts on Sunday, as Exchange's does.
fn rule(pattern: &Value) -> Option<RRule<Unvalidated>> {
    let kind = pattern_type(pattern.get("patternType")?)?;
    let rule = match kind {
        1 => RRule::new(Frequency::Daily).interval(interval(pattern.get("daily")?)?),
        2 => {
            let inner = pattern.get("weekly")?;
            let days: Vec<NWeekday> = inner
                .get("daysOfTheWeek")?
                .as_array()?
                .iter()
                .map(|day| weekday(day).map(NWeekday::Every))
                .collect::<Option<_>>()?;
            if days.is_empty() {
                return None;
            }
            RRule::new(Frequency::Weekly)
                .interval(interval(inner)?)
                .by_weekday(days)
        }
        3 => {
            let inner = pattern.get("monthly")?;
            on_day(RRule::new(Frequency::Monthly), small(inner, "dayOfMonth", 1, 31)?).interval(interval(inner)?)
        }
        4 => {
            let inner = pattern.get("relativeMonthly")?;
            RRule::new(Frequency::Monthly)
                .interval(interval(inner)?)
                .by_weekday(vec![nth(inner)?])
        }
        5 => {
            let inner = pattern.get("yearly")?;
            on_day(
                RRule::new(Frequency::Yearly).by_month(&[month(inner)?]),
                small(inner, "dayOfTheMonth", 1, 31)?,
            )
        }
        6 => {
            let inner = pattern.get("relativeYearly")?;
            RRule::new(Frequency::Yearly)
                .by_month(&[month(inner)?])
                .by_weekday(vec![nth(inner)?])
        }
        _ => return None,
    };
    Some(rule.week_start(Weekday::Sun))
}

/// On a day of the month; in a month without it, on the month's last day (the 31st in April is the 30th).
fn on_day(rule: RRule<Unvalidated>, day: i8) -> RRule<Unvalidated> {
    if day <= 28 {
        rule.by_month_day(vec![day])
    } else {
        rule.by_month_day((28..=day).collect()).by_set_pos(vec![-1])
    }
}

/// The first to fourth or the last (`dayOfTheWeekIndex` 0 to 4) of a weekday in its month.
fn nth(inner: &Value) -> Option<NWeekday> {
    let index = inner.get("dayOfTheWeekIndex")?;
    let names = ["first", "second", "third", "fourth", "last"];
    let index = number(index)
        .and_then(|index| usize::try_from(index).ok())
        .or_else(|| {
            let name = index.as_str()?;
            names.iter().position(|known| known.eq_ignore_ascii_case(name))
        })
        .filter(|index| *index < names.len())?;
    let nth = [1, 2, 3, 4, -1][index];
    Some(NWeekday::Nth(nth, weekday(inner.get("dayOfTheWeek")?)?))
}

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
        let at = NAMES.iter().position(|known| known.eq_ignore_ascii_case(name))?;
        u64::try_from(at + 1).ok()
    })
}

fn interval(inner: &Value) -> Option<u16> {
    let interval = inner.get("interval").map_or(Some(1), number)?;
    u16::try_from(interval)
        .ok()
        .filter(|interval| (1..=999).contains(interval))
}

fn small(inner: &Value, key: &str, least: i8, most: i8) -> Option<i8> {
    number(inner.get(key)?)
        .and_then(|value| i8::try_from(value).ok())
        .filter(|value| (least..=most).contains(value))
}

fn month(inner: &Value) -> Option<Month> {
    Month::try_from(u8::try_from(small(inner, "month", 1, 12)?).ok()?).ok()
}

/// A weekday as a number (0 Sunday to 6 Saturday) or by name.
fn weekday(value: &Value) -> Option<Weekday> {
    const DAYS: [Weekday; 7] = [
        Weekday::Sun,
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
    ];
    if let Some(number) = number(value) {
        return DAYS.get(usize::try_from(number).ok()?).copied();
    }
    let name = value.as_str()?.trim().to_ascii_lowercase();
    DAYS.into_iter()
        .find(|day| name.len() >= 3 && day.to_string().to_ascii_lowercase() == name[..3])
}

fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

/// The day a range's date names: the date as written, whatever offset follows it.
fn day(value: Option<&Value>) -> Option<NaiveDate> {
    let text = value?.as_str()?.trim();
    let date = text.split(['T', ' ']).next()?;
    let day = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    (1970..=9000).contains(&day.year()).then_some(day)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const MINUTE: i64 = 60_000;
    const DAY: i64 = 24 * 60 * MINUTE;

    fn ms(text: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(text).unwrap().timestamp_millis()
    }

    fn local(zone: Zone, starts: &[i64]) -> Vec<String> {
        starts
            .iter()
            .map(|start| zone.local(*start).format("%a %Y-%m-%d %H:%M").to_string())
            .collect()
    }

    /// The occurrences in the `days` days from the series' start, on its zone's clock.
    fn occurrences(pattern: &Value, range: &Value, start: &str, zone_name: &str, days: i64) -> Vec<String> {
        let zone = Zone::of(Some(zone_name), 0);
        let body = json!({"eventRecurrencePattern": pattern, "eventRecurrenceRange": range});
        let start = ms(start);
        let series = series(body.as_object().unwrap(), start, start + 30 * MINUTE, zone)
            .unwrap()
            .unwrap();
        local(zone, &series.starts(start, start + days * DAY))
    }

    #[test]
    fn each_pattern_the_form_writes_gives_its_occurrences() {
        let from = json!({"startDate": "2027-01-04T00:00:00+00:00"});
        let start = "2027-01-04T09:00:00Z";
        assert_eq!(
            occurrences(
                &json!({"patternType": 1, "daily": {"interval": 2}}),
                &from,
                start,
                "Utc",
                5
            ),
            ["Mon 2027-01-04 09:00", "Wed 2027-01-06 09:00", "Fri 2027-01-08 09:00"]
        );
        // every second week on Monday and Friday: the week starts on Sunday
        let weekly = json!({"patternType": "Weekly", "weekly": {"interval": "2", "daysOfTheWeek": [5, "Monday"]}});
        assert_eq!(
            occurrences(&weekly, &from, start, "Utc", 21),
            [
                "Mon 2027-01-04 09:00",
                "Fri 2027-01-08 09:00",
                "Mon 2027-01-18 09:00",
                "Fri 2027-01-22 09:00"
            ]
        );
        let monthly = json!({"patternType": 3, "monthly": {"interval": 1, "dayOfMonth": 31}});
        let from = json!({"startDate": "2027-01-31T00:00:00+00:00"});
        assert_eq!(
            occurrences(&monthly, &from, "2027-01-31T09:00:00Z", "Utc", 95),
            [
                "Sun 2027-01-31 09:00",
                "Sun 2027-02-28 09:00",
                "Wed 2027-03-31 09:00",
                "Fri 2027-04-30 09:00"
            ]
        );
        let last_friday = json!({"patternType": 4,
            "relativeMonthly": {"interval": 1, "dayOfTheWeek": 5, "dayOfTheWeekIndex": 4}});
        let from = json!({"startDate": "2027-01-29"});
        assert_eq!(
            occurrences(&last_friday, &from, "2027-01-29T09:00:00Z", "Utc", 62),
            ["Fri 2027-01-29 09:00", "Fri 2027-02-26 09:00", "Fri 2027-03-26 09:00"]
        );
        let yearly = json!({"patternType": 5, "yearly": {"month": 2, "dayOfTheMonth": 29}});
        let from = json!({"startDate": "2028-02-29"});
        assert_eq!(
            occurrences(&yearly, &from, "2028-02-29T09:00:00Z", "Utc", 366),
            ["Tue 2028-02-29 09:00", "Wed 2029-02-28 09:00"]
        );
        let second_tuesday = json!({"patternType": "RelativeYearly",
            "relativeYearly": {"month": 3, "dayOfTheWeek": "Tuesday", "dayOfTheWeekIndex": "second"}});
        let from = json!({"startDate": "2027-03-09"});
        assert_eq!(
            occurrences(&second_tuesday, &from, "2027-03-09T09:00:00Z", "Utc", 400),
            ["Tue 2027-03-09 09:00", "Tue 2028-03-14 09:00"]
        );
    }

    #[test]
    fn a_series_keeps_its_time_of_day_in_its_own_zone_and_ends_on_its_last_day() {
        let weekly = json!({"patternType": 2, "weekly": {"interval": 1, "daysOfTheWeek": [1]}});
        let range = json!({"startDate": "2027-03-22T00:00:00+01:00", "endDate": "2027-04-05T00:00:00+02:00"});
        let zone = Zone::of(Some("WEuropeSt"), 60);
        let body = json!({"eventRecurrencePattern": weekly, "eventRecurrenceRange": range});
        let start = ms("2027-03-22T10:00:00+01:00");
        let series = series(body.as_object().unwrap(), start, start + 30 * MINUTE, zone)
            .unwrap()
            .unwrap();
        let starts = series.starts(start - DAY, start + 60 * DAY);
        assert_eq!(
            local(zone, &starts),
            ["Mon 2027-03-22 10:00", "Mon 2027-03-29 10:00", "Mon 2027-04-05 10:00"]
        );
        // the clock went forward between the first two: a week is an hour short
        assert_eq!(starts[1] - starts[0], 7 * DAY - 60 * MINUTE);
        assert_eq!(series.until_ms(), Some(ms("2027-04-06T00:30:00+02:00")));
        assert!(series.has(starts[1]) && !series.has(starts[1] + MINUTE));
        // an occurrence that began before the window and is not over is in it; one that ends where it begins isn't
        assert_eq!(
            series.starts(starts[1] + 10 * MINUTE, starts[1] + 20 * MINUTE),
            [starts[1]]
        );
        assert!(series.starts(starts[1] + 30 * MINUTE, starts[2]).is_empty());
    }

    #[test]
    fn an_event_without_a_readable_recurrence_is_no_series_or_is_refused() {
        let zone = Zone::of(None, 0);
        let read = |body: Value| series(body.as_object().unwrap(), 0, MINUTE, zone).map(|series| series.is_some());
        assert_eq!(read(json!({})), Ok(false));
        assert_eq!(
            read(json!({"eventRecurrencePattern": null, "eventRecurrenceRange": null})),
            Ok(false)
        );
        assert_eq!(
            read(json!({"eventRecurrenceRange": {"startDate": "2027-01-04"}})),
            Err(())
        );
        let range = json!({"startDate": "2027-01-04"});
        for pattern in [
            json!({"patternType": 9}),
            json!({"patternType": 1, "daily": {"interval": 0}}),
            json!({"patternType": 2, "weekly": {"daysOfTheWeek": []}}),
            json!({"patternType": 2, "weekly": {"daysOfTheWeek": ["Someday"]}}),
            json!({"patternType": 3, "monthly": {"dayOfMonth": 32}}),
            json!({"patternType": 4, "relativeMonthly": {"dayOfTheWeek": 1, "dayOfTheWeekIndex": 5}}),
            json!({"patternType": 5, "yearly": {"month": 13, "dayOfTheMonth": 1}}),
        ] {
            let body = json!({"eventRecurrencePattern": pattern, "eventRecurrenceRange": range});
            assert_eq!(read(body.clone()), Err(()), "{body}");
        }
        let daily = json!({"patternType": 1, "daily": {"interval": 1}});
        for range in [
            json!({}),
            json!({"startDate": "soon"}),
            json!({"startDate": "2027-01-04", "endDate": "1969-01-01"}),
            json!({"startDate": "2027-01-04", "endDate": 7}),
        ] {
            let body = json!({"eventRecurrencePattern": daily, "eventRecurrenceRange": range});
            assert_eq!(read(body.clone()), Err(()), "{body}");
        }
        let endless = json!({"eventRecurrencePattern": daily, "eventRecurrenceRange": {"startDate": "1970-01-01", "endDate": " "}});
        assert_eq!(read(endless), Ok(true));
    }
}
