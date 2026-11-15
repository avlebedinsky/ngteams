//! One occurrence of a series of meetings given a time or people of its own: who is asked again, who is in the
//! series' chat, and how a channel's moved occurrence is found.

use teams_core::EventWrite;
use teams_core::activity::{ActivityKind, MeetingChange};
use teams_proto::{EventDraft, EventKind, EventResponse, EventTime, RsvpAnswer, ScheduledMeeting, Weekday};

use super::channels::{place, refused, schedule};
use super::events::{change, weekly};
use super::meetings::{answers, create, invited, respond};
use super::{DAY, HOUR, MONDAY, Seat, draft, server, week};
use crate::chatsvc::activity::feed;
use crate::support::backends;

/// What the engine answers a calendar write.
type Written = Result<teams_core::EventWritten, teams_core::EngineError>;

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
async fn an_occurrence_changed_to_a_teams_meeting_gives_its_series_the_meeting() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let mut in_a_room = sync(&bert);
        in_a_room.online = false;
        create(&ada, in_a_room).await;
        let events = week(&ada).await;
        assert!(events.iter().all(|event| !event.online && event.meeting_chat.is_none()));

        // Wednesday alone, with Cora, saved as a Teams meeting
        let wednesday = [("Bert", &bert), ("Cora", &cora)];
        change(&ada, &events[1].id, meeting("Sync", WEDNESDAY, &wednesday)).await;
        let after = week(&bert).await;
        assert!(after[1].online && after[1].join_url.is_some());
        let chat = after[1].meeting_chat.clone().unwrap();
        assert!(
            after.iter().all(|event| event.meeting_chat.as_ref() == Some(&chat)),
            "the meeting is the series'"
        );
        for seat in [&ada, &bert, &cora] {
            let details = seat.engine.meeting(chat.clone()).await.unwrap().unwrap();
            assert_eq!(details.title.as_deref(), Some("Sync"));
        }

        // changed again, it keeps that meeting
        change(&ada, &events[1].id, meeting("Sync, midweek", WEDNESDAY, &wednesday)).await;
        let again = week(&cora).await;
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].meeting_chat.as_ref(), Some(&chat));
        for seat in [ada, bert, cora] {
            seat.stop();
        }
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

        // invited to Friday alone once more; the series moved forgets its Friday as it was, and her with it
        let friday = [("Bert", &bert), ("Cora", &cora)];
        change(&ada, &events[2].id, meeting("Sync", SYNC + 4 * DAY, &friday)).await;
        assert!(in_chat(&cora).await);
        let mut later = meeting("Sync, weekly", SYNC + HOUR, &[("Bert", &bert)]);
        later.recurrence = sync(&bert).recurrence;
        change(&ada, &master.id, later).await;
        assert!(week(&cora).await.is_empty());
        assert!(!in_chat(&cora).await);
        assert!(in_chat(&bert).await);
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_series_ended_is_cancelled_for_who_one_occurrence_alone_invites() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let canceled = async |seat: &Seat| {
            let items = feed(&seat.engine).await.items;
            items
                .iter()
                .filter(|item| item.kind == ActivityKind::Meeting(MeetingChange::Canceled))
                .count()
        };

        // a series nobody else is invited to, but Cora to its Wednesday: cancelled, it stays, cancelled, on hers
        let mut alone = meeting("Focus", SYNC, &[]);
        alone.recurrence = sync(&bert).recurrence;
        let master = create(&ada, alone).await;
        let events = week(&ada).await;
        change(&ada, &events[1].id, meeting("Focus", WEDNESDAY, &[("Cora", &cora)])).await;
        let cancel = EventWrite::Cancel {
            id: master.id.clone(),
            chat: master.meeting_chat.clone(),
            message: None,
        };
        ada.engine.write_event(cancel).await.unwrap();
        let hers = week(&cora).await;
        assert_eq!(hers.len(), 1);
        assert!(hers[0].cancelled);
        assert_eq!(canceled(&cora).await, 1);

        // a series with Bert, Cora on its Wednesday alone: deleted by its organizer, both are told
        let later = SYNC + 3 * HOUR;
        let mut with_bert = meeting("Review", later, &[("Bert", &bert)]);
        with_bert.recurrence = sync(&bert).recurrence;
        let master = create(&ada, with_bert).await;
        let wednesday = week(&ada)
            .await
            .into_iter()
            .find(|event| event.start == EventTime::Instant(later + 2 * DAY));
        let only_cora = [("Bert", &bert), ("Cora", &cora)];
        change(
            &ada,
            &wednesday.unwrap().id,
            meeting("Review", later + 2 * DAY, &only_cora),
        )
        .await;
        ada.engine
            .write_event(EventWrite::Delete { id: master.id.clone() })
            .await
            .unwrap();
        assert_eq!(canceled(&cora).await, 2);
        assert_eq!(canceled(&bert).await, 1);
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}

