//! The browser fence feeds the fence's call registry (`teams_fence`'s `calls`, `docs/protocol/fence-targets.md`): a
//! call start it permitted pends with its body, the Trouter deliveries the page receives bind the call controller's
//! links and wait for their responses, and the one response the fence lets through is the acknowledgement of a
//! pending acceptance on its own connection. Synthetic, shaped like the echo-call capture's (made-up ids, hosts and
//! placeholder MRIs); the response-stage read of the start's answer is in `tests/fence_http.rs`.

use std::io::Write as _;
use std::sync::Arc;

use base64::Engine as _;
use proto_lab::cdp::{Event, SessionId};
use proto_lab::classify::{Classification, Classifier};
use proto_lab::fence_http::{DenyCause, HttpFence, PausedRequest, Verdict};
use proto_lab::fence_ws::{Frame, GuardScript, Payload, WsFence, delivery_of};
use serde_json::{Value, json};
use teams_fence::{
    CALL_START_URL, DeliveryDenyReason, DenyReason, FailClosed, Fence, FenceLog, Membership, MembershipFuture,
    MembershipSource, Mri, PartialReason, Roster, Scope, TEST_CALL_SCENARIO,
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

fn answer() -> Vec<u8> {
    json!({
        "conversationController": format!("{CONVERSATION}?{ROUTE}"),
        "links": {"updateEndpointMetadata": format!("{CONVERSATION}/updateEndpointMetadata?{ROUTE}")},
    })
    .to_string()
    .into_bytes()
}

/// Registers the test call with the fence directly, as the response stage would.
async fn register(lab: &Lab) -> TestResult {
    let Classification::Write(target) =
        classifier().classify("POST", CALL_START_URL, &[], Some(start_body().as_bytes()))
    else {
        return Err("the start isn't a write".into());
    };
    let permit = lab.fence.check(target, &FailClosed).await?;
    let pending = lab
        .fence
        .pend_start(&permit, start_body().as_bytes())
        .ok_or("pending")?;
    lab.fence.register_start(pending, &answer())?;
    Ok(())
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

fn session(id: &str) -> SessionId {
    SessionId::new(id)
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

// --- the acceptance and its response, through the WebSocket layer ---

#[tokio::test]
async fn the_acceptance_s_acknowledgement_passes_once_on_its_own_connection() -> TestResult {
    let lab = new_lab()?;
    register(&lab).await?;
    let ws = ws_fence(&lab)?;
    ws.observe(&created("S1", "ws-1", TROUTER));
    ws.observe(&received("S1", "ws-1", &acceptance_frame(41)?));
    let response = text(TROUTER, &response_frame(41));
    assert_eq!(
        ws.decide_on(Some(&session("S2")), &response).await,
        Verdict::Deny(DenyCause::Unclassified),
        "another session's socket"
    );
    assert_eq!(ws.decide_on(Some(&session("S1")), &response).await, Verdict::Allow);
    assert_eq!(
        ws.decide_on(Some(&session("S1")), &response).await,
        Verdict::Deny(DenyCause::Unclassified),
        "once"
    );
    // an empty response to the same kind of delivery is no acknowledgement
    ws.observe(&received("S1", "ws-1", &acceptance_frame(42)?));
    let empty = format!(
        r#"3:::{{"id":42,"status":200,"headers":{{"MS-CV":"a","trouter-request":{},"trouter-client":"b"}},"body":""}}"#,
        serde_json::to_string(TROUTER_REQUEST)?
    );
    assert_eq!(
        ws.decide_on(Some(&session("S1")), &text(TROUTER, &empty)).await,
        Verdict::Deny(DenyCause::DeliveryResponse(DeliveryDenyReason::Shape))
    );
    Ok(())
}

#[tokio::test]
async fn a_closed_or_reopened_socket_or_a_socket_elsewhere_answers_nothing() -> TestResult {
    let lab = new_lab()?;
    register(&lab).await?;
    let ws = ws_fence(&lab)?;
    // a delivery on a socket that isn't Trouter's binds nothing
    ws.observe(&created("S1", "ws-3", "wss://elsewhere.example/socket"));
    ws.observe(&received("S1", "ws-3", &acceptance_frame(8)?));
    let http = http_fence(&lab);
    let acknowledge = format!("{CC}/callAcceptance/call-1/1/a1/1/acknowledge?i=10-0-0-2");
    let body = json!({"callAcceptanceAcknowledgement": {"links": {}}}).to_string();
    let request = paused("POST", &acknowledge, &body);
    assert_eq!(
        http.decide(&request).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Partial(PartialReason::UnregisteredCall)))
    );
    assert_eq!(
        ws.decide_on(
            Some(&session("S1")),
            &text("wss://elsewhere.example/socket", &response_frame(8))
        )
        .await,
        Verdict::Allow,
        "a frame on a socket of no contacting service passes by the frame rule, as before"
    );
    // on Trouter's socket it binds the call, and its response waits on that socket alone
    ws.observe(&created("S1", "ws-1", TROUTER));
    ws.observe(&received("S1", "ws-1", &acceptance_frame(7)?));
    assert_eq!(http.decide(&request).await, Verdict::Allow);
    ws.observe(&event("S1", "Network.webSocketClosed", json!({"requestId": "ws-1"})));
    ws.observe(&created("S1", "ws-2", TROUTER));
    assert_eq!(
        ws.decide_on(Some(&session("S1")), &text(TROUTER, &response_frame(7)))
            .await,
        Verdict::Deny(DenyCause::Unclassified)
    );
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
async fn a_follow_up_passes_only_on_the_registered_call() -> TestResult {
    let lab = new_lab()?;
    let http = http_fence(&lab);
    let url = format!("{CONVERSATION}/updateEndpointMetadata?{ROUTE}");
    let body = json!({"participants": {"from": {"id": LAB_A}}}).to_string();
    assert_eq!(
        http.decide(&paused("PUT", &url, &body)).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Partial(PartialReason::UnregisteredCall)))
    );
    register(&lab).await?;
    assert_eq!(http.decide(&paused("PUT", &url, &body)).await, Verdict::Allow);
    let other = url.replace("SyntheticConversation0", "SyntheticConversation1");
    assert_eq!(
        http.decide(&paused("PUT", &other, &body)).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Partial(PartialReason::UnregisteredCall)))
    );
    Ok(())
}

