//! Synthetic calendar replies and boundary values; no service or tenant data.

use serde_json::{Value, json};
use teams_proto::{CalendarEvent, CalendarView, EventTime, FloatingTime, MAX_CALENDAR_WINDOW_MS};

fn event() -> Value {
    json!({
        "_synthetic": true,
        "objectId": "synthetic-calendar-regression",
        "startTime": "2026-10-26T14:00:00Z",
        "endTime": "2026-10-26T15:00:00Z"
    })
}

#[test]
fn extreme_calendar_windows_are_refused_without_arithmetic_overflow() {
    for (start, end) in [
        (i64::MIN, i64::MAX),
        (i64::MIN, 0),
        (-1, i64::MAX),
        (i64::MAX, i64::MIN),
    ] {
        assert!(CalendarView::between(start, end).is_none(), "{start}..{end}");
    }
    assert!(CalendarView::between(-MAX_CALENDAR_WINDOW_MS, 0).is_some());
    assert!(CalendarView::between(-MAX_CALENDAR_WINDOW_MS - 1, 0).is_none());
}

#[test]
fn impossible_floating_dates_are_rejected_in_both_event_times() {
    for date in [
        "2026-02-30",
        "2025-02-29",
        "1900-02-29",
        "2100-02-29",
        "2026-04-31",
        "2026-06-31",
        "2026-11-31",
    ] {
        for field in ["startTime", "endTime"] {
            let mut value = event();
            value[field] = json!(format!("{date}T14:00:00"));
            assert!(
                CalendarEvent::created(&serde_json::to_vec(&value).unwrap()).is_none(),
                "{field}: {date}"
            );
        }
    }
}

#[test]
fn valid_month_ends_and_leap_days_keep_supported_floating_formats() {
    for (date, year, month, day) in [
        ("2000-02-29", 2000, 2, 29),
        ("2024-02-29", 2024, 2, 29),
        ("2026-02-28", 2026, 2, 28),
        ("2026-04-30", 2026, 4, 30),
        ("2026-01-31", 2026, 1, 31),
    ] {
        for suffix in ["T14:00", " 14:00:00.1234567"] {
            let mut value = event();
            value["startTime"] = json!(format!("{date}{suffix}"));
            let read = CalendarEvent::created(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(
                read.start,
                EventTime::Floating(FloatingTime {
                    year,
                    month,
                    day,
                    hour: 14,
                    minute: 0,
                    second: 0
                }),
                "{date}{suffix}"
            );
        }
    }
}

#[test]
fn an_impossible_date_is_counted_as_skipped_without_discarding_valid_events() {
    let mut invalid = event();
    invalid["startTime"] = json!("2026-02-30T14:00:00");
    let mut valid = event();
    valid["startTime"] = json!("2024-02-29T14:00:00");
    let expected = CalendarEvent::created(&serde_json::to_vec(&valid).unwrap()).unwrap();
    let answer = CalendarView::read_answer(
        &serde_json::to_vec(&json!({"_synthetic": true, "value": [invalid, valid]})).unwrap(),
    )
    .unwrap();
    assert_eq!(answer.skipped, 1);
    assert_eq!(answer.events, [expected]);
}
