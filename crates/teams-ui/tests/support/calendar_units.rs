//! Offline unit scenarios for the parent module; kept outside runtime coverage sources.

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
        meeting: None,
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

#[test]
fn month_and_week_navigation_preserve_the_displayed_date_range() {
    let span = Span::of(date(2026, 12, 31), Mode::Month);
    let days: Vec<_> = span.dates().collect();
    assert_eq!(days.len(), 42);
    assert_eq!(days.first(), Some(&date(2026, 11, 30)));
    assert_eq!(days.last(), Some(&date(2027, 1, 10)));
    assert!(span.contains(date(2027, 1, 10)));
    assert!(!span.contains(date(2027, 1, 11)));
    assert!(!span.contains(date(2026, 11, 29)));
    let (start, end) = span.window_ms();
    assert_eq!(local(EventTime::Instant(start)).unwrap().date(), days[0]);
    assert_eq!(local(EventTime::Instant(end)).unwrap().date(), span.end());
    assert_eq!(step(date(2026, 12, 31), Mode::Month, true), date(2027, 1, 1));
    assert_eq!(step(date(2026, 10, 30), Mode::WorkWeek, false), date(2026, 10, 23));
    assert!(weekend(date(2026, 10, 31)));
    assert!(weekend(date(2026, 11, 1)));
    assert!(!weekend(date(2026, 10, 30)));
}

#[test]
fn unreadable_times_are_omitted_and_reversed_ends_are_clamped() {
    assert!(local(EventTime::Instant(i64::MAX)).is_none());
    for (month, day, hour) in [(2, 30, 12), (13, 1, 12), (10, 27, 24)] {
        let invalid = EventTime::Floating(FloatingTime {
            year: 2026,
            month,
            day,
            hour,
            minute: 0,
            second: 0,
        });
        assert!(local(invalid).is_none());
        let mut unreadable = (*event("bad", at(27, 9, 0), at(27, 10, 0)).event).clone();
        unreadable.end = invalid;
        assert!(Shown::of(Rc::new(unreadable)).is_none());
    }
    let reversed = event("reversed", at(27, 9, 0), at(27, 8, 0));
    assert_eq!(reversed.end, reversed.start);
    assert!(reversed.on(date(2026, 10, 27)));
    let slots = day_slots(&[reversed], date(2026, 10, 27), 20);
    assert_eq!((slots[0].top, slots[0].length), (9 * 60, 20));
}

#[test]
fn overnight_events_are_clipped_and_touching_blocks_reuse_a_column() {
    let shown = [
        event("overnight", at(26, 23, 30), at(27, 0, 30)),
        event("next", at(27, 0, 30), at(27, 1, 0)),
        event("late", at(27, 23, 55), at(28, 0, 5)),
        event("past", at(25, 12, 0), at(25, 13, 0)),
        event("future", at(28, 12, 0), at(28, 13, 0)),
    ];
    let slots = day_slots(&shown, date(2026, 10, 27), 20);
    assert_eq!(slots.len(), 3);
    assert_eq!((slots[0].top, slots[0].length), (0, 30));
    assert_eq!((slots[1].top, slots[1].length), (30, 30));
    assert_eq!((slots[2].top, slots[2].length), (1435, 5));
    assert!(slots.iter().all(|slot| slot.column == 0 && slot.columns == 1));
    assert!(day_slots(&[], date(2026, 10, 27), 20).is_empty());
}

#[test]
fn chained_overlaps_share_columns_and_reuse_one_when_it_becomes_free() {
    let shown = [
        event("long", at(27, 9, 0), at(27, 10, 0)),
        event("short", at(27, 9, 15), at(27, 9, 30)),
        event("after-short", at(27, 9, 30), at(27, 10, 15)),
    ];
    let slots = day_slots(&shown, date(2026, 10, 27), 0);
    assert_eq!(slots.iter().map(|slot| slot.column).collect::<Vec<_>>(), [0, 1, 1]);
    assert!(slots.iter().all(|slot| slot.columns == 2));
}

#[test]
fn response_and_cancellation_control_visibility_appearance_and_title() {
    let base = event("Review", at(27, 9, 0), at(27, 10, 0));
    assert_eq!(base.look(), Look::Busy);
    assert_eq!(base.title(), "Review");
    for response in [EventResponse::Tentative, EventResponse::NotResponded] {
        let mut changed = (*base.event).clone();
        changed.response = response;
        let shown = Shown::of(Rc::new(changed)).unwrap();
        assert!(shows(&shown.event));
        assert_eq!(shown.look(), Look::Tentative);
    }
    for show_as in [ShowAs::Tentative, ShowAs::Free] {
        let mut changed = (*base.event).clone();
        changed.show_as = show_as;
        assert_eq!(Shown::of(Rc::new(changed)).unwrap().look(), Look::Tentative);
    }
    let mut changed = (*base.event).clone();
    changed.response = EventResponse::Declined;
    assert!(!shows(&changed));
    changed.cancelled = true;
    changed.subject.clear();
    let shown = Shown::of(Rc::new(changed.clone())).unwrap();
    assert_eq!(shown.look(), Look::Cancelled);
    assert_eq!(shown.title(), "Canceled: (No title)");
    changed.subject = "Canceled: Review".to_owned();
    assert_eq!(Shown::of(Rc::new(changed)).unwrap().title(), "Canceled: Review");
}

#[test]
fn month_days_list_all_day_first_then_timed_events_in_start_order() {
    let shown = [
        event("late", at(27, 14, 0), at(27, 15, 0)),
        event("early", at(27, 9, 0), at(27, 10, 0)),
        event("all-day", at(27, 0, 0), at(28, 0, 0)),
        event("other-day", at(28, 9, 0), at(28, 10, 0)),
    ];
    let listed = day_list(&shown, date(2026, 10, 27));
    assert_eq!(
        listed.iter().map(|shown| shown.event.id.as_str()).collect::<Vec<_>>(),
        ["all-day", "early", "late"]
    );
    assert!(!shown[2].on(date(2026, 10, 28)), "midnight end is exclusive");
}
