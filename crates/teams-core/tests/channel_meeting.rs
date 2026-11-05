//! A meeting's chat and a channel's meeting through a running engine (`teams_core::calendar`), with synthetic answers on
//! loopback: the meeting a chat's thread names; the meeting running in a channel, from CSA's `activeMeetups` or else the
//! channel's newest posts; a channel's meeting read from its team's group calendar as the account sees it; and a Meet
//! now's post titled once the account's own meeting is posted. No tenant, real account, token or capture is involved.

#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::{
    CallAction, CallActionError, EngineError, EventDraft, EventShowAs, EventWrite, Organizer, Outbox, ZonedTime,
};
use teams_mock::{CHAT_BASE, CSA_BASE, LAB_A, LAB_B, MIDDLE_TIER_BASE};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, HttpTransport, MeetingChannel, MeetingInfo, ScheduledMeeting};
use teams_store::Store;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CHANNEL: &str = "19:channel0001@thread.tacv2";
const CHAT: &str = "19:meeting_SyntheticMeeting@thread.v2";
const CSA_TEAMS: &str = "/api/csa/amer/api/v3/teams/users/me";
const PROFILES: &str = "/api/mt/part/amer-03/beta/users/fetchShortProfile";
const ZONE: &str = "/api/mt/part/amer-03/beta/me/calendarEvents/timeZoneSettingsWithOffset";
const MAILBOX: &str = "made-up-team@lab.example.com";
const ICAL: &str = "0400000082E00074C5B7101A82E00800000000ABCDEF";
const MAIL_A: &str = "lab-a@lab.example.com";
const MAIL_B: &str = "lab-b@lab.example.com";
const ORGANIZER: &str = "00000000-0000-4000-8000-00000000000a";
const TENANT: &str = "00000000-0000-4000-8000-0000000000d0";
const CONVERSATION: &str =
    "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticMeetup?i=1";

struct Tokens;

impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("synthetic-meeting-token")))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

struct Rig {
    server: MockServer,
    handle: EngineHandle,
    _dir: tempfile::TempDir,
}

async fn running_engine() -> Rig {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let tokens = Arc::new(Tokens);
    let transport = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri())
            .unwrap()
            .with_middle_tier(MIDDLE_TIER_BASE)
            .unwrap()
            .with_csa(CSA_BASE)
            .unwrap()
    };
    let outbox = Outbox::new(Arc::new(transport()), Author::new(Mri::new(LAB_A), "Lab A"));
    let handle = engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("meeting.sqlite")).unwrap(),
        client: Arc::new(Client::new(transport())),
        outbox,
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap();
    Rig {
        server,
        handle,
        _dir: dir,
    }
}

async fn answer(server: &MockServer, verb: &str, endpoint: &str, status: u16, body: Value) {
    Mock::given(method(verb))
        .and(path(endpoint))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

async fn identity(server: &MockServer) {
    answer(
        server,
        "POST",
        PROFILES,
        200,
        json!({"type": "Microsoft.SkypeSpaces.MiddleTier.Models.IUserIdentity",
               "value": [{"mri": LAB_A, "displayName": "Lab A", "email": MAIL_A}]}),
    )
    .await;
    answer(server, "GET", ZONE, 200, json!({"timeZone": "Utc"})).await;
}

/// CSA's teams answer: the channel, with `meetups` running in it.
fn csa(meetups: &Value) -> Value {
    json!({
        "_synthetic": "made-up CSA teams answer",
        "teams": [{
            "id": CHANNEL, "displayName": "Made-up Team",
            "channels": [{"id": CHANNEL, "displayName": "General", "isGeneral": true, "activeMeetups": meetups}],
        }],
        "chats": [],
        "metadata": {"isPartialData": false},
    })
}

fn meetup() -> Value {
    json!({
        "status": "Active",
        "messageId": "1700000000000",
        "conversationUrl": CONVERSATION,
        "meetingInfo": {"organizerId": ORGANIZER, "tenantId": TENANT},
        "organizerId": ORGANIZER,
        "tenantId": TENANT,
    })
}

/// A channel's call post (`Event/Call`), in the shape a Meet now posts.
fn call_post(id: &str, kind: &str) -> Value {
    let meeting = json!({"@type": "http://schema.skype.com/ScheduledMeetingCreated", "meetingtitle": null,
        "tenantId": TENANT, "organizerId": ORGANIZER})
    .to_string();
    json!({
        "id": id,
        "messagetype": "Event/Call",
        "conversationid": CHANNEL,
        "content": format!("<partlist alt =\"\"></partlist><callEventType>{kind}</callEventType>\n<conversationUrl>{CONVERSATION}</conversationUrl>\n<callId>call-1</callId>\n"),
        "properties": {"meeting": meeting},
    })
}

async fn posts(server: &MockServer, messages: Vec<Value>) {
    answer(
        server,
        "GET",
        &format!("/api/chatsvc/amer/v1/users/ME/conversations/{CHANNEL}/messages"),
        200,
        json!({"messages": messages, "_metadata": {}}),
    )
    .await;
}

async fn requests(server: &MockServer, verb: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.method.as_str() == verb)
        .collect()
}

