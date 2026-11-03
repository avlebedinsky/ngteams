//! The native HTTP transport against synthetic mocks: plain typed writes, reads, own state, status handling, credential renewal and no automatic write retry.

use std::future::{self, Future};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::Session;
use teams_auth::token::TokenError;
use teams_fence::Mri;
use teams_proto::chat::MessageText;
use teams_proto::{
    Author, Client, Credentials, DEFAULT_RETRY_WAIT, Endpoint, HttpError, HttpTransport, MAX_BODY, Method,
    ProbeOwnState, Read, Request as PlainRequest, SendMessage, Service, ShortProfileLookup, Transport,
};
use wiremock::matchers::{body_json, body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const LAB_A: &str = "8:orgid:lab-a";
const LAB_B: &str = "8:orgid:lab-b";
const CHAT: &str = "19:lab-chat@thread.v2";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const MESSAGES: &str = "/api/chatsvc/amer/v1/users/ME/conversations/19:lab-chat@thread.v2/messages";
const CONVERSATIONS: &str = "/api/chatsvc/amer/v1/users/ME/conversations";

/// `text` from a to `chat`, as one paragraph.
#[cfg(test)]
fn text(chat: &str, text: &str) -> SendMessage {
    SendMessage::new(
        chat,
        Author::new(Mri::new(LAB_A), "Lab A"),
        MessageText::plain(text).unwrap(),
    )
}

/// A plain "hello" from a to the lab chat.
fn hello() -> SendMessage {
    text(CHAT, "hello")
}

/// The placeholder token of generation `n`: the first one handed out is generation 0, and each rejection moves on.
fn token(generation: usize) -> String {
    format!("placeholder-token-generation-{generation}")
}

fn bearer(generation: usize) -> String {
    format!("Bearer {}", token(generation))
}

/// Hands out placeholder tokens and records what the transport asked for.
#[derive(Default)]
struct FakeTokens {
    generation: AtomicUsize,
    audiences: Mutex<Vec<String>>,
    rejected: Mutex<Vec<String>>,
    signed_out: AtomicBool,
}

impl FakeTokens {
    fn signed_out() -> Self {
        let tokens = Self::default();
        tokens.sign_out();
        tokens
    }

    /// Signs the session out: every later ask answers `NotSignedIn`, as a signed-out `Session` does.
    fn sign_out(&self) {
        self.signed_out.store(true, Ordering::SeqCst);
    }

    fn rejected(&self) -> Vec<String> {
        self.rejected.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn audiences(&self) -> Vec<String> {
        self.audiences.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn current(&self, audience: &str) -> Result<Secret, TokenError> {
        if self.signed_out.load(Ordering::SeqCst) {
            return Err(TokenError::NotSignedIn {
                account: "lab-a".to_owned(),
            });
        }
        self.audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(audience.to_owned());
        Ok(Secret::new(token(self.generation.load(Ordering::SeqCst))))
    }
}

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(self.current(audience))
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
        future::ready(self.current(audience))
    }
}

struct Setup {
    server: MockServer,
    tokens: Arc<FakeTokens>,
    transport: HttpTransport<FakeTokens>,
}

async fn setup() -> Setup {
    setup_with(FakeTokens::default()).await
}

#[cfg(test)]
async fn setup_with(tokens: FakeTokens) -> Setup {
    let server = MockServer::start().await;
    let tokens = Arc::new(tokens);

    let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri())
        .unwrap()
        .with_max_retry_wait(Duration::from_millis(50));
    Setup {
        server,
        tokens,
        transport,
    }
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

fn conversations() -> Get {
    Get(
        Service::ChatService,
        Endpoint::new(["v1", "users", "ME", "conversations"]).with_query("view", "msnp24Equivalent"),
    )
}

fn header_of<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request.headers.get(name).and_then(|value| value.to_str().ok())
}

