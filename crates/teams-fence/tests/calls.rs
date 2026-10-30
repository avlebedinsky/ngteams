//! The call registry (`teams_fence`'s `calls` module, `docs/protocol/fence-targets.md`), under the registry's rule (no
//! build's fence's: `Fence::tracking_calls` here, the registry's resolution as verdicts): a call this run's fence permitted is registered once its start's
//! answer is read, its call controller links are bound only by the acceptance delivered at the start's own acceptance
//! callback, and a follow-up passes only as a captured operation to exactly a link of a live registered call. The lab's
//! rule, which judges only a call's entry, is at the end ("the lab's call rule"). Shaped like the echo-call capture's
//! (`tests/fixtures/calling/echo/`), with made-up ids, hosts and placeholder MRIs.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use teams_fence::{
    CALL_START_URL, CallOperation, Contact, Delivery, DeliveryDecision, DeliveryDenyReason, DenyReason, FailClosed,
    Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, Observed, PartialReason, PendingStart,
    Policy, RegisterError, Request, Roster, Scope, TEST_CALL_SCENARIO, Target, WriteKind, WritePermit, contacting,
};

const LAB_A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const ECHO: &str = "28:00000000-0000-4000-8000-0000000000e0";
const COWORKER: &str = "8:orgid:00000000-0000-4000-8000-0000000000c0";

const PROXY: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep";
const CONV_HOST: &str = "conv-zz-01-prod-aks.conv.skype.com";
const CC_HOST: &str = "cc-zz-01-prod-aks.cc.skype.com";
const ROUTE: &str = "i=10-0-0-1&e=1";
const CALLBACKS: &str =
    "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/callAgent/agent-1";
const ACCEPTANCE_PATH: &str = "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/03030303/call/acceptance/";
const END_PATH: &str = "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/04040404/call/end/";
const CONVERSATION_END_PATH: &str =
    "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/05050505/conversation/conversationEnd/";
const CONNECTION: &str = "session-1/socket-1";
const TROUTER_REQUEST: &str = r#"{"id":"request-1","port":3443,"src":"198.51.100.7"}"#;
/// The acceptance delivery's chain and message ids, which its response echoes.
const CHAIN_ID: &str = "00000000-0000-4000-8000-00000000c4a1";
const MESSAGE_ID: &str = "00000000-0000-4000-8000-00000000e55a";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Lab {
    _dir: tempfile::TempDir,
    log: PathBuf,
    fence: Fence,
}

fn new_lab() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("fence.log");
    let roster = Roster::new([Mri::new(LAB_A), Mri::new(LAB_B)], [Mri::new(ECHO)]);
    let fence = Fence::new(roster, FenceLog::open(&log)?).tracking_calls();
    Ok(Lab { _dir: dir, log, fence })
}

fn policy() -> Policy {
    Policy::new().with_echo_callee(Mri::new(ECHO))
}

/// The test call start, its callbacks on the endpoint's call agent.
fn start_body() -> Vec<u8> {
    json!({
        "conversationRequest": {
            "conversationType": null,
            "suppressDialout": true,
            "links": {
                "conversationEnd": format!("{CALLBACKS}/05050505/conversation/conversationEnd/"),
                "conversationUpdate": format!("{CALLBACKS}/06060606/conversation/conversationUpdate/"),
            },
            "scenario": TEST_CALL_SCENARIO,
        },
        "participants": {"from": {"id": LAB_A, "displayName": "Lab A"}, "to": []},
        "groupChat": null,
        "callInvitation": {
            "callModalities": ["Audio"],
            "links": {
                "acceptance": format!("{CALLBACKS}/03030303/call/acceptance/"),
                "end": format!("{CALLBACKS}/04040404/call/end/"),
            },
            "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
        },
    })
    .to_string()
    .into_bytes()
}

fn conversation(id: &str) -> String {
    format!("{PROXY}/{CONV_HOST}/conv/{id}")
}

/// The start's answer for the conversation `id`: the controller and a few of its links.
fn answer(id: &str) -> Vec<u8> {
    let at = |op: &str| format!("{}/{op}?{ROUTE}", conversation(id));
    json!({
        "conversationController": format!("{}?{ROUTE}", conversation(id)),
        "links": {
            "leave": at("leave"),
            "addParticipant": at("addParticipant"),
            "addParticipantAndModality": at("add"),
            "updateEndpointMetadata": at("updateEndpointMetadata"),
            "removeParticipant": at("removeParticipant"),
        },
    })
    .to_string()
    .into_bytes()
}

fn acknowledge_url(call: &str) -> String {
    format!("{PROXY}/{CC_HOST}/cc/v1/callAcceptance/{call}/1/a1/1/acknowledge?i=10-0-0-2")
}

fn channel_url(call: &str) -> String {
    format!("{PROXY}/{CC_HOST}/cc/v1/mcProxy/{call}/1/k1/1/applyChannelParameters?i=10-0-0-2&e=1")
}

/// The acceptance's body for the call controller's call `call`.
fn acceptance(call: &str) -> Vec<u8> {
    json!({
        "callAcceptance": {
            "acceptedCallModalities": [],
            "links": {
                "acknowledgement": acknowledge_url(call),
                "callLeg": format!("{PROXY}/{CC_HOST}/cc/v1/active/{call}/1/a1/1?i=10-0-0-2&e=1"),
                "applyChannelParameters": channel_url(call),
            },
            "callKeepAliveInterval": 2700,
        }
    })
    .to_string()
    .into_bytes()
}

fn delivery(id: i64, path: &str, body: Vec<u8>) -> Delivery {
    Delivery {
        connection: CONNECTION.to_owned(),
        id,
        path: path.to_owned(),
        trouter_request: Some(TROUTER_REQUEST.to_owned()),
        chain_id: Some(CHAIN_ID.to_owned()),
        message_id: Some(MESSAGE_ID.to_owned()),
        body,
    }
}

fn from() -> Value {
    json!({"id": LAB_A, "displayName": "Lab A", "endpointId": "endpoint", "participantId": "participant", "languageId": "en-us"})
}

/// The 1:1 chat id of `a` and `b` (`8:orgid:<guid>` or `28:<guid>`), the two ids sorted.
fn one_to_one(a: &str, b: &str) -> String {
    let id = |mri: &str| mri.rsplit(':').next().unwrap_or_default().to_owned();
    let mut ids = [id(a), id(b)];
    ids.sort();
    format!("19:{}_{}@unq.gbl.spaces", ids[0], ids[1])
}

/// The captured body of `operation`, with `sender` as its sender and `invitees` invited (an `add`: naming the 1:1 chat
/// of the sender and a lone invitee, and the sender as the meeting's organizer, as the capture does).
fn body(operation: CallOperation, sender: &Value, invitees: &[&str]) -> Vec<u8> {
    let to: Vec<Value> = invitees
        .iter()
        .map(|id| json!({"id": id, "participantId": "invited"}))
        .collect();
    let sender_id = sender["id"].as_str().unwrap_or_default();
    let body = match operation {
        CallOperation::UpdateEndpointMetadata => json!({
            "endpointMetadata": {"holographicCapabilities": 3},
            "participants": {"from": sender},
        }),
        CallOperation::Leave => json!({
            "participants": {"from": sender},
            "conversationTransactionEnd": {"reason": "noError", "code": 0, "phrase": "ConversationEndNoModalityConnected"},
            "callTransactionEnd": {"code": 0, "subCode": 0, "phrase": "CallEndReasonLocalUserInitiated", "resultCategories": ["Success"]},
        }),
        CallOperation::Add => {
            let mut add = json!({
                "disableUnmute": false,
                "groupContext": null,
                "links": {"addParticipantSuccess": format!("{CALLBACKS}/07070707/conversation/addParticipantSuccess/")},
                "participantInvitationData": {"invitationData": {"meetingInfo": {
                    "organizerId": sender_id.rsplit(':').next().unwrap_or_default(),
                    "tenantId": "00000000-0000-4000-8000-0000000000f0",
                }}},
                "participants": {"from": sender, "to": to},
                "replacementDetails": null,
            });
            if let [invitee] = invitees {
                add["groupChat"] = json!({"messageId": null, "threadId": one_to_one(sender_id, invitee)});
            }
            add
        }
        CallOperation::Acknowledge => json!({
            "callAcceptanceAcknowledgement": {"links": {
                "mediaRenegotiation": format!("{CALLBACKS}/08080808/call/mediaRenegotiation/"),
                "transfer": format!("{CALLBACKS}/09090909/call/transfer/"),
            }}
        }),
        CallOperation::ApplyChannelParameters => json!({
            "applyChannelParameters": {"multiChannelParameter": {"mids": ["1"], "mediaParameter": "{\"maxVideoSendCapabilities\":{}}"}}
        }),
        // the callee's side and the media negotiation: `tests/offers.rs`, `tests/negotiation.rs`
        _ => Value::Null,
    };
    body.to_string().into_bytes()
}

/// What the lab rule makes of `method url body`.
fn classify(method: &str, url: &str, body: &[u8]) -> Contact {
    contacting(&Request::new(method, url).with_body(Some(body)), &policy())
}

/// The write `operation` to `url` makes, as the lab rule maps it.
fn write(operation: CallOperation, url: &str, invitees: &[&str]) -> TestResult<Target> {
    match classify(operation.method(), url, &body(operation, &from(), invitees)) {
        Contact::Write(target) => Ok(target),
        other => Err(format!("{operation} to {url} isn't a write: {other:?}").into()),
    }
}

/// The URL of `operation` on the conversation `id`.
fn op_url(operation: CallOperation, id: &str) -> String {
    format!("{}/{}?{ROUTE}", conversation(id), operation.segment())
}

async fn permit_start(fence: &Fence) -> TestResult<WritePermit> {
    let Contact::Write(target) = classify("POST", CALL_START_URL, &start_body()) else {
        return Err("the start isn't a write".into());
    };
    Ok(fence.check(target, &FailClosed).await?)
}

/// A start the fence permitted, pending.
async fn pending(fence: &Fence) -> TestResult<PendingStart> {
    let permit = permit_start(fence).await?;
    fence
        .pend_start(&permit, &start_body())
        .ok_or_else(|| "the permitted start isn't pending".into())
}

