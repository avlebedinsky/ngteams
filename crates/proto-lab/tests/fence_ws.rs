//! The browser fence's WebSocket layer: the frame rule, forced frame denies, the guard script, the decision on one frame, the answer
//! to the guard and the audit of what was sent (against a recording endpoint and synthetic events), and, with Chrome
//! installed, enforcement on a local echo server (which the frame rule lets through: a forced deny stands it in for a
//! socket whose frames the fence denies, all but `allow:` frames): in the page, a dedicated, a shared and a service
//! worker a denied frame never reaches the server, an allowed one does, in order; a frame sent through another realm's `send` (an
//! iframe's, a popup's) is refused whatever it holds; and a frame that gets past the guard fails the run.
//!
//! The Chrome tests run headless on throwaway user-data-dirs against local servers; see `common` for how Chrome is
//! found.

mod common;
#[path = "common/page.rs"]
mod page;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use proto_lab::arming::{ArmOptions, ArmedBrowser, ArmedTarget, Step, TargetKind};
use proto_lab::browser::{Browser, LaunchOptions};
use proto_lab::cdp::{Cdp, Event, SessionId};
use proto_lab::classify::{Classification, Classifier};
use proto_lab::fence_http::{DenyCause, HttpFence, Verdict};
use proto_lab::fence_ws::{
    BINDING, BadToken, ForcedFrameDeny, Frame, GuardRequest, GuardScript, Payload, RESOLVER, WsFence, classify_frame,
    is_trouter_socket, trouter_type,
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

/// The local stand-in: every frame on one local socket server but a text frame starting with `allow:` is
/// force-denied.
fn local_hold(origin: &str) -> Result<ForcedFrameDeny, regex::Error> {
    Ok(ForcedFrameDeny::new(&format!("^{}/", regex::escape(origin)))?.except_text_starting_with("allow:"))
}

fn binary(url: &str, base64: &str) -> Frame {
    Frame {
        url: url.to_owned(),
        payload: Payload::Binary(base64.to_owned()),
    }
}

// --- the frame rule ---

#[test]
fn on_a_trouter_socket_the_session_s_frames_and_a_delivery_s_answer_pass() {
    let regional = "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c";
    for url in [TROUTER, regional, "ws://go.trouter.teams.microsoft.com/other/path"] {
        for text_frame in [
            "0::",
            "1::",
            "2::",
            r#"5:::{"name":"user.authenticate","args":[{"headers":{"Authorization":"Bearer made-up"}}]}"#,
            r#"5:1+::{"name":"ping"}"#,
            r#"5:3+::{"name":"user.activity","args":[{"state":"active","cv":"made-up"}]}"#,
            r#"5:12+::{"name":"trouter.processed_message_loss","args":[{"droppedIndicators":[]}]}"#,
            "5::/endpoint:{}",
            // a delivery's answer, which goes back to the service that delivered it
            r#"3:::{"id":"-123","status":200,"headers":{},"body":""}"#,
            "3::",
        ] {
            assert_eq!(
                classify_frame(&text(url, text_frame)),
                Classification::Allow,
                "{url} {text_frame}"
            );
        }
        for text_frame in [
            // a JSON message, an acknowledgement, an error, a noop: no client frame of the session
            "4:::{}",
            "6:::1+[]",
            "7:::1+0",
            "8::",
            // what doesn't parse
            "",
            "5",
            "52::",
            "5:x::{}",
            "5:1++::{}",
            " 2::",
            "\u{fffd}3\u{fffd}2::",
        ] {
            assert_eq!(
                classify_frame(&text(url, text_frame)),
                Classification::Unknown,
                "{url} {text_frame:?}"
            );
        }
        assert_eq!(classify_frame(&binary(url, "AQID")), Classification::Unknown, "{url}");
    }
}

#[test]
fn socket_io_frames_are_read_by_their_one_digit_type() {
    for (frame, kind) in [
        ("0::", Some(b'0')),
        ("2::", Some(b'2')),
        ("5:1+::{}", Some(b'5')),
        ("5:::{}", Some(b'5')),
        ("3:::{}", Some(b'3')),
        ("1::/v4", Some(b'1')),
        ("5:12::x", Some(b'5')),
        ("5", None),
        ("5:", None),
        ("5:1", None),
        ("5:1+", None),
        ("x::", None),
        ("", None),
        ("55::", None),
    ] {
        assert_eq!(trouter_type(frame), kind, "{frame:?}");
    }
}

#[test]
fn a_trouter_socket_is_any_host_under_the_trouter_domain() -> TestResult {
    for url in [
        TROUTER,
        "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c",
        "wss://GO.Trouter.Teams.Microsoft.com./v4/c",
        "ws://go.trouter.teams.microsoft.com:8443/anything",
    ] {
        assert!(is_trouter_socket(&url::Url::parse(url)?), "{url}");
    }
    for url in [
        "wss://trouter.teams.microsoft.com/v4/c",
        "wss://go.trouter.teams.microsoft.com.example.com/v4/c",
        "wss://gotrouter.teams.microsoft.com/v4/c",
        "wss://127.0.0.1/v4/c",
    ] {
        assert!(!is_trouter_socket(&url::Url::parse(url)?), "{url}");
    }
    Ok(())
}

#[test]
fn every_frame_on_a_contacting_service_s_socket_is_unknown() {
    for url in [
        "wss://api.flightproxy.teams.microsoft.com/api/v2/ep/x",
        "wss://emea.ng.msg.teams.microsoft.com/v1/socket",
        "wss://teams.microsoft.com/api/chatsvc/emea/v1/socket",
        "wss://teams.microsoft.com//api/chatsvc/emea/v1/socket",
        "wss://go.skype.com/x",
        "wss://graph.microsoft.com/beta/subscriptions",
    ] {
        assert_eq!(classify_frame(&text(url, "anything")), Classification::Unknown, "{url}");
        assert_eq!(classify_frame(&binary(url, "AQID")), Classification::Unknown, "{url}");
    }
}

#[test]
fn on_any_other_socket_a_frame_passes_but_one_the_guard_could_not_read() {
    for url in [
        LOCAL,
        "wss://augloop.office.com/",
        "wss://teams.microsoft.com/v2/socket",
        "wss://teams.microsoft.com/api/authsvc/v1.0/authz",
    ] {
        assert_eq!(classify_frame(&text(url, "anything")), Classification::Allow, "{url}");
        assert_eq!(classify_frame(&text(url, "3:::{}")), Classification::Allow, "{url}");
        assert_eq!(classify_frame(&binary(url, "AQID")), Classification::Allow, "{url}");
        for payload in [Payload::Unreadable, Payload::Foreign] {
            let frame = Frame {
                url: url.to_owned(),
                payload,
            };
            assert_eq!(classify_frame(&frame), Classification::Unknown, "{url}");
        }
    }
    assert_eq!(classify_frame(&text("not a url", "allow:1")), Classification::Unknown);
}

#[test]
fn a_forced_frame_deny_matches_its_socket_and_the_text_it_names() {
    let held = ForcedFrameDeny::new("^ws://127\\.0\\.0\\.1:9/")
        .unwrap()
        .with_text_containing(r#""name":"user.activity""#);
    assert!(held.matches(&text(LOCAL, r#"5:3+::{"name":"user.activity","args":[]}"#)));
    assert!(!held.matches(&text(LOCAL, r#"5:1+::{"name":"ping"}"#)));
    assert!(
        !held.matches(&binary(LOCAL, "AQID")),
        "a text rule takes no binary frame"
    );
    assert!(!held.matches(&text(TROUTER, r#"5:3+::{"name":"user.activity","args":[]}"#)));

    let local = local_hold("ws://127.0.0.1:9").unwrap();
    assert!(!local.matches(&text(LOCAL, "allow:1")));
    for frame in [
        text(LOCAL, "deny:allow:1"),
        text(LOCAL, "Allow:1"),
        text(LOCAL, ""),
        binary(LOCAL, "YWxsb3c6MQ=="),
        Frame {
            url: LOCAL.to_owned(),
            payload: Payload::Unreadable,
        },
    ] {
        assert!(local.matches(&frame), "{frame:?}");
    }
    assert!(
        !local.matches(&text("ws://127.0.0.1:10/page", "deny:1")),
        "another socket"
    );
    let every = ForcedFrameDeny::new("^wss://").unwrap().labelled("all");
    assert!(every.matches(&binary(TROUTER, "AQID")) && every.matches(&text(TROUTER, "2::")));
}

#[test]
fn a_forced_frame_deny_by_prefix_takes_only_text_frames_starting_with_it() {
    let responses = ForcedFrameDeny::new("trouter").unwrap().with_text_starting_with("3:::");
    assert!(responses.matches(&text(TROUTER, r#"3:::{"id":-7,"status":200}"#)));
    for frame in [
        text(TROUTER, r#"5:3+::{"name":"3:::"}"#),
        text(TROUTER, "2::"),
        text(TROUTER, " 3:::{}"),
        binary(TROUTER, "Mzo6Ons9"),
    ] {
        assert!(!responses.matches(&frame), "{frame:?}");
    }
    assert!(!responses.matches(&text(LOCAL, "3:::{}")), "another socket");
}

// --- the guard script and its requests ---

/// The token of the tests' guard scripts.
const TOKEN: &str = "5eed0123";

/// What the guard sends for one frame of socket 3.
fn request(seq: u64, kind: &str, data: &str) -> Value {
    json!({"token": TOKEN, "socket": 3, "seq": seq, "url": LOCAL, "kind": kind, "data": data})
}

#[test]
fn a_guard_request_is_read_from_the_binding_payload_with_the_runs_token_only() {
    let script = GuardScript::with_token(TOKEN).unwrap();
    let read = |payload: &Value| GuardRequest::from_payload(&payload.to_string(), &script);
    let text_request = read(&request(7, "text", "allow:1")).unwrap();
    assert_eq!((text_request.socket, text_request.seq), (3, 7));
    assert_eq!(text_request.frame, text(LOCAL, "allow:1"));
    let binary = read(&request(7, "binary", "AQID")).unwrap();
    assert_eq!(binary.frame.payload, Payload::Binary("AQID".to_owned()));
    let unreadable = read(&request(7, "unreadable", "")).unwrap();
    assert_eq!(unreadable.frame.payload, Payload::Unreadable);
    let foreign = read(&request(0, "foreign", "")).unwrap();
    assert_eq!(foreign.frame.payload, Payload::Foreign);

    let with = |field: &str, value: Value| {
        let mut payload = request(7, "text", "");
        payload[field] = value;
        payload.to_string()
    };
    let without_token = {
        let mut payload = request(7, "text", "");
        payload.as_object_mut().unwrap().remove("token");
        payload.to_string()
    };
    let malformed = [
        String::new(),
        "not json".to_owned(),
        request(7, "blob", "x").to_string(),
        json!({"token": TOKEN, "socket": 3, "seq": 7, "url": LOCAL, "kind": "text"}).to_string(),
        with("socket", json!(-1)),
        with("extra", json!(1)),
        without_token,
        with("token", json!("")),
        with("token", json!("5eed0124")),
        with("token", json!(TOKEN.to_uppercase())),
    ];
    for payload in &malformed {
        let error = GuardRequest::from_payload(payload, &script).unwrap_err();
        assert!(!error.to_string().contains("5eed"), "{error}");
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
        let find = |wanted| steps.iter().position(|step| *step == wanted);
        // a step that must be there: its position (comparing `Option`s would let a missing step pass, `None` < `Some`)
        let position = |wanted| find(wanted).unwrap_or_else(|| panic!("{kind:?} has no {wanted:?} step"));
        let resume = position(Step::Resume);
        assert_eq!(resume, steps.len() - 1, "{kind:?}: resuming is the last step");
        let binding = position(Step::Binding);
        let page_like = matches!(kind, TargetKind::Page | TargetKind::Iframe);
        let guard = if page_like {
            assert_eq!(find(Step::Guard), None, "{kind:?}");
            assert!(
                position(Step::Runtime) < binding,
                "{kind:?}: new contexts need Runtime on"
            );
            let guard = position(Step::GuardNewDocuments);
            assert!(
                position(Step::Page) < guard,
                "{kind:?}: new-document scripts need Page on"
            );
            guard
        } else {
            assert_eq!(find(Step::GuardNewDocuments), None, "{kind:?}");
            position(Step::Guard)
        };
        assert!(binding < guard, "{kind:?}: the guard needs its binding");
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

/// A fence over `member_list`, standing the local socket server at `hold` in for one whose frames are denied but
/// `allow:` frames ([`local_hold`]).
fn ws_fence(fence: &Arc<Fence>, hold: Option<&str>, member_list: &[&str]) -> TestResult<WsFence> {
    let ws = WsFence::new(Arc::clone(fence), members(member_list), GuardScript::with_token(TOKEN)?);
    if let Some(origin) = hold {
        ws.force_deny(local_hold(origin)?);
    }
    Ok(ws)
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
    let ws = ws_fence(&lab.fence, None, &[]).unwrap();

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

#[tokio::test]
async fn a_frame_on_another_realms_socket_is_denied_unclassified_whatever_rule_would_match() {
    let lab = lab().unwrap();
    let ws = ws_fence(&lab.fence, Some("ws://127.0.0.1:9"), &[]).unwrap();
    let foreign = Frame {
        url: LOCAL.to_owned(),
        payload: Payload::Foreign,
    };

    assert_eq!(ws.decide(&foreign).await, Verdict::Deny(DenyCause::ForeignSocket));
    assert_eq!(ws.denials()[0].url, "ws://127.0.0.1:9/page");
    let entries = log_entries(&lab.log_path).unwrap();
    assert_eq!(
        entries.into_iter().map(untimed).collect::<Vec<_>>(),
        [json!({
            "verdict": "deny",
            "channel": "web_socket",
            "method": "SEND",
            "url": "ws://127.0.0.1:9/page",
            "reason": "unclassified",
            "message": "no rule classifies this request",
        })]
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
async fn a_frame_that_reaches_nobody_passes_logged_as_allowed_and_one_that_cannot_be_logged_is_denied() {
    let lab = lab().unwrap();
    let ws = ws_fence(&lab.fence, Some("ws://127.0.0.1:9"), &[]).unwrap();
    assert_eq!(ws.decide(&text(LOCAL, "allow:1")).await, Verdict::Allow);
    assert!(ws.denials().is_empty());
    let entries: Vec<Value> = log_entries(&lab.log_path).unwrap().into_iter().map(untimed).collect();
    assert_eq!(
        entries,
        [json!({"verdict": "allow", "event": "allowed", "method": "SEND", "host": "127.0.0.1", "path": "/page"})]
    );

    let fence = Arc::new(Fence::new(
        Roster::new([Mri::new(LAB_A)], []),
        FenceLog::from_writer(Refuses),
    ));
    let ws = ws_fence(&fence, Some("ws://127.0.0.1:9"), &[]).unwrap();
    assert_eq!(
        ws.decide(&text(LOCAL, "allow:1")).await,
        Verdict::Deny(DenyCause::LogUnavailable)
    );
    assert_eq!(ws.denials()[0].cause, DenyCause::LogUnavailable);
    // a forced deny the log won't take is still denied, but as lost, never as the forced deny a scenario expects
    ws.force_deny(ForcedFrameDeny::new("^ws://").unwrap().labelled("held"));
    assert_eq!(
        ws.decide(&text(LOCAL, "allow:1")).await,
        Verdict::Deny(DenyCause::LogUnavailable)
    );
    let denials = ws.denials();
    assert_eq!(denials.len(), 2);
    assert_eq!(denials[1].cause, DenyCause::LogUnavailable);
    assert_eq!(denials[1].label.as_deref(), Some("held"));
}

#[tokio::test]
async fn a_forced_frame_deny_comes_before_the_rule_is_kept_with_its_label_and_logged() {
    let lab = lab().unwrap();
    let ws = ws_fence(&lab.fence, None, &[]).unwrap();
    let ping = text(TROUTER, r#"5:1+::{"name":"ping"}"#);
    let activity = text(TROUTER, r#"5:3+::{"name":"user.activity","args":[{"state":"active"}]}"#);
    assert_eq!(ws.decide(&activity).await, Verdict::Allow, "the rule lets it through");
    ws.force_deny(
        ForcedFrameDeny::new("trouter")
            .unwrap()
            .with_text_containing("user.activity")
            .labelled("held"),
    );
    assert_eq!(ws.decide(&activity).await, Verdict::Deny(DenyCause::Forced));
    assert_eq!(ws.decide(&ping).await, Verdict::Allow);
    let denials = ws.denials();
    assert_eq!(denials.len(), 1);
    assert_eq!(denials[0].label.as_deref(), Some("held"));
    assert_eq!(denials[0].url, "wss://go.trouter.teams.microsoft.com/v4/c");
    let entries: Vec<Value> = log_entries(&lab.log_path).unwrap().into_iter().map(untimed).collect();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[1]["verdict"], "deny");
    assert_eq!(entries[1]["reason"], "forced");
    assert_eq!(entries[1]["url"], "wss://go.trouter.teams.microsoft.com/v4/c");
    let log = std::fs::read_to_string(&lab.log_path).unwrap();
    assert!(
        !log.contains("user.activity") && !log.contains("planted-query"),
        "{log}"
    );
    ws.clear_forced();
    assert_eq!(ws.decide(&activity).await, Verdict::Allow);
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
    let lab_only = ws_fence(&lab.fence, None, &[LAB_A]).unwrap();
    assert_eq!(
        lab_only.judge(Classification::Write(target()), TROUTER).await,
        Verdict::Allow
    );
    let with_coworker = ws_fence(&lab.fence, None, &[LAB_A, COWORKER]).unwrap();
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
    let ws = ws_fence(&lab.fence, Some("ws://127.0.0.1:9"), &[]).unwrap();
    let (url, commands) = recording_endpoint().await.unwrap();
    let (cdp, _events) = Cdp::connect(&url).await.unwrap();
    let ask = |seq: u64, data: &str| request(seq, "text", data);

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
    // a call from a context the guard isn't in can't name the run's token, and gets no answer to read it from
    let mut forged = ask(3, "allow:forged");
    forged["token"] = json!("00");
    let unauthenticated = ws.handle_binding(&cdp, &binding_called("S1", 5, &forged)).await;
    // a frame another realm's guard dropped: denied whatever its socket, never answered, no allow for the audit
    let foreign = ws
        .handle_binding(&cdp, &binding_called("S2", 6, &request(0, "foreign", "")))
        .await;

    assert_eq!(allowed.unwrap(), Some(Verdict::Allow));
    assert_eq!(denied.unwrap(), Some(Verdict::Deny(DenyCause::Forced)));
    assert_eq!(ignored.unwrap(), None);
    assert_eq!(malformed.unwrap(), Some(Verdict::Deny(DenyCause::Malformed)));
    assert_eq!(unauthenticated.unwrap(), Some(Verdict::Deny(DenyCause::Malformed)));
    assert_eq!(foreign.unwrap(), Some(Verdict::Deny(DenyCause::ForeignSocket)));
    let causes: Vec<DenyCause> = ws.denials().iter().map(|denial| denial.cause).collect();
    assert_eq!(
        causes,
        [
            DenyCause::Forced,
            DenyCause::Malformed,
            DenyCause::Malformed,
            DenyCause::ForeignSocket
        ]
    );
    assert_eq!(ws.denials()[3].url, "ws://127.0.0.1:9/page");
    assert_eq!(ws.allows_not_seen_sent(), 1, "only allow:1");
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
            ws.script().verdict_expression(3, seq, allow)
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
    let ws = ws_fence(&lab.fence, None, &[]).unwrap();

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
    let ws = ws_fence(&lab.fence, Some("ws://127.0.0.1:9"), &[]).unwrap();
    let (url, _commands) = recording_endpoint().await.unwrap();
    let (cdp, _events) = Cdp::connect(&url).await.unwrap();
    let ask = request(1, "text", "allow:1");
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

/// Waits until a tab created on a URL (not [`navigate`]d) shows the document at `path`, fully loaded (the `about:blank`
/// before it is `complete` too).
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

/// [`page::evaluate_until`], for up to [`STEP`].
async fn evaluate_until(cdp: &Cdp, session: &SessionId, expression: &str) -> TestResult<Value> {
    page::evaluate_until(cdp, session, expression, STEP).await
}

/// [`page::navigate`]s the tab and waits up to [`STEP`] for the page to load.
async fn navigate(cdp: &Cdp, tab: &ArmedTarget, url: &str) -> TestResult {
    page::navigate(cdp, &tab.session, url, STEP).await
}

/// One armed run on a throwaway profile, the echo server standing in for a socket whose frames are denied.
struct Run {
    armed: ArmedBrowser,
    lab: Lab,
    _profile: tempfile::TempDir,
}

/// Runs `body` on an armed browser whose fence denies every frame to `echo` but `allow:` text frames, then closes the
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
    let http = HttpFence::new(Classifier::new(), Arc::clone(&lab.fence), members(&[]));
    let ws = ws_fence(&lab.fence, Some(echo.origin.as_str()), &[])?;
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
            assert_eq!(denial.cause, DenyCause::Forced, "{denial:?}");
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
        // and the audit saw every allowed frame go out, the workers' included: one it can't see would stay unused
        let poll = async {
            while run.armed.allows_not_seen_sent() > 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(STEP, poll)
            .await
            .map_err(|_| format!("{} allowed frame(s) never seen sent", run.armed.allows_not_seen_sent()))?;
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

/// Three more frames on the same socket from a page that tries to keep the bytes the fence decides on: a buffer whose
/// `constructor[Symbol.species]` stores every buffer `slice` builds from it, and two Blobs whose read a `then` on
/// ArrayBuffer.prototype swaps for a buffer the page keeps (`evil`) and for an object that is no buffer at all.
const KEEP_THE_BYTES: &str = r"
    (() => {
        window.kept = [];
        class Keeper extends ArrayBuffer {
            constructor(...args) {
                super(...args);
                window.kept.push(this);
            }
        }
        const buffer = new Uint8Array([4, 5]).buffer;
        buffer.constructor = { [Symbol.species]: Keeper };
        window.socket.send(buffer);

        const evil = new Uint8Array([6, 7]).buffer;
        window.evil = evil;
        const swaps = { 'blob-1': evil, 'blob-2': { toString: () => 'swapped' } };
        Object.defineProperty(ArrayBuffer.prototype, 'then', {
            configurable: true,
            get() {
                const swap = this === evil ? undefined : swaps[new TextDecoder().decode(this)];
                return swap === undefined ? undefined : (ok) => ok(swap);
            },
        });
        window.socket.send(new Blob(['blob-1']));
        window.socket.send(new Blob(['blob-2']));
    })()";

/// Changes every buffer the page kept, once the fence was asked about them.
const CHANGE_THE_KEPT: &str = r"
    (() => {
        for (const buffer of window.kept) {
            new Uint8Array(buffer).fill(0);
        }
        new Uint8Array(window.evil).fill(1);
        return window.kept.length;
    })()";

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
        let script = GuardScript::with_token(TOKEN)?;
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
            json!({"token": TOKEN, "socket": 1, "seq": 1, "url": url, "kind": "text", "data": "first"}),
            json!({"token": TOKEN, "socket": 1, "seq": 2, "url": url, "kind": "text", "data": "second"}),
            json!({"token": TOKEN, "socket": 1, "seq": 3, "url": url, "kind": "binary", "data": "AgM="}),
            json!({"token": TOKEN, "socket": 1, "seq": 4, "url": url, "kind": "text", "data": "fourth"}),
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

        // what goes out is what was decided, whatever the page kept or swapped in
        answer(KEEP_THE_BYTES).await?;
        let asked = evaluate_until(&cdp, &page, "window.asked.length === 7 ? window.asked.slice(4) : null").await?;
        let expected = [
            json!({"token": TOKEN, "socket": 1, "seq": 5, "url": url, "kind": "binary", "data": "BAU="}),
            json!({"token": TOKEN, "socket": 1, "seq": 6, "url": url, "kind": "binary", "data": "Bgc="}),
            json!({"token": TOKEN, "socket": 1, "seq": 7, "url": url, "kind": "unreadable", "data": ""}),
        ];
        assert_eq!(asked, json!(expected));
        assert_eq!(answer(CHANGE_THE_KEPT).await?, 0, "the page kept the guard's copy");
        for seq in 5..=7 {
            answer(&script.verdict_expression(1, seq, true)).await?;
        }
        assert_eq!(
            echo.received_once("/guard", 5).await,
            ["first", "bytes 2,3", "fourth", "bytes 4,5", "bytes 6,7"]
        );
        settle().await;
        assert_eq!(echo.received_on("/guard").len(), 5, "an unreadable frame is dropped");
        // best effort: the connection may drop before the reply
        let _ = cdp.call(None, "Browser.close", Value::Null).await;
        browser.wait_or_kill(Duration::from_secs(5)).await?;
        TestResult::Ok(())
    })
    .await;
    outcome.expect("the run took over 60 s").unwrap();
}

/// Installs the guard by hand in a same-origin iframe too, with a fence of its own played by the test (`asked` in each
/// realm), and opens a socket from each realm to `/realms`, and one of a subclass to `/realms-sub`.
const TWO_REALMS: &str = r"
    new Promise((resolve, reject) => {
        const iframe = document.body.appendChild(document.createElement('iframe'));
        window.inner = iframe.contentWindow;
        // an indirect eval runs in the realm of the eval it calls
        inner.eval(PLAY_FENCE + GUARD);
        const opened = (socket) => new Promise((ok) => { socket.onopen = ok; socket.onerror = reject; });
        window.outerSocket = new WebSocket(BASE + '/realms');
        window.innerSocket = new inner.WebSocket(BASE + '/realms');
        window.Sub = class extends WebSocket {};
        window.subSocket = new Sub(BASE + '/realms-sub');
        Promise.all([opened(outerSocket), opened(innerSocket), opened(subSocket)]).then(() => resolve('open'));
    })";

/// Sends on each realm's socket through both realms' `send`, and on the subclass socket.
const SEND_ACROSS: &str = r"
    (() => {
        const innerSend = inner.WebSocket.prototype.send;
        outerSocket.send('allow:first');
        innerSend.call(outerSocket, 'allow:second');
        outerSocket.send('allow:third');
        innerSocket.send('allow:inner');
        WebSocket.prototype.send.call(innerSocket, 'allow:inner-through-outer');
        subSocket.send('allow:sub');
        return [window.asked.length, inner.asked.length];
    })()";

/// What the wrapped constructor still looks like from the page.
const LOOKS_NATIVE: &str = r"
    [
        WebSocket.prototype.constructor === WebSocket,
        outerSocket instanceof WebSocket,
        subSocket instanceof WebSocket,
        WebSocket.name,
        WebSocket.length,
        WebSocket.OPEN,
        Object.getPrototypeOf(outerSocket) === WebSocket.prototype,
        typeof WebSocketStream,
    ]";

#[tokio::test]
async fn a_guard_queues_only_its_own_realms_sockets() {
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
        let script = GuardScript::with_token(TOKEN)?;
        evaluate_in(&cdp, &page, PLAY_FENCE, None).await?;
        assert_eq!(evaluate_in(&cdp, &page, script.source(), None).await?, "armed");
        let open = TWO_REALMS
            .replace("BASE", &format!("'{}'", echo.origin))
            .replace("PLAY_FENCE", &serde_json::to_string(PLAY_FENCE)?)
            .replace("GUARD", &serde_json::to_string(script.source())?);
        assert_eq!(evaluate_in(&cdp, &page, &open, None).await?, "open");
        assert_eq!(
            evaluate_in(&cdp, &page, LOOKS_NATIVE, None).await?,
            json!([true, true, true, "WebSocket", 1, 1, true, "undefined"])
        );
        assert_eq!(evaluate_in(&cdp, &page, SEND_ACROSS, None).await?, json!([4, 2]));

        // each realm queues and asks for its own sockets only; a send on the other's is reported and dropped
        let url = format!("{}/realms", echo.origin);
        let sub = format!("{}/realms-sub", echo.origin);
        let foreign = json!({"token": TOKEN, "socket": 0, "seq": 0, "url": url, "kind": "foreign", "data": ""});
        let outer = [
            json!({"token": TOKEN, "socket": 1, "seq": 1, "url": url, "kind": "text", "data": "allow:first"}),
            json!({"token": TOKEN, "socket": 1, "seq": 2, "url": url, "kind": "text", "data": "allow:third"}),
            foreign.clone(),
            json!({"token": TOKEN, "socket": 2, "seq": 1, "url": sub, "kind": "text", "data": "allow:sub"}),
        ];
        let inner = [
            foreign,
            json!({"token": TOKEN, "socket": 1, "seq": 1, "url": url, "kind": "text", "data": "allow:inner"}),
        ];
        assert_eq!(evaluate_in(&cdp, &page, "window.asked", None).await?, json!(outer));
        assert_eq!(evaluate_in(&cdp, &page, "inner.asked", None).await?, json!(inner));

        // the inner fence answers first; the outer one answers its socket's second frame before its first
        let inner_verdict = format!("inner.{}", script.verdict_expression(1, 1, true));
        evaluate_in(&cdp, &page, &inner_verdict, None).await?;
        for (socket, seq) in [(1, 2), (1, 1), (2, 1)] {
            evaluate_in(&cdp, &page, &script.verdict_expression(socket, seq, true), None).await?;
        }
        echo.received_once("/realms", 3).await;
        assert_eq!(echo.received_once("/realms-sub", 1).await, ["allow:sub"]);
        // what must not arrive gets this long to show up anyway
        tokio::time::sleep(Duration::from_millis(300)).await;
        // the two sockets' frames interleave as they please; each socket's keep their order, the foreign ones none
        let received = echo.received_on("/realms");
        let (inner, outer): (Vec<String>, Vec<String>) =
            received.into_iter().partition(|frame| frame.starts_with("allow:inner"));
        assert_eq!(inner, ["allow:inner"]);
        assert_eq!(outer, ["allow:first", "allow:third"]);
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

/// What an isolated world tries next: the binding is there too, so it defines a resolver of its own and asks for a
/// verdict, without the run's token and with a guess, hoping to be answered with the token.
const ASK_FOR_THE_TOKEN: &str = r"
    (() => {
        window.answers = [];
        window.__labFenceWsVerdict = (...args) => {
            window.answers.push(args);
            return true;
        };
        const ask = { socket: 1, seq: 1, url: BASE + '/bypass', kind: 'text', data: 'bypass:text' };
        __labFenceWs(JSON.stringify(ask));
        __labFenceWs(JSON.stringify({ token: '00', ...ask }));
        return typeof __labFenceWs;
    })()";

#[tokio::test]
async fn a_frame_that_gets_past_the_guard_is_caught_by_the_audit_and_fails_the_run() {
    let pages = serve_pages().await;
    let echo = EchoServer::start().await.unwrap();
    with_run(&pages, &echo, async |run| {
        let cdp = run.armed.cdp();
        let page = run.armed.open_page(STEP).await?;
        navigate(cdp, &page, &format!("{}/pages/blank.html", pages.uri())).await?;
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

        // nor does the world get the token: its requests are denied and never answered
        let ask = ASK_FOR_THE_TOKEN.replace("BASE", &format!("'{}'", echo.origin));
        assert_eq!(evaluate_in(cdp, &page.session, &ask, Some(context)).await?, "function");
        let poll = async {
            while run.armed.frame_denials().len() < 2 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(STEP, poll)
            .await
            .map_err(|_| format!("not denied: {:?}", run.armed.frame_denials()))?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let denials = run.armed.frame_denials();
        assert!(
            denials.len() == 2 && denials.iter().all(|denial| denial.cause == DenyCause::Malformed),
            "{denials:?}"
        );
        let answers = evaluate_in(cdp, &page.session, "window.answers", Some(context)).await?;
        assert_eq!(answers, json!([]), "the world was answered");
        Ok(())
    })
    .await
    .unwrap();
}

/// Opens a socket to `/cross-realm` and defines `sendThrough(window, data)`: a send on that socket through another
/// window's `WebSocket.prototype.send` (Blink's brand check takes a socket from any realm). The socket's own first frame
/// goes out before any other window's.
const CROSS_REALM: &str = r"
    new Promise((resolve, reject) => {
        const socket = new WebSocket(BASE + '/cross-realm');
        socket.onerror = () => reject(new Error('the socket failed'));
        socket.onopen = () => {
            window.ownSocket = socket;
            socket.send('allow:own-1');
            window.sendThrough = (other, data) => {
                if (other === null) {
                    return 'no window';
                }
                other.WebSocket.prototype.send.call(socket, data);
                return 'called';
            };
            resolve('open');
        };
    })";

/// Windows the page opens and sends through in the same task, before anything else could run: popups (a separate
/// target each, its initial `about:blank` document scriptable by the opener at once) and a same-origin `about:blank`
/// iframe. Every frame starts with `allow:`, so the fence would allow it if it were asked for as a frame of its own: a
/// popup's guard would record the allow on the popup's session while the browser reports the frame on the opener's, and
/// an iframe's guard would keep a second queue for the socket, its frame overtaking the socket's own.
const OTHER_WINDOWS: [(&str, &str); 6] = [
    ("popup", "sendThrough(open(''), 'allow:popup')"),
    (
        "about:blank popup",
        "sendThrough(open('about:blank'), 'allow:popup about:blank')",
    ),
    (
        "same-origin popup",
        "sendThrough(open('/pages/blank.html'), 'allow:popup same-origin')",
    ),
    ("named popup", "sendThrough(open('', 'lab-named'), 'allow:popup named')"),
    (
        "document.open popup",
        "sendThrough(document.open('', '_blank', ''), 'allow:popup document.open')",
    ),
    (
        "about:blank iframe",
        "sendThrough(document.body.appendChild(document.createElement('iframe')).contentWindow, 'allow:iframe')",
    ),
];

#[tokio::test]
async fn a_window_the_page_opens_lends_it_no_unguarded_send() {
    let pages = serve_pages().await;
    let echo = EchoServer::start().await.unwrap();
    with_run(&pages, &echo, async |run| {
        let cdp = run.armed.cdp();
        let page = run.armed.open_page(STEP).await?;
        navigate(cdp, &page, &format!("{}/pages/blank.html", pages.uri())).await?;
        let script = CROSS_REALM.replace("BASE", &format!("'{}'", echo.origin));
        assert_eq!(evaluate_in(cdp, &page.session, &script, None).await?, "open");
        for (name, attempt) in OTHER_WINDOWS {
            // a gesture each: one gesture lets one popup open
            let params = json!({"expression": attempt, "returnByValue": true, "userGesture": true});
            let result = cdp.call(Some(&page.session), "Runtime.evaluate", params).await?;
            assert_eq!(result["result"]["value"], "called", "{name}: {result}");
        }
        evaluate_in(cdp, &page.session, "ownSocket.send('allow:own-2')", None).await?;

        // Chrome 155 holds the opener inside `open` until the popup is armed, so every frame went through a guard,
        // which refused it: the socket isn't its realm's
        let poll = async {
            while run.armed.frame_denials().len() < OTHER_WINDOWS.len() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(STEP, poll)
            .await
            .map_err(|_| format!("not every frame was asked for: {:?}", run.armed.frame_denials()))?;
        assert_eq!(
            echo.received_once("/cross-realm", 2).await,
            ["allow:own-1", "allow:own-2"]
        );
        // what must not arrive gets this long to show up anyway
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(echo.received_on("/cross-realm"), ["allow:own-1", "allow:own-2"]);
        assert_eq!(run.armed.unsanctioned_frames(), []);
        let denials = run.armed.frame_denials();
        assert_eq!(denials.len(), OTHER_WINDOWS.len());
        for denial in &denials {
            assert_eq!(denial.cause, DenyCause::ForeignSocket, "{denial:?}");
            assert_eq!(denial.url, format!("{}/cross-realm", echo.origin));
        }
        // the socket's own allows were recorded on the session its frames are reported on
        let poll = async {
            while run.armed.allows_not_seen_sent() > 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(STEP, poll)
            .await
            .map_err(|_| format!("{} allowed frame(s) never seen sent", run.armed.allows_not_seen_sent()))?;
        assert!(run.armed.failures().is_empty(), "{:?}", run.armed.failures());
        Ok(())
    })
    .await
    .unwrap();
}
