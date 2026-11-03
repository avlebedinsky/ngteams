//! UPS's presence read on wiremock, through the `test-support` mock origin: as a plain request it goes out whoever it
//! names (a read reaches nobody), logged to `fence.log` as `allowed` first; as an entry of the personal build's sealed
//! `PostRead` list it goes out only with the permit the fence gave for its exact MRIs, in its captured shape (the
//! trailing slash, no query, the captured header values, a list of `{"mri": …}`), to the presence base the transport
//! was given, with the presence token, logged to `fence.log` first and sent once; any read of UPS goes to its base with
//! the trailing slash; its answer is read only when it names nobody but the accounts asked for.
//!
//! Tokens and MRIs here are placeholders; the answers follow the synthetic fixture
//! `tests/fixtures/ups/getpresence-answer.json`.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_fence::{Fence, FenceLog, Mri, Roster};
use teams_proto::{
    AnswerError, BuildError, Client, Credentials, Endpoint, HttpError, HttpTransport, Method, PRESENCE_AUDIENCE,
    PostRead, PostReadError, PresenceRead, ProbePostRead, Read, Request as _, Service, ShortProfileLookup, Transport,
    UserPresence,
};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const LAB_A: &str = "8:orgid:lab-a";
const LAB_B: &str = "8:orgid:lab-b";
const LAB_C: &str = "8:orgid:lab-c";
const ECHO: &str = "28:echo-service";
const COWORKER: &str = "8:orgid:coworker";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const PRESENCE_BASE: &str = "https://teams.microsoft.com/ups/noam";
const GET_PRESENCE: &str = "/ups/noam/v1/presence/getpresence/";
const FIXTURE: &str = include_str!("../../../tests/fixtures/ups/getpresence-answer.json");

fn mris(texts: &[&str]) -> Vec<Mri> {
    texts.iter().copied().map(Mri::new).collect()
}

fn read_of(texts: &[&str]) -> PresenceRead {
    PresenceRead::new(mris(texts)).unwrap()
}

/// The fixture with its placeholders filled in.
fn fixture() -> Value {
    serde_json::from_str(
        &FIXTURE
            .replace("<guid:a>", "lab-a")
            .replace("<guid:b>", "lab-b")
            .replace("<guid:c>", "lab-c"),
    )
    .unwrap()
}

/// Placeholder tokens; records the audiences asked for.
#[derive(Default)]
struct FakeTokens {
    audiences: Mutex<Vec<String>>,
    rejected: Mutex<usize>,
}

impl FakeTokens {
    fn audiences(&self) -> Vec<String> {
        self.audiences.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(audience.to_owned());
        future::ready(Ok(Secret::new("placeholder-presence-token")))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        *self.rejected.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        self.bearer(audience)
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

    /// The requests the transport logged as sent (not the fence's decisions).
    fn sent(&self) -> Vec<Value> {
        self.lines()
            .into_iter()
            .filter(|line| line.get("channel").is_some())
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
    fence: Arc<Fence>,
    transport: HttpTransport<FakeTokens>,
}

async fn setup(presence: Option<&str>) -> Setup {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let log = SharedLog::default();
    let fence = Arc::new(Fence::new(
        Roster::new(mris(&[LAB_A, LAB_B, LAB_C]), mris(&[ECHO])),
        FenceLog::from_writer(log.clone()),
    ));
    let mut transport =
        HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &server.uri()).unwrap();
    if let Some(base) = presence {
        transport = transport.with_presence(base).unwrap();
    }
    Setup {
        server,
        tokens,
        log,
        fence,
        transport,
    }
}

impl Setup {
    async fn send(self, request: PresenceRead) -> (Result<Vec<UserPresence>, String>, Self) {
        let permit = self.fence.check_post_read(PresenceRead::KIND, request.names()).unwrap();
        let client = Client::new(self.transport);
        let answer = client.post_read(request.clone(), permit).await;
        let read = match answer {
            Ok(answer) => request.read_answer(answer.body()).map_err(|error| error.to_string()),
            Err(error) => Err(format!("{error:?}")),
        };
        let Self {
            server,
            tokens,
            log,
            fence,
            ..
        } = self;
        let transport =
            HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &server.uri()).unwrap();
        (
            read,
            Self {
                server,
                tokens,
                log,
                fence,
                transport,
            },
        )
    }
}

