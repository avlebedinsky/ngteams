//! One occurrence of a series of meetings given a time or people of its own: who is asked again, who is in the
//! series' chat, and how a channel's moved occurrence is found.

use teams_core::EventWrite;
use teams_proto::{EventDraft, EventKind, EventResponse, EventTime, RsvpAnswer, ScheduledMeeting, Weekday};

use super::channels::{place, schedule};
use super::events::{change, weekly};
use super::meetings::{answers, create, invited, respond};
use super::{DAY, HOUR, MONDAY, Seat, draft, server, week};
use crate::support::backends;

const SYNC: i64 = MONDAY + 10 * HOUR;
const WEDNESDAY: i64 = SYNC + 2 * DAY;

/// A Teams meeting at `start_ms` with these people, each by first name.
fn meeting(subject: &str, start_ms: i64, people: &[(&str, &Seat)]) -> EventDraft {
    let mut meeting = draft(subject, start_ms, 30);
    meeting.online = true;
    meeting.attendees = people
        .iter()
        .map(|(name, seat)| {
            let address = format!("{}@own.test", name.to_lowercase());
            invited(&address, name, Some(&seat.mri), true)
        })
        .collect();
    meeting
}

/// Ada's series with Bert on Mondays, Wednesdays and Fridays.
fn sync(bert: &Seat) -> EventDraft {
    let mut sync = meeting("Sync", SYNC, &[("Bert", bert)]);
    sync.recurrence = Some(weekly(&[Weekday::Monday, Weekday::Wednesday, Weekday::Friday], None));
    sync
}

#[tokio::test(flavor = "multi_thread")]
async fn an_occurrence_moved_asks_its_people_again_and_one_only_renamed_keeps_their_answers() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let master = create(&ada, sync(&bert)).await;
        respond(&bert, &master.id, RsvpAnswer::Accept).await.unwrap();
        let events = week(&bert).await;
        assert_eq!(answers(&events), vec![EventResponse::Accepted; 3]);
        let wednesday = events[1].id.clone();
        let asked_again = [
            EventResponse::Accepted,
            EventResponse::NotResponded,
            EventResponse::Accepted,
        ];

        // its first change, to another time
        change(&ada, &wednesday, meeting("Sync", WEDNESDAY + HOUR, &[("Bert", &bert)])).await;
        let after = week(&bert).await;
        assert_eq!(after[1].kind, EventKind::Exception);
        assert_eq!(after[1].start, EventTime::Instant(WEDNESDAY + HOUR));
        assert_eq!(answers(&after), asked_again);

        // renamed where it is
        respond(&bert, &wednesday, RsvpAnswer::Accept).await.unwrap();
        change(
            &ada,
            &wednesday,
            meeting("Sync, late", WEDNESDAY + HOUR, &[("Bert", &bert)]),
        )
        .await;
        let after = week(&bert).await;
        assert_eq!(after[1].subject, "Sync, late");
        assert_eq!(answers(&after), vec![EventResponse::Accepted; 3]);

        // moved once more
        change(
            &ada,
            &wednesday,
            meeting("Sync, late", WEDNESDAY + 2 * HOUR, &[("Bert", &bert)]),
        )
        .await;
        assert_eq!(answers(&week(&bert).await), asked_again);
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn who_loses_the_last_invitation_to_a_series_leaves_its_chat() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let master = create(&ada, sync(&bert)).await;
        let chat = master.meeting_chat.clone().unwrap();
        let events = week(&ada).await;
        let in_chat = async |seat: &Seat| seat.engine.meeting(chat.clone()).await.is_ok();

        // invited to Wednesday alone, Cora is in the series' chat
        change(
            &ada,
            &events[1].id,
            meeting("Sync", WEDNESDAY, &[("Bert", &bert), ("Cora", &cora)]),
        )
        .await;
        assert_eq!(week(&cora).await.len(), 1);
        assert!(in_chat(&cora).await);

        // the series changed to invite Bert alone takes Wednesday from her, and the chat
        let mut renamed = sync(&bert);
        renamed.subject = "Sync, weekly".into();
        change(&ada, &master.id, renamed).await;
        assert!(week(&cora).await.is_empty());
        assert!(!in_chat(&cora).await);
        assert!(in_chat(&bert).await);

        // invited to Friday alone and taken off it again; Bert, off Friday, still has the series
        change(&ada, &events[2].id, meeting("Sync", SYNC + 4 * DAY, &[("Cora", &cora)])).await;
        assert!(in_chat(&cora).await);
        assert_eq!(week(&bert).await.len(), 2);
        assert!(in_chat(&bert).await);
        change(&ada, &events[2].id, meeting("Sync", SYNC + 4 * DAY, &[("Bert", &bert)])).await;
        assert!(!in_chat(&cora).await);
        assert!(in_chat(&bert).await);
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}

/// The change of one occurrence of a channel's series, as its details send it: the occurrence as its scheduler's
/// calendar reads it, the form's fields over it.
async fn change_in_channel(ada: &Seat, id: &str, draft: EventDraft, channel: teams_proto::MeetingChannel) {
    let whole = ada.engine.calendar_event(id.to_owned()).await.unwrap();
    let write = EventWrite::UpdateInChannel {
        event: whole.raw,
        draft,
        channel,
    };
    ada.engine.write_event(write).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channels_moved_occurrence_asks_again_and_is_found_by_the_start_it_now_has() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let mut all_hands = draft("All hands", SYNC, 30);
        all_hands.recurrence = Some(weekly(&[Weekday::Monday, Weekday::Wednesday], None));
        let created = schedule(&ada, all_hands, place(&team, &group), true).await;
        respond(&bert, &created.id, RsvpAnswer::Accept).await.unwrap();
        let wednesday = week(&ada).await[1].id.clone();

        // its first change, to another time: asked again
        let late = draft("All hands", WEDNESDAY + HOUR, 30);
        change_in_channel(&ada, &wednesday, late, place(&team, &group)).await;
        let after = week(&bert).await;
        assert_eq!(after[1].kind, EventKind::Exception);
        assert_eq!(after[1].start, EventTime::Instant(WEDNESDAY + HOUR));
        assert_eq!(answers(&after), [EventResponse::Accepted, EventResponse::NotResponded]);
        assert_eq!(week(&ada).await[1].response, EventResponse::Accepted);

        // changed again where it now is: the same occurrence, its answers kept
        respond(&bert, &wednesday, RsvpAnswer::Accept).await.unwrap();
        let renamed = draft("All hands, late", WEDNESDAY + HOUR, 30);
        change_in_channel(&ada, &wednesday, renamed, place(&team, &group)).await;
        let after = week(&bert).await;
        let subjects: Vec<&str> = after.iter().map(|event| event.subject.as_str()).collect();
        assert_eq!(subjects, ["All hands", "All hands, late"]);
        assert_eq!(answers(&after), vec![EventResponse::Accepted; 2]);

        // and cancelled there: the series' other occurrences stay
        let moved = week(&ada).await.remove(1);
        let (meeting, post) = ScheduledMeeting::of_event(&moved).unwrap();
        let cancel = EventWrite::CancelInChannel {
            meeting: Box::new(meeting),
            channel: place(&team, &group),
            post,
            message: None,
        };
        ada.engine.write_event(cancel).await.unwrap();
        let after = week(&bert).await;
        assert_eq!(
            after.iter().map(|event| event.cancelled).collect::<Vec<_>>(),
            [false, true]
        );
        assert_eq!(week(&ada).await.len(), 1);
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}
