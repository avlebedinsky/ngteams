use teams_core::EventWrite;
use teams_proto::{
    CalendarEvent, EventKind, EventResponse, EventTime, Recurrence, RecurrencePattern, ShowAs, Weekday, ZonedTime,
};

use super::{DAY, HOUR, MINUTE, MONDAY, Seat, draft, server, week};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_saves_an_appointment_and_its_calendar_shows_it() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        assert!(week(&ada).await.is_empty());

        let start = MONDAY + DAY + 9 * HOUR;
        let mut dentist = draft("Dentist", start, 45);
        dentist.location = "Main street 5".into();
        dentist.body_html = "<p>Bring the <b>x-rays</b></p>".into();
        dentist.private = true;
        let written = ada.engine.write_event(EventWrite::Create(dentist)).await.unwrap();
        let created = written.created.unwrap();
        assert_eq!(created.subject, "Dentist");
        assert_eq!(created.start, EventTime::Instant(start));
        assert_eq!(created.end, EventTime::Instant(start + 45 * MINUTE));
        assert_eq!(created.kind, EventKind::Single);
        assert!(created.is_organizer && created.private && !created.online && !created.cancelled);

        let events = week(&ada).await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0], created);
        assert_eq!(events[0].response, EventResponse::Organizer);
        assert_eq!(events[0].show_as, ShowAs::Busy);
        assert_eq!(events[0].location.as_deref(), Some("Main street 5"));
        assert_eq!(events[0].preview.as_deref(), Some("Bring the x-rays"));
        assert_eq!(events[0].organizer_name.as_deref(), Some("Ada"));
        assert_eq!(events[0].organizer_address.as_deref(), Some("ada@own.test"));
        assert!(events[0].attendees.is_empty());
        // the store keeps what the read gave
        assert_eq!(
            ada.engine.stored_calendar(MONDAY, MONDAY + 7 * DAY).await.unwrap(),
            events
        );
        // a window that ends where the event starts, or starts where it ends, doesn't hold it
        assert!(ada.engine.calendar(MONDAY, start).await.unwrap().is_empty());
        let after = ada.engine.calendar(start + 45 * MINUTE, start + DAY).await.unwrap();
        assert!(after.is_empty(), "{after:?}");

        let full = ada.engine.calendar_event(created.id.clone()).await.unwrap();
        assert_eq!(full.event, created);
        assert_eq!(full.body_html.as_deref(), Some("<p>Bring the <b>x-rays</b></p>"));
        assert_eq!(full.reminder_minutes, Some(15));
        assert_eq!(full.raw["eventTimeZone"], "Utc");
        assert!(full.recurrence.is_none());
        assert!(!full.raw.contains_key("attendees"), "an appointment names no attendees");

        assert!(week(&bert).await.is_empty(), "an appointment is its owner's alone");
        assert!(bert.engine.calendar_event(created.id).await.is_err());
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_changes_and_deletes_its_event_and_nobody_else_can() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let start = MONDAY + 9 * HOUR;
        let mut first = draft("Dentist", start, 45);
        first.body_html = "<p>Bring the x-rays</p>".into();
        first.categories = vec!["Health".into()];
        let created = ada
            .engine
            .write_event(EventWrite::Create(first))
            .await
            .unwrap()
            .created
            .unwrap();

        let full = ada.engine.calendar_event(created.id.clone()).await.unwrap();
        assert_eq!(full.categories, ["Health"]);
        let mut moved = draft("Dentist, moved", start + DAY, 30);
        moved.show_as = teams_proto::EventShowAs::Away;
        moved.reminder_minutes = None;
        let change = EventWrite::Update {
            id: created.id.clone(),
            event: full.raw.clone(),
            draft: moved.clone(),
        };
        assert!(ada.engine.write_event(change).await.unwrap().created.is_none());
        let events = week(&ada).await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].id, created.id);
        assert_eq!(events[0].subject, "Dentist, moved");
        assert_eq!(events[0].start, EventTime::Instant(start + DAY));
        assert_eq!(events[0].end, EventTime::Instant(start + DAY + 30 * MINUTE));
        assert_eq!(events[0].show_as, ShowAs::Away);
        assert_eq!(events[0].preview, None);
        let changed = ada.engine.calendar_event(created.id.clone()).await.unwrap();
        assert_eq!(changed.body_html.as_deref(), Some(""));
        assert_eq!(changed.reminder_minutes, None);
        assert!(changed.categories.is_empty());

        // someone else's change, answer and delete find no such event
        for write in [
            EventWrite::Update {
                id: created.id.clone(),
                event: full.raw.clone(),
                draft: moved.clone(),
            },
            EventWrite::Delete { id: created.id.clone() },
        ] {
            let refused = bert.engine.write_event(write).await.unwrap_err();
            assert!(refused.to_string().contains("404"), "{refused}");
        }
        assert_eq!(week(&ada).await.len(), 1);

        ada.engine
            .write_event(EventWrite::Delete { id: created.id.clone() })
            .await
            .unwrap();
        assert!(week(&ada).await.is_empty());
        assert!(ada.engine.calendar_event(created.id.clone()).await.is_err());
        let again = ada.engine.write_event(EventWrite::Delete { id: created.id }).await;
        assert!(again.unwrap_err().to_string().contains("404"));
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

pub(crate) fn weekly(days: &[Weekday], last_day_ms: Option<i64>) -> Recurrence {
    weekly_from(MONDAY, days, last_day_ms)
}

/// A weekly series whose first week starts at `first_ms`.
pub(crate) fn weekly_from(first_ms: i64, days: &[Weekday], last_day_ms: Option<i64>) -> Recurrence {
    let at = |ms| ZonedTime { ms, offset_minutes: 0 };
    Recurrence {
        pattern: RecurrencePattern::Weekly {
            interval: 1,
            days: days.to_vec(),
        },
        start: at(first_ms),
        end: last_day_ms.map(at),
    }
}