fn header_of<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request.headers.get(name).and_then(|value| value.to_str().ok())
}

#[tokio::test]
async fn a_presence_read_goes_out_in_its_captured_shape_logged_first_and_its_answer_is_read() {
    let fixture = fixture();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        // the native read asks by MRI alone: the web client's `source` beside its own MRI isn't sent
        let asked: Vec<&str> = case["asked"]
            .as_array()
            .unwrap()
            .iter()
            .map(|ask| ask["mri"].as_str().unwrap())
            .collect();
        let body: Vec<Value> = asked.iter().map(|mri| json!({"mri": mri})).collect();
        let setup = setup(Some(PRESENCE_BASE)).await;
        let log = setup.log.clone();
        let answer = serde_json::to_vec(&case["answer"]).unwrap();
        let content_type = fixture["content_type"].as_str().unwrap().to_owned();
        Mock::given(method("POST"))
            .and(path(GET_PRESENCE))
            .and(header("authorization", "Bearer placeholder-presence-token"))
            .and(header("accept", "application/json"))
            .and(header("content-type", "application/json"))
            .and(header("behavioroverride", "redirectAs404"))
            .and(header("x-ms-client-type", "cdlworker"))
            .and(header("x-ms-client-user-agent", "Teams-V2-Web"))
            .and(header("x-ms-object-id", ""))
            .and(body_json(Value::Array(body)))
            .respond_with(move |_: &Request| {
                // the answer is the capture's only if the request's record was already in fence.log when it arrived
                let logged = log.sent().iter().any(|line| line["post_read"] == "presence");
                ResponseTemplate::new(if logged { 200 } else { 500 }).set_body_raw(answer.clone(), &content_type)
            })
            .expect(1)
            .mount(&setup.server)
            .await;

        let (read, setup) = setup.send(read_of(&asked)).await;

        let presences = read.unwrap();
        assert_eq!(presences.len(), asked.len(), "{name}");
        for (presence, mri) in presences.iter().zip(&asked) {
            assert_eq!(presence.mri.as_str(), *mri, "{name}");
            assert_eq!(presence.availability.as_deref(), Some("Offline"), "{name}");
            assert_eq!(presence.activity.as_deref(), Some("Offline"), "{name}");
        }
        assert_eq!(setup.tokens.audiences(), [PRESENCE_AUDIENCE], "{name}");
        assert_eq!(PRESENCE_AUDIENCE, "https://presence.teams.microsoft.com/");
        let lines = setup.log.lines();
        assert_eq!(
            lines,
            [
                json!({
                    "ts_ms": lines[0]["ts_ms"],
                    "verdict": "allow",
                    "event": "post_read_decision",
                    "post_read": "presence",
                    "names": asked.len(),
                }),
                json!({
                    "ts_ms": lines[1]["ts_ms"],
                    "verdict": "allow",
                    "channel": "http",
                    "method": "POST",
                    "url": format!("{PRESENCE_BASE}/v1/presence/getpresence/"),
                    "post_read": "presence",
                }),
            ],
            "{name}: the decision, then the request as built and checked (not the mock's)"
        );
    }
}

#[tokio::test]
async fn a_presence_read_carries_nothing_its_entry_doesnt_name() {
    let setup = setup(Some(PRESENCE_BASE)).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&setup.server)
        .await;

    let (read, setup) = setup.send(read_of(&[LAB_B, LAB_C])).await;

    assert_eq!(read.unwrap(), Vec::new());
    let requests = setup.server.received_requests().await.unwrap();
    let sent = &requests[0];
    for name in [
        "cookie",
        "origin",
        "referer",
        "x-ms-endpoint-id",
        "x-ms-client-caller",
        "x-ms-correlation-id",
        "x-ms-session-id",
        "x-ms-request-id",
        "x-ms-migration",
    ] {
        assert_eq!(header_of(sent, name), None, "{name}");
    }
    assert_eq!(sent.url.path(), GET_PRESENCE, "the trailing slash");
    assert_eq!(sent.url.query(), None, "no query");
    assert_eq!(
        serde_json::from_slice::<Value>(&sent.body).unwrap(),
        json!([{"mri": LAB_B}, {"mri": LAB_C}])
    );
}

