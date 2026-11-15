use teams_core::EventWrite;
use teams_proto::{EventShowAs, FreeBusy, PersonSchedule, RsvpAnswer, Weekday};

use super::events::weekly;
use super::meetings::invited;
use super::{HOUR, MINUTE, MONDAY, Seat, draft, server};
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

fn at(hours: i64) -> i64 {
    MONDAY + hours * HOUR
}

/// Ada's and Bert's Monday: Ada's dentist, her meeting Bert didn't answer and one Cora declined; Bert's hour away,
/// an hour shown free, a series, and an event deleted again.
async fn a_busy_monday(ada: &Seat, bert: &Seat, cora: &Seat) {
    let mut dentist = draft("Dentist", at(9), 60);
    dentist.private = true;
    save(ada, dentist).await;
    let mut planning = draft("Planning", at(10), 30);
    planning.attendees = vec![invited("bert@own.test", "Bert", None, true)];
    save(ada, planning).await;
    let mut review = draft("Review", at(11), 30);
    review.attendees = vec![invited("cora@own.test", "Cora", None, true)];
    let review = save(ada, review).await;
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
    save(bert, away).await;
    let mut free = draft("Reading", at(15), 60);
    free.show_as = EventShowAs::Free;
    save(bert, free).await;
    let mut gym = draft("Gym", at(17), 30);
    gym.recurrence = Some(weekly(&[Weekday::Monday, Weekday::Wednesday], None));
    save(bert, gym).await;
    let dropped = save(bert, draft("Dropped", at(12), 30)).await;
    bert.engine
        .write_event(EventWrite::Delete { id: dropped })
        .await
        .unwrap();
}

/// Each person asked about, in the order asked, the account itself last. An invitation not answered yet is
/// tentative, a declined one takes no time, and what someone else's time is about isn't told.
fn busy_times_read(people: &[PersonSchedule]) {
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
    assert_eq!(
        times(&people[0]),
        [
            (FreeBusy::Tentative, 600, 630, None),
            (FreeBusy::Away, 780, 840, None),
            (FreeBusy::Busy, 1020, 1050, None),
            (FreeBusy::Busy, 2 * 1440 + 1020, 2 * 1440 + 1050, None),
        ]
    );
    assert!(people[1].busy.is_empty());
    assert!(people[2].busy.is_empty());
    assert_eq!(
        times(&people[3]),
        [
            (FreeBusy::Busy, 540, 600, Some("Dentist")),
            (FreeBusy::Busy, 600, 630, Some("Planning")),
            (FreeBusy::Busy, 660, 690, Some("Review")),
        ]
    );
}

fn minutes(starts: &[i64]) -> Vec<i64> {
    starts.iter().map(|start| (start - MONDAY) / MINUTE).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_scheduler_reads_each_persons_busy_times_and_the_starts_everyone_is_free_at() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        a_busy_monday(&ada, &bert, &cora).await;
        let asked = vec![
            invited("bert@own.test", "Bert", Some(&bert.mri), true),
            invited("cora@own.test", "Cora", None, false),
            invited("stranger@elsewhere.test", "Stranger", None, true),
        ];
        let availability = ada.engine.availability(asked, MONDAY, 30).await.unwrap();
        busy_times_read(&availability.people);

        // the half hours of two working days nobody is busy in
        let starts = minutes(&availability.suggestions);
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
        assert_eq!(minutes(&hour.suggestions)[..4], [480, 690, 720, 750]);

        // someone's calendar added to the account's: a week asked for is read as two spans of five days, a
        // series' occurrences among their busy times
        let asked = vec![invited("bert@own.test", "Bert", None, true)];
        let week = ada.engine.people_calendars(asked, MONDAY, 7).await.unwrap();
        let theirs = week.iter().find(|person| person.address == "bert@own.test").unwrap();
        let starts: Vec<i64> = theirs.busy.iter().map(|busy| busy.start_ms).collect();
        assert_eq!(
            minutes(&starts),
            [600, 780, 1020, 2 * 1440 + 1020, 7 * 1440 + 1020, 9 * 1440 + 1020]
        );
        assert_eq!(theirs.busy[3].end_ms - theirs.busy[3].start_ms, 30 * MINUTE);
        assert!(theirs.busy.iter().all(|busy| busy.subject.is_none()));
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}