#[tokio::test]
async fn a_read_is_a_get_on_the_chat_base_with_the_chat_services_headers() {
    let setup = setup().await;
    Mock::given(method("GET"))
        .and(path(CONVERSATIONS))
        .and(query_param("view", "msnp24Equivalent"))
        .and(header("authorization", bearer(0).as_str()))
        .and(header("x-ms-migration", "True"))
        .and(header("behavioroverride", "redirectAs404"))
        .and(header("accept", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"conversations": []})))
        .expect(1)
        .mount(&setup.server)
        .await;

    let answer = Client::new(setup.transport).read(&conversations()).await.unwrap();

    assert_eq!(answer.status(), 200);
    assert_eq!(
        serde_json::from_slice::<Value>(answer.body()).unwrap(),
        json!({"conversations": []})
    );
    assert_eq!(setup.tokens.audiences(), ["https://ic3.teams.office.com"]);
}

#[tokio::test]
async fn the_registrar_gets_the_token_and_the_migration_flag_and_trouter_no_credential() {
    let setup = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&setup.server)
        .await;
    let registrar = Get(Service::Registrar, Endpoint::new(["prod", "V2", "registrations"]));
    let trouter = Get(
        Service::Trouter,
        Endpoint::new(Vec::<String>::new()).with_query("check", "1"),
    );

    setup.transport.read(&registrar).await.unwrap();
    setup.transport.read(&trouter).await.unwrap();

    let requests = setup.server.received_requests().await.unwrap();
    let [to_registrar, to_trouter] = &requests[..] else {
        panic!("two requests, got {}", requests.len());
    };
    assert_eq!(to_registrar.url.path(), "/registrar/prod/V2/registrations");
    assert_eq!(header_of(to_registrar, "authorization"), Some(bearer(0).as_str()));
    assert_eq!(header_of(to_registrar, "x-ms-migration"), Some("True"));
    assert_eq!(header_of(to_registrar, "x-ms-test-user"), Some("False"));
    assert_eq!(header_of(to_registrar, "behavioroverride"), None);
    assert_eq!(to_trouter.url.path(), "/");
    assert_eq!(to_trouter.url.query(), Some("check=1"));
    assert_eq!(header_of(to_trouter, "authorization"), None);
    assert_eq!(header_of(to_trouter, "x-ms-migration"), None);
}

#[tokio::test]
async fn a_typed_write_is_posted_in_its_wire_form_and_its_answer_comes_back() {
    let setup = setup().await;
    Mock::given(method("POST"))
        .and(path(MESSAGES))
        .and(header("authorization", bearer(0).as_str()))
        .and(header("x-ms-migration", "True"))
        .and(header("behavioroverride", "redirectAs404"))
        .and(header("content-type", "application/json"))
        .and(body_partial_json(json!({
            "content": "<p>hello</p>",
            "messagetype": "RichText/Html",
            "contenttype": "Text",
        })))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header(
                    "Location",
                    "https://teams.microsoft.com/api/chatsvc/amer/v1/messages/1001",
                )
                .set_body_json(json!({"OriginalArrivalTime": 1_760_000_000_000_u64})),
        )
        .expect(1)
        .mount(&setup.server)
        .await;
    let request = hello();

    let answer = Client::new(setup.transport).write(request).await.unwrap();

    assert_eq!(answer.status(), 201);
    assert_eq!(
        answer.location(),
        Some("https://teams.microsoft.com/api/chatsvc/amer/v1/messages/1001")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(answer.body()).unwrap(),
        json!({"OriginalArrivalTime": 1_760_000_000_000_u64})
    );
}

#[tokio::test]
async fn a_message_with_a_client_message_id_is_posted_with_it_and_the_same_target() {
    let setup = setup().await;
    Mock::given(method("POST"))
        .and(path(MESSAGES))
        .and(body_partial_json(json!({
            "content": "<p>hello</p>",
            "messagetype": "RichText/Html",
            "contenttype": "Text",
            "clientmessageid": "1234567890123456789",
        })))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&setup.server)
        .await;
    let request = hello().with_client_message_id("1234567890123456789");
    assert_eq!(request.client_message_id(), Some("1234567890123456789"));

    let answer = Client::new(setup.transport).write(request).await.unwrap();

    assert_eq!(answer.status(), 201);
}

