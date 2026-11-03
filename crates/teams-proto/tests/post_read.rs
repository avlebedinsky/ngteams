//! Typed reads by POST against synthetic mocks: captured lookup shapes, service base validation, one send per request and answers limited to the accounts asked for.

use std::future::{self, Future};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::Mri;
use teams_proto::{
    AnswerError, BuildError, Client, Credentials, Endpoint, HttpError, HttpTransport, MIDDLE_TIER_AUDIENCE,
    ProbePostRead, Read, Refusal, Service, ShortProfile, ShortProfileLookup, Transport,
};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const LAB_A: &str = "8:orgid:lab-a";
const LAB_B: &str = "8:orgid:lab-b";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const MIDDLE_TIER: &str = "https://teams.microsoft.com/api/mt/part/amer-03";
const SHORT_PROFILE: &str = "/api/mt/part/amer-03/beta/users/fetchShortProfile";
const LOOKUP_QUERY: [(&str, &str); 6] = [
    ("isMailAddress", "false"),
    ("enableGuest", "true"),
    ("skypeTeamsInfo", "true"),
    ("canBeSmtpAddress", "false"),
    ("includeIBBarredUsers", "true"),
    ("includeDisabledAccounts", "true"),
];
const FIXTURE: &str = include_str!("../../../tests/fixtures/mt/short-profile.json");

fn token(generation: usize) -> String {
    format!("placeholder-token-generation-{generation}")
}

fn mris(texts: &[&str]) -> Vec<Mri> {
    texts.iter().copied().map(Mri::new).collect()
}

#[cfg(test)]
fn lookup(texts: &[&str]) -> ShortProfileLookup {
    ShortProfileLookup::new(mris(texts)).unwrap()
}

/// The fixture with its placeholders filled in: `<guid:a>` and `<guid:b>` are the lab accounts'.
#[cfg(test)]
fn fixture() -> Value {
    serde_json::from_str(&FIXTURE.replace("<guid:a>", "lab-a").replace("<guid:b>", "lab-b")).unwrap()
}

/// Hands out placeholder tokens and records what the transport asked for.
#[derive(Default)]
struct FakeTokens {
    generation: AtomicUsize,
    audiences: Mutex<Vec<String>>,
    rejected: Mutex<Vec<String>>,
}

impl FakeTokens {
    fn audiences(&self) -> Vec<String> {
        self.audiences.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn rejected(&self) -> Vec<String> {
        self.rejected.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn current(&self, audience: &str) -> Secret {
        self.audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(audience.to_owned());
        Secret::new(token(self.generation.load(Ordering::SeqCst)))
    }
}

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(self.current(audience)))
    }

    fn bearer_rejected(
        &self,
        audience: &str,
        rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.rejected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(rejected.expose().to_owned());
        self.generation.fetch_add(1, Ordering::SeqCst);
        future::ready(Ok(self.current(audience)))
    }
}

struct Setup {
    server: MockServer,
    tokens: Arc<FakeTokens>,
    transport: HttpTransport<FakeTokens>,
}

async fn setup() -> Setup {
    setup_with(Some(MIDDLE_TIER)).await
}

#[cfg(test)]
async fn setup_with(middle_tier: Option<&str>) -> Setup {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());

    let mut transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    if let Some(base) = middle_tier {
        transport = transport.with_middle_tier(base).unwrap();
    }
    Setup {
        server,
        tokens,
        transport,
    }
}

#[cfg(test)]
fn header_of<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request.headers.get(name).and_then(|value| value.to_str().ok())
}

/// Every pair of the captured lookup query, on `mock`.
fn with_lookup_query(mut mock: wiremock::MockBuilder) -> wiremock::MockBuilder {
    for (name, value) in LOOKUP_QUERY {
        mock = mock.and(query_param(name, value));
    }
    mock
}

/// `profile` is what `want` (a profile of the fixture's answers) says.
#[cfg(test)]
fn assert_profile(name: &str, profile: &ShortProfile, want: &Value) {
    let text = |key: &str| want[key].as_str();
    assert_eq!(profile.mri.as_str(), want["mri"].as_str().unwrap(), "{name}");
    assert_eq!(profile.display_name.as_deref(), text("displayName"), "{name}");
    assert_eq!(profile.given_name.as_deref(), text("givenName"), "{name}");
    assert_eq!(profile.surname.as_deref(), text("surname"), "{name}");
    assert_eq!(profile.email.as_deref(), text("email"), "{name}");
    assert_eq!(
        profile.user_principal_name.as_deref(),
        text("userPrincipalName"),
        "{name}"
    );
    assert_eq!(profile.object_id.as_deref(), text("objectId"), "{name}");
    assert_eq!(profile.user_type.as_deref(), Some("Member"), "{name}");
}