#[tokio::test]
async fn anything_the_presence_entry_doesnt_name_is_unknown_and_nothing_goes_out() {
    let probe = || ProbePostRead::presence(read_of(&[LAB_B]));
    let probes = [
        ("a query pair", probe().with_query("source", "ups")),
        (
            "the client's endpoint id",
            probe().with_header("x-ms-endpoint-id", "endpoint"),
        ),
        ("a caller", probe().with_header("x-ms-client-caller", "probe")),
        ("a lookup's flag", probe().with_header("x-ms-migration", "True")),
        (
            "a pinned header with another value too",
            probe().with_header("x-ms-object-id", "lab-b"),
        ),
        (
            "the web client's source",
            probe().with_body(json!([{"mri": LAB_B, "source": "ups"}])),
        ),
        (
            "another key",
            probe().with_body(json!([{"mri": LAB_B, "endpointId": "e"}])),
        ),
        ("the key in another case", probe().with_body(json!([{"MRI": LAB_B}]))),
        ("a lookup's body", probe().with_body(json!([LAB_B]))),
        ("an empty list", probe().with_body(json!([]))),
        ("other MRIs than the permit", probe().with_body(json!([{"mri": LAB_C}]))),
        (
            "more MRIs than the permit",
            probe().with_body(json!([{"mri": LAB_B}, {"mri": LAB_C}])),
        ),
    ];
    for (name, probe) in probes {
        let setup = setup(Some(PRESENCE_BASE)).await;
        let permit = setup.fence.check_post_read(ProbePostRead::KIND, probe.names()).unwrap();

        let error = Client::new(setup.transport).post_read(probe, permit).await.unwrap_err();

        // anything off the captured shape is refused when it is read back
        assert!(
            matches!(error, PostReadError::Transport(HttpError::Unknown)),
            "{name}: {error:?}"
        );
        assert!(setup.server.received_requests().await.unwrap().is_empty(), "{name}");
        assert!(setup.tokens.audiences().is_empty(), "{name}: no token asked for");
        assert!(setup.log.sent().is_empty(), "{name}: no request logged");
    }
    // and with nothing added, it goes out
    let setup = setup(Some(PRESENCE_BASE)).await;
    Mock::given(method("POST"))
        .and(path(GET_PRESENCE))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&setup.server)
        .await;
    let probe = probe();
    let permit = setup.fence.check_post_read(ProbePostRead::KIND, probe.names()).unwrap();
    Client::new(setup.transport).post_read(probe, permit).await.unwrap();
}

#[tokio::test]
async fn a_read_naming_anyone_gets_a_permit_since_it_reaches_nobody() {
    let setup = setup(Some(PRESENCE_BASE)).await;
    let cases: [&[&str]; 3] = [&[LAB_B, COWORKER], &[ECHO], &[COWORKER, ECHO]];
    for names in cases {
        let request = read_of(names);
        let permit = setup
            .fence
            .check_post_read(PresenceRead::KIND, request.names())
            .unwrap();
        assert_eq!(permit.names(), request.names(), "{names:?}");
    }
    assert!(PresenceRead::new(Vec::new()).is_none(), "a read names someone");
    assert!(setup.log.sent().is_empty());
}

