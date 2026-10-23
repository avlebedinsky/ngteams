//! The account's own availability (M2 Task 19) on wiremock, through the `test-support` mock origin: UPS's presence
//! endpoint `PUT` and the manual status (a set of each captured value, and the reset with no body), each a sealed write
//! on the account itself that goes out only under the fence's permit for the fence's own account, in the captured
//! shape (`tests/fixtures/ups/endpoint-requests.json`, `manual-status-requests.json`): `PUT` on the presence base with
//! the trailing slash and no query, the presence token, the captured header values, `x-ms-endpoint-id` and the body
//! naming only the endpoint a registration of this run registered (`teams_proto::register`, the only way to get a
//! `RegisteredEndpoint`).
//!
//! Tokens, ids and MRIs here are placeholders.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_fence::{DenyReason, FailClosed, Fence, FenceLog, Mri, Roster, Scope, Target, WriteKind};
use teams_proto::{
    Client, Credentials, EndpointRegistration, ForceAvailability, HttpError, HttpTransport, ManualAvailability,
    Mutating, PRESENCE_AUDIENCE, Read, Refusal, RegisteredEndpoint, Request as ProtoRequest, Response,
    SetEndpointAvailability, Transport, WriteError, register,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const LAB_A: &str = "8:orgid:lab-a";
const LAB_B: &str = "8:orgid:lab-b";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const PRESENCE_BASE: &str = "https://teams.microsoft.com/ups/noam";
const ENDPOINTS: &str = "/ups/noam/v1/me/endpoints/";
const FORCE: &str = "/ups/noam/v1/me/forceavailability/";
const REGISTRAR: &str = "/registrar/prod/V2/registrations";
const EPID: &str = "00000000-0000-4000-8000-0000000000e1";
const PUSH_URL: &str = "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000A/";
const ENDPOINT_FIXTURE: &str = include_str!("../../../tests/fixtures/ups/endpoint-requests.json");
const MANUAL_FIXTURE: &str = include_str!("../../../tests/fixtures/ups/manual-status-requests.json");
/// The headers HTTP itself adds, not the client's choice.
const TRANSPORT_HEADERS: [&str; 3] = ["host", "content-length", "accept-encoding"];

/// A fixture with its endpoint placeholder filled in with [`EPID`].
fn fixture(text: &str) -> Value {
    serde_json::from_str(&text.replace("<endpoint>", EPID)).unwrap()
}

/// Placeholder tokens; records the audiences asked for and the refused ones.
#[derive(Default)]
struct FakeTokens {
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
}

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(audience.to_owned());
        future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.rejected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(audience.to_owned());
        future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
    }
}

/// A `fence.log` the test reads back.
#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl SharedLog {
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

struct Setup {
    server: MockServer,
    tokens: Arc<FakeTokens>,
    log: SharedLog,
    /// Lab account a's fence: its own account is a.
    fence: Arc<Fence>,
    transport: HttpTransport<FakeTokens>,
}

/// Account a's transport and fence, the transport given the presence base when `presence`.
async fn setup(presence: bool) -> Setup {
    setup_owning(presence, Some(LAB_A)).await
}

/// A transport and fence whose own account is `own` (none when `None`), given the presence base when `presence`.
async fn setup_owning(presence: bool, own: Option<&str>) -> Setup {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let log = SharedLog::default();
    let fence = Fence::new(
        Roster::new([Mri::new(LAB_A), Mri::new(LAB_B)], []),
        FenceLog::from_writer(log.clone()),
    );
    let fence = Arc::new(match own {
        Some(mri) => fence.with_own_account(Mri::new(mri)),
        None => fence,
    });
    let mut transport =
        HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &server.uri()).unwrap();
    if presence {
        transport = transport.with_presence(PRESENCE_BASE).unwrap();
    }
    Setup {
        server,
        tokens,
        log,
        fence,
        transport,
    }
}

/// Registers [`EPID`] through `setup`'s transport, the registrar taking it, and gives the endpoint it registered.
async fn registered(setup: &Setup) -> RegisteredEndpoint {
    let mock = Mock::given(method("POST"))
        .and(path(REGISTRAR))
        .respond_with(ResponseTemplate::new(202))
        .up_to_n_times(1)
        .mount_as_scoped(&setup.server)
        .await;
    let registration = EndpointRegistration::messaging(EPID, PUSH_URL).unwrap();
    let (answer, endpoint) = register(&setup.transport, &registration).await.unwrap();
    drop(mock);
    assert_eq!(answer.status(), 202);
    endpoint.expect("a registration the registrar took registers its endpoint")
}

