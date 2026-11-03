//! Presence subscriptions against synthetic mocks: typed plain and `PostRead` requests, captured wire shape and registered endpoint ownership.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;

use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::Mri;
use teams_proto::{
    AnswerError, Client, Credentials, EndpointRegistration, HttpTransport, PRESENCE_PUSH_PATH, PostRead,
    PresenceSubscription, RegisteredEndpoint, Request as _, SubscriptionStatus, register,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const LAB_B: &str = "8:orgid:lab-b";
const LAB_C: &str = "8:orgid:lab-c";
const COWORKER: &str = "8:orgid:coworker";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const PRESENCE_BASE: &str = "https://teams.microsoft.com/ups/noam";
const REGISTRAR: &str = "/registrar/prod/V2/registrations";
const EPID: &str = "00000000-0000-4000-8000-0000000000e1";
const PUSH_URL: &str = "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000A/";
const FIXTURE: &str = include_str!("../../../tests/fixtures/ups/subscription.json");
/// The headers HTTP itself adds.
const TRANSPORT_HEADERS: [&str; 3] = ["host", "content-length", "accept-encoding"];

fn subscriptions_path() -> String {
    format!("/ups/noam/v1/pubsub/subscriptions/{EPID}")
}

fn mris(texts: &[&str]) -> Vec<Mri> {
    texts.iter().copied().map(Mri::new).collect()
}

/// The fixture with its placeholders filled in.
fn fixture() -> Value {
    serde_json::from_str(
        &FIXTURE
            .replace("<guid:a>", "lab-a")
            .replace("<guid:b>", "lab-b")
            .replace("<guid:c>", "lab-c")
            .replace("<endpoint>", EPID),
    )
    .unwrap()
}

struct FakeTokens;

impl Credentials for FakeTokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("placeholder-presence-token")))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

struct Setup {
    server: MockServer,
    client: Client<HttpTransport<FakeTokens>>,
}

/// The lab a's transport (the lab: a, b and c) to wiremock, with the presence base.
async fn setup() -> Setup {
    let server = MockServer::start().await;
    let transport = HttpTransport::with_mock_origin(Arc::new(FakeTokens), CHAT_BASE, &server.uri())
        .unwrap()
        .with_presence(PRESENCE_BASE)
        .unwrap();
    Setup {
        server,
        client: Client::new(transport),
    }
}

impl Setup {
    /// Registers [`EPID`] at [`PUSH_URL`], the registrar taking it, and gives the endpoint it registered.
    async fn registered(&self) -> RegisteredEndpoint {
        let mock = Mock::given(method("POST"))
            .and(path(REGISTRAR))
            .respond_with(ResponseTemplate::new(202))
            .up_to_n_times(1)
            .mount_as_scoped(&self.server)
            .await;
        let registration = EndpointRegistration::messaging(EPID, PUSH_URL).unwrap();
        let (_, endpoint) = register(self.client.transport(), &registration).await.unwrap();
        drop(mock);
        endpoint.expect("the registrar took it")
    }

    async fn subscriptions(&self) -> Vec<Request> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|request| request.url.path().contains("/pubsub/"))
            .collect()
    }
}

/// The subscription a fixture's request asks for.
fn subscription_of(endpoint: &RegisteredEndpoint, request: &Value) -> PresenceSubscription {
    let names = |key: &str| -> Vec<Mri> {
        request[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| Mri::new(item["mri"].as_str().unwrap()))
            .collect()
    };
    PresenceSubscription::new(
        endpoint,
        PUSH_URL,
        request["shouldPurgePreviousSubscriptions"].as_bool().unwrap(),
        names("subscriptionsToAdd"),
        names("subscriptionsToRemove"),
    )
    .unwrap()
}

/// Every header name of `request` but those HTTP itself adds, lower-cased and sorted.
fn header_names(request: &Request) -> Vec<String> {
    let mut names: Vec<String> = request
        .headers
        .keys()
        .map(|name| name.as_str().to_ascii_lowercase())
        .filter(|name| !TRANSPORT_HEADERS.contains(&name.as_str()))
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_subscription_goes_out_in_its_captured_shape_as_a_plain_request() {
    let fixture = fixture();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let setup = setup().await;
        let endpoint = setup.registered().await;
        Mock::given(method("POST"))
            .and(path(subscriptions_path()))
            .respond_with(ResponseTemplate::new(200).set_body_raw(case["answer"].to_string(), "application/json"))
            .mount(&setup.server)
            .await;
        let subscription = subscription_of(&endpoint, &case["request"]);

        let answer = setup.client.lookup(subscription.clone()).await.unwrap();
        let statuses = subscription.read_answer(answer.body()).unwrap();
        assert!(statuses.iter().all(|status| status.taken), "{name}: {statuses:?}");
        assert_eq!(statuses.len(), subscription.names().len(), "{name}");

        let sent = setup.subscriptions().await;
        assert_eq!(sent.len(), 1, "{name}");
        let request = &sent[0];
        // no trailing slash, no query: as every captured subscription
        assert_eq!(request.url.path(), subscriptions_path(), "{name}");
        assert_eq!(request.url.query(), None, "{name}");
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body, case["request"], "{name}: the captured body, key for key");
        assert_eq!(
            header_names(request),
            [
                "accept",
                "authorization",
                "behavioroverride",
                "content-type",
                "x-ms-client-type",
                "x-ms-client-user-agent",
                "x-ms-object-id"
            ],
            "{name}: the presence read's headers, no endpoint header, caller, session or cookie"
        );
    }
}

