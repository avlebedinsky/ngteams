//! A person's profile card through a running engine, against wiremock as Loki, UPS and the middle tier (the transport's
//! `test-support` mock origin): Loki's configuration read once and its region handed to the transport, every read sent
//! at once and the card made of them, a section whose read failed left out, names and addresses from the short profile
//! when Loki gives none, and the card refused when nothing could be read or the MRI isn't a directory user's.
//!
//! Tokens, ids, names and addresses here are made up; Loki's answers follow `tests/fixtures/loki/persona-card.json`.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineError, EngineHandle, EngineOptions, Parts};
use teams_core::{Availability, Outbox, ProfileCard};
use teams_proto::{Author, Client, ClockTime, Credentials, FreeBusyStatus, HttpTransport, Mri};
use teams_store::Store;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const A: &str = "0000000a-0000-4000-8000-00000000000a";
const B: &str = "0000000b-0000-4000-8000-00000000000b";
const C: &str = "0000000c-0000-4000-8000-00000000000c";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const PRESENCE_BASE: &str = "https://teams.microsoft.com/ups/noam";
const MIDDLE_TIER_BASE: &str = "https://teams.microsoft.com/api/mt/part/amer-03";
const FIXTURE: &str = include_str!("../../../tests/fixtures/loki/persona-card.json");
/// How long a test waits for a card.
const WAIT: Duration = Duration::from_secs(30);

fn mri(guid: &str) -> String {
    format!("8:orgid:{guid}")
}

/// The fixture, b the person asked about and c someone b works with.
fn fixture() -> Value {
    serde_json::from_str(&FIXTURE.replace("<guid:a>", B).replace("<guid:c>", C)).unwrap()
}

/// Placeholder tokens for every audience.
struct FakeTokens;

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
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

/// An engine of lab account a, its reads on `server` with the presence and middle-tier bases, and Loki when `loki`.
fn rig(server: MockServer, loki: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let tokens = Arc::new(FakeTokens);
    let transport = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    let reads = transport()
        .with_presence(PRESENCE_BASE)
        .unwrap()
        .with_middle_tier(MIDDLE_TIER_BASE)
        .unwrap();
    let reads = if loki { reads.with_loki() } else { reads };
    let outbox = Outbox::new(Arc::new(transport()), Author::new(Mri::new(mri(A)), "lab-a"));
    let handle = engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("store").join("a.sqlite")).unwrap(),
        client: Arc::new(Client::new(reads)),
        outbox,
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap();
    Rig {
        server,
        handle,
        _dir: dir,
    }
}

impl Rig {
    async fn card(&self, mri: &str) -> Result<ProfileCard, EngineError> {
        tokio::time::timeout(WAIT, self.handle.profile_card(mri))
            .await
            .expect("no card in time")
    }

    async fn requests_to(&self, path_is: &str) -> Vec<Request> {
        let requests = self.server.received_requests().await.unwrap();
        requests
            .into_iter()
            .filter(|request| request.url.path() == path_is)
            .collect()
    }
}

/// Serves `read`'s fixture answer for b.
async fn serve_loki(server: &MockServer, read: &str) {
    let fixture = fixture();
    Mock::given(method("GET"))
        .and(path(fixture[read]["path"].as_str().unwrap()))
        .and(query_param("aadObjectId", B))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fixture[read]["answer"]))
        .mount(server)
        .await;
}

/// Serves the configuration, naming the fixture's regional base.
async fn serve_configuration(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v1/livepersonacard/configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fixture()["configuration"]["answer"]))
        .mount(server)
        .await;
}