/// A call registered with the conversation `id`.
async fn registered(fence: &Fence, id: &str) -> TestResult {
    let pending = pending(fence).await?;
    fence.register_start(pending, &answer(id))?;
    Ok(())
}

async fn verdict(fence: &Fence, target: Target) -> Result<(), DenyReason> {
    fence
        .check(target, &FailClosed)
        .await
        .map(drop)
        .map_err(|denied| *denied.reason())
}

/// The operations the echo call's capture showed on the caller's call.
const ECHO_OPERATIONS: [CallOperation; 5] = [
    CallOperation::UpdateEndpointMetadata,
    CallOperation::Add,
    CallOperation::Leave,
    CallOperation::Acknowledge,
    CallOperation::ApplyChannelParameters,
];

const UNREGISTERED: DenyReason = DenyReason::Partial(PartialReason::UnregisteredCall);
const ENDED: DenyReason = DenyReason::Partial(PartialReason::CallEnded);

// --- the follow-ups' rows ---

#[test]
fn each_captured_follow_up_is_a_call_operation_on_its_exact_url() -> TestResult {
    for operation in ECHO_OPERATIONS {
        let url = match operation {
            CallOperation::Acknowledge => acknowledge_url("call-1"),
            CallOperation::ApplyChannelParameters => channel_url("call-1"),
            _ => op_url(operation, "conv-1"),
        };
        let invitees: &[&str] = if operation == CallOperation::Add { &[ECHO] } else { &[] };
        let target = write(operation, &url, invitees)?;
        let from = operation.names_sender().then(|| Mri::new(LAB_A));
        let callbacks = match operation {
            CallOperation::Acknowledge => vec![
                format!("{CALLBACKS}/08080808/call/mediaRenegotiation/"),
                format!("{CALLBACKS}/09090909/call/transfer/"),
            ],
            CallOperation::Add => vec![format!("{CALLBACKS}/07070707/conversation/addParticipantSuccess/")],
            _ => Vec::new(),
        };
        let scope = Scope::CallOperation {
            operation,
            url,
            from,
            callbacks: callbacks.into(),
            joins: None,
        };
        let expected = Target::new(operation.kind(), scope).with_added(invitees.iter().copied().map(Mri::new));
        assert_eq!(target, expected, "{operation}");
    }
    Ok(())
}

#[test]
fn a_follow_up_off_its_captured_method_host_path_or_shape_is_unreadable() {
    let url = op_url(CallOperation::Leave, "conv-1");
    let leave = body(CallOperation::Leave, &from(), &[]);
    let mut extra: Value = serde_json::from_slice(&leave).unwrap_or_default();
    extra["transfer"] = json!({"to": "8:orgid:x"});
    let mut duplicate = String::from_utf8_lossy(&leave).into_owned();
    duplicate.insert_str(1, r#""Participants":{},"#);
    for (method, url, body) in [
        ("PUT", url.clone(), leave.clone()),
        ("POST", url.replace("https:", "http:"), leave.clone()),
        (
            "POST",
            url.replace("api.flightproxy", "elsewhere.flightproxy"),
            leave.clone(),
        ),
        ("POST", url.replace("/leave?", "/merge?"), leave.clone()),
        ("POST", url.replace("/leave?", "/leave/x?"), leave.clone()),
        ("POST", url.clone(), extra.to_string().into_bytes()),
        ("POST", url.clone(), duplicate.into_bytes()),
        ("POST", url.clone(), b"{}".to_vec()),
        (
            "POST",
            op_url(CallOperation::Add, "conv-1"),
            body(CallOperation::Add, &from(), &[]),
        ),
        (
            "POST",
            acknowledge_url("c").replace("callAcceptance", "active"),
            body(CallOperation::Acknowledge, &from(), &[]),
        ),
        (
            "POST",
            channel_url("c").replace("mcProxy", "callAcceptance"),
            body(CallOperation::ApplyChannelParameters, &from(), &[]),
        ),
    ] {
        assert_eq!(classify(method, &url, &body), Contact::Unreadable, "{method} {url}");
    }
    let request = Request::new("POST", &url);
    assert_eq!(contacting(&request, &policy()), Contact::Unreadable, "no body");
}

// --- a call start from a chat (`tests/fixtures/calling/chat/starts.json`) ---

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const GROUP_CHAT: &str = "19:0123456789abcdef0123456789abcdef@thread.v2";

/// A source that answers every read with the same members.
struct Members(Membership);

impl Members {
    fn of(members: &[&str]) -> Self {
        Self(Membership::Complete(members.iter().copied().map(Mri::new).collect()))
    }
}

impl MembershipSource for Members {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        let membership = self.0.clone();
        Box::pin(async move { membership })
    }
}

/// A lab of a, b and c with the echo service.
fn new_lab_of_three() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("fence.log");
    let roster = Roster::new([Mri::new(LAB_A), Mri::new(LAB_B), Mri::new(LAB_C)], [Mri::new(ECHO)]);
    let fence = Fence::new(roster, FenceLog::open(&log)?).tracking_calls();
    Ok(Lab { _dir: dir, log, fence })
}

/// A chat's call start from a, its callbacks on the endpoint's call agent: to `callees` in the group chat `group`, or
/// with no group chat (a 1:1 call).
fn chat_start_body(callees: &[&str], group: Option<&str>) -> Vec<u8> {
    let to: Vec<Value> = callees
        .iter()
        .map(|id| json!({"id": id, "participantId": "callee"}))
        .collect();
    json!({
        "conversationRequest": {
            "conversationType": null,
            "suppressDialout": false,
            "links": {
                "conversationEnd": format!("{CALLBACKS}/05050505/conversation/conversationEnd/"),
                "conversationUpdate": format!("{CALLBACKS}/06060606/conversation/conversationUpdate/"),
            },
        },
        "participants": {"from": {"id": LAB_A, "displayName": "Lab A"}, "to": to},
        "groupChat": group.map(|id| json!({"threadId": id, "messageId": null})),
        "callInvitation": {
            "callModalities": ["Audio"],
            "links": {
                "acceptance": format!("{CALLBACKS}/03030303/call/acceptance/"),
                "end": format!("{CALLBACKS}/04040404/call/end/"),
            },
            "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0", "requiredFeatures": "nonByPass"},
        },
    })
    .to_string()
    .into_bytes()
}

/// The target the lab rule makes of a chat's call start.
fn chat_start(body: &[u8]) -> TestResult<Target> {
    match classify("POST", CALL_START_URL, body) {
        Contact::Write(target) => Ok(target),
        other => Err(format!("the chat's start isn't a write: {other:?}").into()),
    }
}

#[tokio::test]
async fn a_call_start_from_a_chat_is_judged_by_the_chat_s_fresh_members_and_every_callee() -> TestResult {
    let lab = new_lab_of_three()?;
    let one = chat_start(&chat_start_body(&[LAB_B], None))?;
    assert_eq!(
        one.scope(),
        &Scope::Conversation {
            id: one_to_one(LAB_A, LAB_B)
        }
    );
    let judged = async |target: &Target, source: &Members| {
        lab.fence
            .check(target.clone(), source)
            .await
            .map(drop)
            .map_err(|denied| *denied.reason())
    };
    assert_eq!(judged(&one, &Members::of(&[LAB_A, LAB_B])).await, Ok(()));
    assert_eq!(
        judged(&one, &Members::of(&[LAB_A, COWORKER])).await,
        Err(DenyReason::Outsiders { count: 1 }),
        "the chat's read has someone outside the lab"
    );
    assert_eq!(
        judged(&one, &Members(Membership::Partial(PartialReason::NoSource))).await,
        Err(DenyReason::Partial(PartialReason::NoSource)),
        "a chat that can't be read in full"
    );
    let group = chat_start(&chat_start_body(&[LAB_B, LAB_C], Some(GROUP_CHAT)))?;
    assert_eq!(group.scope(), &Scope::Conversation { id: GROUP_CHAT.into() });
    assert_eq!(judged(&group, &Members::of(&[LAB_A, LAB_B, LAB_C])).await, Ok(()));
    let a_and_b = chat_start(&chat_start_body(&[LAB_B], Some(GROUP_CHAT)))?;
    assert_eq!(
        judged(&a_and_b, &Members::of(&[LAB_A, LAB_B, LAB_C])).await,
        Ok(()),
        "a lab-only chat with one more lab member than it calls"
    );
    let outsider = chat_start(&chat_start_body(&[LAB_B, COWORKER], Some(GROUP_CHAT)))?;
    assert_eq!(
        judged(&outsider, &Members::of(&[LAB_A, LAB_B, LAB_C])).await,
        Err(DenyReason::Outsiders { count: 1 }),
        "a callee outside the lab, the chat lab-only"
    );
    Ok(())
}

#[tokio::test]
async fn a_permitted_call_start_from_a_chat_pends_and_registers_as_the_test_call_s_does() -> TestResult {
    let lab = new_lab_of_three()?;
    let body = chat_start_body(&[LAB_B, LAB_C], Some(GROUP_CHAT));
    let members = Members::of(&[LAB_A, LAB_B, LAB_C]);
    let permit = lab.fence.check(chat_start(&body)?, &members).await?;
    for (other, why) in [
        (chat_start_body(&[LAB_B], Some(GROUP_CHAT)), "fewer callees"),
        (chat_start_body(&[LAB_B, LAB_C], None), "no group chat"),
        (start_body(), "the test call's start"),
        (b"{}".to_vec(), "not a start"),
    ] {
        assert!(lab.fence.pend_start(&permit, &other).is_none(), "{why}");
    }
    let pending = lab
        .fence
        .pend_start(&permit, &body)
        .ok_or("the permitted start isn't pending")?;
    assert!(lab.fence.pend_start(&permit, &body).is_none(), "a permit pends once");
    let leave = write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?;
    assert_eq!(verdict(&lab.fence, leave.clone()).await, Err(UNREGISTERED));
    lab.fence.register_start(pending, &answer("conv-1"))?;
    let metadata = write(
        CallOperation::UpdateEndpointMetadata,
        &op_url(CallOperation::UpdateEndpointMetadata, "conv-1"),
        &[],
    )?;
    assert_eq!(verdict(&lab.fence, metadata).await, Ok(()));
    assert_eq!(verdict(&lab.fence, leave).await, Ok(()));
    let other = new_lab_of_three()?;
    assert!(
        other.fence.pend_start(&permit, &body).is_none(),
        "another fence's permit"
    );
    Ok(())
}

