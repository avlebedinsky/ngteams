//! Whom the calendar's routes answer and what they take: an enabled account's own middle-tier token, events of
//! that account's calendar alone, and requests in the shapes the scheduling form sends.

use reqwest::Method;
use serde_json::{Value, json};
use teams_core::EventWrite;
use teams_proto::{IC3_AUDIENCE, MIDDLE_TIER_AUDIENCE};

use super::meetings::invited;
use super::{HOUR, MONDAY, Seat, draft, server};
use crate::support::{Backend, TestServer};
use crate::teams::security::{call, token};

const MT: &str = "/api/mt/part/own-01";
const EVENTS: &str = "/api/mt/part/own-01/v2.0/me/calendars/events";
const NEW: &str = "/api/mt/part/own-01/v2.0/me/calendars/default/events";
const VIEW: &str = "/api/mt/part/own-01/v2.0/me/calendars/default/calendarView";
const MEETING: &str = "/api/mt/part/own-01/beta/me/calendarEvents/privateMeeting/schedulingService/create";

/// Each request with the status it must be answered with.
async fn answered(server: &TestServer, token: &str, cases: &[(Method, String, Option<Value>, u16)]) {
    for (method, path, body, status) in cases {
        let (got, answer) = call(server, token, method.clone(), path, body.clone()).await;
        assert_eq!(got, *status, "{method} {path} {body:?}: {answer}");
    }
}

fn event(start: &str, end: &str) -> Value {
    json!({"subject": "Made up", "startTime": start, "endTime": end})
}

fn sound() -> Value {
    event("2027-01-04T09:00:00+00:00", "2027-01-04T10:00:00+00:00")
}

/// A creation of a sound event with `key` set to `value`.
fn with(key: &str, value: Value) -> Value {
    let mut event = sound();
    event[key] = value;
    json!({"calendarEvent": event})
}