fn header_of<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request.headers.get(name).and_then(|value| value.to_str().ok())
}

/// Every header name of `request` but those HTTP itself adds, lower-cased.
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

/// Checks `request` is an own-availability `PUT` in the captured shape of `fixture` (its header values those the
/// fixture pins; its header names among the fixture's, `Accept` asked as JSON as the transport asks every service),
/// at `want_path` with no query, with the presence token and `x-ms-endpoint-id` the registered endpoint.
fn assert_captured_shape(request: &Request, fixture: &Value, want_path: &str) {
    assert_eq!(request.method.as_str(), "PUT");
    assert_eq!(request.url.path(), want_path);
    assert_eq!(request.url.query(), None);
    let headers = fixture["headers"].as_object().unwrap();
    let allowed: Vec<String> = headers.keys().map(|name| name.to_ascii_lowercase()).collect();
    for name in header_names(request) {
        assert!(allowed.contains(&name), "a header no capture showed: {name}");
    }
    for pinned in [
        "content-type",
        "behavioroverride",
        "x-ms-client-type",
        "x-ms-client-user-agent",
        "x-ms-object-id",
    ] {
        assert_eq!(
            header_of(request, pinned),
            headers[pinned].as_str(),
            "{pinned} as captured"
        );
    }
    assert_eq!(header_of(request, "x-ms-endpoint-id"), Some(EPID));
    assert_eq!(header_of(request, "accept"), Some("application/json"));
    assert_eq!(
        header_of(request, "authorization"),
        Some(format!("Bearer placeholder-token-for-{PRESENCE_AUDIENCE}").as_str())
    );
    // nothing that names the client's components or the session, and no cookie
    for absent in ["x-ms-client-caller", "x-ms-session-id", "x-ms-correlation-id", "cookie"] {
        assert_eq!(header_of(request, absent), None, "{absent}");
    }
}

fn own_target(mri: &str) -> Target {
    Target::new(WriteKind::OwnAvailability, Scope::Account { mri: mri.to_owned() })
}

#[tokio::test]
async fn only_a_registration_the_registrar_took_registers_an_endpoint() {
    let setup = setup(true).await;
    let endpoint = registered(&setup).await;
    assert_eq!(endpoint.id(), EPID);
    assert_eq!(
        endpoint.account(),
        Some(&Mri::new(LAB_A)),
        "registered for the fence's own account"
    );
    assert_eq!(format!("{endpoint:?}"), format!("RegisteredEndpoint({EPID:?})"));
    assert_eq!(
        (&setup.log.lines()[0]["event"], &setup.log.lines()[0]["path"]),
        (&json!("allowed"), &json!("/registrar/prod/V2/registrations")),
        "the registration was logged as allowed before it went"
    );

    // the registrar refuses it: the transport answers the status, and nothing is registered
    Mock::given(method("POST"))
        .and(path(REGISTRAR))
        .respond_with(ResponseTemplate::new(403))
        .mount(&setup.server)
        .await;
    let registration = EndpointRegistration::messaging(EPID, PUSH_URL).unwrap();
    let refused = register(&setup.transport, &registration).await.unwrap_err();
    assert!(matches!(refused, HttpError::Status(_)), "{refused:?}");

    // a transport that answers a failure without an error registers nothing either (a fake transport registers at all
    // only under `test-support`: outside it `register` takes only the native transport, an `EndpointRegistrar`, as
    // fence-seal's `proto_register_fake_transport` case shows)
    let (answer, endpoint) = register(&Answering(503), &registration).await.unwrap();
    assert_eq!(answer.status(), 503);
    assert_eq!(endpoint, None);
    let (_, endpoint) = register(&Answering(202), &registration).await.unwrap();
    assert_eq!(endpoint.map(|endpoint| endpoint.id().to_owned()), Some(EPID.to_owned()));
}

/// A transport answering every plain request with one status; it is never asked anything else.
struct Answering(u16);

impl Transport for Answering {
    type Error = HttpError;

    fn read<R: Read>(&self, _: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        future::ready(Err(HttpError::NoRoute))
    }

    fn send<R: ProtoRequest>(&self, _: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        future::ready(Ok(Response::new(self.0, Vec::new())))
    }

    fn deliver<R: Mutating + Send>(
        &self,
        _: teams_proto::Authorized<R>,
    ) -> impl Future<Output = Result<Response, HttpError>> + Send {
        future::ready(Err(HttpError::NoRoute))
    }
}