#[tokio::test]
async fn a_permitted_start_pends_once_whichever_layer_permits_it_first() -> TestResult {
    let lab = new_lab()?;
    let http = http_fence(&lab);
    let start = paused("POST", CALL_START_URL, &start_body());
    assert_eq!(http.decide(&start).await, Verdict::Allow);
    assert_eq!(
        http.decide(&start).await,
        Verdict::Allow,
        "the browser's layer after the target's"
    );
    // nothing is registered until the answer is read at the response stage
    let url = format!("{CONVERSATION}/updateEndpointMetadata?{ROUTE}");
    let body = json!({"participants": {"from": {"id": LAB_A}}}).to_string();
    assert_eq!(
        http.decide(&paused("PUT", &url, &body)).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Partial(PartialReason::UnregisteredCall)))
    );
    Ok(())
}

// --- an incoming call's invitation, offered to the browser's own account ---

const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const INVITATION_PATH: &str = "/v4/f/SyntheticEndpoint00001/";
const ATTACH_URL: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/forked/SyntheticCall1/27/i1/1/attach?i=10-0-0-1";
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

#[tokio::test]
async fn an_offered_invitation_s_receipt_and_attach_pass_and_nothing_for_another_s() -> TestResult {
    let lab = lab_of_b()?;
    let ws = ws_fence(&lab)?;
    let http = http_fence(&lab);
    ws.observe(&created("S1", "ws-1", TROUTER));
    // an invitation to c (the fence is b's): ignored, its receipt denied by the frame rule, its attach unregistered
    ws.observe(&received("S1", "ws-1", &invitation_frame(7, LAB_A, LAB_C)));
    assert_eq!(
        ws.decide_on(Some(&session("S1")), &text(TROUTER, &receipt_frame(7)))
            .await,
        Verdict::Deny(DenyCause::Unclassified)
    );
    assert_eq!(
        http.decide(&paused("POST", ATTACH_URL, &attach_body(LAB_C))).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Partial(PartialReason::UnregisteredCall)))
    );
    // a's invitation to b: offered
    ws.observe(&received("S1", "ws-1", &invitation_frame(8, LAB_A, LAB_B)));
    assert_eq!(
        ws.decide_on(Some(&session("S1")), &text(TROUTER, &receipt_frame(8)))
            .await,
        Verdict::Allow
    );
    assert_eq!(
        ws.decide_on(Some(&session("S1")), &text(TROUTER, &receipt_frame(8)))
            .await,
        Verdict::Deny(DenyCause::Unclassified),
        "once"
    );
    assert_eq!(
        http.decide(&paused("POST", ATTACH_URL, &attach_body(LAB_B))).await,
        Verdict::Allow
    );
    assert_eq!(
        http.decide(&paused("POST", ATTACH_URL, &attach_body(LAB_A))).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::CallMismatch)),
        "sent as the caller"
    );
    Ok(())
}
