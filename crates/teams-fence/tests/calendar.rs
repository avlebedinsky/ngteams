//! The middle tier's calendar rows of `teams_fence::contacting` (`docs/protocol/calendar.md`, "Writing an event"), the
//! fence's judgement of a meeting by its attendees' addresses, and the read of an existing event's members. Every
//! address, id and token here is made up.

use serde_json::{Value, json};
use teams_fence::{
    Contact, DenyReason, Fence, FenceLog, FetchFailure, LiveMembershipSource, Membership, MembershipFuture,
    MembershipSource, MiddleTierCredential, Mri, PartialReason, Policy, Request, Roster, Scope, Target, WriteKind,
    contacting, parse_event,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BASE: &str = "https://teams.microsoft.com/api/mt/part/emea-02";
const EVENT: &str = "AAMkADEventIdMadeUp_-=";
const LAB_A: &str = "bot-a@lab.example.com";
const LAB_B: &str = "bot-b@lab.example.com";
const OUTSIDER: &str = "someone@elsewhere.example.com";
const TOKEN: &str = "eyJ0eXAiOiJKV1QifQ.eyJzdWIiOiJtYWRlLXVwIn0.c2lnbmF0dXJlLW1hZGUtdXA";

fn create_url() -> String {
    format!("{BASE}/v2.0/me/calendars/default/events?isOnlineMeeting=true&shouldDecryptData=true")
}

fn event_url(tail: &str) -> String {
    let id: String = url::form_urlencoded::byte_serialize(EVENT.as_bytes()).collect();
    format!("{BASE}/v2.0/me/calendars/events/{id}{tail}")
}

fn event(attendees: &[&str]) -> Value {
    json!({
        "subject": "lab meeting",
        "startTime": "2026-10-30T15:00:00.000Z",
        "endTime": "2026-10-30T15:30:00.000Z",
        "isOnlineMeeting": true,
        "showAs": "Busy",
        "attendees": attendees
            .iter()
            .map(|address| json!({"address": address, "name": "Lab", "type": "Required"}))
            .collect::<Vec<_>>(),
    })
}

fn verdict(method: &str, url: &str, body: Option<&Value>) -> Contact {
    let bytes = body.map(|body| body.to_string().into_bytes());
    contacting(&Request::new(method, url).with_body(bytes.as_deref()), &Policy::new())
}

fn meeting(scope: Scope, attendees: &[&str]) -> Contact {
    Contact::Write(
        Target::new(WriteKind::Meeting, scope).with_added(attendees.iter().map(|address| Mri::mail(address))),
    )
}

fn on_event() -> Scope {
    Scope::Event { id: EVENT.to_owned() }
}

#[test]
fn a_new_event_is_a_meeting_inviting_its_attendees_by_address() {
    let body = json!({"calendarEvent": event(&[LAB_B]), "schedulingServiceEvent": {"meetingUrl": "x"}});
    assert_eq!(
        verdict("POST", &create_url(), Some(&body)),
        meeting(Scope::New, &[LAB_B])
    );
    let mixed_case = json!({"calendarEvent": event(&["Bot-B@Lab.Example.com"])});
    assert_eq!(
        verdict("POST", &create_url(), Some(&mixed_case)),
        meeting(Scope::New, &[LAB_B])
    );
    let appointment = json!({"calendarEvent": event(&[])});
    assert_eq!(
        verdict("POST", &create_url(), Some(&appointment)),
        meeting(Scope::New, &[])
    );
    // the web client lists the organizer, the account itself, with no address
    let mut with_organizer = event(&[LAB_B]);
    with_organizer["attendees"]
        .as_array_mut()
        .map(|list| list.push(json!({"role": "Admin", "type": "Organizer"})));
    let body = json!({"calendarEvent": with_organizer});
    assert_eq!(
        verdict("POST", &create_url(), Some(&body)),
        meeting(Scope::New, &[LAB_B])
    );
    let mut nameless_required = event(&[]);
    nameless_required["attendees"] = json!([{"role": "Admin", "type": "Required"}]);
    let body = json!({"calendarEvent": nameless_required});
    assert_eq!(verdict("POST", &create_url(), Some(&body)), Contact::Unreadable);
}

#[test]
fn the_scheduling_form_s_free_busy_reads_reach_nobody() {
    for read in ["findMeetingTimes", "schedules", "attendeeWorkHours"] {
        let url = format!("{BASE}/v2.0/me/calendars/{read}");
        let body = json!({"attendees": [{"address": OUTSIDER}]});
        assert_eq!(verdict("PUT", &url, Some(&body)), Contact::None, "{read}");
    }
    let other = format!("{BASE}/v2.0/me/calendars/somethingElse");
    assert_eq!(verdict("PUT", &other, Some(&json!({}))), Contact::Unreadable);
}

#[test]
fn a_new_event_off_its_shape_is_unreadable() {
    let mut cases: Vec<(String, Value)> = Vec::new();
    let with = |key: &str, value: Value| {
        let mut changed = event(&[LAB_B]);
        changed[key] = value;
        json!({"calendarEvent": changed})
    };
    cases.push((create_url(), with("inviteAllMembers", json!(true))));
    cases.push((
        create_url(),
        with("originalGroupChatThreadId", json!("19:chat@thread.v2")),
    ));
    cases.push((create_url(), with("attendees", json!([{"name": "no address"}]))));
    cases.push((
        create_url(),
        with("attendees", json!([{"address": "a@b.example.com, c@d.example.com"}])),
    ));
    cases.push((create_url(), json!({"calendarEvent": event(&[LAB_B]), "extra": 1})));
    cases.push((
        create_url(),
        json!({"calendarEvent": event(&[LAB_B]), "CalendarEvent": {}}),
    ));
    cases.push((
        create_url(),
        json!({"calendarEvent": event(&[LAB_B]), "meetingOptions": [{"name": "presenters", "selectedPeople": [{"upn": OUTSIDER}]}]}),
    ));
    cases.push((
        format!("{}&more=1", create_url()),
        json!({"calendarEvent": event(&[LAB_B])}),
    ));
    cases.push((
        format!("{BASE}/v2.0/me/calendars/default/events?isOnlineMeeting=maybe&shouldDecryptData=true"),
        json!({"calendarEvent": event(&[LAB_B])}),
    ));
    for (url, body) in cases {
        assert_eq!(verdict("POST", &url, Some(&body)), Contact::Unreadable, "{body}");
    }
}

#[test]
fn a_change_cancellation_or_delete_is_a_meeting_on_the_event_it_names() {
    let change = json!({"updatedCalendarEvent": event(&[LAB_B, LAB_A])});
    assert_eq!(
        verdict("PUT", &event_url("?isOnlineMeeting=true"), Some(&change)),
        meeting(on_event(), &[LAB_B, LAB_A])
    );
    let cancel = json!({"conversationId": "19:meeting_x@thread.v2", "cancellationMessage": "sorry"});
    assert_eq!(
        verdict("PUT", &event_url("/cancel"), Some(&cancel)),
        meeting(on_event(), &[])
    );
    assert_eq!(
        verdict("PUT", &event_url("/cancel"), Some(&json!({}))),
        meeting(on_event(), &[])
    );
    assert_eq!(verdict("DELETE", &event_url(""), None), meeting(on_event(), &[]));
}

#[test]
fn a_change_cancellation_or_delete_off_its_shape_is_unreadable() {
    let mut other = event(&[LAB_B]);
    other["objectId"] = json!("AnotherEvent");
    let cases = [
        (
            "PUT",
            event_url("?isOnlineMeeting=true"),
            Some(json!({"updatedCalendarEvent": other})),
        ),
        (
            "PUT",
            event_url(""),
            Some(json!({"updatedCalendarEvent": event(&[LAB_B])})),
        ),
        ("PUT", event_url("/cancel"), Some(json!({"cancellationMessage": 3}))),
        ("PUT", event_url("/cancel"), Some(json!({"webinarId": "w"}))),
        ("PUT", event_url("/cancel?x=1"), Some(json!({}))),
        ("DELETE", event_url(""), Some(json!({"x": 1}))),
        ("POST", event_url("/respond"), Some(json!({"responseType": "Accepted"}))),
        ("POST", event_url("/forward?useICalUId=false"), Some(json!({}))),
    ];
    for (method, url, body) in cases {
        assert_eq!(
            verdict(method, &url, body.as_ref()),
            Contact::Unreadable,
            "{method} {url}"
        );
    }
}

#[test]
fn the_empty_online_meeting_reaches_nobody_unless_it_names_a_chat() {
    let url = format!("{BASE}/beta/me/calendarEvents/privateMeeting/schedulingService/create");
    let body = json!({"isStreamEnabled": false, "meetingType": "Scheduled", "subject": "lab meeting",
        "startTime": "2026-10-30T15:00:00.000Z", "endTime": "2026-10-30T15:30:00.000Z"});
    assert_eq!(verdict("POST", &url, Some(&body)), Contact::None);
    let mut in_chat = body.clone();
    in_chat["threadId"] = json!("19:chat@thread.v2");
    assert_eq!(verdict("POST", &url, Some(&in_chat)), Contact::Unreadable);
    let mut extra = body;
    extra["attendees"] = json!([]);
    assert_eq!(verdict("POST", &url, Some(&extra)), Contact::Unreadable);
}

/// A source that answers every read with one membership.
struct Fixed(Membership);

impl MembershipSource for Fixed {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        let membership = self.0.clone();
        Box::pin(async move { membership })
    }
}