#[tokio::test(flavor = "multi_thread")]
async fn the_calendar_takes_only_an_enabled_accounts_own_middle_tier_token() {
    let server = server(Backend::Sqlite).await;
    let ada = Seat::new(&server, "ada@own.test", "Ada").await;
    let window = format!("{VIEW}?StartDate=2027-01-04T00:00:00.000Z&EndDate=2027-01-05T00:00:00.000Z");
    let routes = [
        window,
        format!("{EVENTS}/nothing"),
        format!("{MT}/beta/me/calendarEvents/timeZoneSettingsWithOffset?timezone=UTC"),
        format!("{MT}/v2.0/places/rooms"),
        format!("{MT}/v2.0/places/roomLists"),
    ];
    let own = token(&server, "ada@own.test", MIDDLE_TIER_AUDIENCE).await;
    let other = token(&server, "ada@own.test", IC3_AUDIENCE).await;
    for path in &routes {
        let (status, _) = call(&server, &own, Method::GET, path, None).await;
        assert!(status == 200 || status == 404, "{path}: {status}");
        for bad in [other.as_str(), "", "a.b.c"] {
            assert_eq!(call(&server, bad, Method::GET, path, None).await.0, 401, "{path}");
        }
    }
    let writes = [(Method::POST, NEW), (Method::POST, MEETING)];
    for (method, path) in writes {
        assert_eq!(
            call(&server, &other, method, path, Some(json!({}))).await.0,
            401,
            "{path}"
        );
    }
    ngteams_server::admin::set_disabled(&server.app.db, &*server.app.fanout, "ada@own.test", true)
        .await
        .unwrap();
    assert_eq!(call(&server, &own, Method::GET, &routes[0], None).await.0, 403);
    ada.stop();
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_and_a_time_zone_are_asked_for_in_the_forms_the_client_writes() {
    let server = server(Backend::Sqlite).await;
    let ada = Seat::new(&server, "ada@own.test", "Ada").await;
    let own = token(&server, "ada@own.test", MIDDLE_TIER_AUDIENCE).await;
    let view = |start: &str, end: &str, status| {
        let path = format!("{VIEW}?StartDate={start}&EndDate={end}&shouldDecryptData=true");
        (Method::GET, path, None, status)
    };
    let zone = format!("{MT}/beta/me/calendarEvents/timeZoneSettingsWithOffset");
    let cases = [
        view("2027-01-04T00:00:00.000Z", "2027-01-05T00:00:00.000Z", 200),
        view("2027-01-04T00:00:00.000Z", "2027-04-14T00:00:00.000Z", 200),
        view("2027-01-04T00:00:00.000Z", "2027-04-15T00:00:00.000Z", 400),
        view("2027-01-05T00:00:00.000Z", "2027-01-05T00:00:00.000Z", 400),
        view("2027-01-05T00:00:00.000Z", "2027-01-04T00:00:00.000Z", 400),
        view("monday", "2027-01-04T00:00:00.000Z", 400),
        (Method::GET, VIEW.to_owned(), None, 400),
        (Method::GET, zone.clone(), None, 400),
        (Method::GET, format!("{zone}?timezone=Mars/Olympus"), None, 404),
    ];
    answered(&server, &own, &cases).await;
    let (status, named) = call(&server, &own, Method::GET, &format!("{zone}?timezone=Asia/Tokyo"), None).await;
    assert_eq!(status, 200);
    assert_eq!(named["local"]["timeZone"], "TokyoSt");
    assert_eq!(named["utcOffsetInMinutes"], 540);
    assert_eq!(named["timeZone"], named["remote"]["timeZone"]);
    ada.stop();
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_is_taken_only_in_the_shape_the_form_sends() {
    let server = server(Backend::Sqlite).await;
    let ada = Seat::new(&server, "ada@own.test", "Ada").await;
    let own = token(&server, "ada@own.test", MIDDLE_TIER_AUDIENCE).await;
    let long = "x".repeat(256);
    let many: Vec<Value> = (0..251).map(|n| json!({"address": format!("p{n}@own.test")})).collect();
    let bodies = [
        json!({}),
        json!({"calendarEvent": "an event"}),
        json!({"calendarEvent": event("2027-01-04T10:00:00+00:00", "2027-01-04T09:00:00+00:00")}),
        json!({"calendarEvent": event("2027-01-04T10:00:00", "2027-01-04T11:00:00")}),
        json!({"calendarEvent": event("2027-01-04T10:00:00+00:00", "2029-01-04T11:00:00+00:00")}),
        json!({"calendarEvent": {"subject": "No times"}}),
        with("subject", json!(long)),
        with("subject", json!(7)),
        with("location", json!({"displayName": "Somewhere"})),
        with("isPrivate", json!("yes")),
        with("showAs", json!("Asleep")),
        with("reminderMinutesBeforeStart", json!(99_999)),
        with("bodyContent", json!(["text"])),
        with("categories", json!("Health")),
        with("categories", json!([7])),
        with("categories", json!(vec!["c"; 26])),
        with("eventTimeZone", json!(7)),
        with("utcOffset", json!(5000)),
        with("utcOffset", json!("three hours")),
        with("eventRecurrencePattern", json!({"patternType": 9})),
        with("attendees", json!("everyone")),
        with("attendees", json!(many)),
        with("attendees", json!([{"address": "nobody@own.test", "type": "Required"}])),
        with("attendees", json!([{"mri": "8:orgid:not-an-id", "type": "Required"}])),
        with("attendees", json!([{"type": "Required"}])),
    ];
    for body in bodies {
        let cases = [(Method::POST, NEW.to_owned(), Some(body), 400)];
        answered(&server, &own, &cases).await;
    }
    let window = format!("{VIEW}?StartDate=2026-12-01T00:00:00.000Z&EndDate=2027-02-01T00:00:00.000Z");
    let (_, listed) = call(&server, &own, Method::GET, &window, None).await;
    assert_eq!(listed["value"], json!([]), "a refused event is kept nowhere");
    // what the form may leave out has its defaults; what it never sends is not kept
    let lean = json!({"calendarEvent": {"startTime": "2027-01-04T09:00:00+02:00", "endTime": "2027-01-04T09:00:00+02:00",
        "isAllDayEvent": true, "objectId": "mine", "isOrganizer": false, "organizerName": "Someone else"}});
    let (status, made) = call(&server, &own, Method::POST, NEW, Some(lean)).await;
    assert_eq!(status, 201, "{made}");
    let made = &made["value"][0];
    assert_ne!(made["objectId"], "mine");
    assert_eq!(made["organizerName"], "Ada");
    assert_eq!(made["isOrganizer"], true);
    assert_eq!(
        (&made["subject"], &made["showAs"], &made["eventTimeZone"]),
        (&json!(""), &json!("Busy"), &json!("Utc"))
    );
    assert_eq!(made["startTime"], "2027-01-04T07:00:00.0000000Z");
    assert_eq!(made["isAllDayEvent"], true);
    let (_, listed) = call(&server, &own, Method::GET, &window, None).await;
    assert_eq!(
        listed["value"].as_array().unwrap().len(),
        1,
        "an event of no length is in its window"
    );
    ada.stop();
    server.stop().await;
}

/// Ada's meeting with Bert, a series of hers, and the tokens of Ada, Bert and Cora (who is invited to nothing).
struct World {
    server: TestServer,
    seats: Vec<Seat>,
    tokens: [String; 3],
    meeting: String,
    series: String,
}

async fn world() -> World {
    let server = server(Backend::Sqlite).await;
    let ada = Seat::new(&server, "ada@own.test", "Ada").await;
    let bert = Seat::new(&server, "bert@own.test", "Bert").await;
    let cora = Seat::new(&server, "cora@own.test", "Cora").await;
    let mut planning = draft("Planning", MONDAY + 9 * HOUR, 30);
    planning.online = true;
    planning.attendees = vec![invited("bert@own.test", "Bert", None, true)];
    let meeting = ada.engine.write_event(EventWrite::Create(planning)).await.unwrap();
    let mut standup = draft("Standup", MONDAY + 10 * HOUR, 15);
    standup.recurrence = Some(super::events::weekly(&[teams_proto::Weekday::Monday], None));
    let series = ada.engine.write_event(EventWrite::Create(standup)).await.unwrap();
    let mut tokens = Vec::new();
    for upn in ["ada@own.test", "bert@own.test", "cora@own.test"] {
        tokens.push(token(&server, upn, MIDDLE_TIER_AUDIENCE).await);
    }
    World {
        server,
        seats: vec![ada, bert, cora],
        tokens: tokens.try_into().unwrap(),
        meeting: meeting.created.unwrap().id,
        series: series.created.unwrap().id,
    }
}

impl World {
    async fn stop(self) {
        for seat in self.seats {
            seat.stop();
        }
        self.server.stop().await;
    }
}

/// Every route that names the event `id`, each with a body it would take.
fn routes_of(id: &str) -> Vec<(Method, String, Option<Value>)> {
    let event = format!("{EVENTS}/{id}");
    vec![
        (Method::GET, event.clone(), None),
        (
            Method::PUT,
            event.clone(),
            Some(json!({"updatedCalendarEvent": sound()})),
        ),
        (Method::PUT, format!("{event}/cancel"), Some(json!({}))),
        (
            Method::PUT,
            format!("{event}/respond"),
            Some(json!({"responseType": "Accept"})),
        ),
        (Method::DELETE, event, None),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_is_there_only_for_the_accounts_whose_calendar_it_is_on() {
    let world = world().await;
    let [_, bert, cora] = &world.tokens;
    // someone it doesn't invite learns nothing of it, by any route; an id that names no event or occurrence too
    let slot = MONDAY + 10 * HOUR;
    let unknown = [
        "nothing".to_owned(),
        format!("{}.soon", world.series),
        format!("{}.{}", world.series, slot + 60_000),
        format!("{}.{slot}", world.meeting),
        format!("{}.1.2", world.series),
    ];
    for (method, path, body) in routes_of(&world.meeting) {
        answered(&world.server, cora, &[(method, path, body, 404)]).await;
    }
    for id in &unknown {
        for (method, path, body) in routes_of(id) {
            answered(&world.server, &world.tokens[0], &[(method, path, body, 404)]).await;
        }
    }
    // an invited account reads it and may not change or cancel it
    let refused = [403, 403];
    for ((method, path, body), status) in routes_of(&world.meeting).into_iter().skip(1).zip(refused) {
        answered(&world.server, bert, &[(method, path, body, status)]).await;
    }
    world.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_of_an_event_is_taken_only_in_its_own_shape() {
    let world = world().await;
    let [ada, bert, _] = &world.tokens;
    let event = format!("{EVENTS}/{}", world.meeting);
    let occurrence = format!("{EVENTS}/{}.{}", world.series, MONDAY + 10 * HOUR);
    let cases = [
        (Method::POST, NEW.to_owned(), None, 400),
        (Method::PUT, event.clone(), None, 400),
        (
            Method::PUT,
            event.clone(),
            Some(json!({"updatedCalendarEvent": {"subject": "No times"}})),
            400,
        ),
        (Method::PUT, format!("{event}/cancel"), None, 400),
        (
            Method::PUT,
            format!("{event}/cancel"),
            Some(json!({"cancellationMessage": 7})),
            400,
        ),
        (
            Method::PUT,
            format!("{event}/cancel"),
            Some(json!({"cancellationMessage": "x".repeat(1001)})),
            400,
        ),
        (
            Method::PUT,
            format!("{event}/respond"),
            Some(json!({"responseType": "Organizer"})),
            403,
        ),
        // an occurrence takes no rule of its own: the series' is left out of its change
        (
            Method::PUT,
            occurrence,
            Some(json!({"updatedCalendarEvent": sound()})),
            204,
        ),
    ];
    answered(&world.server, ada, &cases).await;
    let cases = [
        (Method::PUT, format!("{event}/respond"), None, 400),
        (
            Method::PUT,
            format!("{event}/respond"),
            Some(json!({"responseType": "Maybe"})),
            400,
        ),
        (Method::PUT, format!("{event}/respond"), Some(json!({})), 400),
    ];
    answered(&world.server, bert, &cases).await;
    world.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_teams_meeting_is_made_and_taken_only_by_its_own_organizer() {
    let world = world().await;
    let [ada, bert, _] = &world.tokens;
    let make = |body: Value, status| (Method::POST, MEETING.to_owned(), Some(body), status);
    let cases = [
        (Method::POST, MEETING.to_owned(), None, 400),
        make(json!({}), 400),
        make(json!({"meetingType": "Webinar"}), 400),
        make(json!({"meetingType": "MeetNow", "subject": 7}), 400),
        make(json!({"meetingType": "MeetNow", "subject": "x".repeat(256)}), 400),
    ];
    answered(&world.server, ada, &cases).await;
    let meeting_of = |token: &str, kind: &str| {
        let (server, token, kind) = (&world.server, token.to_owned(), kind.to_owned());
        async move {
            let body = json!({"meetingType": kind, "isStreamEnabled": false});
            let (status, meeting) = call(server, &token, Method::POST, MEETING, Some(body)).await;
            assert_eq!(status, 201, "{meeting}");
            meeting["value"].clone()
        }
    };
    let online = format!("{NEW}?isOnlineMeeting=true&shouldDecryptData=true");
    let taking = |meeting: Value, status| {
        let body = json!({"calendarEvent": sound(), "schedulingServiceEvent": meeting});
        (Method::POST, online.clone(), Some(body), status)
    };
    let (own, theirs, now) = (
        meeting_of(ada, "Scheduled").await,
        meeting_of(bert, "Scheduled").await,
        meeting_of(ada, "MeetNow").await,
    );
    let cases = [
        (
            Method::POST,
            online.clone(),
            Some(json!({"calendarEvent": sound()})),
            400,
        ),
        taking(
            json!({"groupContext": {"threadId": "19:meeting_madeup@thread.v2"}}),
            400,
        ),
        taking(theirs, 400),
        taking(now, 400),
        taking(own.clone(), 201),
        taking(own, 400),
    ];
    answered(&world.server, ada, &cases).await;
    world.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_scheduler_takes_only_an_ask_in_its_own_shape() {
    let world = world().await;
    let [ada, _, _] = &world.tokens;
    let ask = |change: Value| {
        let mut body = json!({"attendees": [{"type": "Organizer"}], "startTime": "2027-01-04T00:00:00.000Z",
            "endTime": "2027-01-06T00:00:00.000Z", "numberOfDays": 2, "meetingDurationInMinutes": 30});
        for (key, value) in change.as_object().unwrap() {
            body[key] = value.clone();
        }
        body
    };
    let people: Vec<Value> = (0..101).map(|n| json!({"address": format!("p{n}@own.test")})).collect();
    let bad = [
        json!({"startTime": null}),
        json!({"attendees": null}),
        json!({"attendees": people}),
        json!({"endTime": "2027-01-03T00:00:00.000Z"}),
        json!({"endTime": "2027-06-04T00:00:00.000Z"}),
        json!({"meetingDurationInMinutes": 0}),
    ];
    for route in ["schedules", "findMeetingTimes"] {
        let path = format!("{MT}/v2.0/me/calendars/{route}");
        let mut cases = vec![
            (Method::PUT, path.clone(), None, 400),
            (Method::PUT, path.clone(), Some(ask(json!({}))), 200),
        ];
        cases.extend(
            bad.iter()
                .map(|change| (Method::PUT, path.clone(), Some(ask(change.clone())), 400)),
        );
        answered(&world.server, ada, &cases).await;
    }
    // a window given by its days alone, and suggestions never past its end
    let path = format!("{MT}/v2.0/me/calendars/findMeetingTimes");
    let (_, times) = call(
        &world.server,
        ada,
        Method::PUT,
        &path,
        Some(ask(json!({"endTime": null}))),
    )
    .await;
    assert_eq!(
        times["timeSuggestions"].as_array().unwrap().len(),
        2 * 20 - 2,
        "{times}"
    );

    world.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_meetings_options_take_only_the_values_controls_offers() {
    let world = world().await;
    let [ada, _, _] = &world.tokens;
    let tenant = ngteams_server::db::meta::tenant(&world.server.app.db).await.unwrap();
    let whole = call(
        &world.server,
        ada,
        Method::GET,
        &format!("{EVENTS}/{}", world.meeting),
        None,
    )
    .await
    .1;
    let data: Value = serde_json::from_str(whole["skypeTeamsData"].as_str().unwrap()).unwrap();
    let chat = data["cid"].as_str().unwrap();
    let options = format!(
        "{MT}/beta/meetings/v1/options/{tenant}/{}/{chat}/0/0",
        world.seats[0].id
    );
    let set = |options: Value| Some(json!({ "options": options }));
    let cases = [
        (Method::POST, options.clone(), None, 400),
        (Method::POST, options.clone(), Some(json!({})), 400),
        (Method::POST, options.clone(), set(json!([])), 400),
        (
            Method::POST,
            options.clone(),
            set(json!([{"name": "Recording", "currentValue": "On"}])),
            400,
        ),
        (
            Method::POST,
            options.clone(),
            set(json!([{"name": "AllowMeetingChat", "currentValue": "Loud"}])),
            400,
        ),
        (
            Method::POST,
            options.clone(),
            set(json!([{"name": "AllowMeetingChat"}])),
            400,
        ),
        (
            Method::POST,
            options,
            set(json!([{"name": "AllowMeetingChat", "currentValue": "InMeeting"}])),
            200,
        ),
    ];
    answered(&world.server, ada, &cases).await;
    world.stop().await;
}