#[tokio::test]
async fn a_call_from_a_chat_takes_follow_ups_only_from_its_caller_and_adds_only_lab_accounts() -> TestResult {
    let lab = new_lab_of_three()?;
    let body = chat_start_body(&[LAB_B], None);
    let permit = lab
        .fence
        .check(chat_start(&body)?, &Members::of(&[LAB_A, LAB_B]))
        .await?;
    let pending = lab.fence.pend_start(&permit, &body).ok_or("not pending")?;
    lab.fence.register_start(pending, &answer("conv-1"))?;
    let by_b = json!({"id": LAB_B, "displayName": "Lab B"});
    let leave_by_b = match classify(
        "POST",
        &op_url(CallOperation::Leave, "conv-1"),
        &body_of(CallOperation::Leave, &by_b),
    ) {
        Contact::Write(target) => target,
        other => return Err(format!("b's leave isn't a write: {other:?}").into()),
    };
    assert_eq!(
        verdict(&lab.fence, leave_by_b).await,
        Err(DenyReason::CallMismatch),
        "b isn't the call's caller"
    );
    Ok(())
}

fn body_of(operation: CallOperation, sender: &Value) -> Vec<u8> {
    body(operation, sender, &[])
}

/// The deliveries of a call from a chat (`tests/fixtures/calling/chat/deliveries.json`), their placeholders filled with
/// this file's lab accounts and made-up ids.
const CHAT_DELIVERIES: &str = include_str!("../../../tests/fixtures/calling/chat/deliveries.json");

fn chat_deliveries() -> TestResult<Value> {
    let mut filled = CHAT_DELIVERIES
        .replace("<guid:a>", LAB_A.trim_start_matches("8:orgid:"))
        .replace("<guid:b>", LAB_B.trim_start_matches("8:orgid:"));
    for (n, placeholder) in ["tenant", "voicemail", "call", "aa", "bb", "3", "4", "agentb"]
        .iter()
        .enumerate()
    {
        filled = filled.replace(
            &format!("<guid:{placeholder}>"),
            &format!("00000000-0000-4000-8000-0000000002{n:02}"),
        );
    }
    Ok(serde_json::from_str(&filled)?)
}

#[tokio::test]
async fn an_unanswered_call_from_a_chat_goes_to_the_callee_s_voicemail_which_accepts_it() -> TestResult {
    let lab = new_lab_of_three()?;
    let body = chat_start_body(&[LAB_B], None);
    let permit = lab
        .fence
        .check(chat_start(&body)?, &Members::of(&[LAB_A, LAB_B]))
        .await?;
    let pending = lab.fence.pend_start(&permit, &body).ok_or("not pending")?;
    lab.fence.register_start(pending, &answer("conv-1"))?;
    let fixture = chat_deliveries()?;
    for (id, case) in (1..).zip(fixture["cases"].as_array().ok_or("cases")?) {
        let path = format!(
            "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/{}",
            case["callback"].as_str().unwrap_or_default()
        );
        let expected = match case["observed"].as_str() {
            Some("acceptance") => Observed::Acceptance,
            _ => Observed::Nothing,
        };
        let observed = lab
            .fence
            .observe_delivery(&delivery(id, &path, case["body"].to_string().into_bytes()));
        assert_eq!(observed, expected, "{}", case["name"]);
    }
    // the voicemail's acceptance bound the call controller's acknowledgement link
    let acknowledge = fixture["cases"][1]["body"]["callAcceptance"]["links"]["acknowledgement"]
        .as_str()
        .ok_or("acknowledgement")?;
    assert_eq!(
        verdict(&lab.fence, write(CallOperation::Acknowledge, acknowledge, &[])?).await,
        Ok(())
    );
    Ok(())
}

// --- registering a call ---

#[tokio::test]
async fn a_permitted_start_registers_its_conversation_once_its_answer_is_read() -> TestResult {
    let lab = new_lab()?;
    let metadata = write(
        CallOperation::UpdateEndpointMetadata,
        &op_url(CallOperation::UpdateEndpointMetadata, "conv-1"),
        &[],
    )?;
    assert_eq!(
        verdict(&lab.fence, metadata.clone()).await,
        Err(UNREGISTERED),
        "nothing registered yet"
    );
    let pending = pending(&lab.fence).await?;
    assert_eq!(
        verdict(&lab.fence, metadata.clone()).await,
        Err(UNREGISTERED),
        "pending, no answer read"
    );
    lab.fence.register_start(pending, &answer("conv-1"))?;
    assert_eq!(verdict(&lab.fence, metadata).await, Ok(()));
    let leave = write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?;
    assert_eq!(verdict(&lab.fence, leave).await, Ok(()));
    Ok(())
}

#[tokio::test]
async fn a_follow_up_on_another_conversation_host_route_or_link_is_unregistered() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    let url = op_url(CallOperation::UpdateEndpointMetadata, "conv-1");
    for other in [
        op_url(CallOperation::UpdateEndpointMetadata, "conv-2"),
        url.replace(CONV_HOST, "conv-yy-02-prod-aks.conv.skype.com"),
        url.replace(ROUTE, "i=10-0-0-9&e=1"),
        url.replace(&format!("?{ROUTE}"), ""),
    ] {
        let target = write(CallOperation::UpdateEndpointMetadata, &other, &[])?;
        assert_eq!(verdict(&lab.fence, target).await, Err(UNREGISTERED), "{other}");
    }
    Ok(())
}

#[tokio::test]
async fn a_call_of_another_run_is_unregistered_and_its_permit_feeds_no_other_fence() -> TestResult {
    let first = new_lab()?;
    let second = new_lab()?;
    registered(&first.fence, "conv-1").await?;
    let target = write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?;
    assert_eq!(verdict(&second.fence, target).await, Err(UNREGISTERED));
    let permit = permit_start(&first.fence).await?;
    assert!(
        second.fence.pend_start(&permit, &start_body()).is_none(),
        "another fence's permit"
    );
    let pending = first.fence.pend_start(&permit, &start_body()).ok_or("pending")?;
    assert!(
        first.fence.pend_start(&permit, &start_body()).is_none(),
        "a permit is taken once"
    );
    assert_eq!(
        second.fence.register_start(pending, &answer("conv-9")),
        Err(RegisterError::OtherFence)
    );
    Ok(())
}

#[tokio::test]
async fn only_a_call_start_permit_with_its_own_body_pends() -> TestResult {
    let lab = new_lab()?;
    let permit = permit_start(&lab.fence).await?;
    let other_caller = String::from_utf8(start_body())?.replace(LAB_A, LAB_B);
    assert!(
        lab.fence.pend_start(&permit, other_caller.as_bytes()).is_none(),
        "another caller"
    );
    assert!(lab.fence.pend_start(&permit, b"{}").is_none(), "not a start");
    let chat = Target::new(WriteKind::ChatCreate, Scope::New).with_added([Mri::new(LAB_A), Mri::new(LAB_B)]);
    let chat_permit = lab.fence.check(chat, &FailClosed).await?;
    assert!(
        lab.fence.pend_start(&chat_permit, &start_body()).is_none(),
        "not a call start"
    );
    assert!(
        lab.fence.pend_start(&permit, &start_body()).is_some(),
        "untaken, with its body"
    );
    Ok(())
}

#[tokio::test]
async fn an_answer_or_acceptance_that_does_not_read_binds_nothing() -> TestResult {
    let lab = new_lab()?;
    let target = write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?;
    for unreadable in [
        b"not json".to_vec(),
        br#"{"conversationController":"https://elsewhere.example/api/v2/ep/h/conv/conv-1"}"#.to_vec(),
        br#"{"conversationController":"https://api.flightproxy.teams.microsoft.com/api/v2/ep/h/conv"}"#.to_vec(),
        br#"{"conversationController":1}"#.to_vec(),
    ] {
        let pending = pending(&lab.fence).await?;
        assert_eq!(
            lab.fence.register_start(pending, &unreadable),
            Err(RegisterError::Unreadable)
        );
        assert_eq!(verdict(&lab.fence, target.clone()).await, Err(UNREGISTERED));
    }
    // a link that isn't the controller's path plus its operation, or another route, is dropped
    let pending = pending(&lab.fence).await?;
    let off = String::from_utf8(answer("conv-1"))?
        .replace("conv-1/leave", "conv-2/leave")
        .replace(
            &format!("conv-1/updateEndpointMetadata?{ROUTE}"),
            "conv-1/updateEndpointMetadata?i=x",
        );
    lab.fence.register_start(pending, off.as_bytes())?;
    assert_eq!(verdict(&lab.fence, target).await, Err(UNREGISTERED));
    let metadata = write(
        CallOperation::UpdateEndpointMetadata,
        &op_url(CallOperation::UpdateEndpointMetadata, "conv-1"),
        &[],
    )?;
    assert_eq!(verdict(&lab.fence, metadata).await, Err(UNREGISTERED));
    let add = write(CallOperation::Add, &op_url(CallOperation::Add, "conv-1"), &[ECHO])?;
    assert_eq!(verdict(&lab.fence, add).await, Ok(()), "the add link stayed");
    Ok(())
}

// --- binding the call controller's call ---

