//! The calendar's read (`docs/protocol/calendar.md`): the middle tier's calendar view in the web client's form, and
//! its answer read from the synthetic fixture `tests/fixtures/mt/calendar-view.json` (every captured answer was empty).

// test code throughout
#![cfg(test)]

use teams_proto::{
    CALENDAR_VIEW_PATH, CalendarAnswerError, CalendarEvent, CalendarView, EventKind, EventResponse, EventTime,
    FloatingTime, Read, Service, ShowAs,
};

const FIXTURE: &str = include_str!("../../../tests/fixtures/mt/calendar-view.json");

#[test]
fn the_read_names_the_window_as_the_web_client_does() {
    // 2026-10-24T21:00:00Z to 2026-10-30T21:00:00Z, a captured window
    let view = CalendarView::between(1_792_875_600_000, 1_793_394_000_000).unwrap();
    assert_eq!(view.service(), Service::MiddleTier);
    let endpoint = view.endpoint();
    assert_eq!(endpoint.segments(), CALENDAR_VIEW_PATH);
    let names: Vec<&str> = endpoint.query().iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["StartDate", "EndDate", "shouldDecryptData"]);
    assert_eq!(endpoint.query()[0].1, "2026-10-24T21:00:00.000Z");
    assert_eq!(endpoint.query()[1].1, "2026-10-30T21:00:00.000Z");
    assert_eq!(endpoint.query()[2].1, "true");
}

#[test]
fn the_answer_reads_every_form_of_an_event() {
    let answer = CalendarView::read_answer(FIXTURE.as_bytes()).unwrap();
    // the master left out, the two that don't read skipped
    assert_eq!(answer.skipped, 2);
    let ids: Vec<&str> = answer.events.iter().map(|event| event.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "AAMkMadeUpEvent0001",
            "AAMkMadeUpEvent0002",
            "AAMkMadeUpEvent0003",
            "AAMkMadeUpEvent0004",
            "AAMkMadeUpEvent0005"
        ]
    );

    the_meeting_reads_whole(&answer.events[0]);
    the_occurrence_reads_its_series_and_chat(&answer.events[1]);
    assert!(answer.events[2].cancelled);
    the_all_day_event_reads_its_local_day(&answer.events[3]);
    assert_eq!(answer.events[4].response, EventResponse::Declined);
}

fn the_meeting_reads_whole(meeting: &CalendarEvent) {
    assert_eq!(meeting.subject, "Code review");
    assert_eq!(meeting.start, EventTime::Instant(1_793_196_000_000));
    assert!(meeting.online);
    assert_eq!(
        meeting.meeting_chat.as_deref(),
        Some("19:meeting_MadeUpMeeting0001@thread.v2")
    );
    assert!(
        meeting
            .join_url
            .as_deref()
            .unwrap()
            .starts_with("https://teams.microsoft.com/l/meetup-join/")
    );
    assert_eq!(meeting.response, EventResponse::Accepted);
    assert_eq!(meeting.attendees.len(), 2);
    assert_eq!(meeting.preview.as_deref(), Some("Walk through the open changes."));
}

fn the_occurrence_reads_its_series_and_chat(occurrence: &CalendarEvent) {
    assert_eq!(occurrence.kind, EventKind::Occurrence);
    assert!(occurrence.kind.recurring());
    assert_eq!(occurrence.response, EventResponse::NotResponded);
    assert_eq!(occurrence.show_as, ShowAs::Tentative);
    // its chat from the join link alone
    assert_eq!(
        occurrence.meeting_chat.as_deref(),
        Some("19:meeting_MadeUpMeeting0002@thread.v2")
    );
}

fn the_all_day_event_reads_its_local_day(all_day: &CalendarEvent) {
    assert!(all_day.all_day);
    assert!(all_day.is_organizer);
    assert_eq!(all_day.location, None);
    assert_eq!(
        all_day.start,
        EventTime::Floating(FloatingTime {
            year: 2026,
            month: 10,
            day: 29,
            hour: 0,
            minute: 0,
            second: 0
        })
    );
}

#[test]
fn each_event_is_kept_as_it_came_and_reads_back_the_same() {
    let answer = CalendarView::read_answer(FIXTURE.as_bytes()).unwrap();
    assert_eq!(answer.entries.len(), answer.events.len());
    for (entry, event) in answer.entries.iter().zip(&answer.events) {
        assert_eq!(CalendarEvent::stored(entry).as_ref(), Some(event));
    }
    assert_eq!(CalendarEvent::stored("not json"), None);
    assert_eq!(CalendarEvent::stored(r#"{"objectId": "x"}"#), None, "no times");
    let master = r#"{"objectId": "m", "startTime": "2026-10-26T09:00:00Z", "endTime": "2026-10-26T10:00:00Z",
        "eventType": "RecurringMaster"}"#;
    assert_eq!(CalendarEvent::stored(master), None, "a series' master isn't shown");
}

#[test]
fn an_answer_without_its_events_is_refused() {
    assert_eq!(
        CalendarView::read_answer(br#"{"type": "x"}"#),
        Err(CalendarAnswerError::Malformed)
    );
    assert_eq!(CalendarView::read_answer(b"[]"), Err(CalendarAnswerError::Malformed));
    assert!(
        CalendarView::read_answer(br#"{"value": []}"#)
            .unwrap()
            .events
            .is_empty()
    );
}