#[tokio::test]
async fn the_endpoint_put_goes_out_in_its_captured_shape_under_the_fences_permit() {
    let setup = setup(true).await;
    let endpoint = registered(&setup).await;
    let fixture = fixture(ENDPOINT_FIXTURE);
    Mock::given(method("PUT"))
        .and(path(ENDPOINTS))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&setup.server)
        .await;
    let request = SetEndpointAvailability::available(Mri::new(LAB_A), &endpoint).unwrap();
    assert_eq!(request.target(), own_target(LAB_A));
    assert_eq!(request.endpoint(), &endpoint);
    assert_eq!(request.account(), &Mri::new(LAB_A));
    // the fence's permit, without a member read (the fail-closed source fetches nothing)
    let permit = setup.fence.check(request.target(), &FailClosed).await.unwrap();

    let answer = Client::new(setup.transport).write(request, permit).await.unwrap();

    assert_eq!(answer.status(), 201);
    let received = setup.server.received_requests().await.unwrap();
    let put = received
        .iter()
        .find(|request| request.method.as_str() == "PUT")
        .unwrap();
    assert_captured_shape(put, &fixture, ENDPOINTS);
    // the body is the captured one, key for key: the only pair a running client sent, Available/Available
    let want = &fixture["requests"][0]["body"];
    assert_eq!(serde_json::from_slice::<Value>(&put.body).unwrap(), *want);
    assert_eq!(want["id"], EPID);
    assert_eq!(header_of(put, "content-type"), Some("application/json"));
    let decisions: Vec<Value> = setup
        .log
        .lines()
        .into_iter()
        .filter(|line| line.get("kind").is_some())
        .collect();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0]["kind"], "own_availability");
    assert_eq!(decisions[0]["verdict"], "allow");
}

#[tokio::test]
async fn each_captured_manual_status_and_its_reset_go_out_in_their_captured_shape() {
    let fixture = fixture(MANUAL_FIXTURE);
    let cases: Vec<(Option<ManualAvailability>, &str)> = vec![
        (Some(ManualAvailability::Busy), "busy"),
        (Some(ManualAvailability::DoNotDisturb), "do not disturb"),
        (Some(ManualAvailability::Away), "appear away"),
        (Some(ManualAvailability::Available), "available"),
        (None, "reset"),
    ];
    assert_eq!(ManualAvailability::ALL.len(), 4, "every captured value, and no other");
    for (status, name) in cases {
        let setup = setup(true).await;
        let endpoint = registered(&setup).await;
        Mock::given(method("PUT"))
            .and(path(FORCE))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&setup.server)
            .await;
        let request = match status {
            Some(status) => ForceAvailability::set(Mri::new(LAB_A), &endpoint, status),
            None => ForceAvailability::reset(Mri::new(LAB_A), &endpoint),
        }
        .unwrap();
        assert_eq!(request.status(), status);
        assert_eq!(request.target(), own_target(LAB_A), "{name}");
        let permit = setup.fence.check(request.target(), &FailClosed).await.unwrap();

        let answer = Client::new(setup.transport).write(request, permit).await.unwrap();

        assert_eq!(answer.status(), 200, "{name}");
        let received = setup.server.received_requests().await.unwrap();
        let put = received
            .iter()
            .find(|request| request.method.as_str() == "PUT")
            .unwrap();
        assert_captured_shape(put, &fixture, FORCE);
        let captured = fixture["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|request| request["name"] == name)
            .unwrap();
        if status.is_some() {
            assert_eq!(
                serde_json::from_slice::<Value>(&put.body).unwrap(),
                captured["body"],
                "{name}"
            );
        } else {
            // the reset: no body at all, as captured, its Content-Type still sent
            assert_eq!(captured["body"], Value::Null);
            assert!(put.body.is_empty(), "the reset has no body");
            assert_eq!(header_of(put, "content-type"), Some("application/json"));
        }
    }
}

/// The three writes of `account`'s availability from `endpoint`, each `None` when it can't be built.
fn own_writes(account: &str, endpoint: &RegisteredEndpoint) -> [Option<Target>; 3] {
    let account = Mri::new(account);
    [
        SetEndpointAvailability::available(account.clone(), endpoint).map(|write| write.target()),
        ForceAvailability::set(account.clone(), endpoint, ManualAvailability::Busy).map(|write| write.target()),
        ForceAvailability::reset(account, endpoint).map(|write| write.target()),
    ]
}

