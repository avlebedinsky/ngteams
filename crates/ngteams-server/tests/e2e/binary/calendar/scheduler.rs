//! The scheduling form's Scheduler: b's and c's busy times and the times all three are free; and b's and c's
//! calendars added to a's, which show their busy times only, never what an event is called or says.

use serde_json::Value;
use teams_proto::{EventShowAs, FreeBusy, FreeBusyAsk, Organizer, PersonSchedule, Schedules, Weekday};

use super::{Trio, User, create, invite};
use crate::calendar::events::weekly_from;
use crate::calendar::{HOUR, MINUTE, coming_monday, draft};
use crate::support::backends;

/// b's Monday: an hour away, an interview of c's own (private, with a body), and a daily-ish gym series.
async fn busy_monday(monday: i64, b: &User, c: &User) {
    let at = |hours: i64| monday + hours * HOUR;
    let mut away = draft("Doctor", at(13), 60);
    away.show_as = EventShowAs::Away;
    away.body_html = "<p>Blood test results</p>".into();
    create(b, away).await;
    let mut gym = draft("Gym", at(17), 30);
    gym.recurrence = Some(weekly_from(monday, &[Weekday::Monday, Weekday::Tuesday], None));
    create(b, gym).await;
    let mut interview = draft("Secret interview", at(10), 60);
    interview.private = true;
    interview.location = "Hidden office".into();
    interview.body_html = "<p>Do not tell anyone</p>".into();
    create(c, interview).await;
}

/// (status, start and end in minutes after `monday`, subject).
fn times(person: &PersonSchedule, monday: i64) -> Vec<(FreeBusy, i64, i64, Option<String>)> {
    person
        .busy
        .iter()
        .map(|busy| {
            (
                busy.status.clone(),
                (busy.start_ms - monday) / MINUTE,
                (busy.end_ms - monday) / MINUTE,
                busy.subject.clone(),
            )
        })
        .collect()
}

/// Every string in the answer, at any depth.
fn strings(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::String(text) => into.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| strings(item, into)),
        Value::Object(map) => map.iter().for_each(|(key, item)| {
            into.push(key.clone());
            strings(item, into);
        }),
        _ => {}
    }
}

/// The schedules answer a's client gets, read as it came: nothing of what b's or c's events are about.
async fn nothing_told_of_others(trio: &Trio, monday: i64) {
    let ask = FreeBusyAsk {
        attendees: vec![invite(&trio.b), invite(&trio.c)],
        organizer: Organizer {
            mri: trio.a.mri.as_str().to_owned(),
            address: Some("a@own.test".into()),
            name: Some("QA Operator".into()),
        },
        start_ms: monday,
        meeting_minutes: 30,
    };
    let answer = trio.a.client().write(Schedules(ask)).await.unwrap();
    assert_eq!(answer.status(), 200);
    let body: Value = serde_json::from_slice(answer.body()).unwrap();
    let mut said = Vec::new();
    strings(&body, &mut said);
    for secret in [
        "Secret interview",
        "Hidden office",
        "Do not tell anyone",
        "Doctor",
        "Blood test",
        "Gym",
    ] {
        assert!(
            said.iter().all(|text| !text.contains(secret)),
            "the answer tells {secret:?}: {body}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_scheduler_shows_b_and_c_busy_and_suggests_times_all_are_free_and_added_calendars_show_no_titles() {
    let monday = coming_monday();
    for backend in backends() {
        let trio = Trio::start(backend).await;
        busy_monday(monday, &trio.b, &trio.c).await;
        create(&trio.a, draft("Focus", monday + 8 * HOUR, 60)).await;

        let asked = vec![invite(&trio.b), invite(&trio.c)];
        let availability = trio.a.engine().availability(asked, monday, 30).await.unwrap();
        let people = &availability.people;
        let addresses: Vec<&str> = people.iter().map(|person| person.address.as_str()).collect();
        assert_eq!(addresses, ["b@own.test", "c@own.test", "a@own.test"]);
        assert_eq!(
            times(&people[0], monday),
            [
                (FreeBusy::Away, 780, 840, None),
                (FreeBusy::Busy, 1020, 1050, None),
                (FreeBusy::Busy, 1440 + 1020, 1440 + 1050, None),
            ]
        );
        assert_eq!(times(&people[1], monday), [(FreeBusy::Busy, 600, 660, None)]);
        assert_eq!(
            times(&people[2], monday),
            [(FreeBusy::Busy, 480, 540, Some("Focus".into()))]
        );

        // the half hours of Monday's working day nobody is busy in
        let starts: Vec<i64> = availability
            .suggestions
            .iter()
            .map(|start| (start - monday) / MINUTE)
            .collect();
        let monday_starts: Vec<i64> = starts.iter().copied().filter(|start| *start < 1440).collect();
        assert_eq!(
            monday_starts,
            [540, 570, 660, 690, 720, 750, 840, 870, 900, 930, 960, 990, 1050],
            "{starts:?}"
        );
        assert!(
            !starts.iter().any(|start| (1440 + 1020..1440 + 1050).contains(start)),
            "{starts:?}"
        );

        // b's and c's calendars added to a's: their busy times over a week, no titles
        let people = vec![invite(&trio.b), invite(&trio.c)];
        let week = trio.a.engine().people_calendars(people, monday, 7).await.unwrap();
        let of = |address: &str| week.iter().find(|person| person.address == address).unwrap();
        let gym: Vec<i64> = of("b@own.test")
            .busy
            .iter()
            .map(|busy| (busy.start_ms - monday) / MINUTE)
            .collect();
        assert_eq!(gym, [780, 1020, 1440 + 1020, 7 * 1440 + 1020, 8 * 1440 + 1020]);
        assert_eq!(times(of("c@own.test"), monday), [(FreeBusy::Busy, 600, 660, None)]);
        for address in ["b@own.test", "c@own.test"] {
            assert!(of(address).busy.iter().all(|busy| busy.subject.is_none()), "{address}");
        }
        nothing_told_of_others(&trio, monday).await;
        trio.stop().await;
    }
}