#[tokio::test]
async fn a_write_refused_with_401_comes_back_unsent_again_and_the_token_is_refreshed() {
    let setup = setup().await;
    Mock::given(method("POST"))
        .and(path(MESSAGES))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&setup.server)
        .await;
    let request = hello();

    let answer = setup.transport.deliver(request).await.unwrap();

    assert_eq!(answer.status(), 401, "the outbox decides what to do about it");
    assert_eq!(
        setup.tokens.rejected(),
        [token(0)],
        "the session refreshed the refused token"
    );
}

#[tokio::test]
async fn a_throttled_write_comes_back_without_a_retry() {
    let setup = setup().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "7"))
        .expect(1)
        .mount(&setup.server)
        .await;
    let request = hello();

    let answer = Client::new(setup.transport).write(request).await.unwrap();

    assert_eq!(answer.status(), 429);
    assert_eq!(answer.retry_after(), Some(Duration::from_secs(7)));
}

/// A plain request of the test's choosing: any type may implement `Request`.
struct Plain {
    service: Service,
    method: Method,
    endpoint: Endpoint,
    body: Option<Value>,
}

impl PlainRequest for Plain {
    fn service(&self) -> Service {
        self.service
    }

    fn method(&self) -> Method {
        self.method
    }

    fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }

    fn body(&self) -> Option<Value> {
        self.body.clone()
    }
}

fn plain(service: Service, method: Method, segments: &[&str], body: Option<Value>) -> Plain {
    Plain {
        service,
        method,
        endpoint: Endpoint::new(segments.iter().copied()),
        body,
    }
}

/// A synthetic plain `PUT` of the account's own setting.
fn probe() -> Plain {
    Plain {
        endpoint: Endpoint::new(["v1", "users", "ME", "properties"]).with_query("name", "probe"),
        ..plain(Service::ChatService, Method::Put, &[], Some(json!({"probe": true})))
    }
}

#[tokio::test]
async fn a_plain_request_is_sent_once() {
    let setup = setup().await;

    Mock::given(method("PUT"))
        .and(path("/api/chatsvc/amer/v1/users/ME/properties"))
        .and(query_param("name", "probe"))
        .and(header("authorization", bearer(0).as_str()))
        .and(body_json(json!({"probe": true})))
        .respond_with(move |_: &Request| ResponseTemplate::new(200))
        .expect(1)
        .mount(&setup.server)
        .await;

    let answer = Client::new(setup.transport).send(&probe()).await.unwrap();

    assert_eq!(answer.status(), 200);
}