#[tokio::test]
async fn a_short_profile_lookup_goes_out_in_its_captured_shape_and_its_answer_is_read() {
    let fixture = fixture();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let asked: Vec<&str> = case["asked"]
            .as_array()
            .unwrap()
            .iter()
            .map(|mri| mri.as_str().unwrap())
            .collect();
        let setup = setup().await;

        let answer = serde_json::to_vec(&case["answer"]).unwrap();
        let content_type = fixture["content_type"].as_str().unwrap().to_owned();
        with_lookup_query(Mock::given(method("POST")).and(path(SHORT_PROFILE)))
            .and(header("authorization", format!("Bearer {}", token(0)).as_str()))
            .and(header("content-type", "application/json;charset=UTF-8"))
            .and(header("accept", "application/json"))
            .and(header("x-ms-client-request-type", "0"))
            .and(header("x-ms-client-type", "cdlworker"))
            .and(header("x-ms-migration", "True"))
            .and(header("x-ms-object-id", ""))
            .and(header("x-ms-test-user", "False"))
            .and(body_json(&case["asked"]))
            .respond_with(move |_: &Request| ResponseTemplate::new(200).set_body_raw(answer.clone(), &content_type))
            .expect(1)
            .mount(&setup.server)
            .await;
        let request = lookup(&asked);

        let answer = Client::new(setup.transport).post_read(request.clone()).await.unwrap();

        assert_eq!(answer.status(), 200, "{name}");
        let profiles = request.read_answer(answer.body()).unwrap();
        assert_eq!(profiles.len(), 1, "{name}");
        assert_profile(name, &profiles[0], &case["answer"]["value"][0]);
        assert_eq!(setup.tokens.audiences(), [MIDDLE_TIER_AUDIENCE], "{name}");
        assert_eq!(MIDDLE_TIER_AUDIENCE, "https://api.spaces.skype.com");
    }
}

#[tokio::test]
async fn a_lookup_carries_nothing_its_entry_doesnt_name() {
    let setup = setup().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"type": "t", "value": []})))
        .expect(1)
        .mount(&setup.server)
        .await;
    let request = lookup(&[LAB_A, LAB_B]);

    setup.transport.post_read(request).await.unwrap();

    let requests = setup.server.received_requests().await.unwrap();
    let sent = &requests[0];
    for name in [
        "cookie",
        "origin",
        "referer",
        "x-ms-client-caller",
        "x-ms-session-id",
        "x-ms-request-id",
    ] {
        assert_eq!(header_of(sent, name), None, "{name}");
    }
    let mut query: Vec<(String, String)> = sent.url.query_pairs().map(|(k, v)| (k.into(), v.into())).collect();
    let mut want: Vec<(String, String)> = LOOKUP_QUERY.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
    query.sort();
    want.sort();
    assert_eq!(query, want);
    assert_eq!(
        serde_json::from_slice::<Value>(&sent.body).unwrap(),
        json!([LAB_A, LAB_B])
    );
}