#[tokio::test]
async fn a_meeting_s_chat_names_its_meeting_and_a_failed_read_is_told() {
    let rig = running_engine().await;
    let thread = format!("/api/chatsvc/amer/v1/threads/{CHAT}");
    let meeting = json!({"organizerId": ORGANIZER, "tenantId": TENANT, "subject": "Made-up sync",
                         "meetingJoinUrl": "https://teams.microsoft.com/l/meetup-join/made-up"})
    .to_string();
    Mock::given(method("GET"))
        .and(path(thread.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"properties": {"meeting": meeting}})))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&rig.server)
        .await;
    let details = rig.handle.requests().meeting(CHAT).await.unwrap().expect("its meeting");
    assert_eq!(details.title.as_deref(), Some("Made-up sync"));
    assert_eq!(details.meeting.organizer_id, ORGANIZER);
    answer(&rig.server, "GET", &thread, 200, json!({"properties": {}})).await;
    assert_eq!(rig.handle.requests().meeting(CHAT).await.unwrap(), None);
    let failed = rig.handle.requests().meeting("19:missing@thread.v2").await.unwrap_err();
    assert!(matches!(failed, EngineError::MeetingUnread), "{failed:?}");
    assert_eq!(failed.kind(), "meeting unread");
}

#[tokio::test]
async fn a_channel_s_running_meeting_comes_from_csa_else_from_its_posts() {
    let rig = running_engine().await;
    let failed = rig.handle.requests().channel_meetup(CHANNEL).await.unwrap_err();
    assert!(matches!(failed, EngineError::MeetingUnread), "neither read: {failed:?}");

    posts(&rig.server, vec![call_post("1700000000002", "callStarted")]).await;
    let from_posts = rig
        .handle
        .requests()
        .channel_meetup(CHANNEL)
        .await
        .unwrap()
        .expect("from the posts while CSA doesn't read");
    assert_eq!(from_posts.message_id, "1700000000002");

    Mock::given(method("GET"))
        .and(path(CSA_TEAMS))
        .respond_with(ResponseTemplate::new(200).set_body_json(csa(&json!([meetup()]))))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&rig.server)
        .await;
    let from_csa = rig
        .handle
        .requests()
        .channel_meetup(CHANNEL)
        .await
        .unwrap()
        .expect("CSA names it");
    assert_eq!(from_csa.message_id, "1700000000000");
    assert_eq!(from_csa.meeting.organizer_id, ORGANIZER);
}

#[tokio::test]
async fn csa_naming_none_and_no_post_starting_one_is_no_meeting() {
    let rig = running_engine().await;
    answer(&rig.server, "GET", CSA_TEAMS, 200, csa(&json!([]))).await;
    assert_eq!(
        rig.handle.requests().channel_meetup(CHANNEL).await.unwrap(),
        None,
        "no posts read"
    );
    posts(&rig.server, vec![call_post("1700000000002", "callEnded")]).await;
    assert_eq!(rig.handle.requests().channel_meetup(CHANNEL).await.unwrap(), None);
}

fn channel_event(uid: &str) -> Value {
    let teams_data = json!({"cid": CHANNEL, "mid": "1700000000000", "uid": uid}).to_string();
    json!({
        "objectId": "MadeUpChannelEvent",
        "iCalUID": ICAL,
        "subject": "Made-up channel meeting",
        "startTime": "2026-11-01T16:00:00Z",
        "endTime": "2026-11-01T16:30:00Z",
        "eventType": "Single",
        "isOrganizer": true,
        "organizerAddress": MAILBOX,
        "skypeTeamsData": teams_data,
        "attendees": [{"address": MAIL_A}, {"address": MAIL_B}, {}],
        "bodyContent": "<p>Made-up notes</p>",
    })
}