#[tokio::test]
async fn a_plain_request_that_reaches_nobody_goes_in_any_method_whoever_its_body_names() {
    let setup = setup().await;
    let transport = setup
        .transport
        .with_middle_tier("https://teams.microsoft.com/api/mt/part/amer-03")
        .unwrap();
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&setup.server)
        .await;
    let outsider = "8:orgid:00000000-0000-4000-8000-0000000000ff";
    let requests = [
        // the registrar, the account's own endpoints, a setting: the account's own session and state
        plain(
            Service::Registrar,
            Method::Post,
            &["prod", "V2", "registrations"],
            Some(json!({"registrationId": "x"})),
        ),
        plain(
            Service::ChatService,
            Method::Delete,
            &["v1", "users", "ME", "endpoints", "endpoint-id"],
            None,
        ),
        plain(
            Service::MiddleTier,
            Method::Patch,
            &["beta", "me", "settings"],
            Some(json!({"names": [outsider]})),
        ),
        // a read sent as a `POST` naming someone outside the lab
        plain(
            Service::MiddleTier,
            Method::Post,
            &["beta", "users", "searchV2"],
            Some(json!([outsider, LAB_B])),
        ),
    ];
    for request in &requests {
        transport.send(request).await.unwrap();
    }
    let lookup = ShortProfileLookup::new([Mri::new(outsider)]).unwrap();
    transport.send(&lookup).await.unwrap();

    let received = setup.server.received_requests().await.unwrap();
    let sent: Vec<(String, String)> = received
        .iter()
        .map(|request| (request.method.to_string(), request.url.path().to_owned()))
        .collect();
    assert_eq!(
        sent,
        [
            ("POST", "/registrar/prod/V2/registrations"),
            ("DELETE", "/api/chatsvc/amer/v1/users/ME/endpoints/endpoint-id"),
            ("PATCH", "/api/mt/part/amer-03/beta/me/settings"),
            ("POST", "/api/mt/part/amer-03/beta/users/searchV2"),
            ("POST", "/api/mt/part/amer-03/beta/users/fetchShortProfile"),
        ]
        .map(|(method, path)| (method.to_owned(), path.to_owned()))
    );
    assert_eq!(
        received[4].body,
        serde_json::to_vec(&json!([outsider])).unwrap(),
        "the profile lookup names the outsider"
    );
}

#[tokio::test]
async fn an_own_state_request_is_sent_once() {
    let setup = setup().await;
    Mock::given(method("POST"))
        .and(path("/registrar/prod/V2/registrations"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&setup.server)
        .await;
    let own = ProbeOwnState::new(
        Service::Registrar,
        Endpoint::new(["prod", "V2", "registrations"]),
        Some(json!({"probe": true})),
    );

    setup.transport.own_state(&own).await.unwrap();
}

const REGISTRATIONS: &str = include_str!("../../../tests/fixtures/trouter/registration.json");
const EPID: &str = "00000000-0000-4000-8000-0000000000e1";
const PUSH_URL: &str = "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000A/";

#[tokio::test]
async fn the_messaging_registration_is_posted_in_its_captured_shape() {
    let setup = setup().await;
    let fixture: Value = serde_json::from_str(REGISTRATIONS).unwrap();
    let mut want = fixture["cases"][0]["body"].clone();
    want["registrationId"] = json!(EPID);
    want["clientDescription"]["platformUIVersion"] = json!(teams_proto::CLIENT_BUILD);

    Mock::given(method("POST"))
        .and(path("/registrar/prod/V2/registrations"))
        .and(header("authorization", bearer(0).as_str()))
        .and(header("x-ms-migration", "True"))
        .and(header("x-ms-test-user", "False"))
        .and(body_json(&want))
        .respond_with(move |_: &Request| ResponseTemplate::new(202))
        .expect(1)
        .mount(&setup.server)
        .await;
    let registration = teams_proto::EndpointRegistration::messaging(EPID, PUSH_URL).unwrap();

    let answer = Client::new(setup.transport).own_state(&registration).await.unwrap();

    assert_eq!(answer.status(), 202);

    assert_eq!(
        fixture["cases"][0]["body"]["clientDescription"]["appId"],
        teams_proto::MESSAGING_APP_ID
    );
}

#[tokio::test]
async fn the_calling_registration_is_posted_in_its_captured_shape_and_registers_no_endpoint() {
    let setup = setup().await;
    let fixture: Value = serde_json::from_str(REGISTRATIONS).unwrap();
    let case = &fixture["cases"][2];
    assert_eq!(case["name"], "the calling stack's endpoint (its own socket)");
    let mut want = case["body"].clone();
    want["registrationId"] = json!(EPID);
    want["clientDescription"]["platformUIVersion"] = json!(teams_proto::CLIENT_BUILD);
    let push = want["transports"]["TROUTER"][0]["path"].as_str().unwrap().to_owned();
    Mock::given(method("POST"))
        .and(path("/registrar/prod/V2/registrations"))
        .and(header("authorization", bearer(0).as_str()))
        .and(header("x-ms-migration", "True"))
        .and(body_json(&want))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&setup.server)
        .await;
    let registration = teams_proto::CallingRegistration::new(EPID, &push).unwrap();
    assert!(teams_proto::CallingRegistration::new("not-a-guid", &push).is_none());
    assert!(teams_proto::CallingRegistration::new(EPID, "https://evil.example.test/v4/f/x/").is_none());

    // The registration is also a plain typed request.
    let answer = teams_proto::register_calling(&setup.transport, &registration)
        .await
        .unwrap();

    assert_eq!(answer.status(), 202);

    assert_eq!(
        (
            case["body"]["clientDescription"]["appId"].as_str(),
            case["body"]["clientDescription"]["templateKey"].as_str()
        ),
        (Some(teams_proto::CALLING_APP_ID), Some(teams_proto::CALLING_TEMPLATE))
    );
    assert_eq!(
        <teams_proto::CallingRegistration as teams_proto::OwnState>::KIND,
        "calling_endpoint_registration"
    );
}

