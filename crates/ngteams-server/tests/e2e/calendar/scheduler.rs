use teams_core::EventWrite;
use teams_proto::{EventShowAs, FreeBusy, PersonSchedule, RsvpAnswer, Weekday};

use super::events::weekly;
use super::meetings::invited;
use super::{DAY, HOUR, MINUTE, MONDAY, Seat, draft, server};
use crate::support::backends;

async fn save(seat: &Seat, draft: teams_proto::EventDraft) -> String {
    let written = seat.engine.write_event(EventWrite::Create(draft)).await.unwrap();
    written.created.unwrap().id
}

/// A person's busy times as (status, start and end in minutes after [`MONDAY`], subject).
fn times(schedule: &PersonSchedule) -> Vec<(FreeBusy, i64, i64, Option<&str>)> {
    schedule
        .busy
        .iter()
        .map(|busy| {
            (
                busy.status.clone(),
                (busy.start_ms - MONDAY) / MINUTE,
                (busy.end_ms - MONDAY) / MINUTE,
                busy.subject.as_deref(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_scheduler_reads_each_persons_busy_times_and_the_starts_everyone_is_free_at() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let at = |hours: i64| MONDAY + hours * HOUR;

        let mut dentist = draft("Dentist", at(9), 60);
        dentist.private = true;
        save(&ada, dentist).await;
        let mut planning = draft("Planning", at(10), 30);
        planning.attendees = vec![invited("bert@own.test", "Bert", None, true)];
        save(&ada, planning).await;
        let mut review = draft("Review", at(11), 30);
        review.attendees = vec![invited("cora@own.test", "Cora", None, true)];
        let review = save(&ada, review).await;
        cora.engine
            .write_event(EventWrite::Respond {
                id: review,
                answer: RsvpAnswer::Decline,
                send: false,
                comment: None,
            })
            .await
            .unwrap();
        let mut away = draft("Away", at(13), 60);
        away.show_as = EventShowAs::Away;
        save(&bert, away).await;
        let mut free = draft("Reading", at(15), 60);
        free.show_as = EventShowAs::Free;
        save(&bert, free).await;
        let mut gym = draft("Gym", at(17), 30);
        gym.recurrence = Some(weekly(&[Weekday::Monday, Weekday::Wednesday], None));
        save(&bert, gym).await;
        let cancelled = save(&bert, draft("Dropped", at(12), 30)).await;
        bert.engine
            .write_event(EventWrite::Delete { id: cancelled })
            .await
            .unwrap();

        let asked = vec![
            invited("bert@own.test", "Bert", Some(&bert.mri), true),
            invited("cora@own.test", "Cora", None, false),
            invited("stranger@elsewhere.test", "Stranger", None, true),
        ];
        let availability = ada.engine.availability(asked, MONDAY, 30).await.unwrap();
        let people = &availability.people;
        let addresses: Vec<&str> = people.iter().map(|person| person.address.as_str()).collect();
        assert_eq!(
            addresses,
            [
                "bert@own.test",
                "cora@own.test",
                "stranger@elsewhere.test",
                "ada@own.test"
            ]
        );
        // an invitation not answered yet is tentative; what someone else's time is about isn't told
        assert_eq!(
            times(&people[0]),
            [
                (FreeBusy::Tentative, 600, 630, None),
                (FreeBusy::Away, 780, 840, None),
                (FreeBusy::Busy, 1020, 1050, None),
                (FreeBusy::Busy, 2 * 1440 + 1020, 2 * 1440 + 1050, None),
            ]
        );
        assert!(people[1].busy.is_empty(), "a declined meeting takes no time");
        assert!(people[2].busy.is_empty());
        assert_eq!(
            times(&people[3]),
            [
                (FreeBusy::Busy, 540, 600, Some("Dentist")),
                (FreeBusy::Busy, 600, 630, Some("Planning")),
                (FreeBusy::Busy, 660, 690, Some("Review")),
            ]
        );

        // the half hours of two working days nobody is busy in
        let starts: Vec<i64> = availability
            .suggestions
            .iter()
            .map(|start| (start - MONDAY) / MINUTE)
            .collect();
        assert_eq!(starts[..8], [480, 510, 630, 690, 720, 750, 840, 870], "{starts:?}");
        assert_eq!(starts.len(), 13 + 20, "{starts:?}");
        assert!(
            starts
                .iter()
                .all(|start| !(1020..1050).contains(start) && start % 30 == 0)
        );
        assert_eq!(starts.last(), Some(&(24 * 60 + 17 * 60 + 30)));
        // an hour's meeting needs the whole hour free
        let hour = ada.engine.availability(Vec::new(), MONDAY, 60).await.unwrap();
        let starts: Vec<i64> = hour.suggestions.iter().map(|start| (start - MONDAY) / MINUTE).collect();
        assert_eq!(starts[..4], [480, 690, 720, 750], "{starts:?}");

        // someone's calendar added to the account's: a week of their busy times, a series' occurrences among them
        let week = ada
            .engine
            .people_calendars(vec![invited("bert@own.test", "Bert", None, true)], MONDAY, 7)
            .await
            .unwrap();
        let bert_week = week.iter().find(|person| person.address == "bert@own.test").unwrap();
        let starts: Vec<i64> = bert_week
            .busy
            .iter()
            .map(|busy| (busy.start_ms - MONDAY) / MINUTE)
            .collect();
        assert_eq!(starts, [600, 780, 1020, 2 * 24 * 60 + 1020]);
        assert_eq!(bert_week.busy[3].end_ms - bert_week.busy[3].start_ms, 30 * MINUTE);
        assert!(bert_week.busy.iter().all(|busy| busy.subject.is_none()));
        assert_eq!(MONDAY % DAY, 0);
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}