#[tokio::test]
async fn a_permit_for_another_entry_or_other_mris_sends_nothing() {
    let setup = setup(Some(PRESENCE_BASE)).await;
    let lookup = ShortProfileLookup::new(mris(&[LAB_B])).unwrap();
    let cases = [
        (
            "a profile lookup's permit",
            setup
                .fence
                .check_post_read(ShortProfileLookup::KIND, lookup.names())
                .unwrap(),
        ),
        (
            "other MRIs",
            setup
                .fence
                .check_post_read(PresenceRead::KIND, &mris(&[LAB_C]))
                .unwrap(),
        ),
        (
            "another order",
            setup
                .fence
                .check_post_read(PresenceRead::KIND, &mris(&[LAB_C, LAB_B]))
                .unwrap(),
        ),
    ];
    let client = Client::new(setup.transport);
    for (name, permit) in cases {
        let request = if name == "another order" {
            read_of(&[LAB_B, LAB_C])
        } else {
            read_of(&[LAB_B])
        };
        let error = client.post_read(request, permit).await.unwrap_err();
        assert!(matches!(error, PostReadError::Mismatch(_)), "{name}: {error:?}");
    }
    assert!(setup.server.received_requests().await.unwrap().is_empty());
    assert!(setup.tokens.audiences().is_empty());
}

#[tokio::test]
async fn without_a_presence_base_nothing_goes_to_ups() {
    let setup = setup(None).await;
    let (read, setup) = setup.send(read_of(&[LAB_B])).await;

    let error = read.unwrap_err();
    assert!(error.contains("NoPresence"), "{error}");
    assert!(setup.server.received_requests().await.unwrap().is_empty());
    assert!(setup.log.sent().is_empty());
}

#[tokio::test]
async fn a_refused_or_throttled_read_isnt_sent_again() {
    for status in [401_u16, 429, 503] {
        let setup = setup(Some(PRESENCE_BASE)).await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).insert_header("Retry-After", "7"))
            .expect(1)
            .mount(&setup.server)
            .await;

        let (read, setup) = setup.send(read_of(&[LAB_B])).await;

        let error = read.unwrap_err();
        assert!(error.contains("Status"), "{status}: {error}");
        assert_eq!(setup.log.sent().len(), 1, "{status}: sent once, under its one permit");
        let refreshed = *setup.tokens.rejected.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(refreshed, usize::from(status == 401), "{status}");
    }
}

#[test]
fn the_presence_base_is_an_afd_host_and_a_geo_only() {
    let tokens = Arc::new(FakeTokens::default());
    let fence = Arc::new(Fence::new(Roster::new([], []), FenceLog::from_writer(io::sink())));
    let build = |base: &str| {
        HttpTransport::new(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE)
            .unwrap()
            .with_presence(base)
    };
    for base in [
        "https://presence.teams.microsoft.com",
        "https://presence.teams.microsoft.com/ups/noam",
        "https://evil.example.com/ups/noam",
        "https://teams.microsoft.com.evil.example.com/ups/noam",
        "http://teams.microsoft.com/ups/noam",
        "https://teams.microsoft.com:8443/ups/noam",
        "https://user@teams.microsoft.com/ups/noam",
        "https://teams.microsoft.com/ups/noam?x=1",
        "https://teams.microsoft.com/ups/noam#x",
        "https://teams.microsoft.com/ups/NOAM",
        "https://teams.microsoft.com/ups/no-am",
        "https://teams.microsoft.com/ups",
        "https://teams.microsoft.com/ups/noam/v1",
        "https://teams.microsoft.com/api/ups/noam",
        " https://teams.microsoft.com/ups/noam",
        "",
    ] {
        assert!(matches!(build(base), Err(BuildError::PresenceBase)), "{base:?}");
        assert!(!HttpTransport::<FakeTokens>::accepts_presence_base(base), "{base:?}");
    }
    for base in [
        "https://teams.microsoft.com/ups/noam",
        "https://teams.microsoft.com/ups/noam/",
        "https://teams.cloud.microsoft/ups/emea",
    ] {
        assert!(build(base).unwrap().has_presence(), "{base}");
    }
    assert!(!HttpTransport::new(tokens, fence, CHAT_BASE).unwrap().has_presence());
}