#[test]
fn a_registration_names_only_a_guid_endpoint_and_a_trouter_push_url() {
    use teams_proto::EndpointRegistration;
    assert!(EndpointRegistration::messaging(EPID, PUSH_URL).is_some());
    assert!(
        EndpointRegistration::messaging(EPID, "https://pub-ent-zz-01-f.trouter.teams.microsoft.com/v4/f/Abc123/")
            .is_some()
    );
    // the endpoint id is base64url: live ids carry `-` and `_` (an `_` refused once cost a whole run its registration)
    for url in [
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/Synthetic_Endpoint0000A/",
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/Synthetic-Endpoint0000A/",
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/-Synthetic_Endpoint000_/",
    ] {
        assert!(EndpointRegistration::messaging(EPID, url).is_some(), "{url}");
    }
    for epid in [
        "",
        "not-a-guid",
        "00000000-0000-4000-8000-0000000000E1",
        "00000000000040008000000000000e1",
    ] {
        assert!(EndpointRegistration::messaging(epid, PUSH_URL).is_none(), "{epid}");
    }
    for url in [
        "http://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000A/",
        "https://trouter.teams.microsoft.com/v4/f/SyntheticEndpoint00000A/",
        "https://evil.example.test/v4/f/SyntheticEndpoint00000A/",
        "https://x.trouter.teams.microsoft.com.example.test/v4/f/SyntheticEndpoint00000A/",
        "https://x.trouter.teams.microsoft.com/v4/f/SyntheticEndpoint00000A",
        "https://x.trouter.teams.microsoft.com/v4/f//",
        "https://x.trouter.teams.microsoft.com/v4/f/a/b/",
        "https://x.trouter.teams.microsoft.com/v4/f/abc/?q=1",
        "https://x.trouter.teams.microsoft.com/v4/f/abc/#f",
        "https://user@x.trouter.teams.microsoft.com/v4/f/abc/",
        "https://X.trouter.teams.microsoft.com/v4/f/abc/",
        "https://x.trouter.teams.microsoft.com/v4/c/abc/",
        "https://x.trouter.teams.microsoft.com/v4/f/ab.c/",
        "https://x.trouter.teams.microsoft.com/v4/f/ab%2Fc/",
        "https://x.trouter.teams.microsoft.com/v4/f/ab+c/",
        "https://x.trouter.teams.microsoft.com/v4/f/ab=/",
    ] {
        assert!(EndpointRegistration::messaging(EPID, url).is_none(), "{url}");
    }
}

#[tokio::test]
async fn a_read_refused_with_401_is_sent_once_more_with_a_refreshed_token() {
    let setup = setup().await;
    Mock::given(method("GET"))
        .and(header("authorization", bearer(0).as_str()))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&setup.server)
        .await;
    Mock::given(method("GET"))
        .and(header("authorization", bearer(1).as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&setup.server)
        .await;

    let answer = setup.transport.read(&conversations()).await.unwrap();

    assert_eq!(answer.status(), 200);
    assert_eq!(setup.tokens.rejected(), [token(0)]);
}

