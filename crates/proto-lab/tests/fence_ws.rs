//! The browser fence's WebSocket layer: the frame classifier, the guard script, the decision on one frame, the answer
//! to the guard and the audit of what was sent (against a recording endpoint and synthetic events), and, with Chrome
//! installed, enforcement on a local echo server: in the page, a dedicated, a shared and a service worker a denied
//! frame never reaches the server, an allowed one does, in order; and a frame that gets past the guard fails the run.
//!
//! The Chrome tests run headless on throwaway user-data-dirs against local servers; see `common` for how Chrome is
//! found.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use proto_lab::arming::{ArmOptions, ArmedBrowser, ArmedTarget, Step, TargetKind};
use proto_lab::browser::{Browser, LaunchOptions};
use proto_lab::cdp::{Cdp, Event, SessionId};
use proto_lab::classify::{Classification, Classifier, OwnKind};
use proto_lab::fence_http::{DenyCause, HttpFence, Verdict};
use proto_lab::fence_ws::{
    BINDING, BadToken, Frame, FrameClassifier, FrameRule, GuardRequest, GuardScript, Payload, RESOLVER, WsFence,
    classify_frame,
};
use serde_json::{Value, json};
use teams_fence::{
    DenyReason, Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, Roster, Scope, Target, WriteKind,
};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Callback, ErrorResponse, Request, Response};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Helpers return errors; the tests unwrap them.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
/// The same, for what a spawned task returns.
type SendResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const TROUTER: &str = "wss://go.trouter.teams.microsoft.com/v4/c?tc=planted-query";
const LOCAL: &str = "ws://127.0.0.1:9/page?tc=planted-query";

fn text(url: &str, data: &str) -> Frame {
    Frame {
        url: url.to_owned(),
        payload: Payload::Text(data.to_owned()),
    }
}

/// A local stand-in rule: text frames starting with `allow:` on any path of one local socket server.
fn local_rule(origin: &str) -> FrameRule {
    FrameRule::new(OwnKind::Authz, origin, "/*", "allow:")
}

// --- the frame classifier ---

#[test]
fn every_frame_is_unknown_on_the_seeded_list() {
    let frames = [
        text(TROUTER, "5:::{\"name\":\"ping\"}"),
        text(TROUTER, ""),
        text("wss://teams.microsoft.com/api/authsvc/v1.0/authz", "anything"),
        Frame {
            url: TROUTER.to_owned(),
            payload: Payload::Binary("AQID".to_owned()),
        },
        Frame {
            url: TROUTER.to_owned(),
            payload: Payload::Unreadable,
        },
        text("not a url", "allow:1"),
    ];
    for frame in &frames {
        assert_eq!(classify_frame(frame), Classification::Unknown, "{frame:?}");
        assert_eq!(FrameClassifier::seeded().classify(frame), Classification::Unknown);
    }
}

#[test]
fn an_own_rule_matches_its_origin_path_and_text_prefix_only() {
    let classifier = FrameClassifier::seeded().with_own(local_rule("ws://127.0.0.1:9"));
    let own = Classification::Own(OwnKind::Authz);
    assert_eq!(classifier.classify(&text(LOCAL, "allow:1")), own);
    assert_eq!(classifier.classify(&text("ws://127.0.0.1:9/x", "allow:")), own);

    let lookalikes = [
        text(LOCAL, "deny:allow:1"),
        text(LOCAL, "Allow:1"),
        text(LOCAL, ""),
        text("wss://127.0.0.1:9/page", "allow:1"),
        text("ws://127.0.0.1:10/page", "allow:1"),
        text("ws://localhost:9/page", "allow:1"),
        text("ws://127.0.0.1:9/page/deeper", "allow:1"),
        text("ws://127.0.0.1:9/", "allow:1"),
        text("http://127.0.0.1:9/page", "allow:1"),
        Frame {
            url: LOCAL.to_owned(),
            // "allow:1" as bytes: a binary frame matches no rule
            payload: Payload::Binary("YWxsb3c6MQ==".to_owned()),
        },
        Frame {
            url: LOCAL.to_owned(),
            payload: Payload::Unreadable,
        },
    ];
    for frame in &lookalikes {
        assert_eq!(classifier.classify(frame), Classification::Unknown, "{frame:?}");
    }
}

// --- the guard script and its requests ---