fn fence() -> Fence {
    let roster = Roster::new([Mri::new("8:orgid:lab-a"), Mri::new("8:orgid:lab-b")], []).with_lab_mail([LAB_A, LAB_B]);
    Fence::new(roster, FenceLog::from_writer(std::io::sink()))
}

fn target(scope: Scope, attendees: &[&str]) -> Target {
    Target::new(WriteKind::Meeting, scope).with_added(attendees.iter().map(|address| Mri::mail(address)))
}

#[tokio::test]
async fn a_meeting_passes_only_when_every_attendee_is_a_lab_account() {
    let fence = fence();
    let none = Fixed(Membership::Complete(Vec::new()));
    assert!(fence.check(target(Scope::New, &[LAB_B]), &none).await.is_ok());
    assert!(
        fence.check(target(Scope::New, &[]), &none).await.is_ok(),
        "an appointment"
    );
    let denied = fence.check(target(Scope::New, &[LAB_B, OUTSIDER]), &none).await;
    assert!(matches!(
        denied.map_err(|denied| *denied.reason()),
        Err(DenyReason::Outsiders { count: 1 })
    ));

    let lab_event = Fixed(Membership::Complete(vec![Mri::mail(LAB_A), Mri::mail(LAB_B)]));
    assert!(fence.check(target(on_event(), &[]), &lab_event).await.is_ok());
    assert!(fence.check(target(on_event(), &[OUTSIDER]), &lab_event).await.is_err());
    let outsider_event = Fixed(Membership::Complete(vec![Mri::mail(LAB_A), Mri::mail(OUTSIDER)]));
    assert!(fence.check(target(on_event(), &[]), &outsider_event).await.is_err());
    let unread = Fixed(Membership::Partial(PartialReason::NoCredential));
    assert!(fence.check(target(on_event(), &[]), &unread).await.is_err());
}