#[tokio::test]
async fn a_second_401_is_returned_without_a_token_in_the_error() {
    let setup = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .expect(2)
        .mount(&setup.server)
        .await;

    let error = setup.transport.read(&conversations()).await.unwrap_err();

    let HttpError::Status(answer) = &error else {
        panic!("a status error, got {error:?}");
    };
    assert_eq!(answer.status(), 401);
    for text in [error.to_string(), format!("{error:?}")] {
        assert!(!text.contains("placeholder-token"), "{text}");
    }
}

#[tokio::test]
async fn a_throttled_read_waits_its_retry_after_and_is_sent_once_more() {
    for status in [429, 503] {
        let setup = setup().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(status).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&setup.server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&setup.server)
            .await;

        let answer = setup.transport.read(&conversations()).await.unwrap();

        assert_eq!(answer.status(), 200, "after {status}");
        assert!(setup.tokens.rejected().is_empty());
    }
}

/// Answers `status` (with `Retry-After: 0`) and signs `tokens` out, as a session signed out while the transport waits.
struct ThrottleThenSignOut {
    status: u16,
    tokens: Arc<FakeTokens>,
}

impl Respond for ThrottleThenSignOut {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        self.tokens.sign_out();
        ResponseTemplate::new(self.status).insert_header("Retry-After", "0")
    }
}

#[tokio::test]
async fn a_session_signed_out_during_the_throttling_wait_sends_nothing_more() {
    for status in [429, 503] {
        let setup = setup().await;
        Mock::given(method("GET"))
            .respond_with(ThrottleThenSignOut {
                status,
                tokens: Arc::clone(&setup.tokens),
            })
            .expect(1)
            .mount(&setup.server)
            .await;
        let probe_setup = setup_with(FakeTokens::default()).await;
        Mock::given(method("PUT"))
            .respond_with(ThrottleThenSignOut {
                status,
                tokens: Arc::clone(&probe_setup.tokens),
            })
            .expect(1)
            .mount(&probe_setup.server)
            .await;

        let read = setup.transport.read(&conversations()).await.unwrap_err();
        let own = probe_setup.transport.send(&probe()).await.unwrap_err();

        assert!(matches!(read, HttpError::Credentials(_)), "after {status}: {read:?}");
        assert!(
            matches!(own, HttpError::Status(_)),
            "a plain write is sent once: {own:?}"
        );
        assert_eq!(
            setup.server.received_requests().await.unwrap().len(),
            1,
            "after {status}"
        );
        assert_eq!(
            probe_setup.server.received_requests().await.unwrap().len(),
            1,
            "after {status}"
        );
    }
}

#[tokio::test]
async fn the_wait_before_a_retry_is_capped() {
    let setup = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "3600"))
        .up_to_n_times(1)
        .mount(&setup.server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&setup.server)
        .await;

    let started = Instant::now();
    setup.transport.read(&conversations()).await.unwrap();

    assert!(
        started.elapsed() < Duration::from_secs(10),
        "waited {:?}",
        started.elapsed()
    );
}

/// How long a read throttled once with `retry_after` (none: no header) takes, under a 2 s cap on the wait.
#[cfg(test)]
async fn throttled_read_takes(retry_after: Option<&str>) -> Duration {
    let mut setup = setup().await;
    setup.transport = setup.transport.with_max_retry_wait(Duration::from_secs(2));
    let mut throttled = ResponseTemplate::new(429);
    if let Some(retry_after) = retry_after {
        throttled = throttled.insert_header("Retry-After", retry_after);
    }
    Mock::given(method("GET"))
        .respond_with(throttled)
        .up_to_n_times(1)
        .expect(1)
        .mount(&setup.server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&setup.server)
        .await;

    let started = Instant::now();
    setup.transport.read(&conversations()).await.unwrap();
    started.elapsed()
}