#[test]
fn a_guard_request_is_read_from_the_binding_payload() {
    let request =
        |kind: &str, data: &str| json!({"socket": 3, "seq": 7, "url": LOCAL, "kind": kind, "data": data}).to_string();
    let read = GuardRequest::from_payload(&request("text", "allow:1")).unwrap();
    assert_eq!((read.socket, read.seq), (3, 7));
    assert_eq!(read.frame, text(LOCAL, "allow:1"));
    let binary = GuardRequest::from_payload(&request("binary", "AQID")).unwrap();
    assert_eq!(binary.frame.payload, Payload::Binary("AQID".to_owned()));
    let unreadable = GuardRequest::from_payload(&request("unreadable", "")).unwrap();
    assert_eq!(unreadable.frame.payload, Payload::Unreadable);

    let malformed = [
        String::new(),
        "not json".to_owned(),
        request("blob", "x"),
        json!({"socket": 3, "seq": 7, "url": LOCAL, "kind": "text"}).to_string(),
        json!({"socket": -1, "seq": 7, "url": LOCAL, "kind": "text", "data": ""}).to_string(),
        json!({"socket": 3, "seq": 7, "url": LOCAL, "kind": "text", "data": "", "extra": 1}).to_string(),
    ];
    for payload in &malformed {
        assert!(GuardRequest::from_payload(payload).is_err(), "{payload}");
    }
}

#[test]
fn the_guard_script_carries_the_names_and_a_token_of_its_own() {
    let script = GuardScript::with_token("0123abcd").unwrap();
    let source = script.source();
    for placeholder in ["{{BINDING}}", "{{RESOLVER}}", "{{TOKEN}}"] {
        assert!(!source.contains(placeholder), "{placeholder} left in the script");
    }
    assert!(source.contains(&format!("'{BINDING}'")));
    assert!(source.contains(&format!("'{RESOLVER}'")));
    assert!(source.contains("'0123abcd'"));
    assert_eq!(
        script.verdict_expression(3, 7, true),
        format!("{RESOLVER}(\"0123abcd\", 3, 7, true)")
    );
    assert!(!format!("{script:?}").contains("0123abcd"), "Debug shows the token");

    for bad in ["", "0123ABCD", "01'+x+'", "token"] {
        assert_eq!(GuardScript::with_token(bad).unwrap_err(), BadToken, "{bad:?}");
    }
    let (first, second) = (GuardScript::generate().unwrap(), GuardScript::generate().unwrap());
    assert_ne!(first.source(), second.source(), "every run gets a token of its own");
    assert_ne!(
        first.verdict_expression(1, 1, true),
        second.verdict_expression(1, 1, true)
    );
}

#[test]
fn every_kind_of_target_gets_the_guard_and_the_audit_before_it_runs() {
    let kinds = [
        TargetKind::Page,
        TargetKind::Iframe,
        TargetKind::Worker,
        TargetKind::SharedWorker,
        TargetKind::ServiceWorker,
        TargetKind::Other("browser_ui".to_owned()),
    ];
    for kind in kinds {
        let steps = kind.steps();
        let position = |wanted| steps.iter().position(|step| *step == wanted);
        let resume = position(Step::Resume);
        let binding = position(Step::Binding).unwrap_or_else(|| panic!("{kind:?} has no binding"));
        let page_like = matches!(kind, TargetKind::Page | TargetKind::Iframe);
        let guard = if page_like {
            assert!(position(Step::Guard).is_none(), "{kind:?}");
            assert!(
                position(Step::Runtime) < Some(binding),
                "{kind:?}: new contexts need Runtime on"
            );
            assert!(position(Step::Page) < position(Step::GuardNewDocuments), "{kind:?}");
            position(Step::GuardNewDocuments)
        } else {
            assert!(position(Step::GuardNewDocuments).is_none(), "{kind:?}");
            position(Step::Guard)
        };
        assert!(Some(binding) < guard, "{kind:?}: the guard needs its binding");
        assert!(guard < resume, "{kind:?}: the guard goes in before the target runs");
        assert!(position(Step::Network) < resume, "{kind:?}: the audit needs Network on");
    }
}

// --- the decision on one frame ---

const LAB_A: &str = "8:orgid:lab-a";
const COWORKER: &str = "8:orgid:coworker";

/// A membership source that answers the same every time.
struct Fixed(Membership);

impl MembershipSource for Fixed {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        let membership = self.0.clone();
        Box::pin(async move { membership })
    }
}

struct Lab {
    _dir: tempfile::TempDir,
    log_path: PathBuf,
    fence: Arc<Fence>,
}

fn lab() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let log_path = dir.path().join("fence.log");
    let roster = Roster::new([Mri::new(LAB_A)], []);
    let fence = Arc::new(Fence::new(roster, FenceLog::open(&log_path)?));
    Ok(Lab {
        _dir: dir,
        log_path,
        fence,
    })
}

fn members(mris: &[&str]) -> Arc<Fixed> {
    Arc::new(Fixed(Membership::Complete(
        mris.iter().map(|&mri| Mri::new(mri)).collect(),
    )))
}

fn ws_fence(fence: &Arc<Fence>, classifier: FrameClassifier, member_list: &[&str]) -> TestResult<WsFence> {
    Ok(WsFence::new(
        classifier,
        Arc::clone(fence),
        members(member_list),
        GuardScript::generate()?,
    ))
}