#[tokio::test]
async fn the_call_controller_s_links_bind_only_through_the_start_s_acceptance() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    let acknowledge = write(CallOperation::Acknowledge, &acknowledge_url("call-1"), &[])?;
    let channel = write(CallOperation::ApplyChannelParameters, &channel_url("call-1"), &[])?;
    assert_eq!(verdict(&lab.fence, acknowledge.clone()).await, Err(UNREGISTERED));
    let elsewhere = "/v4/f/SyntheticEndpoint00000/callAgent/agent-2/03030303/call/acceptance/";
    assert_eq!(
        lab.fence
            .observe_delivery(&delivery(1, elsewhere, acceptance("call-1"))),
        Observed::Nothing
    );
    assert_eq!(verdict(&lab.fence, acknowledge.clone()).await, Err(UNREGISTERED));
    assert_eq!(
        lab.fence
            .observe_delivery(&delivery(2, ACCEPTANCE_PATH, b"{}".to_vec())),
        Observed::AcceptanceUnbound
    );
    assert_eq!(
        lab.fence
            .observe_delivery(&delivery(3, ACCEPTANCE_PATH, acceptance("call-1"))),
        Observed::Acceptance
    );
    assert_eq!(verdict(&lab.fence, acknowledge).await, Ok(()));
    assert_eq!(verdict(&lab.fence, channel).await, Ok(()));
    // a redelivery naming another call binds nothing
    assert_eq!(
        lab.fence
            .observe_delivery(&delivery(4, ACCEPTANCE_PATH, acceptance("call-2"))),
        Observed::AcceptanceUnbound
    );
    let other = write(CallOperation::Acknowledge, &acknowledge_url("call-2"), &[])?;
    assert_eq!(verdict(&lab.fence, other).await, Err(UNREGISTERED));
    Ok(())
}

#[tokio::test]
async fn an_acceptance_naming_two_calls_binds_nothing() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    let mixed = String::from_utf8(acceptance("call-1"))?.replace("mcProxy/call-1", "mcProxy/call-2");
    assert_eq!(
        lab.fence
            .observe_delivery(&delivery(1, ACCEPTANCE_PATH, mixed.into_bytes())),
        Observed::AcceptanceUnbound
    );
    let acknowledge = write(CallOperation::Acknowledge, &acknowledge_url("call-1"), &[])?;
    assert_eq!(verdict(&lab.fence, acknowledge).await, Err(UNREGISTERED));
    Ok(())
}

#[tokio::test]
async fn an_acceptance_before_the_answer_waits_on_the_pending_call() -> TestResult {
    let lab = new_lab()?;
    let pending = pending(&lab.fence).await?;
    assert_eq!(
        lab.fence
            .observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call-1"))),
        Observed::Acceptance
    );
    let acknowledge = write(CallOperation::Acknowledge, &acknowledge_url("call-1"), &[])?;
    assert_eq!(
        verdict(&lab.fence, acknowledge.clone()).await,
        Err(UNREGISTERED),
        "pending grants nothing"
    );
    lab.fence.register_start(pending, &answer("conv-1"))?;
    assert_eq!(verdict(&lab.fence, acknowledge).await, Ok(()));
    Ok(())
}

#[tokio::test]
async fn a_start_sent_again_after_one_not_taken_is_the_call_its_acceptance_binds_to() -> TestResult {
    let lab = new_lab()?;
    // the first attempt was answered without success (a throttle): it ends unregistered
    let refused = pending(&lab.fence).await?;
    lab.fence.abandon_start(refused);
    let again = pending(&lab.fence).await?;
    lab.fence.register_start(again, &answer("conv-1"))?;
    assert_eq!(
        lab.fence
            .observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call-1"))),
        Observed::Acceptance
    );
    let acknowledge = write(CallOperation::Acknowledge, &acknowledge_url("call-1"), &[])?;
    assert_eq!(verdict(&lab.fence, acknowledge).await, Ok(()));
    let leave = write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?;
    assert_eq!(verdict(&lab.fence, leave).await, Ok(()));

    // a fence abandons only its own pending start
    let lab = new_lab()?;
    let refused = pending(&lab.fence).await?;
    let other = new_lab()?;
    other.fence.abandon_start(pending(&lab.fence).await?);
    lab.fence.abandon_start(refused);
    assert_eq!(
        lab.fence
            .observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call-1"))),
        Observed::Acceptance,
        "another fence's abandon left the second pending"
    );
    Ok(())
}

// --- what a follow-up must fit ---

#[tokio::test]
async fn an_add_is_judged_per_invitee() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    let url = op_url(CallOperation::Add, "conv-1");
    assert_eq!(
        verdict(&lab.fence, write(CallOperation::Add, &url, &[ECHO])?).await,
        Ok(())
    );
    assert_eq!(
        verdict(&lab.fence, write(CallOperation::Add, &url, &[LAB_B])?).await,
        Ok(())
    );
    assert_eq!(
        verdict(&lab.fence, write(CallOperation::Add, &url, &[COWORKER])?).await,
        Err(DenyReason::Outsiders { count: 1 })
    );
    assert_eq!(
        verdict(&lab.fence, write(CallOperation::Add, &url, &[ECHO, COWORKER])?).await,
        Err(DenyReason::Outsiders { count: 1 })
    );
    Ok(())
}

/// What the lab rule makes of an `add` to `conv-1` whose body is `body`.
fn classify_add(body: &Value) -> Contact {
    classify(
        "POST",
        &op_url(CallOperation::Add, "conv-1"),
        body.to_string().as_bytes(),
    )
}

#[test]
fn an_add_names_only_the_1_1_chat_of_the_caller_and_its_invitee_and_the_caller_as_organizer() -> TestResult {
    let add: Value = serde_json::from_slice(&body(CallOperation::Add, &from(), &[ECHO]))?;
    let with = |pointer: &str, value: Value| {
        let mut add = add.clone();
        if let Some(slot) = add.pointer_mut(pointer) {
            *slot = value;
        }
        add
    };
    let without = |key: &str| {
        let mut add = add.clone();
        add.as_object_mut().map(|add| add.remove(key));
        add
    };
    let reversed = {
        let id = |mri: &str| mri.rsplit(':').next().unwrap_or_default().to_owned();
        let mut ids = [id(LAB_A), id(ECHO)];
        ids.sort();
        format!("19:{}_{}@unq.gbl.spaces", ids[1], ids[0])
    };
    for readable in [
        add.clone(),
        with("/groupChat/threadId", json!(reversed)),
        without("groupChat"),
        without("participantInvitationData"),
    ] {
        assert!(matches!(classify_add(&readable), Contact::Write(_)), "{readable}");
    }
    let mut two = add.clone();
    two["participants"]["to"] = json!([{"id": ECHO}, {"id": LAB_B}]);
    let mut no_thread = add.clone();
    no_thread["groupChat"] = json!({"messageId": null});
    let mut no_organizer = add.clone();
    no_organizer["participantInvitationData"]["invitationData"]["meetingInfo"] = json!({"tenantId": "t"});
    for unreadable in [
        with("/groupChat/threadId", json!(one_to_one(LAB_A, COWORKER))),
        with("/groupChat/threadId", json!(one_to_one(LAB_B, ECHO))),
        with("/groupChat/threadId", json!("19:a1b2c3@thread.v2")),
        with("/groupChat/threadId", json!(one_to_one(LAB_A, ECHO).to_uppercase())),
        two,
        no_thread,
        with(
            "/participantInvitationData/invitationData/meetingInfo/organizerId",
            json!("00000000-0000-4000-8000-0000000000c0"),
        ),
        no_organizer,
    ] {
        assert_eq!(classify_add(&unreadable), Contact::Unreadable, "{unreadable}");
    }
    // a sender whose id no 1:1 chat id carries names no chat of its own
    let mut odd = add.clone();
    odd["participants"]["from"]["id"] = json!("8:orgid:lab-a");
    assert_eq!(classify_add(&odd), Contact::Unreadable);
    Ok(())
}

