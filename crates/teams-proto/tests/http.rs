//! The native HTTP transport on wiremock, reached through the `test-support` mock origin: reads, a permitted write and
//! the answer it returns, an own-state request and its `fence.log` record, the transport's part of the retry table, and
//! what is refused.
//!
//! Tokens, MRIs and conversation ids here are placeholders, never real ones.

use std::future::{self, Future};
use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::Session;
use teams_auth::token::TokenError;
use teams_fence::{Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, Roster, Scope};
use teams_proto::{
    Client, Credentials, Endpoint, HttpError, HttpTransport, MAX_BODY, Mutating as _, ProbeOwnState, Read, SendMessage,
    Service, StartCall, Transport,
};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const LAB_A: &str = "8:orgid:lab-a";
const LAB_B: &str = "8:orgid:lab-b";
const ECHO: &str = "28:echo-service";
const CHAT: &str = "19:lab-chat@thread.v2";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const MESSAGES: &str = "/api/chatsvc/amer/v1/users/ME/conversations/19:lab-chat@thread.v2/messages";
const CONVERSATIONS: &str = "/api/chatsvc/amer/v1/users/ME/conversations";

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
    signed_out: bool,
}

impl FakeTokens {
    fn signed_out() -> Self {
        Self {
            signed_out: true,
            ..Self::default()
        }
    }