fn log_entries(path: &Path) -> TestResult<Vec<Value>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Value>, _>>()?)
}

/// A log entry without its timestamp.
fn untimed(mut entry: Value) -> Value {
    if let Some(object) = entry.as_object_mut() {
        object.remove("ts_ms");
    }
    entry
}

#[tokio::test]
async fn an_unknown_frame_is_denied_kept_and_logged_without_its_query_or_content() {
    let lab = lab().unwrap();
    let ws = ws_fence(&lab.fence, FrameClassifier::seeded(), &[]).unwrap();

    let verdict = ws.decide(&text(TROUTER, "secret-content")).await;

    assert_eq!(verdict, Verdict::Deny(DenyCause::Unclassified));
    let denials = ws.denials();
    assert_eq!(denials.len(), 1);
    assert_eq!(denials[0].url, "wss://go.trouter.teams.microsoft.com/v4/c");
    assert_eq!(denials[0].cause, DenyCause::Unclassified);
    let entries = log_entries(&lab.log_path).unwrap();
    assert_eq!(
        entries.into_iter().map(untimed).collect::<Vec<_>>(),
        [json!({
            "verdict": "deny",
            "channel": "web_socket",
            "method": "SEND",
            "url": "wss://go.trouter.teams.microsoft.com/v4/c",
            "reason": "unclassified",
            "message": "no rule classifies this request",
        })]
    );
    let log = std::fs::read_to_string(&lab.log_path).unwrap();
    assert!(
        !log.contains("planted-query") && !log.contains("secret-content"),
        "{log}"
    );
}

/// A log sink that takes nothing.
struct Refuses;

impl std::io::Write for Refuses {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("disk full"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn an_own_frame_passes_logged_and_one_that_cannot_be_logged_is_denied() {
    let lab = lab().unwrap();
    let ws = ws_fence(
        &lab.fence,
        FrameClassifier::seeded().with_own(local_rule("ws://127.0.0.1:9")),
        &[],
    )
    .unwrap();
    assert_eq!(ws.decide(&text(LOCAL, "allow:1")).await, Verdict::Allow);
    assert!(ws.denials().is_empty());
    let entries = log_entries(&lab.log_path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["verdict"], "allow");
    assert_eq!(entries[0]["own"], "authz");
    assert_eq!(entries[0]["url"], "ws://127.0.0.1:9/page");

    let fence = Arc::new(Fence::new(
        Roster::new([Mri::new(LAB_A)], []),
        FenceLog::from_writer(Refuses),
    ));
    let ws = ws_fence(
        &fence,
        FrameClassifier::seeded().with_own(local_rule("ws://127.0.0.1:9")),
        &[],
    )
    .unwrap();
    assert_eq!(
        ws.decide(&text(LOCAL, "allow:1")).await,
        Verdict::Deny(DenyCause::LogUnavailable)
    );
    assert_eq!(ws.denials()[0].cause, DenyCause::LogUnavailable);
}

#[tokio::test]
async fn a_mapped_write_frame_goes_through_the_fence_with_a_fresh_membership() {
    let target = || {
        Target::new(
            WriteKind::Message,
            Scope::Conversation {
                id: "19:lab@thread.v2".to_owned(),
            },
        )
    };
    let lab = lab().unwrap();
    let lab_only = ws_fence(&lab.fence, FrameClassifier::seeded(), &[LAB_A]).unwrap();
    assert_eq!(
        lab_only.judge(Classification::Write(target()), TROUTER).await,
        Verdict::Allow
    );
    let with_coworker = ws_fence(&lab.fence, FrameClassifier::seeded(), &[LAB_A, COWORKER]).unwrap();
    assert_eq!(
        with_coworker.judge(Classification::Write(target()), TROUTER).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Outsiders { count: 1 }))
    );
    assert_eq!(with_coworker.denials().len(), 1);
    assert_eq!(log_entries(&lab.log_path).unwrap().len(), 2, "Fence::check logs both");
}

// --- answering the guard, and the audit ---

/// A debugging endpoint that answers every command with a guard's `true` and records them.
async fn recording_endpoint() -> TestResult<(String, Arc<Mutex<Vec<Value>>>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("ws://{}/devtools/browser/fake", listener.local_addr()?);
    let commands = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&commands);
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = tokio_tungstenite::accept_async(stream).await?;
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let command: Value = serde_json::from_str(text.as_str())?;
            let reply = json!({"id": command["id"], "result": {"result": {"type": "boolean", "value": true}}});
            recorded.lock().unwrap_or_else(PoisonError::into_inner).push(command);
            socket.send(Message::text(reply.to_string())).await?;
        }
        SendResult::Ok(())
    });
    Ok((url, commands))
}

