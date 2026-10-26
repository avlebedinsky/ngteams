//! The browser fence under the lab's call rule (`teams_fence`'s `calls`, `docs/protocol/fence-targets.md`): a call is
//! judged by its entry (a start, an answer or a decline of the account's own offered invitation, a join, an `add`),
//! and what it sends once it was let in passes with no registry lookup: its follow-ups, the callee's ring-time sends
//! and every Trouter delivery response, which the frame rule lets through. The Trouter deliveries the page receives
//! still go to the call registry, which offers the account's own invitations from lab callers. Synthetic, shaped like
//! the echo-call and `ab-call` captures (made-up ids, hosts and placeholder MRIs).

use std::io::Write as _;
use std::sync::Arc;

use base64::Engine as _;
use proto_lab::cdp::{Event, SessionId};
use proto_lab::classify::Classifier;
use proto_lab::fence_http::{DenyCause, HttpFence, PausedRequest, Verdict};
use proto_lab::fence_ws::{Frame, GuardScript, Payload, WsFence, delivery_of};
use serde_json::{Value, json};
use teams_fence::{
    CALL_START_URL, DenyReason, Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, PartialReason,
    Roster, Scope, TEST_CALL_SCENARIO,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const LAB_A: &str = "8:orgid:lab-a";
const ECHO: &str = "28:echo-service";
const CONVERSATION: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticConversation0";
const ROUTE: &str = "i=10-0-0-1&e=1";
const CC: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1";
const CALLBACKS: &str =
    "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/callAgent/agent-1";
const ACCEPTANCE_PATH: &str = "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/03030303/call/acceptance/";
const TROUTER: &str =
    "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c?tc=%7B%7D&epid=00000000-0000-4000-8000-000000000001";
const TROUTER_REQUEST: &str = r#"{"id":"request-1","port":3443,"src":"198.51.100.7"}"#;
/// The acceptance delivery's chain and message ids, which its response echoes.
const CHAIN_ID: &str = "00000000-0000-4000-8000-00000000c4a1";
const MESSAGE_ID: &str = "00000000-0000-4000-8000-00000000e55a";

struct Nobody;

impl MembershipSource for Nobody {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(async { Membership::Complete(Vec::new()) })
    }
}

struct Lab {
    _dir: tempfile::TempDir,
    fence: Arc<Fence>,
}

fn new_lab() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let roster = Roster::new([Mri::new(LAB_A)], [Mri::new(ECHO)]);
    let fence = Arc::new(Fence::new(roster, FenceLog::open(&dir.path().join("fence.log"))?));
    Ok(Lab { _dir: dir, fence })
}

fn classifier() -> Classifier {
    Classifier::new().with_test_call_callee(Mri::new(ECHO))
}

fn http_fence(lab: &Lab) -> HttpFence {
    HttpFence::new(classifier(), Arc::clone(&lab.fence), Arc::new(Nobody))
}

fn ws_fence(lab: &Lab) -> TestResult<WsFence> {
    Ok(WsFence::new(
        Arc::clone(&lab.fence),
        Arc::new(Nobody),
        GuardScript::generate()?,
    ))
}

fn start_body() -> String {
    json!({
        "conversationRequest": {"links": {}, "scenario": TEST_CALL_SCENARIO},
        "participants": {"from": {"id": LAB_A}, "to": []},
        "callInvitation": {"links": {"acceptance": format!("{CALLBACKS}/03030303/call/acceptance/")}},
    })
    .to_string()
}

fn acceptance_body() -> String {
    json!({"callAcceptance": {"links": {
        "acknowledgement": format!("{CC}/callAcceptance/call-1/1/a1/1/acknowledge?i=10-0-0-2"),
    }}})
    .to_string()
}

fn gzip_base64(text: &str) -> TestResult<String> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(text.as_bytes())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(encoder.finish()?))
}