#[test]
fn an_event_s_members_are_its_organizer_and_attendees_when_the_answer_is_that_event() {
    let answer = json!({"objectId": EVENT, "organizerAddress": LAB_A,
        "attendees": [{"address": LAB_B, "type": "Required"}, {"emailAddress": {"address": LAB_A}}]});
    assert_eq!(
        parse_event(EVENT, &answer),
        Membership::Complete(vec![Mri::mail(LAB_A), Mri::mail(LAB_B)])
    );
    let malformed = Membership::Partial(PartialReason::FetchFailed(FetchFailure::Malformed));
    let mut other = answer.clone();
    other["objectId"] = json!("Another");
    assert_eq!(parse_event(EVENT, &other), malformed);
    let mut nameless = answer.clone();
    nameless["attendees"] = json!([{"name": "no address"}]);
    assert_eq!(parse_event(EVENT, &nameless), malformed);
    let mut no_list = answer;
    no_list.as_object_mut().map(|object| object.remove("attendees"));
    assert_eq!(parse_event(EVENT, &no_list), malformed);
}

#[test]
fn a_middle_tier_credential_comes_only_from_a_middle_tier_request_with_a_bearer_token() {
    let bearer = format!("Bearer {TOKEN}");
    let url = format!("{BASE}/v2.0/me/calendars/default/calendarView?x=1");
    assert!(MiddleTierCredential::from_request(&url, [("authorization", bearer.as_str())]).is_some());
    assert!(MiddleTierCredential::from_request(&url, [("x-skypetoken", TOKEN)]).is_none());
    let elsewhere = "https://evil.example.com/api/mt/part/emea-02/v2.0/me";
    assert!(MiddleTierCredential::from_request(elsewhere, [("Authorization", bearer.as_str())]).is_none());
    let chat = "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME";
    assert!(MiddleTierCredential::from_request(chat, [("Authorization", bearer.as_str())]).is_none());
    assert!(MiddleTierCredential::bearer(BASE, TOKEN).is_some());
    assert!(MiddleTierCredential::bearer(&format!("{BASE}/v2.0"), TOKEN).is_none());
    assert!(!format!("{:?}", MiddleTierCredential::bearer(BASE, TOKEN)).contains(TOKEN));
}

#[tokio::test]
async fn the_live_source_reads_an_event_from_the_middle_tier() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    let answer = json!({"objectId": EVENT, "organizerAddress": LAB_A, "attendees": [{"address": LAB_B}]});
    Mock::given(method("GET"))
        .and(path(format!("/api/mt/part/emea-02/v2.0/me/calendars/events/{EVENT}")))
        .and(query_param("shouldDecryptData", "true"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer))
        .mount(&server)
        .await;
    let live = LiveMembershipSource::with_mock_origin(&server.uri())?;
    assert_eq!(
        live.members(&on_event()).await,
        Membership::Partial(PartialReason::NoCredential)
    );
    live.set_middle_tier_credential(MiddleTierCredential::bearer(BASE, TOKEN).ok_or("no credential")?);
    assert_eq!(
        live.members(&on_event()).await,
        Membership::Complete(vec![Mri::mail(LAB_A), Mri::mail(LAB_B)])
    );
    assert!(live.reads_in_full(&on_event()));
    Ok(())
}