fn binding_called(session: &str, context: i64, payload: &Value) -> Event {
    Event {
        session_id: Some(SessionId::new(session)),
        method: "Runtime.bindingCalled".to_owned(),
        params: json!({"name": BINDING, "payload": payload.to_string(), "executionContextId": context}),
    }
}

fn frame_sent(session: &str, socket: &str, opcode: u64, data: &str) -> Event {
    Event {
        session_id: Some(SessionId::new(session)),
        method: "Network.webSocketFrameSent".to_owned(),
        params: json!({"requestId": socket, "timestamp": 1.0, "response": {"opcode": opcode, "mask": true, "payloadData": data}}),
    }
}

fn socket_created(session: &str, socket: &str, url: &str) -> Event {
    Event {
        session_id: Some(SessionId::new(session)),
        method: "Network.webSocketCreated".to_owned(),
        params: json!({"requestId": socket, "url": url}),
    }
}

#[tokio::test]
async fn the_verdict_goes_back_to_the_context_that_asked_and_an_allow_covers_one_frame() {
    let lab = lab().unwrap();
    let ws = ws_fence(
        &lab.fence,
        FrameClassifier::seeded().with_own(local_rule("ws://127.0.0.1:9")),
        &[],
    )
    .unwrap();
    let (url, commands) = recording_endpoint().await.unwrap();
    let (cdp, _events) = Cdp::connect(&url).await.unwrap();
    let ask = |seq: u64, data: &str| json!({"socket": 2, "seq": seq, "url": LOCAL, "kind": "text", "data": data});

    let allowed = ws
        .handle_binding(&cdp, &binding_called("S1", 4, &ask(1, "allow:1")))
        .await;
    let denied = ws
        .handle_binding(&cdp, &binding_called("S1", 4, &ask(2, "deny:2")))
        .await;
    let other = Event {
        params: json!({"name": "somethingElse", "payload": "{}", "executionContextId": 4}),
        ..binding_called("S1", 4, &json!({}))
    };
    let ignored = ws.handle_binding(&cdp, &other).await;
    let malformed = ws
        .handle_binding(&cdp, &binding_called("S1", 4, &json!({"socket": 2})))
        .await;

    assert_eq!(allowed.unwrap(), Some(Verdict::Allow));
    assert_eq!(denied.unwrap(), Some(Verdict::Deny(DenyCause::Unclassified)));
    assert_eq!(ignored.unwrap(), None);
    assert_eq!(malformed.unwrap(), Some(Verdict::Deny(DenyCause::Malformed)));
    let commands = commands.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(
        commands.len(),
        2,
        "only the two decided frames are answered: {commands:?}"
    );
    for (command, (seq, allow)) in commands.iter().zip([(1, true), (2, false)]) {
        assert_eq!(command["method"], "Runtime.evaluate");
        assert_eq!(command["sessionId"], "S1");
        assert_eq!(command["params"]["contextId"], 4);
        assert_eq!(
            command["params"]["expression"],
            ws.script().verdict_expression(2, seq, allow)
        );
    }

    // the allow covers the one frame with its content, on its session
    ws.observe(&socket_created("S1", "ws-1", LOCAL));
    ws.observe(&frame_sent("S2", "ws-9", 1, "allow:1"));
    ws.observe(&frame_sent("S1", "ws-1", 2, "allow:1"));
    ws.observe(&frame_sent("S1", "ws-1", 1, "allow:1"));
    ws.observe(&frame_sent("S1", "ws-1", 1, "allow:1"));
    let unsanctioned = ws.unsanctioned();
    assert_eq!(unsanctioned.len(), 3, "{unsanctioned:?}");
    assert_eq!(unsanctioned[0].session, Some(SessionId::new("S2")), "another session");
    assert_eq!(unsanctioned[0].url, None, "its socket's creation wasn't seen");
    assert_eq!(unsanctioned[1].opcode, 2, "the same content as a binary frame");
    assert_eq!(unsanctioned[1].url.as_deref(), Some("ws://127.0.0.1:9/page"));
    assert_eq!(unsanctioned[2].opcode, 1, "a second frame on one allow");
    assert_eq!(unsanctioned[2].len, "allow:1".len());
}