/// The acceptance delivered as the call controller sends it: the body gzipped and in base64.
fn acceptance_frame(id: i64) -> TestResult<String> {
    let message = json!({
        "id": id,
        "method": "POST",
        "url": ACCEPTANCE_PATH,
        "headers": {
            "X-Microsoft-Skype-Content-Encoding": "gzip",
            "X-Microsoft-Skype-Chain-ID": CHAIN_ID,
            "X-Microsoft-Skype-Message-ID": MESSAGE_ID,
            "trouter-request": TROUTER_REQUEST,
        },
        "body": gzip_base64(&acceptance_body())?,
    });
    Ok(format!("3:::{message}"))
}

fn response_frame(id: i64) -> String {
    let body = json!({"callAcceptanceAcknowledgement": {"links": {
        "mediaRenegotiation": format!("{CALLBACKS}/08080808/call/mediaRenegotiation/"),
    }}});
    let message = json!({
        "id": id,
        "status": 200,
        "headers": {
            "X-Microsoft-Skype-Chain-ID": CHAIN_ID,
            "X-Microsoft-Skype-Message-ID": MESSAGE_ID,
            "MS-CV": "SyntheticCv.1.0",
            "trouter-request": TROUTER_REQUEST,
            "trouter-client": "{\"cd\":2}",
        },
        "body": body.to_string(),
    });
    format!("3:::{message}")
}

fn event(session: &str, method: &str, params: Value) -> Event {
    Event {
        session_id: Some(SessionId::new(session)),
        method: method.to_owned(),
        params,
    }
}

fn created(session: &str, socket: &str, url: &str) -> Event {
    event(
        session,
        "Network.webSocketCreated",
        json!({"requestId": socket, "url": url}),
    )
}

fn received(session: &str, socket: &str, data: &str) -> Event {
    event(
        session,
        "Network.webSocketFrameReceived",
        json!({"requestId": socket, "timestamp": 1.0, "response": {"opcode": 1, "mask": false, "payloadData": data}}),
    )
}

fn text(url: &str, data: &str) -> Frame {
    Frame {
        url: url.to_owned(),
        payload: Payload::Text(data.to_owned()),
    }
}

// --- reading a delivery ---

#[test]
fn a_delivery_is_read_with_its_body_decoded() -> TestResult {
    let delivery = delivery_of("c", &acceptance_frame(-12)?).ok_or("a delivery")?;
    assert_eq!(delivery.id, -12);
    assert_eq!(delivery.path, ACCEPTANCE_PATH);
    assert_eq!(delivery.trouter_request.as_deref(), Some(TROUTER_REQUEST));
    assert_eq!(delivery.body, acceptance_body().into_bytes());
    let plain = r#"3:::{"id":1,"url":"/x","headers":{},"body":"{}"}"#;
    assert_eq!(delivery_of("c", plain).ok_or("plain")?.body, b"{}".to_vec());
    for other in [
        "5:::{\"name\":\"trouter.connected\"}".to_owned(),
        "3:::not json".to_owned(),
        r#"3:::{"id":1,"url":"/x","headers":{"X-Microsoft-Skype-Content-Encoding":"gzip"},"body":"bm90IGd6aXA="}"#
            .to_owned(),
        r#"3:::{"url":"/x","headers":{},"body":""}"#.to_owned(),
    ] {
        assert!(delivery_of("c", &other).is_none(), "{other}");
    }
    Ok(())
}

// --- the delivery responses, through the WebSocket layer ---

#[tokio::test]
async fn every_delivery_response_on_a_trouter_socket_passes_by_the_frame_rule() -> TestResult {
    let lab = new_lab()?;
    let ws = ws_fence(&lab)?;
    ws.observe(&created("S1", "ws-1", TROUTER));
    ws.observe(&received("S1", "ws-1", &acceptance_frame(41)?));
    // the acknowledgement of a delivery no registered call waits for, again, and an empty receipt: all pass
    let empty = format!(
        r#"3:::{{"id":42,"status":200,"headers":{{"MS-CV":"a","trouter-request":{},"trouter-client":"b"}},"body":""}}"#,
        serde_json::to_string(TROUTER_REQUEST)?
    );
    for response in [response_frame(41), response_frame(41), empty, response_frame(99)] {
        assert_eq!(ws.decide(&text(TROUTER, &response)).await, Verdict::Allow, "{response}");
    }
    // a JSON message and an acknowledgement are still denied on a Trouter socket
    for other in ["4:::{}", "6:::1+[]"] {
        assert_eq!(
            ws.decide(&text(TROUTER, other)).await,
            Verdict::Deny(DenyCause::Unclassified),
            "{other}"
        );
    }
    Ok(())
}