#[tokio::test]
async fn an_endpoint_registered_for_one_account_names_no_other_accounts_availability() {
    // a's and b's transports, each registering through its own fence's account, and one whose fence owns none
    let a = setup_owning(true, Some(LAB_A)).await;
    let b = setup_owning(true, Some(LAB_B)).await;
    let none = setup_owning(true, None).await;
    let a_endpoint = registered(&a).await;
    let b_endpoint = registered(&b).await;
    let no_ones = registered(&none).await;
    assert_eq!(a_endpoint.account(), Some(&Mri::new(LAB_A)));
    assert_eq!(b_endpoint.account(), Some(&Mri::new(LAB_B)));
    assert_eq!(no_ones.account(), None);
    assert_eq!(
        a_endpoint.id(),
        b_endpoint.id(),
        "the same id, told apart by the account only"
    );

    // each account's own endpoint builds its own writes, and only those
    for (account, endpoint) in [(LAB_A, &a_endpoint), (LAB_B, &b_endpoint)] {
        for target in own_writes(account, endpoint) {
            assert_eq!(target, Some(own_target(account)), "{account}");
        }
    }
    // another account's endpoint, or one registered for no account, builds none
    for (account, endpoint) in [
        (LAB_A, &b_endpoint),
        (LAB_B, &a_endpoint),
        (LAB_A, &no_ones),
        (LAB_B, &no_ones),
    ] {
        assert_eq!(
            own_writes(account, endpoint),
            [None, None, None],
            "{account} from {endpoint:?}"
        );
    }
    for setup in [a, b, none] {
        let received = setup.server.received_requests().await.unwrap();
        assert!(received.iter().all(|request| request.method.as_str() != "PUT"));
    }
}

#[tokio::test]
async fn another_accounts_availability_is_denied_and_nothing_goes_out() {
    let setup = setup(true).await;
    // b's write, from the endpoint b's own transport registered
    let b = setup_owning(true, Some(LAB_B)).await;
    let b_endpoint = registered(&b).await;
    for target in own_writes(LAB_B, &b_endpoint) {
        let denied = setup.fence.check(target.unwrap(), &FailClosed).await.unwrap_err();
        assert_eq!(denied.reason(), &DenyReason::NotOwnAccount);
    }
    // a permit for a's availability doesn't cover b's
    let permit = setup.fence.check(own_target(LAB_A), &FailClosed).await.unwrap();
    let b_write = SetEndpointAvailability::available(Mri::new(LAB_B), &b_endpoint).unwrap();
    let error = Client::new(setup.transport).write(b_write, permit).await.unwrap_err();
    assert!(matches!(error, WriteError::TargetMismatch(_)), "{error:?}");
    for server in [&setup.server, &b.server] {
        let received = server.received_requests().await.unwrap();
        assert!(received.iter().all(|request| request.method.as_str() != "PUT"));
    }
}

#[tokio::test]
async fn without_the_presence_base_nothing_reaches_ups() {
    let with = setup(true).await;
    let endpoint = registered(&with).await;
    let setup = setup(false).await;
    let request = SetEndpointAvailability::available(Mri::new(LAB_A), &endpoint).unwrap();
    let permit = setup.fence.check(request.target(), &FailClosed).await.unwrap();

    let error = Client::new(setup.transport).write(request, permit).await.unwrap_err();

    assert!(
        matches!(error, WriteError::Transport(HttpError::Refused(Refusal::NoPresence))),
        "{error:?}"
    );
    assert!(setup.server.received_requests().await.unwrap().is_empty());
    assert!(setup.tokens.audiences().is_empty(), "no token asked for");
}

#[tokio::test]
async fn a_refused_token_is_renewed_and_the_write_not_sent_again() {
    let setup = setup(true).await;
    let endpoint = registered(&setup).await;
    Mock::given(method("PUT"))
        .and(path(FORCE))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&setup.server)
        .await;
    let request = ForceAvailability::set(Mri::new(LAB_A), &endpoint, ManualAvailability::Busy).unwrap();
    let permit = setup.fence.check(request.target(), &FailClosed).await.unwrap();
    let tokens = Arc::clone(&setup.tokens);

    let answer = Client::new(setup.transport).write(request, permit).await.unwrap();

    assert_eq!(answer.status(), 401, "the outbox decides what to do about it");
    assert_eq!(tokens.rejected(), [PRESENCE_AUDIENCE], "the presence token was renewed");
}