#[tokio::test]
async fn a_callback_handed_out_off_the_start_s_endpoint_and_call_agent_is_a_mismatch() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    lab.fence
        .observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call-1")));
    let acknowledge = String::from_utf8(body(CallOperation::Acknowledge, &from(), &[]))?;
    let add = String::from_utf8(body(CallOperation::Add, &from(), &[ECHO]))?;
    let transfer = format!("{CALLBACKS}/09090909/call/transfer/");
    let success = format!("{CALLBACKS}/07070707/conversation/addParticipantSuccess/");
    let elsewhere = [
        transfer.replace("callAgent/agent-1", "callAgent/agent-2"),
        transfer.replace("SyntheticEndpoint00000", "SyntheticEndpoint00001"),
        transfer.replace(":3443", ":8443"),
        transfer.replace("pub-ent-zz-01-f.trouter", "elsewhere"),
        transfer.replace("https:", "http:"),
        format!("{transfer}?x=1"),
        "https://example.com/v4/f/SyntheticEndpoint00000/callAgent/agent-1/x/".to_owned(),
        "not a link".to_owned(),
    ];
    let judge = |operation: CallOperation, url: String, body: String| {
        let fence = &lab.fence;
        async move {
            let Contact::Write(target) = classify(operation.method(), &url, body.as_bytes()) else {
                return Err(DenyReason::LogUnavailable);
            };
            verdict(fence, target).await
        }
    };
    assert_eq!(
        judge(
            CallOperation::Acknowledge,
            acknowledge_url("call-1"),
            acknowledge.clone()
        )
        .await,
        Ok(())
    );
    assert_eq!(
        judge(CallOperation::Add, op_url(CallOperation::Add, "conv-1"), add.clone()).await,
        Ok(())
    );
    for link in &elsewhere {
        assert_eq!(
            judge(
                CallOperation::Acknowledge,
                acknowledge_url("call-1"),
                acknowledge.replace(&transfer, link)
            )
            .await,
            Err(DenyReason::CallMismatch),
            "acknowledge {link}"
        );
        assert_eq!(
            judge(
                CallOperation::Add,
                op_url(CallOperation::Add, "conv-1"),
                add.replace(&success, link)
            )
            .await,
            Err(DenyReason::CallMismatch),
            "add {link}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_call_whose_start_named_no_call_agent_takes_no_callback() -> TestResult {
    let lab = new_lab()?;
    let start = String::from_utf8(start_body())?.replace(
        &format!("{CALLBACKS}/03030303/call/acceptance/"),
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/acceptance/",
    );
    let Contact::Write(target) = classify("POST", CALL_START_URL, start.as_bytes()) else {
        return Err("the start isn't a write".into());
    };
    let permit = lab.fence.check(target, &FailClosed).await?;
    let pending = lab
        .fence
        .pend_start(&permit, start.as_bytes())
        .ok_or("the permitted start isn't pending")?;
    lab.fence.register_start(pending, &answer("conv-1"))?;
    let url = op_url(CallOperation::Add, "conv-1");
    let add = write(CallOperation::Add, &url, &[ECHO])?;
    assert_eq!(verdict(&lab.fence, add).await, Err(DenyReason::CallMismatch));
    let mut bare: Value = serde_json::from_slice(&body(CallOperation::Add, &from(), &[ECHO]))?;
    bare.as_object_mut().map(|add| add.remove("links"));
    let Contact::Write(add) = classify_add(&bare) else {
        return Err("the add isn't a write".into());
    };
    assert_eq!(verdict(&lab.fence, add).await, Ok(()), "an add handing out nothing");
    Ok(())
}

#[tokio::test]
async fn a_follow_up_must_be_its_operation_s_kind_from_the_caller_adding_nobody() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    let url = op_url(CallOperation::UpdateEndpointMetadata, "conv-1");
    let scope = |from: Option<&str>| Scope::CallOperation {
        operation: CallOperation::UpdateEndpointMetadata,
        url: url.clone(),
        from: from.map(Mri::new),
        callbacks: Box::default(),
        joins: None,
    };
    let handing = Scope::CallOperation {
        operation: CallOperation::UpdateEndpointMetadata,
        url: url.clone(),
        from: Some(Mri::new(LAB_A)),
        callbacks: Box::new([format!("{CALLBACKS}/08080808/call/mediaRenegotiation/")]),
        joins: None,
    };
    for target in [
        Target::new(WriteKind::CallFollowUp, scope(Some(LAB_B))),
        Target::new(WriteKind::CallFollowUp, scope(None)),
        Target::new(WriteKind::Invite, scope(Some(LAB_A))),
        Target::new(WriteKind::CallFollowUp, handing),
        Target::new(WriteKind::CallFollowUp, scope(Some(LAB_A))).with_added([Mri::new(ECHO)]),
    ] {
        assert_eq!(
            verdict(&lab.fence, target.clone()).await,
            Err(DenyReason::CallMismatch),
            "{target:?}"
        );
    }
    // the operation goes with its own link only: the leave link isn't updateEndpointMetadata's
    let swapped = Target::new(
        WriteKind::CallFollowUp,
        Scope::CallOperation {
            operation: CallOperation::UpdateEndpointMetadata,
            url: op_url(CallOperation::Leave, "conv-1"),
            from: Some(Mri::new(LAB_A)),
            callbacks: Box::default(),
            joins: None,
        },
    );
    assert_eq!(verdict(&lab.fence, swapped).await, Err(UNREGISTERED));
    let other_sender = json!({"id": LAB_B});
    let Contact::Write(target) = classify(
        "PUT",
        &url,
        &body(CallOperation::UpdateEndpointMetadata, &other_sender, &[]),
    ) else {
        return Err("not a write".into());
    };
    assert_eq!(verdict(&lab.fence, target).await, Err(DenyReason::CallMismatch));
    Ok(())
}

// --- the end ---

/// The call leg's own link of the call controller's call `call`, the keep-alive's.
fn call_leg_url(call: &str) -> String {
    format!("{PROXY}/{CC_HOST}/cc/v1/active/{call}/1/a1/1?i=10-0-0-2&e=1")
}

/// The keep-alive to `url`, as the lab rule maps it.
fn keep_alive(url: &str) -> TestResult<Target> {
    match classify("POST", url, br#"{"callParticipantUpdate":{}}"#) {
        Contact::Write(target) => Ok(target),
        other => Err(format!("the keep-alive to {url} isn't a write: {other:?}").into()),
    }
}

#[tokio::test]
async fn the_keep_alive_passes_only_at_the_call_leg_link_its_acceptance_bound_while_the_call_is_live() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    let keep = keep_alive(&call_leg_url("call-1"))?;
    assert_eq!(
        verdict(&lab.fence, keep.clone()).await,
        Err(UNREGISTERED),
        "not bound yet"
    );
    lab.fence
        .observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call-1")));
    assert_eq!(verdict(&lab.fence, keep.clone()).await, Ok(()));
    assert_eq!(
        verdict(&lab.fence, keep.clone()).await,
        Ok(()),
        "every time it falls due"
    );
    // another call's leg, another leg of the call, an operation's link under the leg: none bound
    for other in [
        call_leg_url("call-2"),
        call_leg_url("call-1").replace("/1/a1/1?", "/1/a2/1?"),
    ] {
        assert_eq!(
            verdict(&lab.fence, keep_alive(&other)?).await,
            Err(UNREGISTERED),
            "{other}"
        );
    }
    // once the call leaves, it is over
    let leave = write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?;
    assert_eq!(verdict(&lab.fence, leave).await, Ok(()));
    assert_eq!(verdict(&lab.fence, keep.clone()).await, Err(ENDED));
    lab.fence.observe_delivery(&delivery(2, END_PATH, b"{}".to_vec()));
    assert_eq!(verdict(&lab.fence, keep).await, Err(ENDED));
    Ok(())
}

#[tokio::test]
async fn after_its_leave_only_the_leave_passes_and_after_its_end_nothing() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    lab.fence
        .observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call-1")));
    let leave = write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?;
    let metadata = write(
        CallOperation::UpdateEndpointMetadata,
        &op_url(CallOperation::UpdateEndpointMetadata, "conv-1"),
        &[],
    )?;
    let acknowledge = write(CallOperation::Acknowledge, &acknowledge_url("call-1"), &[])?;
    assert_eq!(verdict(&lab.fence, leave.clone()).await, Ok(()));
    assert_eq!(verdict(&lab.fence, metadata.clone()).await, Err(ENDED));
    assert_eq!(verdict(&lab.fence, acknowledge.clone()).await, Err(ENDED));
    assert_eq!(verdict(&lab.fence, leave.clone()).await, Ok(()), "the leave again");
    assert_eq!(
        lab.fence.observe_delivery(&delivery(2, END_PATH, b"{}".to_vec())),
        Observed::Ended
    );
    assert_eq!(verdict(&lab.fence, leave).await, Err(ENDED));
    // nothing revives it
    assert_eq!(
        lab.fence
            .observe_delivery(&delivery(3, ACCEPTANCE_PATH, acceptance("call-1"))),
        Observed::Nothing
    );
    assert_eq!(verdict(&lab.fence, acknowledge).await, Err(ENDED));
    assert_eq!(verdict(&lab.fence, metadata).await, Err(ENDED));
    Ok(())
}

#[tokio::test]
async fn a_rejoin_of_a_conversation_left_takes_its_links_and_its_follow_ups_pass() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    let leave = write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?;
    let metadata = write(
        CallOperation::UpdateEndpointMetadata,
        &op_url(CallOperation::UpdateEndpointMetadata, "conv-1"),
        &[],
    )?;
    assert_eq!(verdict(&lab.fence, leave.clone()).await, Ok(()));
    assert_eq!(verdict(&lab.fence, metadata.clone()).await, Err(ENDED));
    // the same conversation answers a later permitted start: its links are the new call's alone
    registered(&lab.fence, "conv-1").await?;
    assert_eq!(verdict(&lab.fence, metadata.clone()).await, Ok(()));
    assert_eq!(verdict(&lab.fence, leave).await, Ok(()));
    assert_eq!(
        verdict(&lab.fence, metadata).await,
        Err(ENDED),
        "the new call is leaving"
    );
    Ok(())
}

#[tokio::test]
async fn a_rejoin_after_the_call_ended_takes_its_links_too() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    assert_eq!(
        lab.fence.observe_delivery(&delivery(1, END_PATH, b"{}".to_vec())),
        Observed::Ended
    );
    let metadata = write(
        CallOperation::UpdateEndpointMetadata,
        &op_url(CallOperation::UpdateEndpointMetadata, "conv-1"),
        &[],
    )?;
    assert_eq!(verdict(&lab.fence, metadata.clone()).await, Err(ENDED));
    registered(&lab.fence, "conv-1").await?;
    assert_eq!(verdict(&lab.fence, metadata).await, Ok(()));
    Ok(())
}

#[tokio::test]
async fn a_conversation_two_live_calls_hold_passes_for_neither() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    registered(&lab.fence, "conv-1").await?;
    let metadata = write(
        CallOperation::UpdateEndpointMetadata,
        &op_url(CallOperation::UpdateEndpointMetadata, "conv-1"),
        &[],
    )?;
    assert_eq!(verdict(&lab.fence, metadata).await, Err(UNREGISTERED));
    let leave = write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?;
    assert_eq!(verdict(&lab.fence, leave).await, Err(UNREGISTERED));
    Ok(())
}

/// The M3 echo call's deliveries (`tests/fixtures/calling/echo/deliveries.json`), their placeholders filled with this
/// file's lab account and echo service.
const ECHO_DELIVERIES: &str = include_str!("../../../tests/fixtures/calling/echo/deliveries.json");

fn echo_deliveries() -> TestResult<Vec<Value>> {
    let filled = ECHO_DELIVERIES
        .replace("<guid:a>", LAB_A.trim_start_matches("8:orgid:"))
        .replace("<guid:echo>", ECHO.trim_start_matches("28:"));
    let fixture: Value = serde_json::from_str(&filled)?;
    Ok(fixture["cases"].as_array().ok_or("no cases")?.clone())
}