// --- the follow-ups, through the HTTP layer ---

fn paused(method: &str, url: &str, body: &str) -> PausedRequest {
    PausedRequest {
        request_id: "interception-job-1.0".to_owned(),
        headers: Vec::new(),
        method: method.to_owned(),
        url: url.to_owned(),
        body: Some(body.as_bytes().to_vec()),
        body_unread: false,
        network_id: Some("1000.1".to_owned()),
    }
}

#[tokio::test]
async fn a_call_s_follow_ups_pass_with_no_registered_call() -> TestResult {
    let lab = new_lab()?;
    let http = http_fence(&lab);
    // nothing was started, read or bound
    let metadata = format!("{CONVERSATION}/updateEndpointMetadata?{ROUTE}");
    let body = json!({"participants": {"from": {"id": LAB_A}}}).to_string();
    assert_eq!(http.decide(&paused("PUT", &metadata, &body)).await, Verdict::Allow);
    let acknowledge = format!("{CC}/callAcceptance/call-1/1/a1/1/acknowledge?i=10-0-0-2");
    let body = json!({"callAcceptanceAcknowledgement": {"links": {}}}).to_string();
    assert_eq!(http.decide(&paused("POST", &acknowledge, &body)).await, Verdict::Allow);
    let leave = format!("{CONVERSATION}/leave?{ROUTE}");
    let body = json!({"participants": {"from": {"id": LAB_A}}}).to_string();
    assert_eq!(http.decide(&paused("POST", &leave, &body)).await, Verdict::Allow);
    assert!(http.denials().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_permitted_start_passes_on_both_layers() -> TestResult {
    let lab = new_lab()?;
    let http = http_fence(&lab);
    let start = paused("POST", CALL_START_URL, &start_body());
    assert_eq!(http.decide(&start).await, Verdict::Allow);
    assert_eq!(
        http.decide(&start).await,
        Verdict::Allow,
        "the browser's layer after the target's"
    );
    Ok(())
}

// --- an incoming call's invitation, offered to the browser's own account ---

const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const INVITATION_PATH: &str = "/v4/f/SyntheticEndpoint00001/";
const ATTACH_URL: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/forked/SyntheticCall1/27/i1/1/attach?i=10-0-0-1";
/// The call leg's acceptance and rejection, on the leg the invitation's `attach` names.
const ACCEPT_URL: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/incoming/SyntheticCall1/27/t/2/accept?i=10-0-0-1";
const REJECT_URL: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/incoming/SyntheticCall1/27/t/2/reject?i=10-0-0-1";
const INVITED_CONVERSATION: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticConversation1?i=10-0-0-2&e=1";
const B_CALLBACKS: &str =
    "https://pub-ent-zz-02-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00001/callAgent/agent-b";

/// b's lab: a, b and c, its fence b's own.
fn lab_of_b() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let roster = Roster::new([Mri::new(LAB_A), Mri::new(LAB_B), Mri::new(LAB_C)], [Mri::new(ECHO)]);
    let fence = Fence::new(roster, FenceLog::open(&dir.path().join("fence.log"))?).with_own_account(Mri::new(LAB_B));
    Ok(Lab {
        _dir: dir,
        fence: Arc::new(fence),
    })
}

/// A 1:1 call's invitation from `from` to `to`, delivered with `id`.
fn invitation_frame(id: i64, from: &str, to: &str) -> String {
    let gp = json!({
        "callNotification": {
            "from": {"id": from},
            "to": {"id": to},
            "links": {"attach": ATTACH_URL},
        },
        "conversationInvitation": {"conversationController": INVITED_CONVERSATION, "isMultiParty": false},
    });
    let body = json!({"evt": 107, "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string())});
    let message = json!({
        "id": id,
        "method": "POST",
        "url": INVITATION_PATH,
        "headers": {"trouter-request": TROUTER_REQUEST},
        "body": body.to_string(),
    });
    format!("3:::{message}")
}

fn receipt_frame(id: i64) -> String {
    let message = json!({
        "id": id,
        "status": 200,
        "headers": {"MS-CV": "SyntheticCv.1.0", "trouter-request": TROUTER_REQUEST, "trouter-client": "{\"cd\":46}"},
        "body": "",
    });
    format!("3:::{message}")
}

fn attach_body(from: &str) -> String {
    json!({
        "attach": {"requireMediaContent": false, "links": {"end": format!("{B_CALLBACKS}/0a0a0a0a/call/end/")}},
        "capabilities": null,
        "endpointCapabilities": 73463,
        "additionalActions": [{
            "input": {
                "conversationRequest": {
                    "roster": {"type": "Delta", "rosterUpdate": format!("{B_CALLBACKS}/0b0b0b0b/conversation/rosterUpdate/")},
                    "links": {"conversationEnd": format!("{B_CALLBACKS}/0c0c0c0c/conversation/conversationEnd/")},
                },
                "endpointMetadata": {},
                "participants": {"from": {"id": from}},
            },
            "name": "join",
            "url": INVITED_CONVERSATION,
            "waitForResponse": true,
        }],
    })
    .to_string()
}

fn accept_body(from: &str) -> String {
    json!({"callAcceptance": {"acceptedBy": {"id": from}, "acceptedCallModalities": ["audio"]}}).to_string()
}

fn reject_body() -> String {
    json!({"callEnd": {"code": 603, "phrase": "CallEndReasonLocalUserInitiated"}}).to_string()
}

#[tokio::test]
async fn only_an_offered_invitation_is_answered_or_declined_and_its_ring_time_sends_pass() -> TestResult {
    let lab = lab_of_b()?;
    let ws = ws_fence(&lab)?;
    let http = http_fence(&lab);
    let unregistered = Verdict::Deny(DenyCause::Fenced(DenyReason::Partial(PartialReason::UnregisteredCall)));
    ws.observe(&created("S1", "ws-1", TROUTER));
    // an invitation to c (the fence is b's): ignored, so no answer and no decline of it pass
    ws.observe(&received("S1", "ws-1", &invitation_frame(7, LAB_A, LAB_C)));
    assert_eq!(
        http.decide(&paused("POST", ACCEPT_URL, &accept_body(LAB_B))).await,
        unregistered
    );
    assert_eq!(
        http.decide(&paused("DELETE", REJECT_URL, &reject_body())).await,
        unregistered
    );
    // what the page sends by itself while it rings passes, whoever's ring it is
    assert_eq!(ws.decide(&text(TROUTER, &receipt_frame(7))).await, Verdict::Allow);
    assert_eq!(
        http.decide(&paused("POST", ATTACH_URL, &attach_body(LAB_B))).await,
        Verdict::Allow
    );
    // a's invitation to b: offered, and only b's own answer of it passes, with nothing of the leg bound
    ws.observe(&received("S1", "ws-1", &invitation_frame(8, LAB_A, LAB_B)));
    assert_eq!(
        http.decide(&paused("POST", ACCEPT_URL, &accept_body(LAB_A))).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::CallMismatch)),
        "an answer sent as the caller"
    );
    assert_eq!(
        http.decide(&paused("POST", ACCEPT_URL, &accept_body(LAB_B))).await,
        Verdict::Allow
    );
    assert_eq!(
        http.decide(&paused("POST", ACCEPT_URL, &accept_body(LAB_B))).await,
        Verdict::Allow,
        "the browser's layer after the target's"
    );
    Ok(())
}
