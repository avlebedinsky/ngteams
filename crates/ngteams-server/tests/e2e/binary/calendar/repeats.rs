//! Repeating events: daily, weekly, monthly and yearly series with an end date; one occurrence changed, cancelled
//! and deleted, then the whole series; and series in a time zone whose clocks change inside them.

use teams_proto::{CalendarEvent, EventDraft, EventKind, Recurrence, RecurrencePattern, Weekday, ZonedTime};

use super::{Trio, User, cancel, change, create, delete, invite, shown, window};
use crate::calendar::{DAY, HOUR, MINUTE, MONDAY, draft, instant};
use crate::support::backends;

fn at(ms: i64) -> ZonedTime {
    ZonedTime { ms, offset_minutes: 0 }
}

fn repeating(pattern: RecurrencePattern, first_day: i64, last_day: Option<i64>) -> Recurrence {
    Recurrence {
        pattern,
        start: at(first_day),
        end: last_day.map(at),
    }
}

/// The starts of the account's occurrences of `subject` in `days` days from `from`.
async fn starts(user: &User, subject: &str, from: i64, days: i64) -> Vec<i64> {
    window(user, from, days)
        .await
        .iter()
        .filter(|event| event.subject == subject)
        .map(|event| instant(event.start))
        .collect()
}

fn days_after(starts: &[i64], base: i64) -> Vec<i64> {
    starts.iter().map(|start| (start - base) / DAY).collect()
}

/// A series of each kind, every one with its last day.
async fn every_kind(a: &User, b: &User) -> CalendarEvent {
    let mut stretch = draft("Stretch", MONDAY + 7 * HOUR, 15);
    stretch.recurrence = Some(repeating(
        RecurrencePattern::Daily { interval: 1 },
        MONDAY,
        Some(MONDAY + 4 * DAY),
    ));
    let daily = create(a, stretch).await;
    assert_eq!(daily.kind, EventKind::RecurringMaster);
    let mut sync = draft("Sync", MONDAY + 10 * HOUR, 30);
    sync.online = true;
    sync.attendees = vec![invite(b)];
    let days = vec![Weekday::Monday, Weekday::Thursday];
    let weekly = RecurrencePattern::Weekly { interval: 1, days };
    sync.recurrence = Some(repeating(weekly, MONDAY, Some(MONDAY + 24 * DAY)));
    create(a, sync).await;
    // the 31st: the month's last day where the month is shorter
    let mut rent = draft("Rent", MONDAY + 27 * DAY + 8 * HOUR, 15);
    let monthly = RecurrencePattern::Monthly { interval: 1, day: 31 };
    rent.recurrence = Some(repeating(monthly, MONDAY + 27 * DAY, Some(MONDAY + 177 * DAY)));
    create(a, rent).await;
    let mut birthday = draft("Birthday", MONDAY + 70 * DAY + 12 * HOUR, 60);
    let yearly = RecurrencePattern::Yearly { month: 3, day: 15 };
    birthday.recurrence = Some(repeating(yearly, MONDAY + 70 * DAY, Some(MONDAY + 1457 * DAY)));
    create(a, birthday).await;
    daily
}

async fn each_kind_is_read(a: &User, b: &User) {
    let stretch = starts(a, "Stretch", MONDAY - DAY, 14).await;
    assert_eq!(days_after(&stretch, MONDAY), [0, 1, 2, 3, 4]);
    assert!(stretch.iter().all(|start| (start - MONDAY) % DAY == 7 * HOUR));
    let sync = starts(b, "Sync", MONDAY, 35).await;
    assert_eq!(days_after(&sync, MONDAY), [0, 3, 7, 10, 14, 17, 21, 24]);
    // the client reads a window of 62 days at most
    let mut rent = Vec::new();
    for from in [0, 60, 120] {
        rent.extend(starts(a, "Rent", MONDAY + from * DAY, 60).await);
    }
    // 2027: Jan 31, Feb 28, Mar 31, Apr 30, May 31, Jun 30
    assert_eq!(days_after(&rent, MONDAY), [27, 55, 86, 116, 147, 177]);
    assert!(starts(a, "Rent", MONDAY + 180 * DAY, 60).await.is_empty());
    let mut birthdays = Vec::new();
    for year in [60, 426, 791, 1156, 1521] {
        birthdays.extend(starts(a, "Birthday", MONDAY + year * DAY, 20).await);
    }
    // March 15th of 2027 to 2030, none in 2031
    assert_eq!(days_after(&birthdays, MONDAY), [70, 436, 801, 1166]);
}

