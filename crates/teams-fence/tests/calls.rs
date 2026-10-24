//! The call registry (`teams_fence`'s `calls` module, `docs/protocol/fence-targets.md`): a call this run's fence
//! permitted is registered once its start's answer is read, its call controller links are bound only by the acceptance
//! delivered at the start's own acceptance callback, and a follow-up passes only as a captured operation to exactly a
//! link of a live registered call. Shaped like the echo-call capture's (`tests/fixtures/calling/echo/`), with made-up
//! ids, hosts and placeholder MRIs.

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use teams_fence::{
    CALL_START_URL, CallOperation, Contact, Delivery, DeliveryDecision, DeliveryDenyReason, DenyReason, FailClosed,
    Fence, FenceLog, Mri, Observed, PartialReason, PendingStart, Policy, RegisterError, Request, Roster, Scope,
    TEST_CALL_SCENARIO, Target, WriteKind, WritePermit, contacting,
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
    let fence = Fence::new(roster, FenceLog::open(&log)?);
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

const UNREGISTERED: DenyReason = DenyReason::Partial(PartialReason::UnregisteredCall);
const ENDED: DenyReason = DenyReason::Partial(PartialReason::CallEnded);

// --- the follow-ups' rows ---

#[test]
fn each_captured_follow_up_is_a_call_operation_on_its_exact_url() -> TestResult {
    for operation in CallOperation::ALL {
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
    };
    let handing = Scope::CallOperation {
        operation: CallOperation::UpdateEndpointMetadata,
        url: url.clone(),
        from: Some(Mri::new(LAB_A)),
        callbacks: Box::new([format!("{CALLBACKS}/08080808/call/mediaRenegotiation/")]),
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
        "headers": {"MS-CV": "SyntheticCv.1.0", "trouter-request": trouter_request, "trouter-client": "{\"cd\":2}"},
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
            .check_delivery_response("session-2/socket-1", &response(-7, TROUTER_REQUEST, CALLBACKS)),
        DeliveryDecision::NotPending
    ));
    assert!(matches!(
        lab.fence
            .check_delivery_response(CONNECTION, &response(8, TROUTER_REQUEST, CALLBACKS)),
        DeliveryDecision::NotPending
    ));
    let decision = lab
        .fence
        .check_delivery_response(CONNECTION, &response(-7, TROUTER_REQUEST, CALLBACKS));
    assert!(permitted(&decision), "{decision:?}");
    assert!(matches!(
        lab.fence
            .check_delivery_response(CONNECTION, &response(-7, TROUTER_REQUEST, CALLBACKS)),
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
    let empty = json!({"id": 5, "status": 200, "headers": {"MS-CV": "a", "trouter-request": TROUTER_REQUEST, "trouter-client": "b"}, "body": ""});
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
        (5, empty.to_string().into_bytes(), DeliveryDenyReason::Shape),
        (6, failing.into_bytes(), DeliveryDenyReason::Shape),
        (7, extra_header.into_bytes(), DeliveryDenyReason::Shape),
    ] {
        lab.fence
            .observe_delivery(&delivery(id, ACCEPTANCE_PATH, acceptance("call-1")));
        let decision = lab.fence.check_delivery_response(CONNECTION, &sent);
        assert!(
            matches!(decision, DeliveryDecision::Denied(denied) if denied == reason),
            "{id}: {decision:?}"
        );
        let again = lab
            .fence
            .check_delivery_response(CONNECTION, &response(id, TROUTER_REQUEST, CALLBACKS));
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
            .check_delivery_response(CONNECTION, &response(1, TROUTER_REQUEST, CALLBACKS)),
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
        .check_delivery_response(CONNECTION, &response(2, TROUTER_REQUEST, CALLBACKS));
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
            .check_delivery_response(CONNECTION, &response(3, TROUTER_REQUEST, CALLBACKS)),
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