#[tokio::test]
async fn a_channel_s_meeting_is_read_as_its_scheduler_sees_it() {
    let rig = running_engine().await;
    identity(&rig.server).await;
    let read = format!("/api/mt/part/amer-03/v2.0/groups/smtp:{MAILBOX}/calendars/events/iCalUId/{ICAL}");
    Mock::given(method("GET"))
        .and(path(read.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": channel_event(LAB_A)})))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&rig.server)
        .await;
    let full = rig.handle.requests().channel_event(MAILBOX, ICAL).await.unwrap();
    assert!(full.event.is_organizer, "the account scheduled it");
    let attendees: Vec<_> = full
        .event
        .attendees
        .iter()
        .map(|attendee| attendee.address.as_deref())
        .collect();
    assert!(
        !attendees.contains(&Some(MAIL_A)),
        "the account isn't among whom it invites"
    );
    assert!(attendees.contains(&Some(MAIL_B)));

    // scheduled by someone else: the account doesn't organize it, whatever the group's read says
    answer(&rig.server, "GET", &read, 200, channel_event(LAB_B)).await;
    let full = rig.handle.requests().channel_event(MAILBOX, ICAL).await.unwrap();
    assert!(!full.event.is_organizer);

    for (mailbox, uid) in [("not an address", ICAL), (MAILBOX, "not-hex")] {
        let refused = rig.handle.requests().channel_event(mailbox, uid).await.unwrap_err();
        assert!(matches!(refused, EngineError::CalendarUnread), "{refused:?}");
    }
}

#[tokio::test]
async fn a_channel_s_meeting_read_without_the_account_s_address_keeps_whom_it_invites() {
    let rig = running_engine().await;
    // a profile without an address, and no time zone read
    answer(
        &rig.server,
        "POST",
        PROFILES,
        200,
        json!({"type": "Microsoft.SkypeSpaces.MiddleTier.Models.IUserIdentity",
               "value": [{"mri": LAB_A, "displayName": "Lab A", "email": "not-an-address"}]}),
    )
    .await;
    let read = format!("/api/mt/part/amer-03/v2.0/groups/smtp:{MAILBOX}/calendars/events/iCalUId/{ICAL}");
    answer(&rig.server, "GET", &read, 200, channel_event(LAB_A)).await;
    let full = rig.handle.requests().channel_event(MAILBOX, ICAL).await.unwrap();
    assert!(full.event.is_organizer);
    let attendees: Vec<_> = full
        .event
        .attendees
        .iter()
        .map(|attendee| attendee.address.as_deref())
        .collect();
    assert_eq!(
        attendees,
        [Some(MAIL_A), Some(MAIL_B)],
        "the account's own address isn't known: nobody is left out"
    );
}

#[tokio::test]
async fn an_engine_that_makes_no_calls_refuses_every_call_action() {
    let rig = running_engine().await;
    let requests = rig.handle.requests();
    let own = LAB_A.trim_start_matches("8:orgid:");
    let other = LAB_B.trim_start_matches("8:orgid:");
    let meeting = MeetingInfo {
        organizer_id: ORGANIZER.to_owned(),
        tenant_id: TENANT.to_owned(),
    };
    for action in [
        CallAction::Call {
            chat: format!("19:{own}_{other}@unq.gbl.spaces"),
        },
        CallAction::TestCall {
            echo: Mri::new("28:echo-service"),
        },
        CallAction::Join { chat: CHAT.to_owned() },
        CallAction::JoinMeeting {
            chat: CHAT.to_owned(),
            meeting: meeting.clone(),
        },
        CallAction::JoinMeeting {
            chat: CHANNEL.to_owned(),
            meeting,
        },
        CallAction::MeetNow {
            channel: CHANNEL.to_owned(),
        },
        CallAction::JoinChannel {
            channel: CHANNEL.to_owned(),
        },
        // a chat the store doesn't hold: no calling is said before the chat is read, as no chat could be called
        // (an engine that calls refuses these as not a chat: `calls::failures`)
        CallAction::Call {
            chat: "19:unknown@thread.v2".to_owned(),
        },
        CallAction::Call {
            chat: "19:someone_else-entirely@unq.gbl.spaces".to_owned(),
        },
    ] {
        let refused = requests.call(action).await.unwrap_err();
        assert!(
            matches!(refused, EngineError::Call(CallActionError::NoCalling)),
            "{refused:?}"
        );
    }
    let call = call_id();
    for action in [
        CallAction::Answer { call },
        CallAction::Decline { call },
        CallAction::HangUp { call },
        CallAction::Mute { call, muted: true },
        CallAction::Renegotiate { call },
        CallAction::Camera { call, on: true },
        CallAction::Lobby {
            call,
            waiting: Mri::new(LAB_B),
            admit: true,
        },
    ] {
        let refused = requests.call(action).await.unwrap_err();
        assert!(
            matches!(refused, EngineError::Call(CallActionError::NoCalling)),
            "{refused:?}"
        );
    }
}

/// A call's id from a calling machine of its own, whose effects (a test call's start) are never sent.
fn call_id() -> teams_call::CallId {
    let mut machine = teams_call::Calls::new(teams_call::Identity {
        mri: Mri::new(LAB_A),
        display_name: Some("Lab A".to_owned()),
        endpoint_id: "00000000-0000-4000-8000-0000000000e1".to_owned(),
        language_id: None,
        tenant_id: teams_mock::TENANT.to_owned(),
        client: teams_proto::ClientInfo::default(),
        region: Some("amer".to_owned()),
        partition: None,
    });
    machine
        .set_push_url("https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/")
        .unwrap();
    machine
        .place(
            &teams_call::Placement::TestCall {
                echo: Mri::new(teams_mock::ECHO),
            },
            teams_proto::MediaContent::new("v=0\r\n", "application/sdp-ngc-1.0"),
            std::time::Instant::now(),
        )
        .unwrap()
}

#[tokio::test]
async fn a_channel_s_meeting_that_doesn_t_read_is_told() {
    let rig = running_engine().await;
    let failed = rig.handle.requests().channel_event(MAILBOX, ICAL).await.unwrap_err();
    assert!(matches!(failed, EngineError::CalendarUnread), "{failed:?}");
    let read = format!("/api/mt/part/amer-03/v2.0/groups/smtp:{MAILBOX}/calendars/events/iCalUId/{ICAL}");
    let mut other = channel_event(LAB_A);
    other["iCalUID"] = json!("ABCDEF");
    answer(&rig.server, "GET", &read, 200, other).await;
    let failed = rig.handle.requests().channel_event(MAILBOX, ICAL).await.unwrap_err();
    assert!(
        matches!(failed, EngineError::CalendarUnread),
        "another event: {failed:?}"
    );
}

#[tokio::test]
async fn a_meet_now_s_post_is_titled_once_the_account_s_meeting_is_posted() {
    let rig = running_engine().await;
    let now = chrono::Utc::now().timestamp_millis().to_string();
    posts(&rig.server, vec![call_post(&now, "callStarted")]).await;
    Mock::given(method("PUT"))
        .and(path_regex(r"/messages/\d+/properties$"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&rig.server)
        .await;
    rig.handle
        .requests()
        .title_meet_now(CHANNEL, "  Made-up stand-up ")
        .await
        .unwrap();
    let sent = requests(&rig.server, "PUT").await;
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].url.path(),
        format!("/api/chatsvc/amer/v1/users/ME/conversations/{CHANNEL}/messages/{now}/properties")
    );
    assert_eq!(sent[0].url.query(), Some("name=meeting"));
    let body: Value = serde_json::from_slice(&sent[0].body).unwrap();
    assert_eq!(body, json!({"meeting": {"meetingtitle": "Made-up stand-up"}}));
}

#[tokio::test]
async fn a_title_of_nothing_is_not_sent() {
    let rig = running_engine().await;
    let now = chrono::Utc::now().timestamp_millis().to_string();
    posts(&rig.server, vec![call_post(&now, "callStarted")]).await;
    let refused = rig.handle.requests().title_meet_now(CHANNEL, "   ").await.unwrap_err();
    assert!(
        matches!(refused, EngineError::EventNotSaved { kind: "title", .. }),
        "{refused:?}"
    );
    assert_eq!(refused.kind(), "event not saved");
    assert!(requests(&rig.server, "PUT").await.is_empty());
}

fn draft() -> EventDraft {
    EventDraft {
        subject: "Made-up channel meeting".to_owned(),
        start: ZonedTime {
            ms: 1_793_491_200_000,
            offset_minutes: 0,
        },
        end: ZonedTime {
            ms: 1_793_493_000_000,
            offset_minutes: 0,
        },
        time_zone: None,
        all_day: false,
        organizer: Organizer {
            mri: String::new(),
            address: None,
            name: None,
        },
        attendees: Vec::new(),
        location: String::new(),
        body_html: "<p>Made-up notes</p>".to_owned(),
        online: true,
        show_as: EventShowAs::Busy,
        reminder_minutes: Some(15),
        private: false,
        categories: Vec::new(),
        recurrence: None,
    }
}

fn channel(mailbox: &str) -> MeetingChannel {
    MeetingChannel {
        channel: CHANNEL.to_owned(),
        mailbox: mailbox.to_owned(),
    }
}

fn scheduled() -> ScheduledMeeting {
    ScheduledMeeting {
        title: "Made-up channel meeting".to_owned(),
        start_ms: 1_793_491_200_000,
        end_ms: 1_793_493_000_000,
        start_text: "2026-11-01T16:00:00.000Z".to_owned(),
        location: None,
        ical_uid: ICAL.to_owned(),
        event_type: "Single".to_owned(),
        organizer_id: Some(ORGANIZER.to_owned()),
        join_url: None,
        cancelled: false,
    }
}

async fn group_writes(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path().contains("/v2.0/groups/") && request.method.as_str() != "GET")
        .map(|request| request.method.to_string())
        .collect()
}