/// The change of one occurrence of a channel's series, as its details send it: the occurrence as its scheduler's
/// calendar reads it, the form's fields over it.
async fn change_in_channel(ada: &Seat, id: &str, draft: EventDraft, channel: teams_proto::MeetingChannel) {
    changed_in_channel(ada, id, draft, channel).await.unwrap();
}

/// What the server answered that change.
async fn changed_in_channel(ada: &Seat, id: &str, draft: EventDraft, channel: teams_proto::MeetingChannel) -> Written {
    let whole = ada.engine.calendar_event(id.to_owned()).await.unwrap();
    let write = EventWrite::UpdateInChannel {
        event: whole.raw,
        draft,
        channel,
    };
    ada.engine.write_event(write).await
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
        cancel_in_channel(&ada, &moved, place(&team, &group)).await;
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

/// The cancellation of one occurrence of a channel's series, named as the scheduler's calendar now reads it.
async fn cancel_in_channel(ada: &Seat, moved: &teams_proto::CalendarEvent, channel: teams_proto::MeetingChannel) {
    cancelled_in_channel(ada, moved, channel).await.unwrap();
}

/// What the server answered that cancellation.
async fn cancelled_in_channel(
    ada: &Seat,
    moved: &teams_proto::CalendarEvent,
    channel: teams_proto::MeetingChannel,
) -> Written {
    let (meeting, post) = ScheduledMeeting::of_event(moved).unwrap();
    let cancel = EventWrite::CancelInChannel {
        meeting: Box::new(meeting),
        channel,
        post,
        message: None,
    };
    ada.engine.write_event(cancel).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channels_occurrence_isnt_moved_to_where_another_moved_one_of_its_series_starts() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        let mut all_hands = draft("All hands", SYNC, 30);
        all_hands.recurrence = Some(weekly(&[Weekday::Monday, Weekday::Wednesday, Weekday::Friday], None));
        schedule(&ada, all_hands, place(&team, &group), true).await;
        let events = week(&ada).await;
        let (wednesday, friday) = (events[1].id.clone(), events[2].id.clone());
        let thursday = SYNC + 3 * DAY + HOUR;
        let there = draft("All hands, moved", thursday, 30);
        change_in_channel(&ada, &wednesday, there.clone(), place(&team, &group)).await;

        // Friday's can't take that start too: a change there would name either of the two
        let late = draft("All hands, late", thursday, 30);
        refused(
            changed_in_channel(&ada, &friday, late, place(&team, &group)).await,
            "409",
        );
        let starts = |events: &[teams_proto::CalendarEvent]| -> Vec<EventTime> {
            events.iter().map(|event| event.start).collect()
        };
        let times = [SYNC, thursday, SYNC + 4 * DAY].map(EventTime::Instant);
        assert_eq!(starts(&week(&ada).await), times);

        // the one that is there is changed again where it is
        change_in_channel(&ada, &wednesday, there, place(&team, &group)).await;
        let after = week(&ada).await;
        assert_eq!(starts(&after), times);
        assert_eq!(after[1].subject, "All hands, moved");
        assert_eq!(after[2].subject, "All hands");
        ada.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channels_occurrence_isnt_moved_to_the_start_another_of_its_series_still_has() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        let mut all_hands = draft("All hands", SYNC, 30);
        all_hands.recurrence = Some(weekly(&[Weekday::Monday, Weekday::Wednesday, Weekday::Friday], None));
        schedule(&ada, all_hands, place(&team, &group), true).await;
        let events = week(&ada).await;
        let (wednesday, friday) = (events[1].id.clone(), events[2].id.clone());

        // Wednesday's, which nobody changed, is at its own start: Friday's can't take it
        let early = draft("All hands, early", WEDNESDAY, 30);
        refused(
            changed_in_channel(&ada, &friday, early.clone(), place(&team, &group)).await,
            "409",
        );
        let starts = |events: &[teams_proto::CalendarEvent]| -> Vec<EventTime> {
            events.iter().map(|event| event.start).collect()
        };
        assert_eq!(
            starts(&week(&ada).await),
            [SYNC, WEDNESDAY, SYNC + 4 * DAY].map(EventTime::Instant)
        );

        // so Wednesday's is still changed where it is, and Friday's still can't come there
        let renamed = draft("All hands, midweek", WEDNESDAY, 30);
        change_in_channel(&ada, &wednesday, renamed, place(&team, &group)).await;
        refused(
            changed_in_channel(&ada, &friday, early.clone(), place(&team, &group)).await,
            "409",
        );

        // once Wednesday's is cancelled its start is free
        let there = week(&ada).await.remove(1);
        assert_eq!(there.subject, "All hands, midweek");
        cancel_in_channel(&ada, &there, place(&team, &group)).await;
        change_in_channel(&ada, &friday, early, place(&team, &group)).await;
        let after = week(&ada).await;
        assert_eq!(starts(&after), [SYNC, WEDNESDAY].map(EventTime::Instant));
        assert_eq!(after[1].subject, "All hands, early");
        ada.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channels_cancelled_exception_named_again_doesnt_reach_the_occurrence_whose_time_it_took() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let mut all_hands = draft("All hands", SYNC, 30);
        all_hands.recurrence = Some(weekly(&[Weekday::Monday, Weekday::Wednesday, Weekday::Friday], None));
        schedule(&ada, all_hands, place(&team, &group), true).await;
        let events = week(&ada).await;
        let (wednesday, friday) = (events[1].id.clone(), events[2].id.clone());

        // Friday's moved away, Wednesday's moved onto the time Friday's had and cancelled there
        let away = draft("All hands", SYNC + 5 * DAY, 30);
        change_in_channel(&ada, &friday, away, place(&team, &group)).await;
        let moved = draft("All hands, early", SYNC + 4 * DAY, 30);
        change_in_channel(&ada, &wednesday, moved, place(&team, &group)).await;
        let events = week(&ada).await;
        let there = events.iter().find(|event| event.subject == "All hands, early").unwrap();
        cancel_in_channel(&ada, there, place(&team, &group)).await;
        assert_eq!(week(&ada).await.len(), 2);

        // the same cancellation sent again finds nothing: Friday's own occurrence stays
        refused(cancelled_in_channel(&ada, there, place(&team, &group)).await, "404");
        assert_eq!(week(&ada).await.len(), 2);
        let live = week(&bert).await.into_iter().filter(|event| !event.cancelled);
        assert_eq!(live.count(), 2);
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channels_occurrence_moved_to_where_a_cancelled_one_was_is_the_one_found_there() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let mut all_hands = draft("All hands", SYNC, 30);
        all_hands.recurrence = Some(weekly(&[Weekday::Monday, Weekday::Wednesday, Weekday::Friday], None));
        schedule(&ada, all_hands, place(&team, &group), true).await;
        let events = week(&ada).await;
        let (wednesday, friday) = (events[1].id.clone(), events[2].id.clone());
        let thursday = SYNC + 3 * DAY + HOUR;

        // Wednesday's moved to Thursday and cancelled there
        let moved = draft("All hands", thursday, 30);
        change_in_channel(&ada, &wednesday, moved, place(&team, &group)).await;
        let there = week(&ada).await.remove(1);
        assert_eq!(there.start, EventTime::Instant(thursday));
        cancel_in_channel(&ada, &there, place(&team, &group)).await;
        assert_eq!(week(&ada).await.len(), 2);

        // Friday's moved to the same time is the one a change and a cancellation there reach
        let moved = draft("All hands", thursday, 30);
        change_in_channel(&ada, &friday, moved, place(&team, &group)).await;
        let renamed = draft("All hands, moved", thursday, 30);
        change_in_channel(&ada, &friday, renamed, place(&team, &group)).await;
        let live = |events: Vec<teams_proto::CalendarEvent>| -> Vec<String> {
            let live = events.into_iter().filter(|event| !event.cancelled);
            live.map(|event| event.subject).collect()
        };
        assert_eq!(live(week(&bert).await), ["All hands", "All hands, moved"]);
        let there = week(&ada).await.remove(1);
        cancel_in_channel(&ada, &there, place(&team, &group)).await;
        assert_eq!(live(week(&bert).await), ["All hands"]);
        assert_eq!(week(&ada).await.len(), 1);
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}