/// Serves b's presence: offline, last active at 2 s past the epoch, `out_of_office` as given.
async fn serve_presence(server: &MockServer, out_of_office: Value) {
    let item = json!([{
        "mri": mri(B),
        "presence": {
            "calendarData": {"isOutOfOffice": out_of_office},
            "availability": "Offline",
            "activity": "Offline",
            "lastActiveTime": "1970-01-01T00:00:02.000Z",
        },
        "status": 20000,
    }]);
    Mock::given(method("POST"))
        .and(path("/ups/noam/v1/presence/getpresence/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(item))
        .mount(server)
        .await;
}

/// Serves b's short profile, with a job title and an office.
async fn serve_short_profile(server: &MockServer) {
    let answer = json!({
        "type": "Microsoft.SkypeSpaces.MiddleTier.Models.IUserIdentity",
        "value": [{
            "mri": mri(B),
            "displayName": "Lab Bee (profile)",
            "givenName": "Bee",
            "email": "lab-b@lab.example.com",
            "userPrincipalName": "lab-b@lab.example.com",
            "userType": "Member",
            "jobTitle": "Made-up Tester",
            "officeLocation": "Made-up Office 1",
        }],
    });
    Mock::given(method("POST"))
        .and(path("/api/mt/part/amer-03/beta/users/fetchShortProfile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer))
        .mount(server)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_card_is_made_of_every_read_and_loki_is_configured_once() {
    let server = MockServer::start().await;
    serve_configuration(&server).await;
    for read in ["person", "organization", "workingwith", "schedule", "oofstatus"] {
        serve_loki(&server, read).await;
    }
    serve_presence(&server, json!(false)).await;
    serve_short_profile(&server).await;
    let rig = rig(server, true);

    let card = rig.card(&mri(B)).await.unwrap();
    assert_eq!(card.mri, mri(B));
    assert_eq!(
        card.name.as_deref(),
        Some("Lab Account A"),
        "Loki's name before the profile's"
    );
    assert_eq!(card.given_name.as_deref(), Some("Lab"));
    assert_eq!(card.emails, ["lab-a@lab.example.com"]);
    assert_eq!(card.im_addresses, ["lab-a@lab.example.com"]);
    assert_eq!(card.user_type.as_deref(), Some("Member"));
    assert_eq!(card.job_title.as_deref(), Some("Made-up Tester"));
    assert_eq!(card.office.as_deref(), Some("Made-up Office 1"));
    let presence = card.presence.clone().unwrap();
    assert_eq!(presence.availability, Availability::Offline);
    assert_eq!(presence.last_active_ms, Some(2000));
    assert_eq!(card.out_of_office, Some(false));
    let schedule = card.schedule.clone().unwrap();
    assert_eq!(schedule.current, Some(FreeBusyStatus::Free));
    assert_eq!(schedule.work_start, Some(ClockTime { hour: 8, minute: 0 }));
    assert_eq!(schedule.utc_offset_minutes, Some(180));
    assert_eq!(card.organization.clone().unwrap().managers, []);
    let working = card.working_with.clone().unwrap();
    assert_eq!(working.len(), 1);
    assert_eq!(working[0].mri, mri(C));
    assert_eq!(working[0].name.as_deref(), Some("Lab Account C"));
    assert_eq!(working[0].email.as_deref(), Some("lab-c@lab.example.com"));
    let shown = format!("{card:?}");
    assert!(!shown.contains("Lab Account") && !shown.contains("lab-a@"), "{shown}");
    assert!(!format!("{:?}", working[0]).contains("Lab Account"));

    // a second card reads no configuration again
    rig.card(&mri(B)).await.unwrap();
    assert_eq!(rig.requests_to("/api/v1/livepersonacard/configuration").await.len(), 1);
    let person = rig.requests_to("/api/v2/person").await;
    assert_eq!(person.len(), 2);
    let token = person[0].headers.get("authorization").unwrap().to_str().unwrap();
    assert_eq!(token, "Bearer placeholder-token-for-https://loki.delve.office.com");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_section_is_left_out_and_the_profile_stands_in_for_loki_s_names() {
    let server = MockServer::start().await;
    serve_configuration(&server).await;
    serve_loki(&server, "oofstatus").await;
    for read in ["/api/v2/person", "/api/v1/schedule"] {
        Mock::given(path(read))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
    }
    Mock::given(path("/api/v1/organization"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    serve_short_profile(&server).await;
    // no presence: UPS answers nothing the app reads
    let rig = rig(server, true);

    let card = rig.card(&mri(B)).await.unwrap();
    assert_eq!(card.name.as_deref(), Some("Lab Bee (profile)"));
    assert_eq!(card.given_name.as_deref(), Some("Bee"));
    assert_eq!(card.emails, ["lab-b@lab.example.com"]);
    assert!(card.im_addresses.is_empty());
    assert_eq!(card.user_principal_name.as_deref(), Some("lab-b@lab.example.com"));
    assert_eq!(card.schedule, None);
    assert_eq!(card.organization, None);
    assert_eq!(card.working_with, None);
    assert_eq!(card.presence, None);
    assert_eq!(card.out_of_office, Some(false), "Loki's automatic replies off");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_loki_the_card_is_presence_and_profile_and_the_configuration_is_read_again_after_a_failure() {
    let server = MockServer::start().await;
    serve_presence(&server, json!(true)).await;
    let rig = rig(server, false);
    let card = rig.card(&mri(B)).await.unwrap();
    assert_eq!(card.out_of_office, Some(true));
    assert_eq!(card.name, None);
    assert!(
        rig.requests_to("/api/v1/livepersonacard/configuration")
            .await
            .is_empty()
    );

    let server = MockServer::start().await;
    Mock::given(path("/api/v1/livepersonacard/configuration"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    serve_presence(&server, json!(false)).await;
    let rig = self::rig(server, true);
    rig.card(&mri(B)).await.unwrap();
    rig.card(&mri(B)).await.unwrap();
    // each card tried once more, after the transport's one retry of the throttled read
    assert_eq!(rig.requests_to("/api/v1/livepersonacard/configuration").await.len(), 4);
    assert!(
        rig.requests_to("/api/v2/person").await.is_empty(),
        "no region, no person read"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_read_or_no_directory_user_is_no_card() {
    let server = MockServer::start().await;
    let rig = rig(server, true);
    assert!(matches!(rig.card(&mri(B)).await, Err(EngineError::ProfileUnread)));
    assert_eq!(EngineError::ProfileUnread.kind(), "profile unread");
    assert!(teams_core::profile_card::has_card(&mri(B)));
    assert!(!teams_core::profile_card::has_card(
        "28:00000000-0000-4000-8000-0000000000ec"
    ));

    let before = rig.server.received_requests().await.unwrap().len();
    for other in ["28:00000000-0000-4000-8000-0000000000ec", "8:orgid:not-a-guid"] {
        assert!(matches!(rig.card(other).await, Err(EngineError::ProfileUnread)));
    }
    assert_eq!(
        rig.server.received_requests().await.unwrap().len(),
        before,
        "nothing sent"
    );
}