#[tokio::test]
async fn as_a_typed_post_read_it_goes_once_for_its_exact_mris() {
    let setup = setup().await;
    let endpoint = setup.registered().await;
    Mock::given(method("POST"))
        .and(path(subscriptions_path()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"addStatuses":[{"mri":"8:orgid:lab-b","source":"ups","status":20200}],"removeStatuses":[]}"#,
            "application/json",
        ))
        .mount(&setup.server)
        .await;
    let subscription = PresenceSubscription::new(&endpoint, PUSH_URL, true, mris(&[LAB_B]), []).unwrap();

    let answer = setup.client.post_read(subscription.clone()).await.unwrap();
    assert_eq!(
        subscription.read_answer(answer.body()).unwrap(),
        [SubscriptionStatus {
            mri: Mri::new(LAB_B),
            removed: false,
            taken: true
        }]
    );
    let sent = setup.subscriptions().await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url.path(), subscriptions_path());
}

#[tokio::test]
async fn a_subscription_names_someone_once_never_a_bot_or_a_stream_and_only_at_a_push_url() {
    let setup = setup().await;
    let endpoint = setup.registered().await;
    let make = |push_url: &str, add: &[&str], remove: &[&str]| {
        PresenceSubscription::new(&endpoint, push_url, false, mris(add), mris(remove))
    };
    assert!(make(PUSH_URL, &[LAB_B], &[LAB_C]).is_some());
    assert!(
        make(PUSH_URL, &[COWORKER], &[]).is_some(),
        "a typed subscription may name any account"
    );
    assert!(make(PUSH_URL, &[], &[]).is_none(), "naming nobody");
    assert!(make(PUSH_URL, &[LAB_B], &[LAB_B]).is_none(), "naming someone twice");
    assert!(
        make(PUSH_URL, &[LAB_B, "28:00000000-0000-4000-8000-0000000000ec"], &[]).is_none(),
        "a bot"
    );
    assert!(make(PUSH_URL, &["48:notes"], &[]).is_none(), "a stream");
    for push_url in [
        "https://example.com/v4/f/SyntheticEndpoint00000A/",
        "http://pub-ent-zz-01-f.trouter.teams.microsoft.com/v4/f/SyntheticEndpoint00000A/",
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000A/unifiedPresenceService",
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000A/?x=1",
    ] {
        assert!(make(push_url, &[LAB_B], &[]).is_none(), "{push_url}");
    }
    let subscription = make(PUSH_URL, &[LAB_B], &[LAB_C]).unwrap();
    assert_eq!(subscription.added(), mris(&[LAB_B]));
    assert_eq!(subscription.removed(), mris(&[LAB_C]));
    assert_eq!(subscription.names(), mris(&[LAB_B, LAB_C]));
    assert_eq!(
        subscription.body().unwrap()["trouterUri"],
        format!("{PUSH_URL}{PRESENCE_PUSH_PATH}")
    );
}

#[tokio::test]
async fn an_answer_naming_anyone_not_asked_is_dropped_and_another_status_is_not_taken() {
    let setup = setup().await;
    let endpoint = setup.registered().await;
    let subscription = PresenceSubscription::new(&endpoint, PUSH_URL, false, mris(&[LAB_B, LAB_C]), []).unwrap();
    let unasked = r#"{"addStatuses":[{"mri":"8:orgid:lab-b","source":"ups","status":20200},
        {"mri":"8:orgid:coworker","source":"ups","status":20200}],"removeStatuses":[]}"#;
    assert_eq!(
        subscription.read_answer(unasked.as_bytes()),
        Err(AnswerError::Unasked { count: 1 })
    );
    // a removal's status for someone added isn't one asked for either
    let misplaced = r#"{"addStatuses":[],"removeStatuses":[{"mri":"8:orgid:lab-b","status":20200}]}"#;
    assert_eq!(
        subscription.read_answer(misplaced.as_bytes()),
        Err(AnswerError::Unasked { count: 1 })
    );
    for malformed in [
        "not json",
        "[]",
        r#"{"addStatuses":[]}"#,
        r#"{"addStatuses":[{"status":20200}],"removeStatuses":[]}"#,
    ] {
        assert_eq!(
            subscription.read_answer(malformed.as_bytes()),
            Err(AnswerError::Malformed),
            "{malformed}"
        );
    }
    // the web client's polling only (30010), an error, a status as text: none taken; someone left out isn't either
    let mixed = r#"{"addStatuses":[{"mri":"8:orgid:lab-b","source":"ups","status":30010},
        {"mri":"8:orgid:lab-c","source":"ups","status":"20200"}],"removeStatuses":[]}"#;
    let statuses = subscription.read_answer(mixed.as_bytes()).unwrap();
    assert!(statuses.iter().all(|status| !status.taken), "{statuses:?}");
}