#[tokio::test]
async fn a_channel_s_meeting_is_created_changed_and_cancelled_on_its_team_s_calendar() {
    let rig = running_engine().await;
    identity(&rig.server).await;
    Mock::given(method("POST"))
        .and(path_regex(r"/v2\.0/groups/"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"value": [channel_event(LAB_A)]})))
        .mount(&rig.server)
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(r"/v2\.0/groups/"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&rig.server)
        .await;
    let written = rig
        .handle
        .write_event(EventWrite::CreateInChannel {
            draft: draft(),
            channel: channel(MAILBOX),
            personal_invites: true,
        })
        .await
        .unwrap();
    assert_eq!(
        written.created.map(|event| event.subject),
        Some("Made-up channel meeting".to_owned())
    );
    let event = channel_event(LAB_A).as_object().cloned().unwrap();
    rig.handle
        .write_event(EventWrite::UpdateInChannel {
            event,
            draft: draft(),
            channel: channel(MAILBOX),
        })
        .await
        .unwrap();
    rig.handle
        .write_event(EventWrite::CancelInChannel {
            meeting: Box::new(scheduled()),
            channel: channel(MAILBOX),
            post: "1700000000000".to_owned(),
            message: Some("Made-up reason".to_owned()),
        })
        .await
        .unwrap();
    let sent = group_writes(&rig.server).await;
    assert_eq!(sent.iter().filter(|verb| *verb == "POST").count(), 1, "{sent:?}");
    assert_eq!(sent.len(), 3, "{sent:?}");
}