    fn rejected(&self) -> Vec<String> {
        self.rejected.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn audiences(&self) -> Vec<String> {
        self.audiences.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn current(&self, audience: &str) -> Result<Secret, TokenError> {
        if self.signed_out {
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

/// A `fence.log` the test can read back.
#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl SharedLog {
    #[cfg(test)]
    fn lines(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Write for SharedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A `fence.log` that can't be written.
struct BrokenLog;

impl Write for BrokenLog {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("disk full"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("disk full"))
    }
}

/// Answers every fetch with lab accounts only.
struct LabOnly;

impl MembershipSource for LabOnly {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(async { Membership::Complete(vec![Mri::new(LAB_A), Mri::new(LAB_B)]) })
    }
}

fn fence(log: impl Write + Send + 'static) -> Arc<Fence> {
    Arc::new(Fence::new(
        Roster::new([Mri::new(LAB_A), Mri::new(LAB_B)], [Mri::new(ECHO)]),
        FenceLog::from_writer(log),
    ))
}

struct Setup {
    server: MockServer,
    tokens: Arc<FakeTokens>,
    log: SharedLog,
    transport: HttpTransport<FakeTokens>,
}

async fn setup() -> Setup {
    setup_with(FakeTokens::default()).await
}

#[cfg(test)]
async fn setup_with(tokens: FakeTokens) -> Setup {
    let server = MockServer::start().await;
    let tokens = Arc::new(tokens);
    let log = SharedLog::default();
    let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), fence(log.clone()), CHAT_BASE, &server.uri())
        .unwrap()
        .with_max_retry_wait(Duration::from_millis(50));
    Setup {
        server,
        tokens,
        log,
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
    assert!(setup.log.lines().is_empty(), "a read isn't an own-state request");
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
    assert_eq!(header_of(to_registrar, "behavioroverride"), None);
    assert_eq!(to_trouter.url.path(), "/");
    assert_eq!(to_trouter.url.query(), Some("check=1"));
    assert_eq!(header_of(to_trouter, "authorization"), None);
    assert_eq!(header_of(to_trouter, "x-ms-migration"), None);
}

#[tokio::test]
async fn a_permitted_write_is_posted_in_its_wire_form_and_its_answer_comes_back() {
    let setup = setup().await;
    Mock::given(method("POST"))
        .and(path(MESSAGES))
        .and(header("authorization", bearer(0).as_str()))
        .and(header("x-ms-migration", "True"))
        .and(header("behavioroverride", "redirectAs404"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({
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
    let request = SendMessage::new(CHAT, "<p>hello</p>");
    let permit = setup.transport_fence().check(request.target(), &LabOnly).await.unwrap();

    let answer = Client::new(setup.transport).write(request, permit).await.unwrap();

    assert_eq!(answer.status(), 201);
    assert_eq!(
        answer.location(),
        Some("https://teams.microsoft.com/api/chatsvc/amer/v1/messages/1001")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(answer.body()).unwrap(),
        json!({"OriginalArrivalTime": 1_760_000_000_000_u64})
    );
    let lines = setup.log.lines();
    assert_eq!(lines.len(), 1, "only the fence's decision: {lines:?}");
    assert_eq!(lines[0]["verdict"], "allow");
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
    let request = SendMessage::new(CHAT, "hello");
    let permit = setup.transport_fence().check(request.target(), &LabOnly).await.unwrap();

    let answer = setup
        .transport
        .deliver(teams_proto::Authorized::new(request, permit).unwrap())
        .await
        .unwrap();

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
    let request = SendMessage::new(CHAT, "hello");
    let permit = setup.transport_fence().check(request.target(), &LabOnly).await.unwrap();

    let answer = Client::new(setup.transport).write(request, permit).await.unwrap();

    assert_eq!(answer.status(), 429);
    assert_eq!(answer.retry_after(), Some(Duration::from_secs(7)));
}

#[tokio::test]
async fn a_write_the_transport_has_no_route_for_sends_nothing() {
    let setup = setup().await;
    let call = StartCall::test_call(Mri::new(LAB_A), Mri::new(ECHO));
    let permit = setup.transport_fence().check(call.target(), &LabOnly).await.unwrap();

    let error = Client::new(setup.transport).write(call, permit).await.unwrap_err();

    assert!(
        matches!(error, teams_proto::WriteError::Transport(HttpError::NoRoute)),
        "{error:?}"
    );
    assert!(setup.server.received_requests().await.unwrap().is_empty());
}

fn probe() -> ProbeOwnState {
    ProbeOwnState::new(
        Service::ChatService,
        Endpoint::new(["v1", "users", "ME", "properties"]).with_query("name", "probe"),
        Some(json!({"probe": true})),
    )
}

#[tokio::test]
async fn an_own_state_request_is_logged_before_it_is_sent() {
    let setup = setup().await;
    let log = setup.log.clone();
    // the answer is a success only if the request's record was already in fence.log when it arrived
    Mock::given(method("POST"))
        .and(path("/api/chatsvc/amer/v1/users/ME/properties"))
        .and(query_param("name", "probe"))
        .and(header("authorization", bearer(0).as_str()))
        .and(body_json(json!({"probe": true})))
        .respond_with(move |_: &Request| {
            let logged = log.lines().iter().any(|line| line["own"] == "test_probe");
            ResponseTemplate::new(if logged { 200 } else { 500 })
        })
        .expect(1)
        .mount(&setup.server)
        .await;

    let answer = Client::new(setup.transport).own_state(&probe()).await.unwrap();

    assert_eq!(answer.status(), 200);
    assert_eq!(
        setup.log.lines(),
        [json!({
            "ts_ms": setup.log.lines()[0]["ts_ms"],
            "verdict": "allow",
            "channel": "http",
            "method": "POST",
            "url": "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/properties",
            "own": "test_probe",
        })],
        "the URL as built and checked (not the mock's), without its query"
    );
}

#[tokio::test]
async fn an_own_state_request_that_cant_be_logged_isnt_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let transport = HttpTransport::with_mock_origin(
        Arc::new(FakeTokens::default()),
        fence(BrokenLog),
        CHAT_BASE,
        &server.uri(),
    )
    .unwrap();

    let error = transport.own_state(&probe()).await.unwrap_err();

    assert!(matches!(error, HttpError::LogUnavailable(_)), "{error:?}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_own_state_retry_is_logged_again() {
    let setup = setup().await;
    Mock::given(method("POST"))
        .and(header("authorization", bearer(0).as_str()))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&setup.server)
        .await;
    Mock::given(method("POST"))
        .and(header("authorization", bearer(1).as_str()))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&setup.server)
        .await;

    let answer = setup.transport.own_state(&probe()).await.unwrap();

    assert_eq!(answer.status(), 204);
    assert_eq!(setup.log.lines().len(), 2, "one record per send");
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
    let request = SendMessage::new(CHAT, "hello");
    let permit = setup.transport_fence().check(request.target(), &LabOnly).await.unwrap();

    let read = setup.transport.read(&conversations()).await.unwrap_err();
    let own = setup.transport.own_state(&probe()).await.unwrap_err();
    let write = Client::new(setup.transport).write(request, permit).await.unwrap_err();

    assert!(matches!(read, HttpError::Credentials(_)), "{read:?}");
    assert!(matches!(own, HttpError::Credentials(_)), "{own:?}");
    assert!(
        matches!(write, teams_proto::WriteError::Transport(HttpError::Credentials(_))),
        "{write:?}"
    );
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

impl Setup {
    /// A fence over the same roster as the transport's, for permits (its log is separate).
    fn transport_fence(&self) -> Arc<Fence> {
        fence(self.log.clone())
    }
}