const STANDUP: i64 = MONDAY + 10 * HOUR;

fn standup() -> teams_proto::EventDraft {
    let mut standup = draft("Standup", STANDUP, 15);
    standup.recurrence = Some(weekly(&[Weekday::Monday, Weekday::Wednesday], Some(MONDAY + 16 * DAY)));
    standup
}

pub(crate) async fn change(seat: &Seat, id: &str, draft: teams_proto::EventDraft) {
    let whole = seat.engine.calendar_event(id.to_owned()).await.unwrap();
    seat.engine
        .write_event(EventWrite::Update {
            id: id.to_owned(),
            event: whole.raw,
            draft,
        })
        .await
        .unwrap();
}

/// The series' week as its occurrences, each read whole as the calendar lists it.
async fn occurrences_of(ada: &Seat, master: &CalendarEvent) -> Vec<CalendarEvent> {
    let events = week(ada).await;
    assert_eq!(events.len(), 2, "{events:?}");
    for (event, day) in events.iter().zip([0, 2]) {
        assert_eq!(event.kind, EventKind::Occurrence);
        assert_eq!(event.series_master_id.as_deref(), Some(master.id.as_str()));
        assert_eq!(event.start, EventTime::Instant(STANDUP + day * DAY));
        assert_eq!(event.subject, "Standup");
    }
    assert_ne!(events[0].id, events[1].id);
    read_whole(ada, master, &events).await;
    events
}

async fn read_whole(ada: &Seat, master: &CalendarEvent, events: &[CalendarEvent]) {
    // it ends on its last day: three weeks of two
    let all = ada.engine.calendar(MONDAY - 7 * DAY, MONDAY + 35 * DAY).await.unwrap();
    assert_eq!(all.len(), 6, "{all:?}");
    let whole = ada.engine.calendar_event(master.id.clone()).await.unwrap();
    assert_eq!(whole.event.kind, EventKind::RecurringMaster);
    assert_eq!(whole.recurrence, standup().recurrence);
    let wednesday = ada.engine.calendar_event(events[1].id.clone()).await.unwrap();
    assert_eq!(wednesday.event, events[1]);
}

/// One occurrence moved and renamed is an exception of its series, under the same id; the series renamed leaves
/// it what it was given; one deleted leaves the others.
async fn one_at_a_time(ada: &Seat, master: &CalendarEvent, events: &[CalendarEvent]) {
    let late = draft("Standup, late", STANDUP + 2 * DAY + HOUR, 15);
    change(ada, &events[1].id, late).await;
    let after = week(ada).await;
    assert_eq!(after.len(), 2, "{after:?}");
    assert_eq!(after[0], events[0]);
    assert_eq!(after[1].id, events[1].id);
    assert_eq!(after[1].kind, EventKind::Exception);
    assert_eq!(after[1].subject, "Standup, late");
    assert_eq!(after[1].start, EventTime::Instant(STANDUP + 2 * DAY + HOUR));
    assert_eq!(after[1].series_master_id.as_deref(), Some(master.id.as_str()));

    renamed_and_one_deleted(ada, master, events).await;
}

async fn renamed_and_one_deleted(ada: &Seat, master: &CalendarEvent, events: &[CalendarEvent]) {
    let mut renamed = standup();
    renamed.subject = "Daily".into();
    change(ada, &master.id, renamed).await;
    let after = week(ada).await;
    let subjects: Vec<&str> = after.iter().map(|event| event.subject.as_str()).collect();
    assert_eq!(subjects, ["Daily", "Standup, late"]);

    ada.engine
        .write_event(EventWrite::Delete {
            id: after[0].id.clone(),
        })
        .await
        .unwrap();
    let after = week(ada).await;
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(after[0].subject, "Standup, late");
    assert!(ada.engine.calendar_event(events[0].id.clone()).await.is_err());
    // the id of a day the series has no occurrence on names nothing
    let tuesday = format!("{}.{}", master.id, STANDUP + DAY);
    assert!(ada.engine.calendar_event(tuesday).await.is_err());
}

/// The series moved to Tuesdays, never ending: its occurrences are new ones, what the old ones were given is gone.
async fn reshaped(ada: &Seat, master: &CalendarEvent) {
    let mut tuesdays = standup();
    tuesdays.start.ms += DAY;
    tuesdays.end.ms += DAY;
    tuesdays.recurrence = Some(weekly(&[Weekday::Tuesday], None));
    change(ada, &master.id, tuesdays).await;
    let after = ada.engine.calendar(MONDAY, MONDAY + 14 * DAY).await.unwrap();
    let starts: Vec<EventTime> = after.iter().map(|event| event.start).collect();
    assert_eq!(
        starts,
        [EventTime::Instant(STANDUP + DAY), EventTime::Instant(STANDUP + 8 * DAY)]
    );
    let later = ada
        .engine
        .calendar(MONDAY + 700 * DAY, MONDAY + 707 * DAY)
        .await
        .unwrap();
    assert_eq!(later.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_series_is_read_by_its_occurrences_and_changed_whole_or_one_at_a_time() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let written = ada.engine.write_event(EventWrite::Create(standup())).await.unwrap();
        let master = written.created.unwrap();
        assert_eq!(master.kind, EventKind::RecurringMaster);
        let events = occurrences_of(&ada, &master).await;
        one_at_a_time(&ada, &master, &events).await;
        reshaped(&ada, &master).await;
        ada.engine
            .write_event(EventWrite::Delete { id: master.id.clone() })
            .await
            .unwrap();
        assert!(week(&ada).await.is_empty());
        ada.stop();
        server.stop().await;
    }
}
