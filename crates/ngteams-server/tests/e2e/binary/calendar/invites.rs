//! An invitation: a invites b and c, who each have it and answer it, change their answer and take it off; a reads
//! every answer. An address that is no account is refused, b changes nothing of a's event, and d, not invited,
//! reads nothing of it, not by naming its ids, its link or its chat either.

use reqwest::Method;
use serde_json::json;
use teams_core::EventWrite;
use teams_proto::chat::ChatReads;
use teams_proto::{CalendarEvent, EventResponse, FullEvent, RsvpAnswer, ShowAs, Weekday};

use super::{Trio, User, cancel, change, create, delete, invite, raw, refused, respond, window};
use crate::calendar::events::weekly;
use crate::calendar::meetings::invited;
use crate::calendar::{HOUR, MONDAY, draft};
use crate::support::backends;

const PLANNING: i64 = MONDAY + 3 * 24 * HOUR + 15 * HOUR;

/// Each invited account's answer as a reads the event whole: (address, answer).
async fn answers(a: &User, id: &str) -> Vec<(String, Option<EventResponse>)> {
    let whole = a.engine().calendar_event(id.to_owned()).await.unwrap();
    whole
        .event
        .attendees
        .iter()
        .map(|attendee| (attendee.address.clone().unwrap_or_default(), attendee.response.clone()))
        .collect()
}

fn roster(
    a: Option<EventResponse>,
    b: Option<EventResponse>,
    c: Option<EventResponse>,
) -> Vec<(String, Option<EventResponse>)> {
    vec![
        ("a@own.test".into(), a),
        ("b@own.test".into(), b),
        ("c@own.test".into(), c),
    ]
}

/// The account's own copy, read by its client.
async fn own(user: &User) -> CalendarEvent {
    let events = window(user, MONDAY, 7).await;
    assert_eq!(events.len(), 1, "{}: {events:?}", user.upn);
    events.into_iter().next().unwrap()
}

async fn each_answers(trio: &Trio, id: &str) {
    let (a, b, c) = (&trio.a, &trio.b, &trio.c);
    let organizer = Some(EventResponse::Organizer);
    let none = Some(EventResponse::NoResponse);
    assert_eq!(answers(a, id).await, roster(organizer.clone(), none.clone(), none));
    for user in [b, c] {
        let event = own(user).await;
        assert_eq!(
            (event.response, event.show_as),
            (EventResponse::NotResponded, ShowAs::Tentative)
        );
        assert!(!event.is_organizer);
    }
    first_answers(trio, id).await;
    changed_answers(trio, id).await;
}

async fn first_answers(trio: &Trio, id: &str) {
    let (a, b, c) = (&trio.a, &trio.b, &trio.c);
    let organizer = Some(EventResponse::Organizer);
    respond(b, id, RsvpAnswer::Accept).await.unwrap();
    respond(c, id, RsvpAnswer::Decline).await.unwrap();
    let (accepted, declined) = (Some(EventResponse::Accepted), Some(EventResponse::Declined));
    assert_eq!(
        answers(a, id).await,
        roster(organizer.clone(), accepted.clone(), declined)
    );
    assert_eq!(own(b).await.response, EventResponse::Accepted);
    assert_eq!(own(c).await.response, EventResponse::Declined);
}

/// Each changes its mind: tentative, then b accepts again.
async fn changed_answers(trio: &Trio, id: &str) {
    let (a, b, c) = (&trio.a, &trio.b, &trio.c);
    let organizer = Some(EventResponse::Organizer);
    let accepted = Some(EventResponse::Accepted);
    respond(c, id, RsvpAnswer::Tentative).await.unwrap();
    respond(b, id, RsvpAnswer::Tentative).await.unwrap();
    let tentative = Some(EventResponse::Tentative);
    assert_eq!(
        answers(a, id).await,
        roster(organizer.clone(), tentative.clone(), tentative)
    );
    assert_eq!(own(c).await.response, EventResponse::Tentative);
    assert_eq!(own(c).await.show_as, ShowAs::Tentative);
    respond(b, id, RsvpAnswer::Accept).await.unwrap();
    assert_eq!(own(b).await.show_as, ShowAs::Busy);
    assert_eq!(answers(a, id).await[1].1, accepted);
}