#[tokio::test]
async fn a_channel_s_meeting_write_that_doesn_t_read_is_never_sent() {
    let rig = running_engine().await;
    identity(&rig.server).await;
    let refused = rig
        .handle
        .write_event(EventWrite::CreateInChannel {
            draft: draft(),
            channel: channel("not an address"),
            personal_invites: false,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(refused, EngineError::EventNotSaved { kind: "creation", .. }),
        "{refused:?}"
    );
    let mut elsewhere = channel_event(LAB_A).as_object().cloned().unwrap();
    elsewhere.insert(
        "skypeTeamsData".to_owned(),
        json!(json!({"cid": "19:other@thread.tacv2"}).to_string()),
    );
    let refused = rig
        .handle
        .write_event(EventWrite::UpdateInChannel {
            event: elsewhere,
            draft: draft(),
            channel: channel(MAILBOX),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(refused, EngineError::EventNotSaved { kind: "change", .. }),
        "{refused:?}"
    );
    let refused = rig
        .handle
        .write_event(EventWrite::CancelInChannel {
            meeting: Box::new(scheduled()),
            channel: channel(MAILBOX),
            post: "not-digits".to_owned(),
            message: None,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(
            refused,
            EngineError::EventNotSaved {
                kind: "cancellation",
                ..
            }
        ),
        "{refused:?}"
    );
    assert!(group_writes(&rig.server).await.is_empty(), "nothing sent");
}

#[tokio::test]
async fn a_resend_of_no_row_and_a_sync_the_service_refuses_are_told() {
    let rig = running_engine().await;
    let requests = rig.handle.requests();
    let missing = requests.resend(424_242).await;
    assert!(missing.is_err(), "no such row: {missing:?}");
    let synced = requests.sync_conversation(CHANNEL).await;
    assert!(synced.is_err(), "the service answers nothing: {synced:?}");
    let caught_up = requests.sync().await;
    assert!(caught_up.is_err(), "the conversations don't read: {caught_up:?}");
    assert!(group_writes(&rig.server).await.is_empty());
}