#[tokio::test]
async fn anything_the_entry_doesnt_name_is_unknown_and_nothing_goes_out() {
    let probes = [
        (
            "a query pair",
            ProbePostRead::new(lookup(&[LAB_A])).with_query("extra", "1"),
        ),
        (
            "a captured query pair twice, with another value",
            ProbePostRead::new(lookup(&[LAB_A])).with_query("isMailAddress", "true"),
        ),
        (
            "a header",
            ProbePostRead::new(lookup(&[LAB_A])).with_header("x-ms-client-caller", "probe"),
        ),
        (
            "a cookie",
            ProbePostRead::new(lookup(&[LAB_A])).with_header("Cookie", "a=b"),
        ),
        (
            "a pinned header with another value too",
            ProbePostRead::new(lookup(&[LAB_A])).with_header("x-ms-object-id", "lab-a"),
        ),
        (
            "a body field",
            ProbePostRead::new(lookup(&[LAB_A])).with_body(json!({"mris": [LAB_A], "extra": 1})),
        ),
        (
            "a list item that isn't an MRI",
            ProbePostRead::new(lookup(&[LAB_A])).with_body(json!([LAB_A, {"extra": 1}])),
        ),
        (
            "an empty list",
            ProbePostRead::new(lookup(&[LAB_A])).with_body(json!([])),
        ),
        (
            "a body naming other MRIs than the typed request",
            ProbePostRead::new(lookup(&[LAB_A])).with_body(json!([LAB_B])),
        ),
        (
            "a body naming more MRIs than the typed request",
            ProbePostRead::new(lookup(&[LAB_A])).with_body(json!([LAB_A, LAB_B])),
        ),
    ];
    for (name, probe) in probes {
        let setup = setup().await;

        let error = Client::new(setup.transport).post_read(probe).await.unwrap_err();

        assert!(matches!(error, HttpError::Unknown), "{name}: {error:?}");
        assert!(setup.server.received_requests().await.unwrap().is_empty(), "{name}");
        assert!(setup.tokens.audiences().is_empty(), "{name}: no token asked for");
    }
}

#[tokio::test]
async fn a_probe_with_nothing_added_goes_out() {
    let setup = setup().await;
    with_lookup_query(Mock::given(method("POST")).and(path(SHORT_PROFILE)))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&setup.server)
        .await;
    let probe = ProbePostRead::new(lookup(&[LAB_A]));

    let answer = Client::new(setup.transport).post_read(probe).await.unwrap();

    assert_eq!(answer.status(), 200, "the probe's refusals above are its additions'");
}

#[tokio::test]
async fn a_refused_lookup_isnt_sent_again_and_the_token_is_refreshed() {
    let setup = setup().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&setup.server)
        .await;
    let request = lookup(&[LAB_A]);

    let error = Client::new(setup.transport).post_read(request).await.unwrap_err();

    let HttpError::Status(answer) = error else {
        panic!("{error:?}");
    };
    assert_eq!(answer.status(), 401);
    assert_eq!(setup.tokens.rejected(), [token(0)], "refreshed for a later lookup");
}

#[tokio::test]
async fn a_throttled_lookup_comes_back_with_its_wait_and_isnt_sent_again() {
    let setup = setup().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "7"))
        .expect(1)
        .mount(&setup.server)
        .await;
    let request = lookup(&[LAB_A]);

    let error = Client::new(setup.transport).post_read(request).await.unwrap_err();

    let HttpError::Status(answer) = error else {
        panic!("{error:?}");
    };
    assert_eq!(answer.retry_after(), Some(Duration::from_secs(7)));
}

