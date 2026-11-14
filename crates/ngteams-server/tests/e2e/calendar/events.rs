use teams_core::EventWrite;
use teams_proto::{EventKind, EventResponse, EventTime, ShowAs};

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
