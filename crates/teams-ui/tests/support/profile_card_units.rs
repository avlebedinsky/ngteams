//! Offline unit scenarios for the profile card's words; kept outside runtime coverage sources.

use super::*;

const TWELVE: &str = "%-I:%M %p";
const THREE_EAST: i32 = 180;

/// A UTC moment, ms.
fn utc(day: u32, hour: u32, minute: u32) -> i64 {
    Utc.with_ymd_and_hms(2026, 11, day, hour, minute, 0)
        .single()
        .unwrap()
        .timestamp_millis()
}

fn now(ms: i64) -> Now<'static> {
    Now {
        ms,
        viewer_offset: THREE_EAST,
        clock: TWELVE,
    }
}

/// Monday to Friday, 8:00 to 17:00 at UTC+3, free.
fn schedule() -> Schedule {
    Schedule {
        current: Some(FreeBusyStatus::Free),
        next: Some(FreeBusyStatus::Free),
        until_ms: None,
        work_days: Weekday::WORK_WEEK.to_vec(),
        work_start: Some(ClockTime { hour: 8, minute: 0 }),
        work_end: Some(ClockTime { hour: 17, minute: 0 }),
        utc_offset_minutes: Some(THREE_EAST),
        zone_name: Some("GMT+03:00".to_owned()),
    }
}

#[test]
fn ago_is_long_on_the_full_card_and_short_on_the_compact_one() {
    let now = utc(11, 19, 0);
    let minutes = |m: i64| now - m * 60_000;
    assert_eq!(ago(now, now, Length::Long), "just now");
    assert_eq!(ago(now + 5_000, now, Length::Short), "just now", "a clock ahead is now");
    assert_eq!(ago(minutes(1), now, Length::Long), "1 minute ago");
    assert_eq!(ago(minutes(7), now, Length::Long), "7 minutes ago");
    assert_eq!(ago(minutes(7), now, Length::Short), "7 min ago");
    assert_eq!(ago(minutes(60), now, Length::Long), "1 hour ago");
    assert_eq!(ago(minutes(4 * 60 + 10), now, Length::Long), "4 hours ago");
    assert_eq!(ago(minutes(4 * 60 + 10), now, Length::Short), "4 h ago");
    assert_eq!(ago(minutes(30 * 60), now, Length::Long), "yesterday");
    assert_eq!(ago(minutes(30 * 60), now, Length::Short), "yesterday");
    assert_eq!(ago(minutes(3 * 24 * 60), now, Length::Long), "3 days ago");
    assert_eq!(ago(minutes(3 * 24 * 60), now, Length::Short), "3 d ago");
}

#[test]
fn presence_says_last_seen_offline_or_the_state() {
    let now = utc(11, 19, 0);
    let read = |availability: Availability, last: Option<i64>| CardPresence {
        availability,
        last_active_ms: last,
        out_of_office: Some(false),
    };
    let four_hours = Some(now - 4 * 3_600_000);
    let offline = read(Availability::Offline, four_hours);
    assert_eq!(
        presence(Some(&offline), &Availability::Unknown, now, Length::Long).as_deref(),
        Some("Last seen 4 hours ago")
    );
    assert_eq!(
        presence(Some(&offline), &Availability::Offline, now, Length::Short).as_deref(),
        Some("Last seen 4 h ago")
    );
    let never = read(Availability::Offline, None);
    assert_eq!(
        presence(Some(&never), &Availability::Unknown, now, Length::Long).as_deref(),
        Some("Offline")
    );
    // the live presence wins over the card's read
    assert_eq!(
        presence(Some(&offline), &Availability::Busy, now, Length::Long).as_deref(),
        Some("Busy")
    );
    assert_eq!(
        presence(None, &Availability::Available, now, Length::Long).as_deref(),
        Some("Available")
    );
    assert_eq!(presence(None, &Availability::Unknown, now, Length::Long), None);
    let unknown = read(Availability::Unknown, four_hours);
    assert_eq!(
        presence(Some(&unknown), &Availability::Unknown, now, Length::Short).as_deref(),
        Some("Last seen 4 h ago")
    );
}

#[test]
fn the_availability_line_joins_what_is_known() {
    let both = availability_line(Some("Offline".into()), Some("Free on 11/12".into()));
    assert_eq!(both.as_deref(), Some("Offline \u{2022} Free on 11/12"));
    assert_eq!(availability_line(None, Some("Free".into())).as_deref(), Some("Free"));
    assert_eq!(availability_line(Some("Away".into()), None).as_deref(), Some("Away"));
    assert_eq!(availability_line(None, None), None);
}