#[tokio::test]
async fn without_a_middle_tier_base_nothing_goes_there() {
    let setup = setup_with(None).await;
    let request = lookup(&[LAB_A]);

    let error = Client::new(setup.transport).post_read(request).await.unwrap_err();

    assert!(matches!(error, HttpError::Refused(Refusal::NoMiddleTier)), "{error:?}");
    assert!(setup.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_middle_tier_base_is_an_afd_host_and_a_partition_only() {
    let setup = setup().await;
    let refused = [
        "https://evil.example.com/api/mt/part/amer-03",
        "https://teams.microsoft.com.evil.example.com/api/mt/part/amer-03",
        "http://teams.microsoft.com/api/mt/part/amer-03",
        "https://teams.microsoft.com:8443/api/mt/part/amer-03",
        "https://user@teams.microsoft.com/api/mt/part/amer-03",
        "https://teams.microsoft.com/api/mt/part/amer-03?x=1",
        "https://teams.microsoft.com/api/mt/part/amer-03#x",
        "https://teams.microsoft.com/api/mt/part/AMER-03",
        "https://teams.microsoft.com/api/mt/part/amer",
        "https://teams.microsoft.com/api/mt/part/amer-03/beta",
        "https://teams.microsoft.com/api/mt/amer-03",
        "https://TEAMS.microsoft.com/api/mt/part/amer-03",
        " https://teams.microsoft.com/api/mt/part/amer-03",
        "",
    ];
    for base in refused {
        let transport = HttpTransport::new(Arc::clone(&setup.tokens), CHAT_BASE)
            .unwrap()
            .with_middle_tier(base);
        assert!(matches!(transport, Err(BuildError::MiddleTierBase)), "{base:?}");
    }
    for base in [
        "https://teams.microsoft.com/api/mt/part/amer-03",
        "https://teams.microsoft.com/api/mt/part/amer-03/",
        "https://teams.cloud.microsoft/api/mt/part/emea-01",
    ] {
        let transport = HttpTransport::new(Arc::clone(&setup.tokens), CHAT_BASE)
            .unwrap()
            .with_middle_tier(base);
        assert!(transport.is_ok(), "{base}");
    }
}

#[tokio::test]
async fn a_lookup_goes_to_the_base_it_was_given() {
    let setup = setup_with(Some("https://teams.cloud.microsoft/api/mt/part/emea-01/")).await;
    with_lookup_query(Mock::given(method("POST")).and(path("/api/mt/part/emea-01/beta/users/fetchShortProfile")))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&setup.server)
        .await;
    let request = lookup(&[LAB_A]);

    Client::new(setup.transport).post_read(request).await.unwrap();
}

/// A read of the test's choosing.
struct Get(Service, Endpoint);

impl Read for Get {
    fn service(&self) -> Service {
        self.0
    }

    fn endpoint(&self) -> Endpoint {
        self.1.clone()
    }
}

#[tokio::test]
async fn a_middle_tier_read_is_a_get_on_its_base() {
    let setup = setup().await;
    Mock::given(method("GET"))
        .and(header("x-ms-migration", "True"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&setup.server)
        .await;

    for segments in [vec!["beta", "users", "lab-a"], vec!["beta", "me", "settings"]] {
        setup
            .transport
            .read(&Get(Service::MiddleTier, Endpoint::new(segments)))
            .await
            .unwrap();
    }
    let error = setup
        .transport
        .read(&Get(Service::MiddleTier, Endpoint::new(["beta", "users", ".."])))
        .await
        .unwrap_err();
    assert!(matches!(error, HttpError::Refused(Refusal::Segment)), "{error:?}");

    let paths: Vec<String> = setup
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert_eq!(
        paths,
        [
            "/api/mt/part/amer-03/beta/users/lab-a",
            "/api/mt/part/amer-03/beta/me/settings"
        ]
    );
    assert_eq!(setup.tokens.audiences(), [MIDDLE_TIER_AUDIENCE, MIDDLE_TIER_AUDIENCE]);
}

#[test]
fn an_answer_naming_anyone_not_asked_for_is_dropped_whole() {
    let fixture = fixture();
    let of_b = serde_json::to_vec(&fixture["cases"][1]["answer"]).unwrap();
    let mut of_a_and_b = fixture["cases"][0]["answer"].clone();
    of_a_and_b["value"]
        .as_array_mut()
        .unwrap()
        .push(fixture["cases"][1]["answer"]["value"][0].clone());

    assert_eq!(
        lookup(&[LAB_A]).read_answer(&of_b),
        Err(AnswerError::Unasked { count: 1 })
    );
    assert_eq!(
        lookup(&[LAB_A]).read_answer(&serde_json::to_vec(&of_a_and_b).unwrap()),
        Err(AnswerError::Unasked { count: 1 })
    );
    assert_eq!(
        lookup(&[LAB_A, LAB_B])
            .read_answer(&serde_json::to_vec(&of_a_and_b).unwrap())
            .map(|profiles| profiles.len()),
        Ok(2)
    );
    let untyped = serde_json::to_vec(&json!({"value": [{"mri": LAB_A}]})).unwrap();
    for malformed in [&b"[]"[..], b"{}", b"{\"value\": [{}]}", &untyped[..], b"not json"] {
        assert_eq!(lookup(&[LAB_A]).read_answer(malformed), Err(AnswerError::Malformed));
    }
}

#[test]
fn a_profiles_debug_names_only_its_mri() {
    let fixture = fixture();
    let answer = serde_json::to_vec(&fixture["cases"][1]["answer"]).unwrap();
    let profiles = lookup(&[LAB_B]).read_answer(&answer).unwrap();

    let shown = format!("{profiles:?}");

    assert!(shown.contains(LAB_B));
    assert!(
        !shown.contains("Lab Account B") && !shown.contains("example.com"),
        "{shown}"
    );
}
