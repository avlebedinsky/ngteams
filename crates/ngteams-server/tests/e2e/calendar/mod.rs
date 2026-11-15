//! The calendar: the real own-cloud engine's scheduling flows against the server's middle tier.

mod channels;
pub(crate) mod events;
mod exceptions;
mod feed;
mod golden;
pub(crate) mod meetings;
mod nodes;
mod options;
mod push;
mod scheduler;
mod security;

use teams_proto::{CalendarEvent, EventDraft, EventShowAs, EventTime, Organizer, ZonedTime};

pub(crate) use crate::teams::{Seat, server};

pub(crate) const MINUTE: i64 = 60_000;
pub(crate) const HOUR: i64 = 60 * MINUTE;
pub(crate) const DAY: i64 = 24 * HOUR;

/// Monday 2027-01-04 00:00 UTC.
pub(crate) const MONDAY: i64 = 1_799_020_800_000;

/// A Monday 00:00 UTC at least a week from now: for what the server answers by the clock (it suggests no time
/// that is past).
pub(crate) fn coming_monday() -> i64 {
    const FIRST_MONDAY: i64 = 4 * DAY;
    let now = i64::try_from(std::time::SystemTime::UNIX_EPOCH.elapsed().unwrap().as_millis()).unwrap();
    let weeks = (now + 7 * DAY - FIRST_MONDAY).div_euclid(7 * DAY) + 1;
    FIRST_MONDAY + weeks * 7 * DAY
}

/// An event from `start_ms` for `minutes`, as the scheduling form drafts it before the engine completes it.
pub(crate) fn draft(subject: &str, start_ms: i64, minutes: i64) -> EventDraft {
    let at = |ms| ZonedTime { ms, offset_minutes: 0 };
    EventDraft {
        subject: subject.into(),
        start: at(start_ms),
        end: at(start_ms + minutes * MINUTE),
        time_zone: None,
        zone: Some("UTC".into()),
        all_day: false,
        organizer: Organizer {
            mri: String::new(),
            address: None,
            name: None,
        },
        attendees: Vec::new(),
        location: String::new(),
        body_html: String::new(),
        online: false,
        show_as: EventShowAs::Busy,
        reminder_minutes: Some(15),
        private: false,
        categories: Vec::new(),
        recurrence: None,
    }
}

/// The account's events of the week from [`MONDAY`], by start.
pub(crate) async fn week(seat: &Seat) -> Vec<CalendarEvent> {
    let mut events = seat.engine.calendar(MONDAY, MONDAY + 7 * DAY).await.unwrap();
    events.sort_by_key(|event| instant(event.start));
    events
}

pub(crate) fn instant(time: EventTime) -> i64 {
    match time {
        EventTime::Instant(ms) => ms,
        EventTime::Floating(_) => panic!("the server writes instants"),
    }
}
