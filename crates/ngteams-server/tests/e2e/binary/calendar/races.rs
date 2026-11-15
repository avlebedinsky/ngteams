//! Writes at once: b and c answering together, b answering differently from its two clients together, and a's change
//! of one occurrence racing its change of the whole series from another client (the known item
//! `docs/backlog/calendar-occurrence-change-races-a-series-reshape.md`: what it may leave, a later change of the
//! series repairs).

use teams_core::EngineError;
use teams_proto::{CalendarEvent, EventKind, EventResponse, RsvpAnswer, Weekday};

use super::{Trio, User, change, create, invite, respond, window};
use crate::calendar::events::weekly;
use crate::calendar::{DAY, HOUR, MONDAY, draft, instant};
use crate::support::backends;

/// a's roster of the event: each invited account's answer.
async fn answers(a: &User, id: &str) -> Vec<Option<EventResponse>> {
    let whole = a.engine().calendar_event(id.to_owned()).await.unwrap();
    whole
        .event
        .attendees
        .iter()
        .skip(1)
        .map(|attendee| attendee.response.clone())
        .collect()
}

fn own_answer(answer: RsvpAnswer) -> EventResponse {
    match answer {
        RsvpAnswer::Accept => EventResponse::Accepted,
        RsvpAnswer::Decline => EventResponse::Declined,
        RsvpAnswer::Tentative => EventResponse::Tentative,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answers_given_at_once_are_each_kept_and_one_accounts_two_answers_leave_one() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let b2 = trio.client("b@own.test", true).await;
        let mut planning = draft("Planning", MONDAY + 2 * DAY + 9 * HOUR, 30);
        planning.online = true;
        planning.attendees = vec![invite(&trio.b), invite(&trio.c)];
        let id = create(&trio.a, planning).await.id;
        let rounds = [
            (RsvpAnswer::Accept, RsvpAnswer::Decline),
            (RsvpAnswer::Decline, RsvpAnswer::Tentative),
            (RsvpAnswer::Tentative, RsvpAnswer::Accept),
        ];
        for (of_b, of_c) in rounds {
            let (b, c) = tokio::join!(respond(&trio.b, &id, of_b), respond(&trio.c, &id, of_c));
            b.unwrap();
            c.unwrap();
            let both = vec![Some(own_answer(of_b)), Some(own_answer(of_c))];
            assert_eq!(answers(&trio.a, &id).await, both);
        }
        for _ in 0..3 {
            let (here, there) = tokio::join!(
                respond(&trio.b, &id, RsvpAnswer::Accept),
                respond(&b2, &id, RsvpAnswer::Decline)
            );
            here.unwrap();
            there.unwrap();
            let kept = answers(&trio.a, &id).await.remove(0).unwrap();
            assert!(
                matches!(kept, EventResponse::Accepted | EventResponse::Declined),
                "{kept:?}"
            );
            for user in [&trio.b, &b2] {
                let own = window(user, MONDAY, 7).await;
                assert_eq!(own[0].response, kept, "b's copy and a's roster agree");
            }
        }
        drop(b2);
        trio.stop().await;
    }
}

/// Neither write failed on the server's side: a 5xx is a fault, a refusal of the race is not.
fn not_a_fault(written: &Result<(), EngineError>) {
    if let Err(error) = written {
        let text = error.to_string();
        assert!(!text.contains("answered 5"), "{text}");
        eprintln!("race: one write refused: {text}");
    }
}

fn of_series<'a>(events: &'a [CalendarEvent], master: &str) -> Vec<&'a CalendarEvent> {
    events
        .iter()
        .filter(|event| event.series_master_id.as_deref() == Some(master))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_occurrence_change_racing_a_series_change_leaves_a_series_a_later_change_repairs() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let a2 = trio.client("a@own.test", false).await;
        for round in 0..3 {
            let mut sync = draft(&format!("Sync {round}"), MONDAY + 10 * HOUR, 30);
            sync.online = true;
            sync.attendees = vec![invite(&trio.b)];
            sync.recurrence = Some(weekly(&[Weekday::Monday, Weekday::Wednesday], None));
            let master = create(&trio.a, sync.clone()).await;
            let wednesday = of_series(&window(&trio.a, MONDAY, 7).await, &master.id)[1].id.clone();
            let mut moved = draft(&format!("Sync {round}, moved"), MONDAY + 2 * DAY + 15 * HOUR, 30);
            moved.online = true;
            moved.attendees = vec![invite(&trio.b)];
            let mut tuesdays = sync.clone();
            tuesdays.start.ms += DAY;
            tuesdays.end.ms += DAY;
            tuesdays.recurrence = Some(weekly(&[Weekday::Tuesday], None));
            let (one, all) = tokio::join!(
                change(&trio.a, &wednesday, moved),
                change(&a2, &master.id, tuesdays.clone())
            );
            not_a_fault(&one);
            not_a_fault(&all);
            let own = window(&trio.a, MONDAY, 14).await;
            let theirs = window(&trio.b, MONDAY, 14).await;
            let starts = |events: &[&CalendarEvent]| events.iter().map(|e| instant(e.start)).collect::<Vec<_>>();
            eprintln!(
                "race round {round}: occurrence {}, series {}; a's starts {:?}",
                one.is_ok(),
                all.is_ok(),
                starts(&of_series(&own, &master.id))
            );
            assert_eq!(
                starts(&of_series(&own, &master.id)),
                starts(&of_series(&theirs, &master.id))
            );

            // a later change of the series: every occurrence on Thursday, nothing of the race left
            let mut thursdays = sync;
            thursdays.start.ms += 3 * DAY;
            thursdays.end.ms += 3 * DAY;
            thursdays.recurrence = Some(weekly(&[Weekday::Thursday], None));
            change(&trio.a, &master.id, thursdays).await.unwrap();
            for user in [&trio.a, &trio.b] {
                let events = window(user, MONDAY, 14).await;
                let series = of_series(&events, &master.id);
                assert_eq!(
                    starts(&series),
                    [MONDAY + 3 * DAY + 10 * HOUR, MONDAY + 10 * DAY + 10 * HOUR],
                    "{}: {series:?}",
                    user.upn
                );
                assert!(series.iter().all(|event| event.kind == EventKind::Occurrence));
            }
            super::delete(&trio.a, &master.id).await.unwrap();
        }
        drop(a2);
        trio.stop().await;
    }
}