/// b may read a's event and answer it, never change or cancel it.
async fn b_changes_nothing(trio: &Trio, created: &CalendarEvent, planning: &teams_proto::EventDraft) {
    let (a, b) = (&trio.a, &trio.b);
    let mut taken = planning.clone();
    taken.subject = "Mine now".into();
    refused(change(b, &created.id, taken).await, "403");
    refused(cancel(b, created, Some("No")).await, "403");
    let path = format!("/v2.0/me/calendars/events/{}", created.id);
    let (status, _) = raw(
        trio.rig.other(),
        "b@own.test",
        Method::PUT,
        &format!("{path}/cancel"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 403);
    let event = own(a).await;
    assert_eq!(event.subject, "Planning");
    assert!(!event.cancelled);
}

/// Every route that names the event `id`, each with a body it would take.
fn routes_of(id: &str) -> Vec<(Method, String, Option<serde_json::Value>)> {
    let event = format!("/v2.0/me/calendars/events/{id}");
    let sound =
        json!({"subject": "Made up", "startTime": "2027-01-04T09:00:00+00:00", "endTime": "2027-01-04T10:00:00+00:00"});
    vec![
        (Method::GET, event.clone(), None),
        (Method::PUT, event.clone(), Some(json!({"updatedCalendarEvent": sound}))),
        (Method::PUT, format!("{event}/cancel"), Some(json!({}))),
        (
            Method::PUT,
            format!("{event}/respond"),
            Some(json!({"responseType": "Accept"})),
        ),
        (Method::DELETE, event, None),
    ]
}

/// d's client and every route that names `id` find nothing.
async fn nothing_by_id(trio: &Trio, d: &User, id: &str) {
    assert!(d.engine().calendar_event(id.to_owned()).await.is_err(), "{id}");
    refused(respond(d, id, RsvpAnswer::Accept).await, "404");
    refused(delete(d, id).await, "404");
    for (method, path, body) in routes_of(id) {
        let (status, answer) = raw(trio.rig.other(), "d@own.test", method.clone(), &path, body).await;
        assert_eq!(status, 404, "{method} {path}: {answer}");
    }
}

/// d, not invited, learns nothing of the meeting or of a's series: not its events, occurrences, chat, link or
/// options, by any route.
async fn d_reads_nothing(trio: &Trio, meeting: &CalendarEvent, series: &CalendarEvent) {
    let d = trio.client("d@own.test", false).await;
    assert!(window(&d, MONDAY, 7).await.is_empty());
    let occurrence = format!("{}.{}", series.id, MONDAY + 9 * HOUR);
    for id in [&meeting.id, &series.id, &occurrence] {
        nothing_by_id(trio, &d, id).await;
    }
    nothing_by_link_chat_or_options(trio, &d, meeting).await;
    // and an event of d's own names none of a's: the window it is read in holds only its own
    let mine = create(&d, draft("Mine", MONDAY + 9 * HOUR, 30)).await;
    let all = window(&d, MONDAY, 7).await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, mine.id);
}

async fn nothing_by_link_chat_or_options(trio: &Trio, d: &User, meeting: &CalendarEvent) {
    let chat = meeting.meeting_chat.clone().unwrap();
    let link = meeting.join_url.clone().unwrap();
    let cloud = teams_proto::Cloud::own(&trio.rig.origin).unwrap();
    assert_eq!(
        teams_proto::join_url_chat_in(&cloud, &link).as_deref(),
        Some(chat.as_str())
    );
    assert!(d.engine().meeting(chat.clone()).await.is_err());
    let client = d.client();
    assert!(ChatReads::new(&client).messages(&chat, 10).await.is_err());
    let listed = ChatReads::new(&client).conversations().await.unwrap();
    assert!(listed.conversations.iter().all(|entry| entry.id != chat));
    let info = meeting.meeting.clone().unwrap();
    let options = format!(
        "/beta/meetings/v1/options/{}/{}/{chat}/0/0",
        info.tenant_id, info.organizer_id
    );
    let (status, _) = raw(trio.rig.other(), "d@own.test", Method::GET, &options, None).await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invited_accounts_have_the_event_answer_it_and_take_it_off_and_nobody_else_reads_or_changes_it() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let (a, b, c) = (&trio.a, &trio.b, &trio.c);
        // an address that is no account of the server is refused, and nothing is saved
        let mut planning = draft("Planning", PLANNING, 30);
        planning.online = true;
        planning.attendees = vec![invite(b), invited("nobody@own.test", "Nobody", None, true)];
        refused(
            a.engine().write_event(EventWrite::Create(planning.clone())).await,
            "400",
        );
        assert!(window(a, MONDAY, 7).await.is_empty());
        assert!(window(b, MONDAY, 7).await.is_empty());

        planning.attendees = vec![invite(b), invite(c)];
        let created = create(a, planning.clone()).await;
        each_answers(&trio, &created.id).await;
        b_changes_nothing(&trio, &created, &planning).await;
        let mut standup = draft("Standup", MONDAY + 9 * HOUR, 15);
        standup.recurrence = Some(weekly(&[Weekday::Monday], None));
        let series = create(a, standup).await;
        d_reads_nothing(&trio, &created, &series).await;

        // c takes it off its calendar: gone there, still on a's and b's
        delete(c, &created.id).await.unwrap();
        assert!(window(c, MONDAY, 7).await.is_empty());
        assert!(c.engine().calendar_event(created.id.clone()).await.is_err());
        assert_eq!(own(b).await.id, created.id);
        let whole: FullEvent = a.engine().calendar_event(created.id.clone()).await.unwrap();
        assert_eq!(whole.event.attendees[1].response, Some(EventResponse::Accepted));
        refused(respond(c, &created.id, RsvpAnswer::Accept).await, "404");
        trio.stop().await;
    }
}