/// One occurrence of the meeting series moved, one cancelled, one of the daily series deleted.
async fn one_occurrence(a: &User, b: &User, daily: &CalendarEvent) {
    let occurrences = sync(window(a, MONDAY, 14).await);
    assert_eq!(occurrences.len(), 4, "{occurrences:?}");
    let thursday = &occurrences[1];
    assert_eq!(thursday.kind, EventKind::Occurrence);
    let mut moved = draft("Sync, moved", MONDAY + 3 * DAY + 11 * HOUR, 30);
    moved.online = true;
    moved.attendees = vec![invite(b)];
    change(a, &thursday.id, moved).await.unwrap();
    cancel(a, &occurrences[2], Some("Holiday")).await.unwrap();

    moved_and_cancelled_are_read(a, b).await;
    one_deleted(a, daily).await;
}

fn sync(events: Vec<CalendarEvent>) -> Vec<CalendarEvent> {
    events
        .into_iter()
        .filter(|event| event.subject.contains("Sync"))
        .collect()
}

async fn moved_and_cancelled_are_read(a: &User, b: &User) {
    let own = sync(window(a, MONDAY, 14).await);
    assert_eq!(
        shown(&own),
        [
            ("Sync".to_owned(), MONDAY + 10 * HOUR),
            ("Sync, moved".to_owned(), MONDAY + 3 * DAY + 11 * HOUR),
            ("Sync".to_owned(), MONDAY + 10 * DAY + 10 * HOUR)
        ]
    );
    assert_eq!(own[1].kind, EventKind::Exception);
    let theirs = sync(window(b, MONDAY, 14).await);
    assert_eq!(theirs.len(), 4, "{theirs:?}");
    assert_eq!(theirs[1].subject, "Sync, moved");
    assert!(theirs[2].cancelled && !theirs[3].cancelled);
}

async fn one_deleted(a: &User, daily: &CalendarEvent) {
    let stretch = window(a, MONDAY, 7).await;
    let wednesday = stretch
        .iter()
        .find(|event| event.subject == "Stretch" && instant(event.start) == MONDAY + 2 * DAY + 7 * HOUR)
        .unwrap();
    assert_eq!(wednesday.series_master_id.as_deref(), Some(daily.id.as_str()));
    delete(a, &wednesday.id).await.unwrap();
    let left = starts(a, "Stretch", MONDAY, 7).await;
    assert_eq!(days_after(&left, MONDAY), [0, 1, 3, 4]);
}

/// The whole meeting series renamed (what was given one occurrence stays), then cancelled; the daily series
/// deleted.
async fn whole_series(a: &User, b: &User, daily: &CalendarEvent) {
    let master_id = window(a, MONDAY, 7).await[1].series_master_id.clone().unwrap();
    let mut renamed = draft("Team sync", MONDAY + 10 * HOUR, 30);
    renamed.online = true;
    renamed.attendees = vec![invite(b)];
    let days = vec![Weekday::Monday, Weekday::Thursday];
    let weekly = RecurrencePattern::Weekly { interval: 1, days };
    renamed.recurrence = Some(repeating(weekly, MONDAY, Some(MONDAY + 24 * DAY)));
    change(a, &master_id, renamed).await.unwrap();
    let theirs = window(b, MONDAY, 14).await;
    let subjects: Vec<&str> = theirs
        .iter()
        .filter(|event| event.subject.contains("ync"))
        .map(|event| event.subject.as_str())
        .collect();
    assert_eq!(subjects.len(), 4, "{subjects:?}");
    assert_eq!(
        (subjects[0], subjects[1], subjects[3]),
        ("Team sync", "Sync, moved", "Team sync")
    );
    assert!(subjects[2].starts_with("Canceled: "), "{subjects:?}");
    assert_eq!(starts(a, "Team sync", MONDAY, 35).await.len(), 6);
    series_cancelled(a, b, &master_id).await;
    delete(a, &daily.id).await.unwrap();
    assert!(starts(a, "Stretch", MONDAY - DAY, 14).await.is_empty());
    assert_eq!(starts(a, "Rent", MONDAY, 60).await.len(), 2);
}

/// The whole meeting series cancelled: gone from a's calendar, cancelled on b's until b takes it off.
async fn series_cancelled(a: &User, b: &User, master_id: &str) {
    let master = a.engine().calendar_event(master_id.to_owned()).await.unwrap().event;
    cancel(a, &master, Some("Done")).await.unwrap();
    assert!(
        window(a, MONDAY, 35)
            .await
            .iter()
            .all(|event| !event.subject.contains("ync"))
    );
    let theirs = window(b, MONDAY, 35).await;
    assert_eq!(theirs.len(), 8, "{theirs:?}");
    assert!(theirs.iter().all(|event| event.cancelled));
    delete(b, master_id).await.unwrap();
    assert!(window(b, MONDAY, 35).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daily_weekly_monthly_and_yearly_series_end_on_their_last_day_and_change_one_occurrence_or_all() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let daily = every_kind(&trio.a, &trio.b).await;
        each_kind_is_read(&trio.a, &trio.b).await;
        one_occurrence(&trio.a, &trio.b, &daily).await;
        whole_series(&trio.a, &trio.b, &daily).await;
        trio.stop().await;
    }
}