#[tokio::test]
async fn a_frame_sent_without_an_allow_is_a_logged_breach() {
    let lab = lab().unwrap();
    let ws = ws_fence(&lab.fence, FrameClassifier::seeded(), &[]).unwrap();

    ws.observe(&socket_created("S1", "ws-1", TROUTER));
    ws.observe(&frame_sent("S1", "ws-1", 1, "secret-content"));
    ws.observe(&frame_sent("S1", "ws-1", 2, "AQID"));
    // the URL is kept until the socket closes
    ws.observe(&Event {
        method: "Network.webSocketClosed".to_owned(),
        ..socket_created("S1", "ws-1", TROUTER)
    });
    ws.observe(&frame_sent("S1", "ws-1", 1, "late"));

    let unsanctioned = ws.unsanctioned();
    let urls: Vec<Option<&str>> = unsanctioned.iter().map(|frame| frame.url.as_deref()).collect();
    let trouter = Some("wss://go.trouter.teams.microsoft.com/v4/c");
    assert_eq!(urls, [trouter, trouter, None]);
    let entries: Vec<Value> = log_entries(&lab.log_path).unwrap().into_iter().map(untimed).collect();
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries[0],
        json!({
            "verdict": "breach",
            "channel": "web_socket",
            "method": "SEND",
            "url": "wss://go.trouter.teams.microsoft.com/v4/c",
            "reason": "sent_without_allow",
            "message": "this left the browser without the fence's allow",
        })
    );
    assert_eq!(entries[2]["url"], "", "an unknown socket is logged without a URL");
    let log = std::fs::read_to_string(&lab.log_path).unwrap();
    assert!(
        !log.contains("planted-query") && !log.contains("secret-content"),
        "{log}"
    );
}

#[tokio::test]
async fn a_detached_session_takes_its_allows_with_it() {
    let lab = lab().unwrap();
    let ws = ws_fence(
        &lab.fence,
        FrameClassifier::seeded().with_own(local_rule("ws://127.0.0.1:9")),
        &[],
    )
    .unwrap();
    let (url, _commands) = recording_endpoint().await.unwrap();
    let (cdp, _events) = Cdp::connect(&url).await.unwrap();
    let ask = json!({"socket": 1, "seq": 1, "url": LOCAL, "kind": "text", "data": "allow:1"});
    ws.handle_binding(&cdp, &binding_called("S1", 1, &ask)).await.unwrap();
    ws.handle_binding(&cdp, &binding_called("S2", 1, &ask)).await.unwrap();

    ws.forget(&SessionId::new("S1"));
    ws.observe(&frame_sent("S2", "ws-2", 1, "allow:1"));
    assert!(ws.unsanctioned().is_empty(), "S2's allow stays");
    ws.observe(&frame_sent("S1", "ws-1", 1, "allow:1"));
    assert_eq!(ws.unsanctioned().len(), 1, "S1's allow went with it");
}

// --- enforcement in a real browser ---

const STEP: Duration = Duration::from_secs(15);

/// The local pages, under `/pages/`.
const PAGES: [(&str, &str, &str); 7] = [
    ("ws.html", include_str!("fixtures/pages/ws.html"), "text/html"),
    (
        "ws-scenario.js",
        include_str!("fixtures/pages/ws-scenario.js"),
        "text/javascript",
    ),
    (
        "ws-worker.js",
        include_str!("fixtures/pages/ws-worker.js"),
        "text/javascript",
    ),
    (
        "ws-shared-worker.js",
        include_str!("fixtures/pages/ws-shared-worker.js"),
        "text/javascript",
    ),
    (
        "ws-service-worker.js",
        include_str!("fixtures/pages/ws-service-worker.js"),
        "text/javascript",
    ),
    ("blank.html", include_str!("fixtures/pages/blank.html"), "text/html"),
    ("favicon.ico", "", "image/x-icon"),
];

async fn serve_pages() -> MockServer {
    let server = MockServer::start().await;
    for (name, body, content_type) in PAGES {
        Mock::given(method("GET"))
            .and(path(format!("/pages/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, content_type))
            .mount(&server)
            .await;
    }
    server
}

/// A local WebSocket server that echoes every message and records it as (path, message), text as it is and binary as
/// `bytes 1,2,3`.
struct EchoServer {
    origin: String,
    received: Arc<Mutex<Vec<(String, String)>>>,
}

impl EchoServer {
    async fn start() -> TestResult<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("ws://{}", listener.local_addr()?);
        let received = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&received);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(echo(stream, Arc::clone(&recorded)));
            }
        });
        Ok(Self { origin, received })
    }

    /// What arrived on one path, in order.
    fn received_on(&self, wanted: &str) -> Vec<String> {
        self.received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(path, _)| path == wanted)
            .map(|(_, message)| message.clone())
            .collect()
    }

    /// What arrived on one path once `count` messages have, or after [`STEP`].
    async fn received_once(&self, wanted: &str, count: usize) -> Vec<String> {
        let poll = async {
            while self.received_on(wanted).len() < count {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let _ = tokio::time::timeout(STEP, poll).await;
        self.received_on(wanted)
    }
}

/// Waits until a page shows the document at `path`, fully loaded (the `about:blank` before it is `complete` too).
async fn loaded(cdp: &Cdp, session: &SessionId, path: &str) -> TestResult {
    let expression = format!("location.pathname === '{path}' && document.readyState === 'complete' || null");
    evaluate_until(cdp, session, &expression).await?;
    Ok(())
}

/// Notes the path a socket was opened on.
struct PathOf<'a>(&'a mut String);