#[test]
fn off_work_hours_say_the_next_working_day() {
    // Wednesday 22:02 at UTC+3, as Teams showed "Free on 11/12"
    let evening = now(utc(11, 19, 2));
    assert_eq!(calendar(&schedule(), &evening).as_deref(), Some("Free on 11/12"));
    // Friday evening: Monday
    assert_eq!(
        calendar(&schedule(), &now(utc(13, 18, 0))).as_deref(),
        Some("Free on 11/16")
    );
    // Saturday at noon: Monday
    assert_eq!(
        calendar(&schedule(), &now(utc(14, 9, 0))).as_deref(),
        Some("Free on 11/16")
    );
    // Wednesday 6:00, before the day starts: today
    assert_eq!(
        calendar(&schedule(), &now(utc(11, 3, 0))).as_deref(),
        Some("Free on 11/11")
    );
    // every day a working day when none is named, the day after
    let mut daily = schedule();
    daily.work_days.clear();
    assert_eq!(calendar(&daily, &evening).as_deref(), Some("Free on 11/12"));
}

#[test]
fn work_hours_say_until_when() {
    // Wednesday 10:00 at UTC+3
    let morning = now(utc(11, 7, 0));
    assert_eq!(calendar(&schedule(), &morning).as_deref(), Some("Free until 5:00 PM"));
    let mut busy = schedule();
    busy.current = Some(FreeBusyStatus::Busy);
    busy.until_ms = Some(utc(11, 8, 30));
    assert_eq!(calendar(&busy, &morning).as_deref(), Some("Busy until 11:30 AM"));
    busy.until_ms = None;
    assert_eq!(calendar(&busy, &morning).as_deref(), Some("Busy"));
    let mut tentative = schedule();
    tentative.current = Some(FreeBusyStatus::Tentative);
    tentative.until_ms = Some(utc(12, 8, 0));
    assert_eq!(calendar(&tentative, &morning).as_deref(), Some("Tentative until 11/12"));
    let mut elsewhere = schedule();
    elsewhere.current = Some(FreeBusyStatus::WorkingElsewhere);
    assert_eq!(calendar(&elsewhere, &morning).as_deref(), Some("Working elsewhere"));
    let mut free = schedule();
    free.until_ms = Some(utc(11, 9, 0));
    assert_eq!(calendar(&free, &morning).as_deref(), Some("Free until 12:00 PM"));
    // no hours: always at work, and nothing to be free until
    let mut open = schedule();
    open.work_start = None;
    assert_eq!(calendar(&open, &now(utc(11, 19, 2))).as_deref(), Some("Free"));
}

#[test]
fn out_of_office_and_unknown_words() {
    let morning = now(utc(11, 7, 0));
    let mut away = schedule();
    away.current = Some(FreeBusyStatus::OutOfOffice);
    away.until_ms = Some(utc(16, 6, 0));
    assert_eq!(calendar(&away, &morning).as_deref(), Some("Out of office until 11/16"));
    away.until_ms = None;
    assert_eq!(calendar(&away, &morning).as_deref(), Some("Out of office"));
    let mut unknown = schedule();
    unknown.current = Some(FreeBusyStatus::Unknown);
    assert_eq!(calendar(&unknown, &morning), None);
    unknown.current = Some(FreeBusyStatus::Other("someday".into()));
    assert_eq!(calendar(&unknown, &morning), None);
    unknown.current = None;
    assert_eq!(calendar(&unknown, &morning), None);
}

#[test]
fn work_hours_line() {
    assert_eq!(
        work_hours(&schedule(), TWELVE).as_deref(),
        Some("Work hours: 8:00 AM - 5:00 PM")
    );
    assert_eq!(
        work_hours(&schedule(), "%H:%M").as_deref(),
        Some("Work hours: 08:00 - 17:00")
    );
    let mut open = schedule();
    open.work_end = None;
    assert_eq!(work_hours(&open, TWELVE), None);
    open.work_end = Some(ClockTime { hour: 25, minute: 0 });
    open.work_start = Some(ClockTime { hour: 8, minute: 0 });
    assert_eq!(work_hours(&open, TWELVE), None, "a time no clock shows");
}

#[test]
fn zones_ahead_and_behind() {
    assert_eq!(zone_difference(180, 180), "Same time zone as you");
    assert_eq!(zone_difference(180, 60), "You're 2 hours ahead");
    assert_eq!(zone_difference(120, 60), "You're 1 hour ahead");
    assert_eq!(zone_difference(60, 180), "You're 2 hours behind");
    assert_eq!(zone_difference(0, 30), "You're 30 minutes behind");
    assert_eq!(zone_difference(330, 0), "You're 5 hours 30 minutes ahead");
    assert_eq!(zone_difference(1, 0), "You're 1 minute ahead");
    assert_eq!(zone_difference(61, 0), "You're 1 hour 1 minute ahead");
}

#[test]
fn the_clock_line() {
    let evening = now(utc(11, 19, 2));
    assert_eq!(
        clock_line(Some(THREE_EAST), false, &evening).as_deref(),
        Some("10:02 PM - Same time zone as you")
    );
    assert_eq!(
        clock_line(Some(60), false, &evening).as_deref(),
        Some("8:02 PM - You're 2 hours ahead")
    );
    assert_eq!(
        clock_line(None, true, &evening).as_deref(),
        Some("10:02 PM - Your local time")
    );
    assert_eq!(clock_line(None, false, &evening), None);
}

#[test]
fn the_local_clock_reads() {
    assert!(local_offset().abs() <= 14 * 60);
    assert!(now_ms() > utc(1, 0, 0));
}