/// A weekly Monday 09:00 in Berlin from 2027-03-15 to 2027-04-12: the clocks go forward on 2027-03-28.
fn berlin(b: &User) -> EventDraft {
    let first = MONDAY + 70 * DAY;
    let local = |ms, offset_minutes| ZonedTime { ms, offset_minutes };
    let mut review = draft("Review", first + 8 * HOUR, 30);
    review.start = local(first + 8 * HOUR, 60);
    review.end = local(first + 8 * HOUR + 30 * MINUTE, 60);
    review.zone = Some("Europe/Berlin".into());
    review.online = true;
    review.attendees = vec![invite(b)];
    review.recurrence = Some(Recurrence {
        pattern: RecurrencePattern::Weekly {
            interval: 1,
            days: vec![Weekday::Monday],
        },
        start: local(first - HOUR, 60),
        end: Some(local(first + 28 * DAY - 2 * HOUR, 120)),
    });
    review
}

/// Daily 10:00 in New York from Friday 2027-03-12 to Tuesday 2027-03-16: the clocks go forward on 2027-03-14.
fn new_york() -> EventDraft {
    let first = MONDAY + 67 * DAY;
    let local = |ms, offset_minutes| ZonedTime { ms, offset_minutes };
    let mut call = draft("Call", first + 15 * HOUR, 30);
    call.start = local(first + 15 * HOUR, -300);
    call.end = local(first + 15 * HOUR + 30 * MINUTE, -300);
    call.zone = Some("America/New_York".into());
    call.recurrence = Some(Recurrence {
        pattern: RecurrencePattern::Daily { interval: 1 },
        start: local(first + 5 * HOUR, -300),
        end: Some(local(first + 4 * DAY + 4 * HOUR, -240)),
    });
    call
}

/// Each start as (days after 2027-03-15, its UTC hour).
fn utc_hours(starts: &[i64]) -> Vec<(i64, i64)> {
    let base = MONDAY + 70 * DAY;
    starts
        .iter()
        .map(|start| ((start - base).div_euclid(DAY), (start - base).rem_euclid(DAY) / HOUR))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_series_keeps_its_local_time_across_a_daylight_saving_change_of_its_own_time_zone() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let (a, b) = (&trio.a, &trio.b);
        let from = MONDAY + 63 * DAY;
        let review = create(a, berlin(b)).await;
        let whole = a.engine().calendar_event(review.id.clone()).await.unwrap();
        assert_ne!(whole.raw["eventTimeZone"], "Utc", "{:?}", whole.raw["eventTimeZone"]);
        // 09:00 in Berlin: 08:00 UTC in winter time, 07:00 UTC in summer time; b reads the same instants
        let berlin_hours = [(0, 8), (7, 8), (14, 7), (21, 7), (28, 7)];
        for user in [a, b] {
            assert_eq!(
                utc_hours(&starts(user, "Review", from, 42).await),
                berlin_hours,
                "{}",
                user.upn
            );
        }
        create(a, new_york()).await;
        // 10:00 in New York: 15:00 UTC, 14:00 UTC from the Sunday on
        let calls = utc_hours(&starts(a, "Call", from, 42).await);
        assert_eq!(calls, [(-3, 15), (-2, 15), (-1, 14), (0, 14), (1, 14)]);

        // one occurrence after the change moved to 10:00 in Berlin
        let occurrences: Vec<CalendarEvent> = window(a, from, 42)
            .await
            .into_iter()
            .filter(|event| event.subject == "Review")
            .collect();
        let mut later = berlin(b);
        later.recurrence = None;
        later.start = ZonedTime {
            ms: MONDAY + 91 * DAY + 8 * HOUR,
            offset_minutes: 120,
        };
        later.end = ZonedTime {
            ms: MONDAY + 91 * DAY + 8 * HOUR + 30 * MINUTE,
            offset_minutes: 120,
        };
        change(a, &occurrences[3].id, later).await.unwrap();
        let moved = [(0, 8), (7, 8), (14, 7), (21, 8), (28, 7)];
        assert_eq!(utc_hours(&starts(b, "Review", from, 42).await), moved);

        // the series moved to 10:00 in Berlin: every occurrence follows the clock, the moved one is forgotten
        let mut series = berlin(b);
        series.start.ms += HOUR;
        series.end.ms += HOUR;
        change(a, &review.id, series).await.unwrap();
        let hours = [(0, 9), (7, 9), (14, 8), (21, 8), (28, 8)];
        assert_eq!(utc_hours(&starts(b, "Review", from, 42).await), hours);
        trio.stop().await;
    }
}