#[tokio::test]
async fn a_read_goes_to_the_base_it_was_given() {
    let setup = setup(Some("https://teams.cloud.microsoft/ups/emea/")).await;
    Mock::given(method("POST"))
        .and(path("/ups/emea/v1/presence/getpresence/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&setup.server)
        .await;

    let (read, setup) = setup.send(read_of(&[LAB_B])).await;

    read.unwrap();
    assert_eq!(
        setup.log.sent()[0]["url"],
        "https://teams.cloud.microsoft/ups/emea/v1/presence/getpresence/"
    );
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
async fn a_read_of_ups_goes_to_its_base_with_the_trailing_slash() {
    let setup = setup(Some(PRESENCE_BASE)).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&setup.server)
        .await;
    for segments in [vec!["v1", "me", "endpoints"], vec!["v1", "pubsub", "subscriptions"]] {
        setup
            .transport
            .read(&Get(Service::Presence, Endpoint::new(segments)))
            .await
            .unwrap();
    }
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
        ["/ups/noam/v1/me/endpoints/", "/ups/noam/v1/pubsub/subscriptions/"]
    );
    assert!(setup.log.lines().is_empty(), "a read isn't logged");
}

#[tokio::test]
async fn the_presence_read_as_a_plain_request_goes_whoever_it_names_logged_as_allowed() {
    let setup = setup(Some(PRESENCE_BASE)).await;
    Mock::given(method("POST"))
        .and(path(GET_PRESENCE))
        .and(header("x-ms-client-type", "cdlworker"))
        .and(body_json(json!([{"mri": LAB_B}, {"mri": COWORKER}])))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&setup.server)
        .await;
    let read = read_of(&[LAB_B, COWORKER]);
    assert_eq!(read.method(), Method::Post);

    setup.transport.send(&read).await.unwrap();

    let lines = setup.log.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["event"], "allowed");
    assert_eq!(lines[0]["method"], "POST");
    assert_eq!(lines[0]["host"], "teams.microsoft.com");
    assert_eq!(lines[0]["path"], "/ups/noam/v1/presence/getpresence/");
    assert_eq!(setup.tokens.audiences(), [PRESENCE_AUDIENCE]);
}

#[test]
fn an_answer_naming_anyone_not_asked_for_is_dropped_whole() {
    let fixture = fixture();
    let of_three = serde_json::to_vec(&fixture["cases"][2]["answer"]).unwrap();
    assert_eq!(
        read_of(&[LAB_B]).read_answer(&of_three),
        Err(AnswerError::Unasked { count: 2 })
    );
    assert_eq!(
        read_of(&[LAB_A, LAB_B, LAB_C])
            .read_answer(&of_three)
            .map(|presences| presences.len()),
        Ok(3)
    );
    for malformed in [&b"{}"[..], b"[{}]", b"[{\"mri\": 7}]", b"[1]", b"not json", b""] {
        assert_eq!(read_of(&[LAB_A]).read_answer(malformed), Err(AnswerError::Malformed));
    }
}

#[test]
fn an_item_without_the_captured_success_gives_no_availability_and_unseen_words_are_kept() {
    let item = |status: Value, presence: Value| {
        let mut item = json!({"mri": LAB_B, "status": status});
        if !presence.is_null() {
            item["presence"] = presence;
        }
        serde_json::to_vec(&json!([item])).unwrap()
    };
    let busy = json!({"availability": "Busy", "activity": "InACall"});
    let read = read_of(&[LAB_B]);
    let cases = [
        (item(json!(20000), busy.clone()), Some("Busy"), Some("InACall")),
        (item(json!(40400), busy.clone()), None, None),
        (item(Value::Null, busy), None, None),
        (item(json!(20000), Value::Null), None, None),
        (
            item(json!(20000), json!({"availability": "SomethingNew"})),
            Some("SomethingNew"),
            None,
        ),
    ];
    for (body, availability, activity) in cases {
        let presences = read.read_answer(&body).unwrap();
        assert_eq!(presences[0].availability.as_deref(), availability);
        assert_eq!(presences[0].activity.as_deref(), activity);
    }
}