impl Callback for PathOf<'_> {
    fn on_request(self, request: &Request, response: Response) -> Result<Response, ErrorResponse> {
        request.uri().path().clone_into(self.0);
        Ok(response)
    }
}

async fn echo(stream: tokio::net::TcpStream, received: Arc<Mutex<Vec<(String, String)>>>) -> SendResult<()> {
    let mut path = String::new();
    let mut socket = tokio_tungstenite::accept_hdr_async(stream, PathOf(&mut path)).await?;
    while let Some(Ok(message)) = socket.next().await {
        let seen = match &message {
            Message::Text(text) => text.as_str().to_owned(),
            Message::Binary(bytes) => {
                let bytes: Vec<String> = bytes.iter().map(u8::to_string).collect();
                format!("bytes {}", bytes.join(","))
            }
            _ => continue,
        };
        received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((path.clone(), seen));
        socket.send(message).await?;
    }
    Ok(())
}

async fn evaluate_in(cdp: &Cdp, session: &SessionId, expression: &str, context: Option<i64>) -> TestResult<Value> {
    let mut params = json!({"expression": expression, "returnByValue": true, "awaitPromise": true});
    if let Some(context) = context {
        params["contextId"] = json!(context);
    }
    let result = cdp.call(Some(session), "Runtime.evaluate", params).await?;
    if let Some(exception) = result.get("exceptionDetails") {
        return Err(format!("{expression} threw: {exception}").into());
    }
    Ok(result["result"]["value"].clone())
}