#[tokio::test]
async fn a_throttled_read_waits_its_retry_after_before_the_retry() {
    let waited = throttled_read_takes(Some("1")).await;

    assert!(waited >= Duration::from_secs(1), "waited {waited:?}");
}

#[tokio::test]
async fn a_throttled_read_without_a_retry_after_waits_the_default() {
    let waited = throttled_read_takes(None).await;

    let expected = DEFAULT_RETRY_WAIT.min(Duration::from_secs(2));
    assert!(waited >= expected, "waited {waited:?}, expected at least {expected:?}");
}

#[tokio::test]
async fn throttled_twice_is_returned_with_its_retry_after() {
    let setup = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .expect(2)
        .mount(&setup.server)
        .await;

    let error = setup.transport.read(&conversations()).await.unwrap_err();

    let HttpError::Status(answer) = &error else {
        panic!("a status error, got {error:?}");
    };
    assert_eq!(answer.status(), 429);
    assert_eq!(answer.retry_after(), Some(Duration::ZERO));
}

#[tokio::test]
async fn any_other_failure_status_is_returned_at_once() {
    let setup = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).insert_header("Location", "https://elsewhere.example/"))
        .expect(1)
        .mount(&setup.server)
        .await;

    let error = setup.transport.read(&conversations()).await.unwrap_err();

    let HttpError::Status(answer) = &error else {
        panic!("a status error, got {error:?}");
    };
    assert_eq!(answer.status(), 404);
    assert_eq!(
        answer.location(),
        Some("https://elsewhere.example/"),
        "kept, never followed"
    );
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let setup = setup().await;
    let elsewhere = format!("{}/elsewhere", setup.server.uri());
    Mock::given(method("GET"))
        .and(path(CONVERSATIONS))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", elsewhere.as_str()))
        .expect(1)
        .mount(&setup.server)
        .await;
    Mock::given(path("/elsewhere"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&setup.server)
        .await;

    let error = setup.transport.read(&conversations()).await.unwrap_err();

    assert!(
        matches!(&error, HttpError::Status(answer) if answer.status() == 302),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_body_over_the_cap_is_refused() {
    let setup = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; MAX_BODY + 1]))
        .mount(&setup.server)
        .await;

    let error = setup.transport.read(&conversations()).await.unwrap_err();

    assert!(matches!(error, HttpError::BodyTooLarge), "{error:?}");
}

#[tokio::test]
async fn without_a_token_nothing_is_sent() {
    let setup = setup_with(FakeTokens::signed_out()).await;
    let request = hello();

    let read = setup.transport.read(&conversations()).await.unwrap_err();
    let own = setup.transport.send(&probe()).await.unwrap_err();
    let write = Client::new(setup.transport).write(request).await.unwrap_err();

    assert!(matches!(read, HttpError::Credentials(_)), "{read:?}");
    assert!(matches!(own, HttpError::Credentials(_)), "{own:?}");
    assert!(matches!(write, HttpError::Credentials(_)), "{write:?}");
    assert!(setup.server.received_requests().await.unwrap().is_empty());
}

#[test]
fn the_account_session_is_the_transports_credentials() {
    fn credentials<C: Credentials>() {}
    credentials::<Session<MemorySecretStore>>();
}

#[test]
fn a_responses_debug_shows_the_bodys_length_only() {
    let answer = teams_proto::Response::new(200, "a private message");
    let text = format!("{answer:?}");
    assert!(!text.contains("private"), "{text}");
    assert!(text.contains("body_len: 17"), "{text}");
}

#[tokio::test]
async fn a_plain_mutation_is_sent_without_a_permit_and_never_retried() {
    let setup = setup().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&setup.server)
        .await;
    assert!(matches!(
        setup.transport.send(&probe()).await,
        Err(HttpError::Status(_))
    ));
    assert_eq!(setup.server.received_requests().await.unwrap().len(), 1);
    assert_eq!(setup.tokens.rejected().len(), 1, "renewed only for a later request");
}