#[tokio::test]
async fn the_echo_call_s_updates_bind_nothing_and_its_end_after_the_leave_ends_it() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    lab.fence
        .observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call-1")));
    let metadata = write(
        CallOperation::UpdateEndpointMetadata,
        &op_url(CallOperation::UpdateEndpointMetadata, "conv-1"),
        &[],
    )?;
    let leave = write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?;
    let cases = echo_deliveries()?;
    assert_eq!(cases.len(), 5);
    for (id, case) in (2..).zip(&cases) {
        let name = case["name"].as_str().unwrap_or_default();
        let path = format!(
            "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/{}",
            case["callback"].as_str().unwrap_or_default()
        );
        let body = case["body"].to_string().into_bytes();
        if case["observed"] == "ended" {
            // the client leaves first; the call's end follows
            assert_eq!(verdict(&lab.fence, leave.clone()).await, Ok(()), "{name}");
            assert_eq!(
                lab.fence.observe_delivery(&delivery(id, &path, body)),
                Observed::Ended,
                "{name}"
            );
            assert_eq!(verdict(&lab.fence, leave.clone()).await, Err(ENDED), "{name}");
        } else {
            assert_eq!(
                lab.fence.observe_delivery(&delivery(id, &path, body)),
                Observed::Nothing,
                "{name}"
            );
            assert_eq!(
                verdict(&lab.fence, metadata.clone()).await,
                Ok(()),
                "{name}: the call stays live"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn the_conversation_s_end_ends_a_live_call() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    assert_eq!(
        lab.fence
            .observe_delivery(&delivery(1, CONVERSATION_END_PATH, Vec::new())),
        Observed::Ended
    );
    let metadata = write(
        CallOperation::UpdateEndpointMetadata,
        &op_url(CallOperation::UpdateEndpointMetadata, "conv-1"),
        &[],
    )?;
    assert_eq!(verdict(&lab.fence, metadata).await, Err(ENDED));
    Ok(())
}

// --- the acceptance's delivery response ---

/// The client's response to delivery `id`, with `trouter_request` and an acknowledgement whose links are under
/// `callbacks`.
fn response(id: i64, trouter_request: &str, callbacks: &str) -> Vec<u8> {
    let acknowledgement = json!({"callAcceptanceAcknowledgement": {"links": {
        "mediaRenegotiation": format!("{callbacks}/08080808/call/mediaRenegotiation/"),
        "updateMediaDescriptions": format!("{callbacks}/0a0a0a0a/call/updateMediaDescriptions"),
    }}});
    json!({
        "id": id,
        "status": 200,
        "headers": {
            "X-Microsoft-Skype-Chain-ID": CHAIN_ID,
            "X-Microsoft-Skype-Message-ID": MESSAGE_ID,
            "MS-CV": "SyntheticCv.1.0",
            "trouter-request": trouter_request,
            "trouter-client": "{\"cd\":2}",
        },
        "body": acknowledgement.to_string(),
    })
    .to_string()
    .into_bytes()
}

fn permitted(decision: &DeliveryDecision) -> bool {
    matches!(decision, DeliveryDecision::Permitted(_))
}

#[tokio::test]
async fn the_acceptance_s_acknowledgement_answers_its_delivery_once() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    lab.fence
        .observe_delivery(&delivery(-7, ACCEPTANCE_PATH, acceptance("call-1")));
    assert!(matches!(
        lab.fence
            .check_delivery_response(
                "session-2/socket-1",
                &response(-7, TROUTER_REQUEST, CALLBACKS),
                &FailClosed
            )
            .await,
        DeliveryDecision::NotPending
    ));
    assert!(matches!(
        lab.fence
            .check_delivery_response(CONNECTION, &response(8, TROUTER_REQUEST, CALLBACKS), &FailClosed)
            .await,
        DeliveryDecision::NotPending
    ));
    let decision = lab
        .fence
        .check_delivery_response(CONNECTION, &response(-7, TROUTER_REQUEST, CALLBACKS), &FailClosed)
        .await;
    assert!(permitted(&decision), "{decision:?}");
    assert!(matches!(
        lab.fence
            .check_delivery_response(CONNECTION, &response(-7, TROUTER_REQUEST, CALLBACKS), &FailClosed)
            .await,
        DeliveryDecision::NotPending
    ));
    let entries = fs::read_to_string(&lab.log)?;
    assert!(entries.contains(r#""event":"delivery_response""#), "{entries}");
    assert!(!entries.contains(LAB_A), "no participant's MRI in fence.log: {entries}");
    Ok(())
}

#[tokio::test]
async fn a_response_off_the_captured_acknowledgement_is_denied_and_used_up() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    let mut empty: Value = serde_json::from_slice(&response(5, TROUTER_REQUEST, CALLBACKS))?;
    empty["body"] = json!("");
    let empty = empty.to_string();
    // without the chain and message ids it echoes (the form no capture showed)
    let unechoed = String::from_utf8(response(8, TROUTER_REQUEST, CALLBACKS))?
        .replace(&format!(r#""X-Microsoft-Skype-Chain-ID":"{CHAIN_ID}","#), "")
        .replace(&format!(r#""X-Microsoft-Skype-Message-ID":"{MESSAGE_ID}","#), "");
    let other_chain = String::from_utf8(response(9, TROUTER_REQUEST, CALLBACKS))?.replace(CHAIN_ID, "another-chain");
    let other_message =
        String::from_utf8(response(10, TROUTER_REQUEST, CALLBACKS))?.replace(MESSAGE_ID, "another-message");
    let failing =
        String::from_utf8(response(6, TROUTER_REQUEST, CALLBACKS))?.replace(r#""status":200"#, r#""status":500"#);
    let extra_header =
        String::from_utf8(response(7, TROUTER_REQUEST, CALLBACKS))?.replace(r#""MS-CV""#, r#""X-Extra":"x","MS-CV""#);
    let other_agent = CALLBACKS.replace("agent-1", "agent-2");
    let other_host = CALLBACKS.replace("pub-ent-zz-01-f", "pub-ent-yy-02-f");
    for (id, sent, reason) in [
        (
            1,
            response(1, "{\"id\":\"another\"}", CALLBACKS),
            DeliveryDenyReason::Uncorrelated,
        ),
        (2, response(2, TROUTER_REQUEST, &other_agent), DeliveryDenyReason::Shape),
        (3, response(3, TROUTER_REQUEST, &other_host), DeliveryDenyReason::Shape),
        (5, empty.into_bytes(), DeliveryDenyReason::Shape),
        (8, unechoed.into_bytes(), DeliveryDenyReason::Shape),
        (9, other_chain.into_bytes(), DeliveryDenyReason::Uncorrelated),
        (10, other_message.into_bytes(), DeliveryDenyReason::Uncorrelated),
        (6, failing.into_bytes(), DeliveryDenyReason::Shape),
        (7, extra_header.into_bytes(), DeliveryDenyReason::Shape),
    ] {
        lab.fence
            .observe_delivery(&delivery(id, ACCEPTANCE_PATH, acceptance("call-1")));
        let decision = lab.fence.check_delivery_response(CONNECTION, &sent, &FailClosed).await;
        assert!(
            matches!(decision, DeliveryDecision::Denied(denied) if denied == reason),
            "{id}: {decision:?}"
        );
        let again = lab
            .fence
            .check_delivery_response(CONNECTION, &response(id, TROUTER_REQUEST, CALLBACKS), &FailClosed)
            .await;
        assert!(matches!(again, DeliveryDecision::NotPending), "{id}: used up");
    }
    Ok(())
}

#[tokio::test]
async fn a_closed_connection_or_a_call_no_longer_live_answers_nothing() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    lab.fence
        .observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call-1")));
    lab.fence.close_connection(CONNECTION);
    assert!(matches!(
        lab.fence
            .check_delivery_response(CONNECTION, &response(1, TROUTER_REQUEST, CALLBACKS), &FailClosed)
            .await,
        DeliveryDecision::NotPending
    ));
    // a pending call's acceptance waits for the answer, and its response finds the call not live
    let other = new_lab()?;
    let pending = pending(&other.fence).await?;
    other
        .fence
        .observe_delivery(&delivery(2, ACCEPTANCE_PATH, acceptance("call-1")));
    let decision = other
        .fence
        .check_delivery_response(CONNECTION, &response(2, TROUTER_REQUEST, CALLBACKS), &FailClosed)
        .await;
    assert!(
        matches!(decision, DeliveryDecision::Denied(DeliveryDenyReason::NotLive)),
        "{decision:?}"
    );
    other.fence.register_start(pending, &answer("conv-1"))?;
    // the leave ends the call's pending responses
    other
        .fence
        .observe_delivery(&delivery(3, ACCEPTANCE_PATH, acceptance("call-1")));
    let leave = write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?;
    assert_eq!(verdict(&other.fence, leave).await, Ok(()));
    assert!(matches!(
        other
            .fence
            .check_delivery_response(CONNECTION, &response(3, TROUTER_REQUEST, CALLBACKS), &FailClosed)
            .await,
        DeliveryDecision::NotPending
    ));
    Ok(())
}

#[tokio::test]
async fn the_follow_ups_are_logged_by_kind_scope_and_operation_never_the_sender() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    let metadata = write(
        CallOperation::UpdateEndpointMetadata,
        &op_url(CallOperation::UpdateEndpointMetadata, "conv-1"),
        &[],
    )?;
    verdict(&lab.fence, metadata)
        .await
        .map_err(|reason| format!("{reason}"))?;
    let text = fs::read_to_string(&lab.log)?;
    let last: Value = serde_json::from_str(text.lines().last().ok_or("an entry")?)?;
    assert_eq!(last["kind"], "call_follow_up");
    assert_eq!(last["scope"]["call_operation"]["operation"], "update_endpoint_metadata");
    assert!(last["scope"]["call_operation"].get("from").is_none(), "{last}");
    assert!(!text.contains(LAB_A), "{text}");
    Ok(())
}

// --- the caller's media negotiation (`ab-call --call video`, `abc-call --call video`) ---

#[tokio::test]
async fn the_caller_s_renegotiation_and_media_lines_go_to_the_links_its_acceptance_bound() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    let renegotiate = format!("{PROXY}/{CC_HOST}/cc/v1/active/call-1/1/a1/1/renegotiate?i=10-0-0-2&e=1");
    let lines = format!("{PROXY}/{CC_HOST}/cc/v1/active/call-1/1/a1/1/updateMediaDescriptions?i=10-0-0-2&e=1");
    let mut body: Value = serde_json::from_slice(&acceptance("call-1"))?;
    body["callAcceptance"]["links"]["mediaRenegotiation"] = json!(renegotiate);
    body["callAcceptance"]["links"]["updateMediaDescriptions"] = json!(lines);
    let negotiation = json!({"mediaNegotiation": {
        "callModalities": ["Audio", "Video"],
        "sender": from(),
        "links": {"mediaAnswer": format!("{CALLBACKS}/0b0b0b0b/call/mediaAnswer/")},
        "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0", "negotiationTag": "t;v_1"},
    }})
    .to_string()
    .into_bytes();
    let media_lines = json!({"UpdateMediaDescriptions": {"mediaDescriptions": {
        "descriptions": [{"mid": "1", "direction": "sendrecv", "label": "main-video"}],
        "requestId": 2,
    }}})
    .to_string()
    .into_bytes();
    let write = |url: &str, body: &[u8]| match classify("POST", url, body) {
        Contact::Write(target) => Ok(target),
        other => Err(format!("{url} isn't a write: {other:?}")),
    };
    // before the acceptance bound them, the links are nobody's
    assert_eq!(
        verdict(&lab.fence, write(&renegotiate, &negotiation)?).await,
        Err(UNREGISTERED)
    );
    lab.fence
        .observe_delivery(&delivery(1, ACCEPTANCE_PATH, body.to_string().into_bytes()));
    assert_eq!(verdict(&lab.fence, write(&renegotiate, &negotiation)?).await, Ok(()));
    assert_eq!(verdict(&lab.fence, write(&lines, &media_lines)?).await, Ok(()));
    // the peer's renegotiation, delivered at the caller's own callback, hands out where its answer goes
    let answer_url = format!("{PROXY}/{CC_HOST}/cc/v1/negotiations/call-1/1/t/2/answer?i=10-0-0-2");
    let answer = json!({"mediaAnswer": {
        "sender": from(),
        "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
    }})
    .to_string()
    .into_bytes();
    assert_eq!(
        verdict(&lab.fence, write(&answer_url, &answer)?).await,
        Err(UNREGISTERED)
    );
    let renegotiation = json!({"mediaNegotiation": {"links": {"mediaAnswer": answer_url}}})
        .to_string()
        .into_bytes();
    let at = "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/0c0c0c0c/call/mediaRenegotiation/";
    assert_eq!(
        lab.fence.observe_delivery(&delivery(2, at, renegotiation)),
        Observed::Renegotiation
    );
    assert_eq!(verdict(&lab.fence, write(&answer_url, &answer)?).await, Ok(()));
    Ok(())
}

/// A receipt, the empty response the web client gives a renegotiation's delivery (`ab-call --call video`): `{"id",
/// "status": 200, "headers": {"MS-CV", "trouter-request", "trouter-client"}, "body": ""}`.
fn receipt(id: i64, trouter_request: &str) -> Vec<u8> {
    json!({
        "id": id,
        "status": 200,
        "headers": {"MS-CV": "SyntheticCv.1.0", "trouter-request": trouter_request, "trouter-client": "{\"cd\":0}"},
        "body": "",
    })
    .to_string()
    .into_bytes()
}

#[tokio::test]
async fn a_renegotiation_or_its_answer_delivered_at_the_call_s_own_callback_gets_one_receipt_while_the_call_is_live()
-> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    lab.fence
        .observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call-1")));
    let answer_url = format!("{PROXY}/{CC_HOST}/cc/v1/negotiations/call-1/1/t/2/answer?i=10-0-0-2");
    let renegotiation = json!({"mediaNegotiation": {"links": {"mediaAnswer": answer_url}}})
        .to_string()
        .into_bytes();
    let at = "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/0c0c0c0c/call/mediaRenegotiation/";
    assert_eq!(
        lab.fence.observe_delivery(&delivery(2, at, renegotiation.clone())),
        Observed::Renegotiation
    );
    // on another connection, or another delivery's id: no receipt waits
    for (connection, id) in [("session-2/socket-1", 2), (CONNECTION, 9)] {
        assert!(matches!(
            lab.fence
                .check_delivery_response(connection, &receipt(id, TROUTER_REQUEST), &FailClosed)
                .await,
            DeliveryDecision::NotPending
        ));
    }
    let decision = lab
        .fence
        .check_delivery_response(CONNECTION, &receipt(2, TROUTER_REQUEST), &FailClosed)
        .await;
    assert!(permitted(&decision), "{decision:?}");
    // once
    assert!(matches!(
        lab.fence
            .check_delivery_response(CONNECTION, &receipt(2, TROUTER_REQUEST), &FailClosed)
            .await,
        DeliveryDecision::NotPending
    ));

    // a receipt that isn't the captured one (a body, another request's id) is denied and used up
    lab.fence.observe_delivery(&delivery(3, at, renegotiation.clone()));
    let mut with_body: Value = serde_json::from_slice(&receipt(3, TROUTER_REQUEST))?;
    with_body["body"] = json!("{\"answer\":1}");
    assert!(matches!(
        lab.fence
            .check_delivery_response(CONNECTION, with_body.to_string().as_bytes(), &FailClosed)
            .await,
        DeliveryDecision::Denied(_)
    ));
    lab.fence.observe_delivery(&delivery(4, at, renegotiation.clone()));
    assert!(matches!(
        lab.fence
            .check_delivery_response(CONNECTION, &receipt(4, r#"{"id":"another"}"#), &FailClosed)
            .await,
        DeliveryDecision::Denied(_)
    ));

    // a delivery at a callback that isn't the call's binds nothing and waits for no receipt
    let elsewhere = "/v4/f/SyntheticEndpoint00000/callAgent/agent-2/0c0c0c0c/call/mediaRenegotiation/";
    assert_eq!(
        lab.fence
            .observe_delivery(&delivery(5, elsewhere, renegotiation.clone())),
        Observed::Nothing
    );
    assert!(matches!(
        lab.fence
            .check_delivery_response(CONNECTION, &receipt(5, TROUTER_REQUEST), &FailClosed)
            .await,
        DeliveryDecision::NotPending
    ));

    // once the call ended, a receipt waiting is denied
    lab.fence.observe_delivery(&delivery(6, at, renegotiation));
    assert_eq!(
        lab.fence.observe_delivery(&delivery(7, END_PATH, b"{}".to_vec())),
        Observed::Ended
    );
    assert!(matches!(
        lab.fence
            .check_delivery_response(CONNECTION, &receipt(6, TROUTER_REQUEST), &FailClosed)
            .await,
        DeliveryDecision::Denied(DeliveryDenyReason::NotLive)
    ));
    let entries = fs::read_to_string(&lab.log)?;
    assert!(entries.contains("call_negotiation_receipt"), "{entries}");
    Ok(())
}

#[tokio::test]
async fn an_acknowledgement_of_the_account_s_answer_binds_no_link_and_gets_its_receipt() -> TestResult {
    let lab = new_lab()?;
    registered(&lab.fence, "conv-1").await?;
    lab.fence
        .observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call-1")));
    // the answerer told its answer was acknowledged (`{"mediaAcknowledgement": {"reason": "noError"}}`): no link,
    // its receipt alone, once
    let acknowledged = "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/0d0d0d0d/call/mediaAcknowledgement/";
    let body = json!({"mediaAcknowledgement": {"reason": "noError"}})
        .to_string()
        .into_bytes();
    assert_eq!(
        lab.fence.observe_delivery(&delivery(8, acknowledged, body)),
        Observed::Renegotiation
    );
    let decision = lab
        .fence
        .check_delivery_response(CONNECTION, &receipt(8, TROUTER_REQUEST), &FailClosed)
        .await;
    assert!(permitted(&decision), "{decision:?}");
    assert!(matches!(
        lab.fence
            .check_delivery_response(CONNECTION, &receipt(8, TROUTER_REQUEST), &FailClosed)
            .await,
        DeliveryDecision::NotPending
    ));
    Ok(())
}

// --- the lab's call rule: only a call's entry is judged ---

/// A lab of a, b and c with the echo service whose fence decides calls by the lab's rule (a lab fence's own).
fn lab_rule() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("fence.log");
    let roster = Roster::new([Mri::new(LAB_A), Mri::new(LAB_B), Mri::new(LAB_C)], [Mri::new(ECHO)]);
    let fence = Fence::new(roster, FenceLog::open(&log)?);
    assert!(!fence.tracks_calls(), "a lab fence judges a call's entry only");
    Ok(Lab { _dir: dir, log, fence })
}

#[tokio::test]
async fn under_the_lab_rule_a_call_s_follow_ups_pass_with_no_registered_call() -> TestResult {
    let lab = lab_rule()?;
    // nothing was started, pended, registered or delivered: every captured follow-up passes, with no member read
    let follow_ups = [
        write(
            CallOperation::UpdateEndpointMetadata,
            &op_url(CallOperation::UpdateEndpointMetadata, "conv-1"),
            &[],
        )?,
        write(CallOperation::Acknowledge, &acknowledge_url("call-1"), &[])?,
        write(CallOperation::ApplyChannelParameters, &channel_url("call-1"), &[])?,
        keep_alive(&call_leg_url("call-1"))?,
        write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?,
        // after its leave, and again (the web client resends it)
        write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?,
        keep_alive(&call_leg_url("call-1"))?,
    ];
    for target in follow_ups {
        assert_eq!(verdict(&lab.fence, target.clone()).await, Ok(()), "{target}");
    }
    // a call's end delivered changes nothing of that
    lab.fence.observe_delivery(&delivery(2, END_PATH, b"{}".to_vec()));
    assert_eq!(verdict(&lab.fence, keep_alive(&call_leg_url("call-1"))?).await, Ok(()));
    let log = fs::read_to_string(&lab.log)?;
    assert_eq!(
        log.matches(r#""kind":"call_follow_up""#).count(),
        8,
        "each one logged: {log}"
    );
    assert!(!log.contains(LAB_A), "no sender in fence.log: {log}");
    Ok(())
}

#[tokio::test]
async fn under_the_lab_rule_every_delivery_response_passes_logged() -> TestResult {
    let lab = lab_rule()?;
    // no delivery waits: the acknowledgement passes all the same, on any connection, in any shape
    for (connection, response) in [
        (CONNECTION, response(-7, TROUTER_REQUEST, CALLBACKS)),
        (
            "session-2/socket-1",
            response(8, "another request", "https://elsewhere.example"),
        ),
        (CONNECTION, br#"{"id":9,"status":200,"headers":{},"body":""}"#.to_vec()),
    ] {
        let decision = lab
            .fence
            .check_delivery_response(connection, &response, &FailClosed)
            .await;
        assert!(permitted(&decision), "{decision:?}");
    }
    // the acceptance's acknowledgement of a registered call names its kind
    registered(&lab.fence, "conv-1").await?;
    lab.fence
        .observe_delivery(&delivery(-3, ACCEPTANCE_PATH, acceptance("call-1")));
    let decision = lab
        .fence
        .check_delivery_response(CONNECTION, &response(-3, TROUTER_REQUEST, CALLBACKS), &FailClosed)
        .await;
    assert!(matches!(decision, DeliveryDecision::Permitted(ref permit) if permit.delivery() == -3));
    // a frame that names no delivery answers none
    assert!(matches!(
        lab.fence
            .check_delivery_response(CONNECTION, br#"{"status":200}"#, &FailClosed)
            .await,
        DeliveryDecision::NotPending
    ));
    let log = fs::read_to_string(&lab.log)?;
    assert_eq!(log.matches(r#""event":"delivery_response""#).count(), 4, "{log}");
    assert!(log.contains("call_acceptance_acknowledgement"), "{log}");
    assert!(log.contains("call_delivery_response"), "{log}");
    Ok(())
}

#[tokio::test]
async fn under_the_lab_rule_a_start_still_goes_only_to_lab_callees_in_a_lab_only_chat() -> TestResult {
    let lab = lab_rule()?;
    let judged = async |target: Target, source: &Members| {
        lab.fence
            .check(target, source)
            .await
            .map(drop)
            .map_err(|denied| *denied.reason())
    };
    let group = chat_start(&chat_start_body(&[LAB_B, LAB_C], Some(GROUP_CHAT)))?;
    assert_eq!(
        judged(group.clone(), &Members::of(&[LAB_A, LAB_B, LAB_C])).await,
        Ok(())
    );
    assert_eq!(
        judged(group, &Members::of(&[LAB_A, LAB_B, COWORKER])).await,
        Err(DenyReason::Outsiders { count: 1 }),
        "a chat with someone outside the lab"
    );
    let outsider = chat_start(&chat_start_body(&[LAB_B, COWORKER], Some(GROUP_CHAT)))?;
    assert_eq!(
        judged(outsider, &Members::of(&[LAB_A, LAB_B, LAB_C])).await,
        Err(DenyReason::Outsiders { count: 1 }),
        "a callee outside the lab"
    );
    let one = chat_start(&chat_start_body(&[COWORKER], None))?;
    assert_eq!(
        judged(one, &Members::of(&[LAB_A, COWORKER])).await,
        Err(DenyReason::Outsiders { count: 1 }),
        "a 1:1 call to someone outside the lab"
    );
    let partial = chat_start(&chat_start_body(&[LAB_B], None))?;
    assert_eq!(
        judged(partial, &Members(Membership::Partial(PartialReason::NoSource))).await,
        Err(DenyReason::Partial(PartialReason::NoSource)),
        "a chat that can't be read in full"
    );
    Ok(())
}

#[tokio::test]
async fn under_the_lab_rule_an_add_is_judged_per_invitee_on_any_call() -> TestResult {
    let lab = lab_rule()?;
    // no call registered: an add is judged by its sender and invitees alone
    let url = op_url(CallOperation::Add, "conv-1");
    for invitee in [ECHO, LAB_B] {
        assert_eq!(
            verdict(&lab.fence, write(CallOperation::Add, &url, &[invitee])?).await,
            Ok(()),
            "{invitee}"
        );
    }
    assert_eq!(
        verdict(&lab.fence, write(CallOperation::Add, &url, &[COWORKER])?).await,
        Err(DenyReason::Outsiders { count: 1 })
    );
    assert_eq!(
        verdict(&lab.fence, write(CallOperation::Add, &url, &[ECHO, COWORKER])?).await,
        Err(DenyReason::Outsiders { count: 1 })
    );
    let log = fs::read_to_string(&lab.log)?;
    assert_eq!(log.matches(r#""kind":"invite""#).count(), 4, "{log}");
    Ok(())
}

/// A log that takes every entry until `broken` is set.
struct Breakable(Arc<AtomicBool>);

impl std::io::Write for Breakable {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.0.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("disk full"));
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn under_the_lab_rule_a_leave_the_log_refuses_leaves_the_registry_as_it_was() -> TestResult {
    let broken = Arc::new(AtomicBool::new(false));
    let roster = Roster::new([Mri::new(LAB_A), Mri::new(LAB_B), Mri::new(LAB_C)], [Mri::new(ECHO)]);
    let fence = Fence::new(roster, FenceLog::from_writer(Breakable(Arc::clone(&broken))));
    registered(&fence, "conv-1").await?;
    fence.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call-1")));
    broken.store(true, Ordering::SeqCst);
    let leave = write(CallOperation::Leave, &op_url(CallOperation::Leave, "conv-1"), &[])?;
    assert_eq!(verdict(&fence, leave).await, Err(DenyReason::LogUnavailable));
    // the leave never went: the call is still live there, a renegotiation of it still read as the call's
    let answer_url = format!("{PROXY}/{CC_HOST}/cc/v1/negotiations/call-1/1/t/2/answer?i=10-0-0-2");
    let renegotiation = json!({"mediaNegotiation": {"links": {"mediaAnswer": answer_url}}})
        .to_string()
        .into_bytes();
    let at = "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/0c0c0c0c/call/mediaRenegotiation/";
    assert_eq!(
        fence.observe_delivery(&delivery(2, at, renegotiation)),
        Observed::Renegotiation
    );
    Ok(())
}

// --- a call guest (`lab.toml`'s `call_guests`): the user's own account, a participant of the lab's calls only ---

const GUEST: &str = "8:orgid:00000000-0000-4000-8000-0000000000f0";

/// [`lab_rule`]'s lab with [`GUEST`] as its call guest.
fn lab_rule_with_guest() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("fence.log");
    let roster = Roster::new([Mri::new(LAB_A), Mri::new(LAB_B), Mri::new(LAB_C)], [Mri::new(ECHO)])
        .with_call_guests([Mri::new(GUEST)]);
    let fence = Fence::new(roster, FenceLog::open(&log)?);
    Ok(Lab { _dir: dir, log, fence })
}

#[tokio::test]
async fn a_call_guest_is_called_in_its_1_1_chat_and_in_a_chat_of_lab_accounts_plus_it() -> TestResult {
    let lab = lab_rule_with_guest()?;
    let judged = async |target: Target, source: &Members| {
        lab.fence
            .check(target, source)
            .await
            .map(drop)
            .map_err(|denied| *denied.reason())
    };
    let one = chat_start(&chat_start_body(&[GUEST], None))?;
    assert_eq!(
        one.scope(),
        &Scope::Conversation {
            id: one_to_one(LAB_A, GUEST)
        }
    );
    assert_eq!(judged(one, &Members::of(&[LAB_A, GUEST])).await, Ok(()));
    let group = chat_start(&chat_start_body(&[LAB_B, LAB_C, GUEST], Some(GROUP_CHAT)))?;
    assert_eq!(
        judged(group.clone(), &Members::of(&[LAB_A, LAB_B, LAB_C, GUEST])).await,
        Ok(())
    );
    // anyone else is still outside the lab, a guest or not
    let outsider = chat_start(&chat_start_body(&[COWORKER], None))?;
    assert_eq!(
        judged(outsider, &Members::of(&[LAB_A, COWORKER])).await,
        Err(DenyReason::Outsiders { count: 1 })
    );
    assert_eq!(
        judged(group, &Members::of(&[LAB_A, LAB_B, LAB_C, GUEST, COWORKER])).await,
        Err(DenyReason::Outsiders { count: 1 }),
        "a chat with the guest and an outsider"
    );
    // an `add` of the guest to a running call, never of anyone else
    let url = op_url(CallOperation::Add, "conv-1");
    assert_eq!(
        verdict(&lab.fence, write(CallOperation::Add, &url, &[GUEST])?).await,
        Ok(())
    );
    assert_eq!(
        verdict(&lab.fence, write(CallOperation::Add, &url, &[COWORKER])?).await,
        Err(DenyReason::Outsiders { count: 1 })
    );
    Ok(())
}

#[tokio::test]
async fn nothing_but_a_call_reaches_a_call_guest() -> TestResult {
    let lab = lab_rule_with_guest()?;
    let with_guest = Members::of(&[LAB_A, LAB_B, LAB_C, GUEST]);
    let lab_only = Members::of(&[LAB_A, LAB_B, LAB_C]);
    let pair = Members::of(&[LAB_A, GUEST]);
    let nobody = Members::of(&[]);
    let group = Scope::Conversation { id: GROUP_CHAT.into() };
    let one = Scope::Conversation {
        id: one_to_one(LAB_A, GUEST),
    };
    let guest = || [Mri::new(GUEST)];
    let writes = [
        (Target::new(WriteKind::Message, group.clone()), &with_guest),
        (Target::new(WriteKind::Reaction, group.clone()), &with_guest),
        (Target::new(WriteKind::Edit, group.clone()), &with_guest),
        (Target::new(WriteKind::Message, one), &pair),
        (
            Target::new(WriteKind::MemberChange, group.clone()).with_added(guest()),
            &lab_only,
        ),
        (Target::new(WriteKind::Invite, group).with_added(guest()), &lab_only),
        (
            Target::new(WriteKind::ChatCreate, Scope::New).with_added([Mri::new(LAB_A), Mri::new(GUEST)]),
            &nobody,
        ),
    ];
    for (target, members) in writes {
        let denied = lab.fence.check(target.clone(), members).await;
        assert_eq!(
            denied.err().map(|denied| *denied.reason()),
            Some(DenyReason::Outsiders { count: 1 }),
            "{target}"
        );
    }
    Ok(())
}