/// Polls `expression` until it gives something other than `null`.
async fn evaluate_until(cdp: &Cdp, session: &SessionId, expression: &str) -> TestResult<Value> {
    let poll = async {
        loop {
            let value = evaluate_in(cdp, session, expression, None).await?;
            if !value.is_null() {
                return Ok(value);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(STEP, poll)
        .await
        .map_err(|_| format!("{expression} stayed null for {STEP:?}"))?
}

async fn navigate(cdp: &Cdp, page: &ArmedTarget, url: &str) -> TestResult {
    let navigated = cdp
        .call(Some(&page.session), "Page.navigate", json!({ "url": url }))
        .await?;
    if let Some(error) = navigated.get("errorText") {
        return Err(format!("navigating to {url}: {error}").into());
    }
    Ok(())
}

/// One armed run on a throwaway profile, with an own rule standing in for the echo server.
struct Run {
    armed: ArmedBrowser,
    lab: Lab,
    _profile: tempfile::TempDir,
}

/// Runs `body` on an armed browser whose frame classifier allows `allow:` text frames to `echo`, then closes the
/// browser whatever `body` returned. Without Chrome it does nothing.
async fn with_run<F>(pages: &MockServer, echo: &EchoServer, body: F) -> TestResult
where
    F: AsyncFnOnce(&Run) -> TestResult,
{
    let Some(chrome) = common::chrome_or_skip() else {
        return Ok(());
    };
    let profile = tempfile::tempdir()?;
    let browser = Browser::launch(&chrome, &LaunchOptions::new(profile.path().join("p")), STEP).await?;
    let lab = lab()?;
    let http = HttpFence::new(Classifier::seeded(), Arc::clone(&lab.fence), members(&[]));
    let ws = ws_fence(
        &lab.fence,
        FrameClassifier::seeded().with_own(local_rule(&echo.origin)),
        &[],
    )?;
    let options = ArmOptions {
        service_worker_origins: vec![pages.uri()],
    };
    let run = Run {
        armed: ArmedBrowser::arm(browser, http, ws, &options).await?,
        lab,
        _profile: profile,
    };
    let outcome = tokio::time::timeout(Duration::from_secs(90), body(&run)).await;
    run.armed.close(Duration::from_secs(10)).await?;
    outcome.map_err(|_| "the run took over 90 s")?
}

const SCOPES: [&str; 4] = ["page", "dedicated-worker", "shared-worker", "service-worker"];

/// What reaches the echo server from each scope, in order: the allowed frames only.
const ALLOWED: [&str; 4] = ["allow:1", "allow:3", "allow:5", "allow:done"];

#[tokio::test]
async fn frames_leave_only_on_allow_and_in_order_in_every_scope() {
    let pages = serve_pages().await;
    let echo = EchoServer::start().await.unwrap();
    with_run(&pages, &echo, async |run| {
        let cdp = run.armed.cdp();
        let page = run.armed.open_page(STEP).await?;
        navigate(cdp, &page, &format!("{}/pages/ws.html?ws={}", pages.uri(), echo.origin)).await?;
        let results = evaluate_until(
            cdp,
            &page.session,
            "window.results && Object.keys(window.results).length === 4 ? window.results : null",
        )
        .await?;

        for scope in SCOPES {
            let result = &results[scope];
            assert_eq!(result["echoes"], json!(ALLOWED), "{scope}: {result}");
            assert_eq!(
                echo.received_once(&format!("/{scope}"), ALLOWED.len()).await,
                ALLOWED,
                "{scope}"
            );
            assert_eq!(
                result["early"], "InvalidStateError",
                "{scope}: a send before open throws"
            );
            assert_eq!(
                result["hidden"],
                json!({"binding": "undefined", "stream": "undefined", "forged": false}),
                "{scope}"
            );
        }

        // each scope's two denied text frames, its bytes and its Blob, logged and kept without the query
        let mut denied: BTreeMap<String, usize> = BTreeMap::new();
        for denial in run.armed.frame_denials() {
            assert_eq!(denial.cause, DenyCause::Unclassified, "{denial:?}");
            *denied.entry(denial.url).or_default() += 1;
        }
        let expected: BTreeMap<String, usize> = SCOPES
            .iter()
            .map(|scope| (format!("{}/{scope}", echo.origin), 4))
            .collect();
        assert_eq!(denied, expected);
        let log = std::fs::read_to_string(&run.lab.log_path)?;
        assert!(!log.contains("planted-query") && !log.contains("deny:"), "{log}");
        let entries = log_entries(&run.lab.log_path)?;
        let count = |verdict: &str| entries.iter().filter(|entry| entry["verdict"] == verdict).count();
        assert_eq!(count("deny"), 4 * SCOPES.len(), "every deny is in fence.log");
        assert_eq!(count("allow"), ALLOWED.len() * SCOPES.len(), "and every allow");

        assert_eq!(run.armed.unsanctioned_frames(), [], "every frame sent had an allow");
        assert!(run.armed.denials().is_empty(), "{:?}", run.armed.denials());
        assert!(run.armed.failures().is_empty(), "{:?}", run.armed.failures());
        let failed = run.armed.verdict().unwrap_err();
        assert_eq!(
            failed.frame_denials.len(),
            4 * SCOPES.len(),
            "denied frames fail the run"
        );
        Ok(())
    })
    .await
    .unwrap();
}

/// The test plays the fence for a guard installed by hand: the binding is a function that keeps what the guard asks.
const PLAY_FENCE: &str = r"
    window.asked = [];
    window.__labFenceWs = (payload) => window.asked.push(JSON.parse(payload));
";

/// Opens a socket to `/guard` and sends four frames; the bytes are changed right after their send.
const SEND_FOUR: &str = r"
    new Promise((resolve, reject) => {
        const socket = new WebSocket(BASE + '/guard');
        window.socket = socket;
        socket.onerror = () => reject(new Error('the socket failed'));
        socket.onopen = () => {
            socket.send('first');
            socket.send('second');
            const bytes = new Uint8Array([1, 2, 3]);
            socket.send(bytes.subarray(1));
            bytes.fill(9);
            socket.send('fourth');
            resolve(window.asked.length);
        };
    })";

#[tokio::test]
async fn the_guard_replays_in_order_only_what_the_fence_allowed() {
    let Some(chrome) = common::chrome_or_skip() else {
        return;
    };
    let pages = serve_pages().await;
    let echo = EchoServer::start().await.unwrap();
    let profile = tempfile::tempdir().unwrap();
    let browser = Browser::launch(&chrome, &LaunchOptions::new(profile.path().join("p")), STEP)
        .await
        .unwrap();
    // the browser moves into the run: an error or the timeout drops it, which kills it
    let outcome = tokio::time::timeout(Duration::from_secs(60), async move {
        let (cdp, _events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
        let blank = format!("{}/pages/blank.html", pages.uri());
        let target = cdp.call(None, "Target.createTarget", json!({"url": blank})).await?;
        let attach = json!({"targetId": target["targetId"], "flatten": true});
        let attached = cdp.call(None, "Target.attachToTarget", attach).await?;
        let page = SessionId::new(attached["sessionId"].as_str().ok_or("no sessionId")?);
        loaded(&cdp, &page, "/pages/blank.html").await?;
        evaluate_in(&cdp, &page, PLAY_FENCE, None).await?;
        let script = GuardScript::with_token("0123abcd")?;
        assert_eq!(evaluate_in(&cdp, &page, script.source(), None).await?, "armed");
        let send = SEND_FOUR.replace("BASE", &format!("'{}'", echo.origin));
        assert_eq!(
            evaluate_in(&cdp, &page, &send, None).await?,
            4,
            "every frame asks first"
        );
        let asked = evaluate_in(&cdp, &page, "window.asked", None).await?;
        let url = format!("{}/guard", echo.origin);
        let expected = [
            json!({"socket": 1, "seq": 1, "url": url, "kind": "text", "data": "first"}),
            json!({"socket": 1, "seq": 2, "url": url, "kind": "text", "data": "second"}),
            json!({"socket": 1, "seq": 3, "url": url, "kind": "binary", "data": "AgM="}),
            json!({"socket": 1, "seq": 4, "url": url, "kind": "text", "data": "fourth"}),
        ];
        assert_eq!(asked, json!(expected));

        let answer = async |expression: &str| evaluate_in(&cdp, &page, expression, None).await;
        // what must not arrive gets this long to show up anyway
        let settle = || tokio::time::sleep(Duration::from_millis(300));
        // nothing leaves before its verdict, nor on a forged one
        assert_eq!(answer(&format!("{RESOLVER}('ffff', 1, 1, true)")).await?, false);
        answer(&script.verdict_expression(1, 4, true)).await?;
        answer(&script.verdict_expression(1, 3, true)).await?;
        settle().await;
        assert_eq!(
            echo.received_on("/guard"),
            Vec::<String>::new(),
            "a frame overtook the first"
        );
        // the first lets out itself only; the second's deny lets the rest out, in order, as they were sent
        answer(&script.verdict_expression(1, 1, true)).await?;
        assert_eq!(echo.received_once("/guard", 1).await, ["first"]);
        settle().await;
        assert_eq!(echo.received_on("/guard"), ["first"]);
        answer(&script.verdict_expression(1, 2, false)).await?;
        assert_eq!(echo.received_once("/guard", 3).await, ["first", "bytes 2,3", "fourth"]);
        // a frame is decided once
        answer(&script.verdict_expression(1, 2, true)).await?;
        settle().await;
        assert_eq!(echo.received_on("/guard").len(), 3);
        // best effort: the connection may drop before the reply
        let _ = cdp.call(None, "Browser.close", Value::Null).await;
        browser.wait_or_kill(Duration::from_secs(5)).await?;
        TestResult::Ok(())
    })
    .await;
    outcome.expect("the run took over 60 s").unwrap();
}

/// What an isolated world (a context the guard isn't in) sends: one text and one binary frame, straight out.
const BYPASS: &str = r"
    new Promise((resolve, reject) => {
        const socket = new WebSocket(BASE + '/bypass?tc=planted-query');
        socket.onerror = () => reject(new Error('the socket failed'));
        socket.onopen = () => {
            socket.send('bypass:text');
            socket.send(new Uint8Array([7, 8]));
            socket.send('bypass:done');
        };
        socket.onmessage = (event) => { if (event.data === 'bypass:done') resolve('sent'); };
    })";

#[tokio::test]
async fn a_frame_that_gets_past_the_guard_is_caught_by_the_audit_and_fails_the_run() {
    let pages = serve_pages().await;
    let echo = EchoServer::start().await.unwrap();
    with_run(&pages, &echo, async |run| {
        let cdp = run.armed.cdp();
        let page = run.armed.open_page(STEP).await?;
        navigate(cdp, &page, &format!("{}/pages/blank.html", pages.uri())).await?;
        loaded(cdp, &page.session, "/pages/blank.html").await?;
        let tree = cdp.call(Some(&page.session), "Page.getFrameTree", Value::Null).await?;
        let world = json!({"frameId": tree["frameTree"]["frame"]["id"], "worldName": "bypass"});
        let created = cdp.call(Some(&page.session), "Page.createIsolatedWorld", world).await?;
        let context = created["executionContextId"].as_i64().ok_or("no context")?;
        let bypass = BYPASS.replace("BASE", &format!("'{}'", echo.origin));
        assert_eq!(evaluate_in(cdp, &page.session, &bypass, Some(context)).await?, "sent");

        // the guard isn't there, so the frames got out; the audit sees them afterwards
        assert_eq!(
            echo.received_once("/bypass", 3).await,
            ["bypass:text", "bytes 7,8", "bypass:done"]
        );
        let poll = async {
            while run.armed.unsanctioned_frames().len() < 3 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(STEP, poll)
            .await
            .map_err(|_| format!("the audit missed: {:?}", run.armed.unsanctioned_frames()))?;
        let unsanctioned = run.armed.unsanctioned_frames();
        let opcodes: Vec<u64> = unsanctioned.iter().map(|frame| frame.opcode).collect();
        assert_eq!(opcodes, [1, 2, 1]);
        for frame in &unsanctioned {
            assert_eq!(frame.url.as_deref(), Some(format!("{}/bypass", echo.origin).as_str()));
            assert_eq!(frame.session.as_ref(), Some(&page.session));
        }
        let breaches = log_entries(&run.lab.log_path)?
            .into_iter()
            .filter(|entry| entry["verdict"] == "breach")
            .count();
        assert_eq!(breaches, 3, "every breach is in fence.log");
        let failed = run.armed.verdict().unwrap_err();
        assert_eq!(failed.unsanctioned.len(), 3);
        assert!(
            failed.to_string().contains("3 frame(s) sent without an allow"),
            "{failed}"
        );
        Ok(())
    })
    .await
    .unwrap();
}
