//! Offered invitations (`teams_fence`'s `calls::offer`, `docs/protocol/fence-targets.md`, "Offered invitations"): an
//! incoming call is offered only when it is readable, addressed to the fence's own account and called by a lab account.
//! Under the registry's rule (no build's fence's: `Fence::tracking_calls` here, the registry's resolution as verdicts)
//! only its ring-time sends pass on it
//! (its delivery response, the callee's `attach`), each bound to it, a group call's only while its group chat reads
//! lab-only, and nothing passes once its ring ended. Under the lab's rule (at the end) only its answer and its decline
//! are judged, and a join by the chat it names. Shaped like the `ab-call` and
//! `abc-call` captures (`tests/fixtures/calling/chat/`), with made-up ids.

use std::path::PathBuf;

use base64::Engine as _;
use serde_json::{Value, json};
use teams_fence::{
    CALL_START_URL, CallFloor, CallOperation, Contact, Delivery, DeliveryDecision, DeliveryDenyReason, DenyReason,
    FailClosed, Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, Observed, PartialReason, Policy,
    Request, Roster, Scope, TEST_CALL_SCENARIO, Target, WriteKind, contacting,
};

const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const LAB_A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const COWORKER: &str = "8:orgid:00000000-0000-4000-8000-0000000000c0";
const ECHO: &str = "28:00000000-0000-4000-8000-0000000000e0";
const GROUP: &str = "0123456789abcdef0123456789abcdef";
const CONNECTION: &str = "session-b/socket-1";
const TROUTER_REQUEST: &str = r#"{"id":"request-1","src":"198.51.100.7","port":3443}"#;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A named change to a captured body.
type BodyEdit = Box<dyn Fn(&mut Value)>;
/// A named change to a captured body, used once.
type BodyEditOnce = Box<dyn FnOnce(&mut Value)>;

const CHAT_DELIVERIES: &str = include_str!("../../../tests/fixtures/calling/chat/deliveries.json");
const CHAT_FOLLOW_UPS: &str = include_str!("../../../tests/fixtures/calling/chat/follow_ups.json");

/// A fixture with its placeholders filled with this file's accounts and made-up ids.
fn fill(text: &str) -> String {
    let mut filled = text
        .replace("<guid:a>", GUID_A)
        .replace("<guid:b>", GUID_B)
        .replace("<group>", GROUP);
    for (n, placeholder) in ["tenant", "leg", "call", "aa", "bb", "3", "4", "agentb"]
        .iter()
        .enumerate()
    {
        filled = filled.replace(
            &format!("<guid:{placeholder}>"),
            &format!("00000000-0000-4000-8000-0000000003{n:02}"),
        );
    }
    filled
}

/// The invitation fixture `index` (0: a 1:1 call's, 1: a group call's), its `gp` as the wire carries it (base64 JSON),
/// as a delivery with this id.
fn invitation(index: usize, id: i64) -> TestResult<(Delivery, Value)> {
    let fixture: Value = serde_json::from_str(&fill(CHAT_DELIVERIES))?;
    let case = &fixture["invitations"][index];
    let gp = case["body"]["gp"].clone();
    Ok((invitation_delivery(case["path"].as_str().ok_or("path")?, id, &gp), gp))
}

fn invitation_delivery(path: &str, id: i64, gp: &Value) -> Delivery {
    let evt = if gp.get("groupChat").is_some_and(|chat| !chat.is_null()) {
        109
    } else {
        107
    };
    invitation_delivery_as(path, id, evt, gp)
}

/// An invitation delivery with this `evt`, whatever its `gp` names.
fn invitation_delivery_as(path: &str, id: i64, evt: u64, gp: &Value) -> Delivery {
    let body = json!({
        "evt": evt,
        "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string()),
    });
    Delivery {
        connection: CONNECTION.to_owned(),
        id,
        path: path.to_owned(),
        trouter_request: Some(TROUTER_REQUEST.to_owned()),
        chain_id: None,
        message_id: None,
        body: body.to_string().into_bytes(),
    }
}

/// The captured response to a delivery with this id.
fn receipt(id: i64) -> Value {
    json!({
        "id": id,
        "status": 200,
        "headers": {"MS-CV": "SyntheticCv.1.0", "trouter-request": TROUTER_REQUEST, "trouter-client": "{\"cd\":46}"},
        "body": "",
    })
}

/// The callee's attach for the 1:1 invitation (`follow_ups.json`), as a request.
fn attach_case() -> TestResult<(String, Value)> {
    let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS))?;
    let case = &fixture["cases"][1];
    Ok((case["url"].as_str().ok_or("url")?.to_owned(), case["body"].clone()))
}

/// The write `body` posted to `url` is, by the lab rule.
fn attach_target(url: &str, body: &Value) -> TestResult<Target> {
    let body = body.to_string();
    match contacting(
        &Request::new("POST", url).with_body(Some(body.as_bytes())),
        &Policy::new(),
    ) {
        Contact::Write(target) => Ok(target),
        other => Err(format!("the attach is {other:?}").into()),
    }
}

/// The group call's attach: the 1:1 attach's, sent for the group invitation.
fn group_attach(gp: &Value) -> TestResult<(String, Value)> {
    let (_, mut body) = attach_case()?;
    body["additionalActions"][0]["url"] = gp["conversationInvitation"]["conversationController"].clone();
    Ok((
        gp["callNotification"]["links"]["attach"]
            .as_str()
            .ok_or("attach")?
            .to_owned(),
        body,
    ))
}

struct Lab {
    _dir: tempfile::TempDir,
    log: PathBuf,
    fence: Fence,
}

/// b's fence (its own account b) in a lab of a, b and c with the echo service.
fn lab_of(own: Option<&str>) -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("fence.log");
    let roster = Roster::new([Mri::new(LAB_A), Mri::new(LAB_B), Mri::new(LAB_C)], [Mri::new(ECHO)]);
    let fence = Fence::new(roster, FenceLog::open(&log)?).tracking_calls();
    let fence = match own {
        Some(own) => fence.with_own_account(Mri::new(own)),
        None => fence,
    };
    Ok(Lab { _dir: dir, log, fence })
}

fn lab_b() -> TestResult<Lab> {
    lab_of(Some(LAB_B))
}

/// A membership source answering every scope with one membership, counting its reads.
struct Members(Membership, std::sync::atomic::AtomicUsize);

impl Members {
    fn of(mris: &[&str]) -> Self {
        Self::new(Membership::Complete(mris.iter().map(|mri| Mri::new(*mri)).collect()))
    }

    fn new(membership: Membership) -> Self {
        Self(membership, std::sync::atomic::AtomicUsize::new(0))
    }

    fn reads(&self) -> usize {
        self.1.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl MembershipSource for Members {
    fn members<'a>(&'a self, scope: &'a Scope) -> MembershipFuture<'a> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let membership = match scope {
            Scope::Conversation { id } if id == &format!("19:{GROUP}@thread.v2") || id == MEETING_CHAT => {
                self.0.clone()
            }
            _ => Membership::Partial(PartialReason::NoSource),
        };
        Box::pin(async move { membership })
    }
}

async fn respond(lab: &Lab, response: &Value, source: &dyn MembershipSource) -> DeliveryDecision {
    lab.fence
        .check_delivery_response(CONNECTION, response.to_string().as_bytes(), source)
        .await
}

fn denied_reason(decision: &DeliveryDecision) -> Option<DeliveryDenyReason> {
    match decision {
        DeliveryDecision::Denied(reason) => Some(*reason),
        _ => None,
    }
}

// --- offering ---

#[tokio::test]
async fn a_lab_caller_s_invitation_to_the_fence_s_own_account_is_offered_and_its_receipt_passes_once() -> TestResult {
    let lab = lab_b()?;
    let (delivery, _) = invitation(0, 41)?;
    assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Offered);
    let decision = respond(&lab, &receipt(41), &FailClosed).await;
    assert!(matches!(decision, DeliveryDecision::Permitted(ref permit) if permit.delivery() == 41));
    // once: the same response again answers nothing waiting
    assert!(matches!(
        respond(&lab, &receipt(41), &FailClosed).await,
        DeliveryDecision::NotPending
    ));
    // the receipt's other captured form: no MS-CV
    let (again, _) = invitation(0, 42)?;
    lab.fence.observe_delivery(&again);
    let mut bare = receipt(42);
    bare["headers"].as_object_mut().ok_or("headers")?.remove("MS-CV");
    assert!(matches!(
        respond(&lab, &bare, &FailClosed).await,
        DeliveryDecision::Permitted(_)
    ));
    let log = std::fs::read_to_string(&lab.log)?;
    assert!(log.contains("call_invitation_receipt"), "{log}");
    Ok(())
}

#[tokio::test]
async fn an_invitation_the_fence_wouldn_t_answer_is_ignored_and_nothing_passes_for_it() -> TestResult {
    let (delivery, gp) = invitation(0, 42)?;
    // a fence of no account, and a's fence (the invitation is b's)
    for own in [None, Some(LAB_A)] {
        let lab = lab_of(own)?;
        assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Ignored, "{own:?}");
        assert!(matches!(
            respond(&lab, &receipt(42), &FailClosed).await,
            DeliveryDecision::NotPending
        ));
    }
    // a caller outside the lab, an outsider as the callee, the caller calling itself
    let lab = lab_b()?;
    for (pointer, mri) in [
        ("/callNotification/from/id", COWORKER),
        ("/callNotification/to/id", COWORKER),
        ("/callNotification/from/id", LAB_B),
    ] {
        let mut other = gp.clone();
        *other.pointer_mut(pointer).ok_or(pointer)? = Value::from(mri);
        let delivery = invitation_delivery(&delivery.path, 43, &other);
        assert_eq!(
            lab.fence.observe_delivery(&delivery),
            Observed::Ignored,
            "{pointer} {mri}"
        );
    }
    assert!(matches!(
        respond(&lab, &receipt(43), &FailClosed).await,
        DeliveryDecision::NotPending
    ));
    Ok(())
}

#[tokio::test]
async fn an_invitation_that_doesn_t_read_is_ignored_and_nothing_passes_for_it() -> TestResult {
    let lab = lab_b()?;
    let (delivery, gp) = invitation(0, 44)?;
    let edits: [(&str, BodyEdit); 6] = [
        ("a meeting", Box::new(|gp| gp["groupContext"] = json!({"id": "x"}))),
        (
            "a call it replaces",
            Box::new(|gp| gp["callNotification"]["replaces"] = json!("x")),
        ),
        (
            "no attach",
            Box::new(|gp| gp["callNotification"]["links"]["attach"] = Value::Null),
        ),
        (
            "attach off the proxy",
            Box::new(|gp| {
                gp["callNotification"]["links"]["attach"] = json!("https://cc.example/cc/v1/forked/c/1/attach");
            }),
        ),
        (
            "no conversation",
            Box::new(|gp| gp["conversationInvitation"] = json!({})),
        ),
        (
            "a chat that isn't a group chat",
            Box::new(|gp| gp["groupChat"] = json!({"threadId": "48:notes"})),
        ),
    ];
    for (name, edit) in edits {
        let mut other = gp.clone();
        edit(&mut other);
        let delivery = invitation_delivery(&delivery.path, 44, &other);
        assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Ignored, "{name}");
    }
    // a body at the root that may be an invitation, however little of it reads
    let at_root = |body: &[u8]| Delivery {
        body: body.to_vec(),
        ..delivery.clone()
    };
    for body in [
        br#"{"evt":107,"gp":"not base64"}"#.as_slice(),
        br#"{"evt":109}"#,
        br#"{"evt":107,"evt":115,"cp":"x"}"#,
        br#"{"gp":"e30="}"#,
        br#"{"evt":115,"callNotification":{}}"#,
        br#"{"evt":115,"gp":"not base64"}"#,
        br#"{"evt":115,"gp":"eyJjYWxsTm90aWZpY2F0aW9uIjp7fX0="}"#,
        b"not json",
        b"[]",
    ] {
        assert_eq!(
            lab.fence.observe_delivery(&at_root(body)),
            Observed::Ignored,
            "{}",
            String::from_utf8_lossy(body)
        );
    }
    // the root's other captured events aren't invitations
    assert_eq!(
        lab.fence.observe_delivery(&at_root(br#"{"evt":115,"cp":"H4sI"}"#)),
        Observed::Nothing
    );
    // not at the endpoint's root
    let mut deeper = delivery.clone();
    deeper.path = format!("{}callAgent/x/", delivery.path);
    assert_eq!(lab.fence.observe_delivery(&deeper), Observed::Nothing);
    assert!(matches!(
        respond(&lab, &receipt(44), &FailClosed).await,
        DeliveryDecision::NotPending
    ));
    Ok(())
}

#[tokio::test]
async fn an_invitation_reads_only_in_its_call_kind_s_captured_shape() -> TestResult {
    let lab = lab_b()?;
    let (delivery, gp) = invitation(0, 45)?;
    // a group call's other participants are named only by its chat: its kind must be the captured one
    let (group, group_gp) = invitation(1, 45)?;
    let without_chat = |chat: Option<Value>| {
        let mut gp = group_gp.clone();
        if let Some(object) = gp.as_object_mut() {
            object.remove("groupChat");
            if let Some(chat) = chat {
                object.insert("groupChat".to_owned(), chat);
            }
        }
        gp
    };
    let multi_party = |gp: &Value, value: Value| {
        let mut gp = gp.clone();
        gp["conversationInvitation"]["isMultiParty"] = value;
        gp
    };
    let mut no_flag = gp.clone();
    if let Some(invitation) = no_flag["conversationInvitation"].as_object_mut() {
        invitation.remove("isMultiParty");
    }
    let shapes: [(&str, &str, u64, Value); 7] = [
        ("a group call naming no chat", &group.path, 109, without_chat(None)),
        (
            "a group call's chat null",
            &group.path,
            109,
            without_chat(Some(Value::Null)),
        ),
        (
            "a group call that isn't multi-party",
            &group.path,
            109,
            multi_party(&group_gp, json!(false)),
        ),
        ("a 1:1 call naming a group chat", &group.path, 107, group_gp.clone()),
        (
            "a multi-party 1:1 call",
            &delivery.path,
            107,
            multi_party(&gp, json!(true)),
        ),
        ("a 1:1 call that doesn't say", &delivery.path, 107, no_flag),
        ("another event", &delivery.path, 108, gp.clone()),
    ];
    for (name, path, evt, gp) in shapes {
        let delivery = invitation_delivery_as(path, 45, evt, &gp);
        // none reads, and each may be an invitation: ignored, never offered
        assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Ignored, "{name}");
    }
    assert!(matches!(
        respond(&lab, &receipt(45), &FailClosed).await,
        DeliveryDecision::NotPending
    ));
    // the captured shapes read
    assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Offered);
    assert_eq!(lab.fence.observe_delivery(&group), Observed::Offered);
    Ok(())
}

#[tokio::test]
async fn only_the_captured_receipt_answers_an_offered_invitation() -> TestResult {
    let edits: [(&str, BodyEdit, DeliveryDenyReason); 5] = [
        (
            "a 404",
            Box::new(|r| r["status"] = json!(404)),
            DeliveryDenyReason::Shape,
        ),
        (
            "a body",
            Box::new(|r| r["body"] = json!("{}")),
            DeliveryDenyReason::Shape,
        ),
        (
            "another header",
            Box::new(|r| r["headers"]["Trouter-Responder"] = json!("ClientLib")),
            DeliveryDenyReason::Shape,
        ),
        (
            "another key",
            Box::new(|r| r["extra"] = json!(1)),
            DeliveryDenyReason::Shape,
        ),
        (
            "another request",
            Box::new(|r| r["headers"]["trouter-request"] = json!("{\"id\":\"other\"}")),
            DeliveryDenyReason::Uncorrelated,
        ),
    ];
    for (name, edit, reason) in edits {
        let lab = lab_b()?;
        let (delivery, _) = invitation(0, 45)?;
        lab.fence.observe_delivery(&delivery);
        let mut response = receipt(45);
        edit(&mut response);
        assert_eq!(
            denied_reason(&respond(&lab, &response, &FailClosed).await),
            Some(reason),
            "{name}"
        );
        // the delivery's one response was taken
        assert!(
            matches!(
                respond(&lab, &receipt(45), &FailClosed).await,
                DeliveryDecision::NotPending
            ),
            "{name}"
        );
    }
    // on another connection it answers nothing waiting
    let lab = lab_b()?;
    let (mut delivery, _) = invitation(0, 46)?;
    delivery.connection = "session-b/socket-2".to_owned();
    lab.fence.observe_delivery(&delivery);
    assert!(matches!(
        respond(&lab, &receipt(46), &FailClosed).await,
        DeliveryDecision::NotPending
    ));
    lab.fence.close_connection("session-b/socket-2");
    Ok(())
}

// --- the callee's attach ---

#[tokio::test]
async fn the_callee_s_attach_passes_only_on_its_offered_invitation() -> TestResult {
    let lab = lab_b()?;
    let (url, body) = attach_case()?;
    let target = attach_target(&url, &body)?;
    // before the invitation arrived: no offer holds the link
    let early = lab.fence.check(target.clone(), &FailClosed).await;
    assert_eq!(
        early.err().map(|denied| *denied.reason()),
        Some(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    let (delivery, _) = invitation(0, 47)?;
    lab.fence.observe_delivery(&delivery);
    let permit = lab.fence.check(target.clone(), &FailClosed).await?;
    assert_eq!(permit.target().kind(), WriteKind::CallRinging);
    // the client tries it again on a failure: the same attach passes again while it rings
    lab.fence.check(target, &FailClosed).await?;
    let log = std::fs::read_to_string(&lab.log)?;
    assert!(log.contains("call_ringing"), "{log}");
    Ok(())
}

#[tokio::test]
async fn an_attach_that_doesn_t_fit_its_invitation_is_a_mismatch() -> TestResult {
    let (url, body) = attach_case()?;
    let other_agent = |body: &mut Value| {
        let link = body["attach"]["links"]["end"]
            .as_str()
            .unwrap_or_default()
            .replace("/callAgent/", "/callAgent/x");
        body["attach"]["links"]["end"] = Value::from(link);
    };
    let other_endpoint = |body: &mut Value| {
        let link = body["attach"]["links"]["end"]
            .as_str()
            .unwrap_or_default()
            .replace("SyntheticEndpoint00001", "SyntheticEndpoint00009");
        body["attach"]["links"]["end"] = Value::from(link);
    };
    let edits: [(&str, BodyEdit); 5] = [
        (
            "from a",
            Box::new(|body| body["additionalActions"][0]["input"]["participants"]["from"]["id"] = json!(LAB_A)),
        ),
        (
            "another conversation",
            Box::new(|body| {
                body["additionalActions"][0]["url"] = json!(
                    "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/Other?i=1"
                );
            }),
        ),
        ("a callback on another agent", Box::new(other_agent)),
        ("a callback on another endpoint", Box::new(other_endpoint)),
        (
            "a callback with a query",
            Box::new(|body| {
                let link = format!("{}?x=1", body["attach"]["links"]["end"].as_str().unwrap_or_default());
                body["attach"]["links"]["end"] = Value::from(link);
            }),
        ),
    ];
    for (name, edit) in edits {
        let lab = lab_b()?;
        let (delivery, _) = invitation(0, 48)?;
        lab.fence.observe_delivery(&delivery);
        let mut other = body.clone();
        edit(&mut other);
        let denied = lab.fence.check(attach_target(&url, &other)?, &FailClosed).await;
        assert_eq!(
            denied.err().map(|denied| *denied.reason()),
            Some(DenyReason::CallMismatch),
            "{name}"
        );
    }
    // another link than the invitation's attach
    let lab = lab_b()?;
    let (delivery, _) = invitation(0, 48)?;
    lab.fence.observe_delivery(&delivery);
    let elsewhere = url.replace("SyntheticCall1", "SyntheticCall9");
    let denied = lab.fence.check(attach_target(&elsewhere, &body)?, &FailClosed).await;
    assert_eq!(
        denied.err().map(|denied| *denied.reason()),
        Some(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    Ok(())
}

#[tokio::test]
async fn a_permitted_attach_binds_its_agent_and_the_ring_ends_at_its_end_callback() -> TestResult {
    let lab = lab_b()?;
    let (url, body) = attach_case()?;
    let (delivery, gp) = invitation(0, 49)?;
    lab.fence.observe_delivery(&delivery);
    lab.fence.check(attach_target(&url, &body)?, &FailClosed).await?;
    // a second attach must use the agent the first one bound
    let mut moved = body.clone();
    let pointer = "/attach/links/end";
    let link = moved
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .replace("/callAgent/", "/callAgent/y");
    *moved.pointer_mut(pointer).ok_or(pointer)? = Value::from(link);
    let denied = lab.fence.check(attach_target(&url, &moved)?, &FailClosed).await;
    assert_eq!(
        denied.err().map(|denied| *denied.reason()),
        Some(DenyReason::CallMismatch)
    );
    // the call's end at the attach's own `call/end` callback ends the ring
    let end = body["attach"]["links"]["end"].as_str().ok_or("end")?;
    let path = url::Url::parse(end)?.path().to_owned();
    let ended = Delivery {
        path,
        id: 50,
        body: b"{}".to_vec(),
        ..delivery.clone()
    };
    assert_eq!(lab.fence.observe_delivery(&ended), Observed::RingEnded);
    let after = lab.fence.check(attach_target(&url, &body)?, &FailClosed).await;
    assert_eq!(
        after.err().map(|denied| *denied.reason()),
        Some(DenyReason::Partial(PartialReason::CallEnded))
    );
    // a later delivery of the same invitation is ignored, and its receipt stays denied
    let again = invitation_delivery(&delivery.path, 51, &gp);
    assert_eq!(lab.fence.observe_delivery(&again), Observed::Ignored);
    assert!(matches!(
        respond(&lab, &receipt(51), &FailClosed).await,
        DeliveryDecision::NotPending
    ));
    Ok(())
}

#[tokio::test]
async fn the_conversation_s_end_at_the_attach_s_callback_ends_the_ring_too() -> TestResult {
    let lab = lab_b()?;
    let (url, body) = attach_case()?;
    let (delivery, _) = invitation(0, 52)?;
    lab.fence.observe_delivery(&delivery);
    lab.fence.check(attach_target(&url, &body)?, &FailClosed).await?;
    let end = body["additionalActions"][0]["input"]["conversationRequest"]["links"]["conversationEnd"]
        .as_str()
        .ok_or("conversationEnd")?;
    let ended = Delivery {
        path: url::Url::parse(end)?.path().to_owned(),
        id: 53,
        ..delivery.clone()
    };
    assert_eq!(lab.fence.observe_delivery(&ended), Observed::RingEnded);
    // nothing else ends it: a roster update is no end
    Ok(())
}

// --- a group call's invitation: its chat read fresh ---

#[tokio::test]
async fn a_group_call_s_ring_time_sends_pass_only_while_its_chat_reads_lab_only() -> TestResult {
    let (delivery, gp) = invitation(1, 60)?;
    let (url, body) = group_attach(&gp)?;
    let cases: [(&str, Members, Option<DenyReason>, Option<DeliveryDenyReason>); 5] = [
        ("lab-only", Members::of(&[LAB_A, LAB_B, LAB_C]), None, None),
        (
            "an outsider",
            Members::of(&[LAB_A, LAB_B, COWORKER]),
            Some(DenyReason::Outsiders { count: 1 }),
            Some(DeliveryDenyReason::Parties),
        ),
        (
            "partial",
            Members::new(Membership::Partial(PartialReason::NoSource)),
            Some(DenyReason::Partial(PartialReason::NoSource)),
            Some(DeliveryDenyReason::Parties),
        ),
        (
            "without the callee",
            Members::of(&[LAB_A, LAB_C]),
            Some(DenyReason::CallMismatch),
            Some(DeliveryDenyReason::Parties),
        ),
        (
            "without the caller",
            Members::of(&[LAB_B, LAB_C]),
            Some(DenyReason::CallMismatch),
            Some(DeliveryDenyReason::Parties),
        ),
    ];
    for (name, members, attach, response) in cases {
        let lab = lab_b()?;
        assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Offered, "{name}");
        let decided = respond(&lab, &receipt(60), &members).await;
        assert_eq!(denied_reason(&decided), response, "{name}");
        let checked = lab.fence.check(attach_target(&url, &body)?, &members).await;
        assert_eq!(checked.err().map(|denied| *denied.reason()), attach, "{name}");
        assert_eq!(members.reads(), 2, "{name}: each decision reads the chat");
    }
    Ok(())
}

/// A membership source that, while it reads, ends the ring with a delivery at the attach's end callback (`end`), and
/// then answers with a lab-only membership: a ring that ends while a group invitation's chat is read.
struct EndsRing<'a> {
    fence: &'a Fence,
    end: Delivery,
    ended: std::sync::atomic::AtomicBool,
}

impl<'f> EndsRing<'f> {
    fn new(fence: &'f Fence, end: Delivery) -> Self {
        Self {
            fence,
            end,
            ended: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn ended(&self) -> bool {
        self.ended.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl MembershipSource for EndsRing<'_> {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        let ended = self.fence.observe_delivery(&self.end) == Observed::RingEnded;
        self.ended.fetch_or(ended, std::sync::atomic::Ordering::Relaxed);
        let lab_only = Membership::Complete([LAB_A, LAB_B, LAB_C].into_iter().map(Mri::new).collect());
        Box::pin(async move { lab_only })
    }
}

/// The group invitation `delivery`'s attach (permitted while its chat reads lab-only) and the call's end delivered at
/// that attach's `call/end` callback, with this id.
fn group_ring_end(delivery: &Delivery, gp: &Value, id: i64) -> TestResult<(String, Value, Delivery)> {
    let (url, body) = group_attach(gp)?;
    let end = body["attach"]["links"]["end"].as_str().ok_or("end")?;
    let ended = Delivery {
        path: url::Url::parse(end)?.path().to_owned(),
        id,
        body: b"{}".to_vec(),
        ..delivery.clone()
    };
    Ok((url, body, ended))
}

#[tokio::test]
async fn a_ring_that_ends_while_a_group_invitation_s_chat_is_read_lets_nothing_through() -> TestResult {
    let lab_only = Members::of(&[LAB_A, LAB_B, LAB_C]);
    let (delivery, gp) = invitation(1, 62)?;
    let (url, body, ended) = group_ring_end(&delivery, &gp, 63)?;
    // the attach sent again: the ring ends while its chat is read
    let lab = lab_b()?;
    lab.fence.observe_delivery(&delivery);
    lab.fence.check(attach_target(&url, &body)?, &lab_only).await?;
    let source = EndsRing::new(&lab.fence, ended.clone());
    let again = lab.fence.check(attach_target(&url, &body)?, &source).await;
    assert!(source.ended(), "the read ended the ring");
    assert_eq!(
        again.err().map(|denied| *denied.reason()),
        Some(DenyReason::Partial(PartialReason::CallEnded))
    );
    // the delivery's response: the ring ends while its chat is read
    let lab = lab_b()?;
    lab.fence.observe_delivery(&delivery);
    lab.fence.check(attach_target(&url, &body)?, &lab_only).await?;
    let source = EndsRing::new(&lab.fence, ended);
    let decided = respond(&lab, &receipt(62), &source).await;
    assert!(source.ended(), "the read ended the ring");
    assert_eq!(denied_reason(&decided), Some(DeliveryDenyReason::NotLive));
    Ok(())
}

// --- the attach's answer and the callee's progress ---

/// The follow-ups fixture's progress case and the attach answer.
fn progress_case() -> TestResult<(String, Value, Value)> {
    let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS))?;
    let case = &fixture["cases"][2];
    Ok((
        case["url"].as_str().ok_or("url")?.to_owned(),
        case["body"].clone(),
        fixture["attach_answer"].clone(),
    ))
}

/// b's lab with the 1:1 invitation offered and b's attach permitted, its permit kept.
async fn attached() -> TestResult<(Lab, teams_fence::WritePermit)> {
    let lab = lab_b()?;
    let (delivery, _) = invitation(0, 70)?;
    lab.fence.observe_delivery(&delivery);
    let (url, body) = attach_case()?;
    let permit = lab.fence.check(attach_target(&url, &body)?, &FailClosed).await?;
    Ok((lab, permit))
}

#[tokio::test]
async fn the_progress_passes_only_to_the_link_its_attach_s_answer_bound() -> TestResult {
    let (lab, permit) = attached().await?;
    let (url, body, answer) = progress_case()?;
    let progress = attach_target(&url, &body)?;
    assert_eq!(progress.kind(), WriteKind::CallRinging);
    // the answer isn't read yet: no link to go to
    let early = lab.fence.check(progress.clone(), &FailClosed).await;
    assert_eq!(
        early.err().map(|denied| *denied.reason()),
        Some(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    let pending = lab.fence.pend_answer(&permit).ok_or("pending")?;
    assert!(lab.fence.pend_answer(&permit).is_none(), "a permit pends once");
    lab.fence.bind_answer(pending, answer.to_string().as_bytes())?;
    lab.fence.check(progress, &FailClosed).await?;
    // only from the callee
    let mut from_a = body.clone();
    from_a["callProgress"]["sender"]["id"] = json!(LAB_A);
    let denied = lab.fence.check(attach_target(&url, &from_a)?, &FailClosed).await;
    assert_eq!(
        denied.err().map(|denied| *denied.reason()),
        Some(DenyReason::CallMismatch)
    );
    // the leg's other links bind nothing a progress could go to
    let accept = answer["callInvitation"]["links"]["acceptance"]
        .as_str()
        .ok_or("acceptance")?;
    let elsewhere = lab
        .fence
        .check(
            attach_target(&accept.replace("/accept", "/progress").replace("/t/2/", "/t/3/"), &body)?,
            &FailClosed,
        )
        .await;
    assert_eq!(
        elsewhere.err().map(|denied| *denied.reason()),
        Some(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    Ok(())
}

#[tokio::test]
async fn an_attach_answer_naming_another_call_binds_none_of_its_leg_links() -> TestResult {
    let (lab, permit) = attached().await?;
    let (url, body, answer) = progress_case()?;
    let other = answer
        .to_string()
        .replace("incoming/SyntheticCall1", "incoming/SyntheticCall9");
    let pending = lab.fence.pend_answer(&permit).ok_or("pending")?;
    // its conversation links still bind; the leg links of another call bind nothing
    lab.fence.bind_answer(pending, other.as_bytes())?;
    let progress = url.replace("SyntheticCall1", "SyntheticCall9");
    let denied = lab.fence.check(attach_target(&progress, &body)?, &FailClosed).await;
    assert_eq!(
        denied.err().map(|denied| *denied.reason()),
        Some(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    // a permit for anything but an attach pends nothing
    let (start_lab, start_permit) = attached().await?;
    drop(start_lab);
    assert!(lab.fence.pend_answer(&start_permit).is_none(), "another fence's permit");
    Ok(())
}

#[test]
fn only_a_ringing_progress_in_its_captured_shape_is_a_write() -> TestResult {
    let (url, body, _) = progress_case()?;
    let verdict = |body: &Value| {
        let body = body.to_string();
        contacting(
            &Request::new("POST", &url).with_body(Some(body.as_bytes())),
            &Policy::new(),
        )
    };
    assert!(matches!(verdict(&body), Contact::Write(_)));
    let edits: [(&str, BodyEdit); 4] = [
        (
            "forwarded",
            Box::new(|body| body["callProgress"]["status"] = json!("forwarded")),
        ),
        (
            "another key",
            Box::new(|body| body["callProgress"]["forwardingDestinationType"] = json!("voicemail")),
        ),
        (
            "no sender",
            Box::new(|body| body["callProgress"]["sender"] = Value::Null),
        ),
        ("an acceptance", Box::new(|body| body["callAcceptance"] = json!({}))),
    ];
    for (name, edit) in edits {
        let mut other = body.clone();
        edit(&mut other);
        assert_eq!(verdict(&other), Contact::Unreadable, "{name}");
    }
    Ok(())
}

// --- the callee's answer and decline ---

/// The follow-ups fixture's case `index` as a write: 3 the endpoint state, 4 the acceptance, 5 the decline.
fn case_write(index: usize, edit: impl FnOnce(&mut Value)) -> TestResult<Target> {
    let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS))?;
    let case = &fixture["cases"][index];
    let mut body = case["body"].clone();
    edit(&mut body);
    let (method, url) = (
        case["method"].as_str().ok_or("method")?,
        case["url"].as_str().ok_or("url")?,
    );
    // a case without a body (`null`) is sent with none
    let body = (!body.is_null()).then(|| body.to_string());
    match contacting(
        &Request::new(method, url).with_body(body.as_deref().map(str::as_bytes)),
        &Policy::new(),
    ) {
        Contact::Write(target) => Ok(target),
        other => Err(format!("case {index} is {other:?}").into()),
    }
}

/// b's lab with the 1:1 invitation offered, attached and its attach's answer bound.
async fn bound() -> TestResult<Lab> {
    let (lab, permit) = attached().await?;
    let (_, _, answer) = progress_case()?;
    let pending = lab.fence.pend_answer(&permit).ok_or("pending")?;
    lab.fence.bind_answer(pending, answer.to_string().as_bytes())?;
    Ok(lab)
}

async fn reason(lab: &Lab, target: Target) -> Option<DenyReason> {
    lab.fence
        .check(target, &FailClosed)
        .await
        .err()
        .map(|denied| *denied.reason())
}

const STATE: usize = 3;
const ACCEPT: usize = 4;
const REJECT: usize = 5;

#[tokio::test]
async fn the_callee_s_answer_passes_on_its_ringing_invitation_and_then_only_itself_again() -> TestResult {
    let lab = bound().await?;
    let ended = Some(DenyReason::Partial(PartialReason::CallEnded));
    lab.fence.check(case_write(STATE, |_| ())?, &FailClosed).await?;
    let permit = lab.fence.check(case_write(ACCEPT, |_| ())?, &FailClosed).await?;
    assert_eq!(permit.target().kind(), WriteKind::CallAnswer);
    // answered: its endpoint state may go on, and the same accept again (the browser's second layer, the client's
    // resend); nothing of the ring
    lab.fence.check(case_write(STATE, |_| ())?, &FailClosed).await?;
    lab.fence.check(case_write(ACCEPT, |_| ())?, &FailClosed).await?;
    assert_eq!(reason(&lab, case_write(REJECT, |_| ())?).await, ended);
    let (url, body, _) = progress_case()?;
    assert_eq!(reason(&lab, attach_target(&url, &body)?).await, ended);
    let log = std::fs::read_to_string(&lab.log)?;
    assert!(log.contains("call_answer"), "{log}");
    Ok(())
}

#[tokio::test]
async fn an_answer_off_its_invitation_is_denied() -> TestResult {
    // before the attach's answer bound the acceptance link
    let (lab, _) = attached().await?;
    assert_eq!(
        reason(&lab, case_write(ACCEPT, |_| ())?).await,
        Some(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    let lab = bound().await?;
    let from_a = case_write(ACCEPT, |body| body["callAcceptance"]["acceptedBy"]["id"] = json!(LAB_A))?;
    assert_eq!(reason(&lab, from_a).await, Some(DenyReason::CallMismatch));
    let elsewhere = case_write(ACCEPT, |body| {
        let link = body["callAcceptance"]["links"]["transfer"]
            .as_str()
            .unwrap_or_default()
            .replace("/callAgent/", "/callAgent/z");
        body["callAcceptance"]["links"]["transfer"] = Value::from(link);
    })?;
    assert_eq!(reason(&lab, elsewhere).await, Some(DenyReason::CallMismatch));
    let state_from_a = case_write(STATE, |body| body["from"]["id"] = json!(LAB_A))?;
    assert_eq!(reason(&lab, state_from_a).await, Some(DenyReason::CallMismatch));
    // still ringing: the callee's own answer goes
    lab.fence.check(case_write(ACCEPT, |_| ())?, &FailClosed).await?;
    Ok(())
}

#[tokio::test]
async fn the_decline_passes_and_ends_the_ring_leaving_only_itself_to_pass_again() -> TestResult {
    let lab = bound().await?;
    let permit = lab.fence.check(case_write(REJECT, |_| ())?, &FailClosed).await?;
    assert_eq!(permit.target().kind(), WriteKind::CallDecline);
    let ended = Some(DenyReason::Partial(PartialReason::CallEnded));
    lab.fence.check(case_write(REJECT, |_| ())?, &FailClosed).await?;
    assert_eq!(reason(&lab, case_write(ACCEPT, |_| ())?).await, ended);
    assert_eq!(reason(&lab, case_write(STATE, |_| ())?).await, ended);
    let log = std::fs::read_to_string(&lab.log)?;
    assert!(log.contains("call_decline"), "{log}");
    Ok(())
}

#[test]
fn only_the_captured_answer_decline_and_endpoint_state_are_writes() -> TestResult {
    let unreadable = |index: usize, edit: Box<dyn FnOnce(&mut Value)>| case_write(index, edit).is_err();
    // a reject that isn't the callee's decline (the client's own after a failed answer)
    assert!(unreadable(
        REJECT,
        Box::new(|body| {
            body["callEnd"] = json!({"code": 490, "subCode": 4525, "phrase": "CallEndReasonFetchCorsOrNetwork"});
        })
    ));
    assert!(unreadable(
        REJECT,
        Box::new(|body| body["callEnd"]["code"] = json!(487))
    ));
    assert!(unreadable(
        ACCEPT,
        Box::new(|body| body["callAcceptance"]["transferTarget"] = json!("x"))
    ));
    assert!(unreadable(
        ACCEPT,
        Box::new(|body| body["callAcceptance"]["acceptedBy"] = Value::Null)
    ));
    assert!(unreadable(
        STATE,
        Box::new(|body| body["endpointState"]["state"]["isOnHold"] = json!(true))
    ));
    for (index, kind) in [
        (STATE, WriteKind::CallFollowUp),
        (ACCEPT, WriteKind::CallAnswer),
        (REJECT, WriteKind::CallDecline),
    ] {
        assert_eq!(case_write(index, |_| ())?.kind(), kind, "case {index}");
    }
    Ok(())
}

#[test]
fn every_operation_on_an_invitation_is_judged_like_an_answer() {
    for operation in CallOperation::ALL
        .into_iter()
        .filter(|operation| operation.on_invitation())
    {
        let kind = operation.kind();
        assert!(
            matches!(
                kind,
                WriteKind::CallRinging | WriteKind::CallAnswer | WriteKind::CallDecline | WriteKind::CallFollowUp
            ),
            "{operation}"
        );
        assert_eq!(
            operation.names_sender(),
            operation != CallOperation::Reject,
            "{operation}"
        );
    }
}

#[tokio::test]
async fn a_declined_or_answered_invitation_s_call_end_ends_it_for_good() -> TestResult {
    let (url, body) = attach_case()?;
    let end = body["attach"]["links"]["end"].as_str().ok_or("end")?.to_owned();
    for decision in [ACCEPT, REJECT] {
        let lab = bound().await?;
        lab.fence.check(case_write(decision, |_| ())?, &FailClosed).await?;
        let (delivery, _) = invitation(0, 80)?;
        let ended = Delivery {
            path: url::Url::parse(&end)?.path().to_owned(),
            id: 81,
            ..delivery
        };
        assert_eq!(
            lab.fence.observe_delivery(&ended),
            Observed::RingEnded,
            "case {decision}"
        );
        for index in [STATE, ACCEPT, REJECT] {
            assert_eq!(
                reason(&lab, case_write(index, |_| ())?).await,
                Some(DenyReason::Partial(PartialReason::CallEnded)),
                "case {decision} then {index}"
            );
        }
        assert_eq!(
            reason(&lab, attach_target(&url, &body)?).await,
            Some(DenyReason::Partial(PartialReason::CallEnded))
        );
    }
    Ok(())
}

// --- the answered call: its operations, its media negotiation ---

const METADATA: usize = 6;
const PROPERTIES: usize = 7;
const LEAVE: usize = 8;
const RENEGOTIATE: usize = 9;
const MEDIA_ANSWER: usize = 10;
const MEDIA_ACKNOWLEDGE: usize = 11;
const MEDIA_LINES: usize = 12;
const ROSTER_JOIN: usize = 13;
const MEDIA_JOIN: usize = 14;

/// b's lab with the 1:1 invitation answered: attached, the attach's answer bound, the accept permitted and its answer
/// bound.
async fn answered() -> TestResult<Lab> {
    let lab = bound().await?;
    let permit = lab.fence.check(case_write(ACCEPT, |_| ())?, &FailClosed).await?;
    let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS))?;
    let pending = lab.fence.pend_answer(&permit).ok_or("the accept pends")?;
    lab.fence
        .bind_answer(pending, fixture["accept_answer"].to_string().as_bytes())?;
    Ok(lab)
}

/// The follow-ups fixture's delivery `index` at the callee's endpoint and call agent.
fn negotiation_delivery(index: usize, id: i64) -> TestResult<Delivery> {
    let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS))?;
    let case = &fixture["deliveries"][index];
    let (delivery, _) = invitation(0, id)?;
    Ok(Delivery {
        path: format!(
            "/v4/f/SyntheticEndpoint00001/callAgent/00000000-0000-4000-8000-000000000307/{}",
            case["callback"].as_str().ok_or("callback")?
        ),
        body: case["body"].to_string().into_bytes(),
        ..delivery
    })
}

#[tokio::test]
async fn the_answered_call_s_operations_pass_until_its_callee_left() -> TestResult {
    let lab = answered().await?;
    for index in [METADATA, PROPERTIES, MEDIA_LINES, RENEGOTIATE] {
        lab.fence.check(case_write(index, |_| ())?, &FailClosed).await?;
    }
    // before the answer they had nowhere to go: nothing of the answered call passes while it rings
    let ringing = bound().await?;
    for index in [METADATA, PROPERTIES, LEAVE] {
        assert_eq!(
            reason(&ringing, case_write(index, |_| ())?).await,
            Some(DenyReason::Partial(PartialReason::CallEnded)),
            "case {index} while ringing"
        );
    }
    for index in [MEDIA_LINES, RENEGOTIATE] {
        assert_eq!(
            reason(&ringing, case_write(index, |_| ())?).await,
            Some(DenyReason::Partial(PartialReason::UnregisteredCall)),
            "case {index} before the accept's answer"
        );
    }
    // the callee leaves: then only its leave again
    lab.fence.check(case_write(LEAVE, |_| ())?, &FailClosed).await?;
    lab.fence.check(case_write(LEAVE, |_| ())?, &FailClosed).await?;
    assert_eq!(
        reason(&lab, case_write(METADATA, |_| ())?).await,
        Some(DenyReason::Partial(PartialReason::CallEnded))
    );
    Ok(())
}

#[tokio::test]
async fn the_answered_call_s_operations_are_the_callee_s_own() -> TestResult {
    let lab = answered().await?;
    let from_a = |pointer: &'static str| {
        move |body: &mut Value| {
            if let Some(at) = body.pointer_mut(pointer) {
                *at = json!(LAB_A);
            }
        }
    };
    for (index, pointer) in [
        (METADATA, "/participants/from/id"),
        (LEAVE, "/participants/from/id"),
        (RENEGOTIATE, "/mediaNegotiation/sender/id"),
    ] {
        assert_eq!(
            reason(&lab, case_write(index, from_a(pointer))?).await,
            Some(DenyReason::CallMismatch),
            "case {index}"
        );
    }
    // another participant's properties aren't the sender's own: unreadable
    let other = case_write(PROPERTIES, |body| body["to"][0]["id"] = json!(LAB_A));
    assert!(other.is_err());
    Ok(())
}

#[tokio::test]
async fn a_renegotiation_and_an_answer_delivered_bind_the_links_their_replies_go_to() -> TestResult {
    let lab = answered().await?;
    // nothing delivered yet: no link to answer at
    assert_eq!(
        reason(&lab, case_write(MEDIA_ANSWER, |_| ())?).await,
        Some(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    assert_eq!(
        lab.fence.observe_delivery(&negotiation_delivery(0, 90)?),
        Observed::Renegotiation
    );
    lab.fence.check(case_write(MEDIA_ANSWER, |_| ())?, &FailClosed).await?;
    assert_eq!(
        reason(&lab, case_write(MEDIA_ACKNOWLEDGE, |_| ())?).await,
        Some(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    assert_eq!(
        lab.fence.observe_delivery(&negotiation_delivery(1, 91)?),
        Observed::Renegotiation
    );
    lab.fence
        .check(case_write(MEDIA_ACKNOWLEDGE, |_| ())?, &FailClosed)
        .await?;
    // an acknowledgement with a body is no acknowledgement the capture showed
    assert!(case_write(MEDIA_ACKNOWLEDGE, |body| *body = json!({})).is_err());
    // each of the two deliveries gets its receipt, once (M3 Task 18)
    for id in [90, 91] {
        assert!(
            matches!(
                respond(&lab, &receipt(id), &FailClosed).await,
                DeliveryDecision::Permitted(_)
            ),
            "the receipt of {id}"
        );
        assert!(matches!(
            respond(&lab, &receipt(id), &FailClosed).await,
            DeliveryDecision::NotPending
        ));
    }
    // once the callee left, a receipt of a renegotiation delivered before is denied
    assert_eq!(
        lab.fence.observe_delivery(&negotiation_delivery(0, 94)?),
        Observed::Renegotiation
    );
    lab.fence.check(case_write(LEAVE, |_| ())?, &FailClosed).await?;
    assert_eq!(
        denied_reason(&respond(&lab, &receipt(94), &FailClosed).await),
        Some(DeliveryDenyReason::NotLive)
    );
    Ok(())
}

#[tokio::test]
async fn a_delivery_naming_another_call_binds_no_reply_link() -> TestResult {
    let lab = answered().await?;
    let mut other = negotiation_delivery(0, 92)?;
    other.body = String::from_utf8(other.body)?
        .replace("negotiations/SyntheticCall1", "negotiations/SyntheticCall9")
        .into_bytes();
    assert_eq!(lab.fence.observe_delivery(&other), Observed::Nothing);
    assert_eq!(
        reason(&lab, case_write(MEDIA_ANSWER, |_| ())?).await,
        Some(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    // nor does a delivery off the callee's own call agent
    let mut elsewhere = negotiation_delivery(0, 93)?;
    elsewhere.path = elsewhere.path.replace("callAgent/", "callAgent/x");
    assert_eq!(lab.fence.observe_delivery(&elsewhere), Observed::Nothing);
    Ok(())
}

// --- the join of a running group call ---

#[tokio::test]
async fn a_group_call_s_invitation_binds_its_conversation_to_its_chat_for_a_join() -> TestResult {
    let lab_only = Members::of(&[LAB_A, LAB_B, LAB_C]);
    // no invitation bound the conversation to the chat: no join
    let lab = lab_b()?;
    assert_eq!(
        lab.fence
            .check(case_write(ROSTER_JOIN, |_| ())?, &lab_only)
            .await
            .err()
            .map(|denied| *denied.reason()),
        Some(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    let (delivery, gp) = invitation(1, 94)?;
    lab.fence.observe_delivery(&delivery);
    let permit = lab.fence.check(case_write(ROSTER_JOIN, |_| ())?, &lab_only).await?;
    assert_eq!(permit.target().kind(), WriteKind::Call);
    assert_eq!(lab_only.reads(), 1, "the join reads the chat fresh");
    // whatever the ring did since: the binding stays, even once the ring ended (the live join comes after it)
    let (url, body, ended) = group_ring_end(&delivery, &gp, 96)?;
    lab.fence.check(attach_target(&url, &body)?, &lab_only).await?;
    assert_eq!(lab.fence.observe_delivery(&ended), Observed::RingEnded);
    lab.fence.check(case_write(ROSTER_JOIN, |_| ())?, &lab_only).await?;
    let cases: [(&str, Members, BodyEditOnce, DenyReason); 4] = [
        (
            "an outsider in the chat",
            Members::of(&[LAB_A, LAB_B, COWORKER]),
            Box::new(|_| ()),
            DenyReason::Outsiders { count: 1 },
        ),
        (
            "another chat",
            Members::of(&[LAB_A, LAB_B, LAB_C]),
            Box::new(|body| body["groupChat"]["threadId"] = json!("19:fedcba9876543210fedcba9876543210@thread.v2")),
            DenyReason::Partial(PartialReason::UnregisteredCall),
        ),
        (
            "from a",
            Members::of(&[LAB_A, LAB_B, LAB_C]),
            Box::new(|body| body["participants"]["from"]["id"] = json!(LAB_A)),
            DenyReason::CallMismatch,
        ),
        (
            "a callback on another endpoint",
            Members::of(&[LAB_A, LAB_B, LAB_C]),
            Box::new(|body| {
                let link = body["conversationRequest"]["links"]["conversationEnd"]
                    .as_str()
                    .unwrap_or_default()
                    .replace("SyntheticEndpoint00001", "SyntheticEndpoint00009");
                body["conversationRequest"]["links"]["conversationEnd"] = Value::from(link);
            }),
            DenyReason::CallMismatch,
        ),
    ];
    for (name, members, edit, expected) in cases {
        let denied = lab.fence.check(case_write(ROSTER_JOIN, edit)?, &members).await;
        assert_eq!(denied.err().map(|denied| *denied.reason()), Some(expected), "{name}");
    }
    Ok(())
}

#[tokio::test]
async fn a_join_with_media_pends_and_registers_its_call_as_a_start_does() -> TestResult {
    let lab = lab_b()?;
    let (delivery, gp) = invitation(1, 95)?;
    lab.fence.observe_delivery(&delivery);
    // the join comes once the joiner's ring ended
    let (url, attach, ended) = group_ring_end(&delivery, &gp, 97)?;
    lab.fence
        .check(attach_target(&url, &attach)?, &Members::of(&[LAB_A, LAB_B, LAB_C]))
        .await?;
    assert_eq!(lab.fence.observe_delivery(&ended), Observed::RingEnded);
    let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS))?;
    let body = fixture["cases"][MEDIA_JOIN]["body"].to_string();
    let permit = lab
        .fence
        .check(case_write(MEDIA_JOIN, |_| ())?, &Members::of(&[LAB_A, LAB_B, LAB_C]))
        .await?;
    let pending = lab
        .fence
        .pend_start(&permit, body.as_bytes())
        .ok_or("the media join pends")?;
    // its answer: the conversation, as a start's
    let conversation = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticConversation2";
    let answer = json!({
        "conversationController": format!("{conversation}?i=10-0-0-2&e=1"),
        "links": {"updateEndpointMetadata": format!("{conversation}/updateEndpointMetadata?i=10-0-0-2&e=1")},
    });
    lab.fence.register_start(pending, answer.to_string().as_bytes())?;
    // its call's operations pass, sent by the one who joined
    let metadata = json!({"participants": {"from": {"id": LAB_B}}, "endpointMetadata": {"holographicCapabilities": 3}})
        .to_string();
    let url = format!("{conversation}/updateEndpointMetadata?i=10-0-0-2&e=1");
    let Contact::Write(target) = contacting(
        &Request::new("PUT", &url).with_body(Some(metadata.as_bytes())),
        &Policy::new(),
    ) else {
        return Err("the metadata isn't a write".into());
    };
    lab.fence.check(target, &FailClosed).await?;
    // a join without media starts nothing to pend
    let roster = fixture["cases"][ROSTER_JOIN]["body"].to_string();
    let roster_permit = lab
        .fence
        .check(case_write(ROSTER_JOIN, |_| ())?, &Members::of(&[LAB_A, LAB_B, LAB_C]))
        .await?;
    assert!(lab.fence.pend_start(&roster_permit, roster.as_bytes()).is_none());
    Ok(())
}

// --- the lab's call rule: an answer, a decline and a join are judged; the rest of a call passes ---

/// b's fence under the lab's own call rule (no `tracking_calls`), in a lab of a, b and c with the echo service.
fn lab_rule_b() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("fence.log");
    let roster = Roster::new([Mri::new(LAB_A), Mri::new(LAB_B), Mri::new(LAB_C)], [Mri::new(ECHO)]);
    let fence = Fence::new(roster, FenceLog::open(&log)?).with_own_account(Mri::new(LAB_B));
    assert!(!fence.tracks_calls());
    Ok(Lab { _dir: dir, log, fence })
}

#[tokio::test]
async fn under_the_lab_rule_the_ring_time_sends_and_the_answered_call_s_pass_with_nothing_bound() -> TestResult {
    let lab = lab_rule_b()?;
    // nothing offered, attached or bound: the attach, the endpoint state and the receipt pass all the same
    let (url, body) = attach_case()?;
    lab.fence.check(attach_target(&url, &body)?, &FailClosed).await?;
    lab.fence.check(case_write(STATE, |_| ())?, &FailClosed).await?;
    assert!(matches!(
        respond(&lab, &receipt(51), &FailClosed).await,
        DeliveryDecision::Permitted(_)
    ));
    let log = std::fs::read_to_string(&lab.log)?;
    assert!(log.contains(r#""kind":"call_ringing""#), "{log}");
    Ok(())
}

#[tokio::test]
async fn under_the_lab_rule_only_an_offered_invitation_is_answered_or_declined() -> TestResult {
    let unregistered = Some(DenyReason::Partial(PartialReason::UnregisteredCall));
    // no invitation offered: no answer, no decline
    let lab = lab_rule_b()?;
    assert_eq!(reason(&lab, case_write(ACCEPT, |_| ())?).await, unregistered);
    assert_eq!(reason(&lab, case_write(REJECT, |_| ())?).await, unregistered);
    // an outsider's invitation is ignored, never offered: answering it is denied too
    let (delivery, gp) = invitation(0, 61)?;
    let mut outsider = gp.clone();
    outsider["callNotification"]["from"]["id"] = json!(COWORKER);
    let outsider = invitation_delivery(&delivery.path, 61, &outsider);
    assert_eq!(lab.fence.observe_delivery(&outsider), Observed::Ignored);
    assert_eq!(reason(&lab, case_write(ACCEPT, |_| ())?).await, unregistered);
    assert_eq!(reason(&lab, case_write(REJECT, |_| ())?).await, unregistered);
    // a lab caller's: answered with no attach answer bound (the leg its link names ties it to the invitation)
    assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Offered);
    let from_a = case_write(ACCEPT, |body| body["callAcceptance"]["acceptedBy"]["id"] = json!(LAB_A))?;
    assert_eq!(reason(&lab, from_a).await, Some(DenyReason::CallMismatch));
    let permit = lab.fence.check(case_write(ACCEPT, |_| ())?, &FailClosed).await?;
    assert_eq!(permit.target().kind(), WriteKind::CallAnswer);
    // the same accept again (the browser's second layer), never a decline once answered
    lab.fence.check(case_write(ACCEPT, |_| ())?, &FailClosed).await?;
    assert_eq!(
        reason(&lab, case_write(REJECT, |_| ())?).await,
        Some(DenyReason::Partial(PartialReason::CallEnded))
    );
    // on another fence the same invitation is declined, and then only declined again
    let other = lab_rule_b()?;
    other.fence.observe_delivery(&delivery);
    let permit = other.fence.check(case_write(REJECT, |_| ())?, &FailClosed).await?;
    assert_eq!(permit.target().kind(), WriteKind::CallDecline);
    other.fence.check(case_write(REJECT, |_| ())?, &FailClosed).await?;
    assert_eq!(
        reason(&other, case_write(ACCEPT, |_| ())?).await,
        Some(DenyReason::Partial(PartialReason::CallEnded))
    );
    let log = std::fs::read_to_string(&lab.log)?;
    assert!(log.contains(r#""kind":"call_answer""#), "{log}");
    Ok(())
}

#[tokio::test]
async fn under_the_lab_rule_a_group_invitation_is_answered_only_while_its_chat_reads_lab_only() -> TestResult {
    let lab = lab_rule_b()?;
    let (delivery, gp) = invitation(1, 62)?;
    // the 1:1 case's accept, sent to the group invitation's leg
    let leg = gp["callNotification"]["links"]["attach"]
        .as_str()
        .ok_or("attach")?
        .to_owned();
    let accept = |url: String| -> TestResult<Target> {
        let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS))?;
        let body = fixture["cases"][ACCEPT]["body"].to_string();
        match contacting(
            &Request::new("POST", &url).with_body(Some(body.as_bytes())),
            &Policy::new(),
        ) {
            Contact::Write(target) => Ok(target),
            other => Err(format!("the accept is {other:?}").into()),
        }
    };
    let url = leg.replace("/forked/", "/incoming/").replace("/attach", "/accept");
    let url = url.replacen("/i1/", "/t/", 1);
    assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Offered);
    let outsider = Members::of(&[LAB_A, LAB_B, COWORKER]);
    let denied = lab.fence.check(accept(url.clone())?, &outsider).await;
    assert_eq!(
        denied.err().map(|denied| *denied.reason()),
        Some(DenyReason::Outsiders { count: 1 })
    );
    let lab_only = Members::of(&[LAB_A, LAB_B, LAB_C]);
    lab.fence.check(accept(url)?, &lab_only).await?;
    assert_eq!(lab_only.reads(), 1, "the answer reads the chat fresh");
    Ok(())
}

#[tokio::test]
async fn under_the_lab_rule_a_join_is_judged_by_the_chat_it_names() -> TestResult {
    let lab = lab_rule_b()?;
    // no invitation binds the conversation to the chat: the chat the join names is what it is judged by
    let lab_only = Members::of(&[LAB_A, LAB_B, LAB_C]);
    let permit = lab.fence.check(case_write(ROSTER_JOIN, |_| ())?, &lab_only).await?;
    assert_eq!(permit.target().kind(), WriteKind::Call);
    lab.fence.check(case_write(MEDIA_JOIN, |_| ())?, &lab_only).await?;
    assert_eq!(lab_only.reads(), 2, "each join reads the chat fresh");
    // the caller's own join of its call's chat passes too
    let from_a = case_write(ROSTER_JOIN, |body| body["participants"]["from"]["id"] = json!(LAB_A))?;
    lab.fence.check(from_a, &lab_only).await?;
    let cases: [(&str, Members, DenyReason); 3] = [
        (
            "an outsider in the chat",
            Members::of(&[LAB_A, LAB_B, COWORKER]),
            DenyReason::Outsiders { count: 1 },
        ),
        (
            "a chat that can't be read in full",
            Members::new(Membership::Partial(PartialReason::NoSource)),
            DenyReason::Partial(PartialReason::NoSource),
        ),
        (
            "a chat that doesn't list the joiner",
            Members::of(&[LAB_A, LAB_C]),
            DenyReason::CallMismatch,
        ),
    ];
    for (name, members, expected) in cases {
        let denied = lab.fence.check(case_write(ROSTER_JOIN, |_| ())?, &members).await;
        assert_eq!(denied.err().map(|denied| *denied.reason()), Some(expected), "{name}");
    }
    Ok(())
}

/// A meeting's chat (made up), which [`Members`] answers for as for the group chat.
const MEETING_CHAT: &str = "19:meeting_SyntheticMeeting0001@thread.v2";

/// The group call's join (`MEDIA_JOIN`) as a meeting's entry from its pre-join: the meeting's chat, its organizer and
/// tenant, nobody to ring, nothing shared.
fn meeting_entry() -> TestResult<Target> {
    case_write(MEDIA_JOIN, |body| {
        body["groupChat"]["threadId"] = json!(MEETING_CHAT);
        body["meetingInfo"] = json!({"organizerId": GUID_A, "tenantId": GUID_B});
        body["participants"]["to"] = json!([]);
        body["contentSharing"] = Value::Null;
    })
}

#[tokio::test]
async fn under_the_lab_rule_a_meeting_s_entry_is_judged_by_the_meeting_s_chat() -> TestResult {
    let lab = lab_rule_b()?;
    let lab_only = Members::of(&[LAB_A, LAB_B]);
    let permit = lab.fence.check(meeting_entry()?, &lab_only).await?;
    assert_eq!(permit.target().kind(), WriteKind::Call);
    assert_eq!(lab_only.reads(), 1, "the meeting's chat is read fresh");
    let cases: [(&str, Members, DenyReason); 3] = [
        (
            "an outsider invited",
            Members::of(&[LAB_A, LAB_B, COWORKER]),
            DenyReason::Outsiders { count: 1 },
        ),
        (
            "a chat that can't be read in full",
            Members::new(Membership::Partial(PartialReason::NoSource)),
            DenyReason::Partial(PartialReason::NoSource),
        ),
        (
            "a chat that doesn't list the joiner",
            Members::of(&[LAB_A, LAB_C]),
            DenyReason::CallMismatch,
        ),
    ];
    for (name, members, expected) in cases {
        let denied = lab.fence.check(meeting_entry()?, &members).await;
        assert_eq!(denied.err().map(|denied| *denied.reason()), Some(expected), "{name}");
    }
    Ok(())
}

/// b's Admit (`admit`) or Deny (`removeParticipant`) of `who` waiting in a meeting's lobby, as a write.
fn lobby_decision(segment: &str, who: &str) -> TestResult<Target> {
    let url = format!(
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticMeeting1/{segment}?i=10-0-0-1&e=1"
    );
    let body = json!({
        "participants": {"from": {"id": LAB_B, "displayName": "Lab B"}, "to": [{"id": who}]},
        "links": {},
    })
    .to_string();
    match contacting(
        &Request::new("POST", &url).with_body(Some(body.as_bytes())),
        &Policy::new(),
    ) {
        Contact::Write(target) => Ok(target),
        other => Err(format!("{segment} is {other:?}").into()),
    }
}

#[tokio::test]
async fn under_the_lab_rule_a_lobby_s_admit_and_deny_reach_only_lab_accounts() -> TestResult {
    let lab = lab_rule_b()?;
    for segment in ["admit", "removeParticipant"] {
        // nobody's members are read: the people it names are what it is judged by
        lab.fence.check(lobby_decision(segment, LAB_A)?, &FailClosed).await?;
        assert_eq!(
            reason(&lab, lobby_decision(segment, COWORKER)?).await,
            Some(DenyReason::Outsiders { count: 1 }),
            "{segment}"
        );
    }
    Ok(())
}

// --- a contact (`lab.toml`'s `contacts`): its calls are offered, answered and joined like a lab account's ---

const GUEST: &str = "8:orgid:00000000-0000-4000-8000-0000000000f0";

/// [`lab_rule_b`] with [`GUEST`] (the user's own account) as the lab's contact.
fn lab_rule_b_with_contact() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("fence.log");
    let roster = Roster::new([Mri::new(LAB_A), Mri::new(LAB_B), Mri::new(LAB_C)], [Mri::new(ECHO)])
        .with_contacts([Mri::new(GUEST)]);
    let fence = Fence::new(roster, FenceLog::open(&log)?).with_own_account(Mri::new(LAB_B));
    Ok(Lab { _dir: dir, log, fence })
}

#[tokio::test]
async fn the_contact_s_invitation_is_offered_and_answered_and_an_outsider_s_still_isn_t() -> TestResult {
    let lab = lab_rule_b_with_contact()?;
    let (delivery, gp) = invitation(0, 71)?;
    let from = |mri: &str| {
        let mut other = gp.clone();
        other["callNotification"]["from"]["id"] = json!(mri);
        invitation_delivery(&delivery.path, 71, &other)
    };
    assert_eq!(lab.fence.observe_delivery(&from(COWORKER)), Observed::Ignored);
    assert_eq!(
        reason(&lab, case_write(ACCEPT, |_| ())?).await,
        Some(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    assert_eq!(lab.fence.observe_delivery(&from(GUEST)), Observed::Offered);
    let permit = lab.fence.check(case_write(ACCEPT, |_| ())?, &FailClosed).await?;
    assert_eq!(permit.target().kind(), WriteKind::CallAnswer);
    // an invitation to the contact isn't the lab's to offer: the lab answers only its own calls
    let to_guest = {
        let mut other = gp.clone();
        other["callNotification"]["to"]["id"] = json!(GUEST);
        invitation_delivery(&delivery.path, 72, &other)
    };
    assert_eq!(lab.fence.observe_delivery(&to_guest), Observed::Ignored);
    Ok(())
}

#[tokio::test]
async fn a_group_call_with_the_contact_is_answered_and_joined_but_never_with_an_outsider() -> TestResult {
    let with_guest = Members::of(&[LAB_A, LAB_B, LAB_C, GUEST]);
    let with_outsider = Members::of(&[LAB_A, LAB_B, GUEST, COWORKER]);
    let lab = lab_rule_b_with_contact()?;
    // the join of a running call in a chat of lab accounts plus the contact
    lab.fence.check(case_write(ROSTER_JOIN, |_| ())?, &with_guest).await?;
    lab.fence.check(case_write(MEDIA_JOIN, |_| ())?, &with_guest).await?;
    let denied = lab.fence.check(case_write(ROSTER_JOIN, |_| ())?, &with_outsider).await;
    assert_eq!(
        denied.err().map(|denied| *denied.reason()),
        Some(DenyReason::Outsiders { count: 1 })
    );
    // a join sent as an outsider is denied
    let as_outsider = case_write(ROSTER_JOIN, |body| body["participants"]["from"]["id"] = json!(COWORKER))?;
    let denied = lab.fence.check(as_outsider, &with_guest).await;
    assert_eq!(
        denied.err().map(|denied| *denied.reason()),
        Some(DenyReason::Outsiders { count: 1 })
    );
    // the contact's group call, answered while its chat reads lab accounts plus the contact
    let (delivery, mut gp) = invitation(1, 73)?;
    gp["callNotification"]["from"]["id"] = json!(GUEST);
    let delivery = invitation_delivery(&delivery.path, 73, &gp);
    assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Offered);
    let leg = gp["callNotification"]["links"]["attach"]
        .as_str()
        .ok_or("attach")?
        .replace("/forked/", "/incoming/")
        .replace("/attach", "/accept")
        .replacen("/i1/", "/t/", 1);
    let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS))?;
    let body = fixture["cases"][ACCEPT]["body"].to_string();
    let accept = || match contacting(
        &Request::new("POST", &leg).with_body(Some(body.as_bytes())),
        &Policy::new(),
    ) {
        Contact::Write(target) => Ok(target),
        other => Err(format!("the accept is {other:?}")),
    };
    let denied = lab.fence.check(accept()?, &with_outsider).await;
    assert_eq!(
        denied.err().map(|denied| *denied.reason()),
        Some(DenyReason::Outsiders { count: 1 })
    );
    lab.fence.check(accept()?, &with_guest).await?;
    Ok(())
}

// --- the personal build's rule ---

/// b's personal fence (its own account b): it offers any invitation to b, one at a time, and denies nothing.
fn personal_b() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("fence.log");
    let fence = Fence::personal(Mri::new(LAB_B), FenceLog::open(&log)?);
    Ok(Lab { _dir: dir, log, fence })
}

/// The 1:1 invitation to b, its caller a colleague outside the lab.
fn coworker_invitation(id: i64) -> TestResult<(Delivery, Value)> {
    let (delivery, gp) = invitation(0, id)?;
    let mut from_coworker = gp;
    *from_coworker.pointer_mut("/callNotification/from/id").ok_or("from")? = Value::from(COWORKER);
    Ok((invitation_delivery(&delivery.path, id, &from_coworker), from_coworker))
}

#[tokio::test]
async fn the_personal_fence_offers_any_invitation_to_the_account_and_its_receipt_reads_no_chat() -> TestResult {
    let lab = personal_b()?;
    let (delivery, gp) = coworker_invitation(90)?;
    assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Offered);
    // the lab's fence ignores the same invitation
    assert_eq!(lab_b()?.fence.observe_delivery(&delivery), Observed::Ignored);
    let unread = Members::new(Membership::Partial(PartialReason::NoSource));
    let decision = respond(&lab, &receipt(90), &unread).await;
    assert!(matches!(decision, DeliveryDecision::Permitted(ref permit) if permit.delivery() == 90));
    assert_eq!(unread.reads(), 0, "whoever calls, no chat is read for it");
    // the callee's own ring-time send passes on it
    let (url, body) = attach_case()?;
    lab.fence.check(attach_target(&url, &body)?, &unread).await?;
    // an invitation addressed to someone else, or from the account itself, is ignored
    for (pointer, mri) in [("/callNotification/to/id", LAB_A), ("/callNotification/from/id", LAB_B)] {
        let mut other = gp.clone();
        *other.pointer_mut(pointer).ok_or(pointer)? = Value::from(mri);
        let other = invitation_delivery(&delivery.path, 91, &other);
        assert_eq!(lab.fence.observe_delivery(&other), Observed::Ignored, "{pointer}");
    }
    Ok(())
}

#[tokio::test]
async fn the_personal_fence_offers_one_ringing_invitation_at_a_time() -> TestResult {
    let lab = personal_b()?;
    let (first, gp) = coworker_invitation(92)?;
    assert_eq!(lab.fence.observe_delivery(&first), Observed::Offered);
    let (url, body) = attach_case()?;
    lab.fence.check(attach_target(&url, &body)?, &FailClosed).await?;
    // while it rings, another invitation (the group call's) isn't offered (the personal fence would let a response go,
    // but the call machine is never handed an invitation not offered)
    let (second, _) = invitation(1, 93)?;
    assert_eq!(lab.fence.observe_delivery(&second), Observed::Ignored);
    // the ringing one's own redelivery still is
    let again = invitation_delivery(&first.path, 94, &gp);
    assert_eq!(lab.fence.observe_delivery(&again), Observed::Offered);
    // its ring ends at its attach's `call/end` callback: the next invitation is offered
    let end = body["attach"]["links"]["end"].as_str().ok_or("end")?;
    let ended = Delivery {
        path: url::Url::parse(end)?.path().to_owned(),
        id: 95,
        body: b"{}".to_vec(),
        ..first.clone()
    };
    assert_eq!(lab.fence.observe_delivery(&ended), Observed::RingEnded);
    let (third, _) = invitation(1, 96)?;
    assert_eq!(lab.fence.observe_delivery(&third), Observed::Offered);
    Ok(())
}

#[tokio::test]
async fn an_attach_sent_again_after_one_its_service_refused_passes_and_its_invitation_is_answered() -> TestResult {
    // the personal fence and the lab's: an attach answered 401 or 503 binds nothing and retires nothing (the transport
    // gives its pending answer up), so the attempt after it goes on the same invitation, which is then answered
    for lab in [personal_b()?, lab_rule_b()?] {
        let (delivery, _) = invitation(0, 97)?;
        assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Offered);
        let (url, body) = attach_case()?;
        let refused = lab.fence.check(attach_target(&url, &body)?, &FailClosed).await?;
        let _refused = lab.fence.pend_answer(&refused).ok_or("the attach pends")?;
        let permit = lab.fence.check(attach_target(&url, &body)?, &FailClosed).await?;
        let pending = lab.fence.pend_answer(&permit).ok_or("the attach sent again pends")?;
        let (_, _, answer) = progress_case()?;
        lab.fence.bind_answer(pending, answer.to_string().as_bytes())?;
        lab.fence.check(case_write(STATE, |_| ())?, &FailClosed).await?;
        let accept = lab.fence.check(case_write(ACCEPT, |_| ())?, &FailClosed).await?;
        assert_eq!(accept.target().kind(), WriteKind::CallAnswer);
    }
    Ok(())
}

#[tokio::test]
async fn an_invitation_the_call_machine_let_go_is_retired_and_the_next_one_is_offered() -> TestResult {
    let lab = personal_b()?;
    let (first, gp) = coworker_invitation(97)?;
    assert_eq!(lab.fence.observe_delivery(&first), Observed::Offered);
    let (url, body) = attach_case()?;
    // its attach permitted: it rings with its end known, and no other invitation is offered meanwhile
    lab.fence.check(attach_target(&url, &body)?, &FailClosed).await?;
    let (next, _) = invitation(1, 98)?;
    assert_eq!(lab.fence.observe_delivery(&next), Observed::Ignored);
    // the attach failed for good (not sent for a credential failure, no answer, a refusal no attempt follows): the
    // call machine lets the invitation go, and the service may never have taken the callbacks its ring would end at
    lab.fence.let_go(&url);
    // it isn't offered again (the personal fence denies nothing: an attach on it would pass, and changes nothing)
    assert_eq!(reason(&lab, attach_target(&url, &body)?).await, None);
    let again = invitation_delivery(&first.path, 99, &gp);
    assert_eq!(lab.fence.observe_delivery(&again), Observed::Ignored);
    // and it holds no call open: the next invitation is offered
    let (next, _) = invitation(1, 100)?;
    assert_eq!(lab.fence.observe_delivery(&next), Observed::Offered);
    // an invitation answered isn't let go: its call goes on
    let answered = answered().await?;
    answered.fence.let_go(&url);
    answered.fence.check(case_write(METADATA, |_| ())?, &FailClosed).await?;
    Ok(())
}

/// The fixture `text` on call leg `SyntheticCall<leg>` instead of the invitation fixture's (`SyntheticCall1`): a
/// re-invitation to the same conversation, on a new leg.
fn on_leg(text: &str, leg: u8) -> String {
    text.replace("SyntheticCall1/", &format!("SyntheticCall{leg}/"))
}

/// b, invited to the 1:1 conversation on call leg `leg`, attached and its attach's answer bound.
async fn bound_on_leg(lab: &Lab, leg: u8, id: i64) -> TestResult {
    let (delivery, gp) = invitation(0, id)?;
    let gp: Value = serde_json::from_str(&on_leg(&gp.to_string(), leg))?;
    let delivery = invitation_delivery(&delivery.path, id, &gp);
    assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Offered, "leg {leg}");
    let (url, body) = attach_case()?;
    let permit = lab
        .fence
        .check(attach_target(&on_leg(&url, leg), &body)?, &FailClosed)
        .await?;
    let (_, _, answer) = progress_case()?;
    let pending = lab.fence.pend_answer(&permit).ok_or("pending")?;
    lab.fence
        .bind_answer(pending, on_leg(&answer.to_string(), leg).as_bytes())?;
    Ok(())
}

/// The follow-ups fixture's case `index` (as [`case_write`]) on call leg `leg`.
fn case_on_leg(index: usize, leg: u8) -> TestResult<Target> {
    let fixture: Value = serde_json::from_str(&on_leg(&fill(CHAT_FOLLOW_UPS), leg))?;
    let case = &fixture["cases"][index];
    let body = (!case["body"].is_null()).then(|| case["body"].to_string());
    let (method, url) = (
        case["method"].as_str().ok_or("method")?,
        case["url"].as_str().ok_or("url")?,
    );
    match contacting(
        &Request::new(method, url).with_body(body.as_deref().map(str::as_bytes)),
        &Policy::new(),
    ) {
        Contact::Write(target) => Ok(target),
        other => Err(format!("case {index} on leg {leg} is {other:?}").into()),
    }
}

#[tokio::test]
async fn after_a_decline_a_re_invitation_to_the_same_conversation_is_answered() -> TestResult {
    let lab = personal_b()?;
    bound_on_leg(&lab, 1, 100).await?;
    lab.fence.check(case_on_leg(REJECT, 1)?, &FailClosed).await?;
    // the declined invitation keeps the conversation's links only as a tombstone (the personal fence denies nothing)
    assert_eq!(reason(&lab, case_on_leg(STATE, 1)?).await, None);
    // a re-invitation to the same conversation on a new leg: its attach's answer binds the same conversation links
    bound_on_leg(&lab, 2, 101).await?;
    lab.fence.check(case_on_leg(STATE, 2)?, &FailClosed).await?;
    let permit = lab.fence.check(case_on_leg(ACCEPT, 2)?, &FailClosed).await?;
    assert_eq!(permit.target().kind(), WriteKind::CallAnswer);
    // the declined leg stays declined; an answer or a decline on it is logged and passes, moving nothing
    assert_eq!(reason(&lab, case_on_leg(ACCEPT, 1)?).await, None);
    lab.fence.check(case_on_leg(REJECT, 1)?, &FailClosed).await?;
    // and the answered call's own operations go on the conversation's links
    lab.fence.check(case_on_leg(STATE, 2)?, &FailClosed).await?;
    Ok(())
}

/// The fixture `text` in the group call's conversation (`SyntheticConversation2`), on its call leg
/// `SyntheticCall<leg>`: the 1:1 case's sends, made in the group call.
fn in_group_call(text: &str, leg: u8) -> String {
    let leg = format!("SyntheticCall{leg}/");
    text.replace("SyntheticConversation1", "SyntheticConversation2")
        .replace("SyntheticCall1/", &leg)
        .replace("SyntheticCall2/", &leg)
}

/// The follow-ups fixture's case `index` (as [`case_write`]) in the group call, on call leg `leg`.
fn group_case(index: usize, leg: u8) -> TestResult<Target> {
    let fixture: Value = serde_json::from_str(&in_group_call(&fill(CHAT_FOLLOW_UPS), leg))?;
    let case = &fixture["cases"][index];
    let body = (!case["body"].is_null()).then(|| case["body"].to_string());
    let (method, url) = (
        case["method"].as_str().ok_or("method")?,
        case["url"].as_str().ok_or("url")?,
    );
    match contacting(
        &Request::new(method, url).with_body(body.as_deref().map(str::as_bytes)),
        &Policy::new(),
    ) {
        Contact::Write(target) => Ok(target),
        other => Err(format!("case {index} in the group call is {other:?}").into()),
    }
}

/// b, re-invited to the group call on call leg `leg` (delivery `id`), attaches and answers: each answer bound.
async fn group_call_answered(lab: &Lab, members: &Members, leg: u8, id: i64) -> TestResult {
    let (delivery, gp) = invitation(1, id)?;
    let gp: Value = serde_json::from_str(&in_group_call(&gp.to_string(), leg))?;
    let delivery = invitation_delivery(&delivery.path, id, &gp);
    assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Offered, "leg {leg}");
    let (url, body) = group_attach(&gp)?;
    let permit = lab.fence.check(attach_target(&url, &body)?, members).await?;
    let fixture: Value = serde_json::from_str(&in_group_call(&fill(CHAT_FOLLOW_UPS), leg))?;
    let pending = lab.fence.pend_answer(&permit).ok_or("the attach pends")?;
    lab.fence
        .bind_answer(pending, fixture["attach_answer"].to_string().as_bytes())?;
    lab.fence.check(group_case(STATE, leg)?, members).await?;
    let permit = lab.fence.check(group_case(ACCEPT, leg)?, members).await?;
    let pending = lab.fence.pend_answer(&permit).ok_or("the accept pends")?;
    lab.fence
        .bind_answer(pending, fixture["accept_answer"].to_string().as_bytes())?;
    Ok(())
}

#[tokio::test]
async fn after_leaving_a_group_call_a_re_invitation_to_it_is_answered_and_its_call_goes_on() -> TestResult {
    let lab = personal_b()?;
    // never read: the personal fence reads no members
    let members = Members::new(Membership::Partial(PartialReason::NoSource));
    // b joins the running group call once its ring ended: its join registers the call on the conversation's links
    let (delivery, gp) = invitation(1, 110)?;
    assert_eq!(lab.fence.observe_delivery(&delivery), Observed::Offered);
    let (url, attach, ring_end) = group_ring_end(&delivery, &gp, 111)?;
    lab.fence.check(attach_target(&url, &attach)?, &members).await?;
    assert_eq!(lab.fence.observe_delivery(&ring_end), Observed::RingEnded);
    let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS))?;
    let join = fixture["cases"][MEDIA_JOIN]["body"].to_string();
    let permit = lab.fence.check(case_write(MEDIA_JOIN, |_| ())?, &members).await?;
    let pending = lab
        .fence
        .pend_start(&permit, join.as_bytes())
        .ok_or("the media join pends")?;
    let conversation = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticConversation2";
    let answer = json!({
        "conversationController": format!("{conversation}?i=10-0-0-2&e=1"),
        "links": {
            "updateEndpointMetadata": format!("{conversation}/updateEndpointMetadata?i=10-0-0-2&e=1"),
            "leave": format!("{conversation}/leave?i=10-0-0-2&e=1"),
        },
    });
    lab.fence.register_start(pending, answer.to_string().as_bytes())?;
    lab.fence.check(group_case(METADATA, 2)?, &members).await?;
    // b leaves while the others stay: the call it left keeps the conversation's links as a tombstone
    lab.fence.check(group_case(LEAVE, 2)?, &members).await?;
    assert_eq!(reason_with(&lab, group_case(METADATA, 2)?, &members).await, None);
    // re-invited to the same conversation on a new leg, b answers: the conversation's links act on the answered call
    group_call_answered(&lab, &members, 3, 112).await?;
    for index in [METADATA, PROPERTIES, MEDIA_LINES, RENEGOTIATE] {
        lab.fence.check(group_case(index, 3)?, &members).await?;
    }
    // b leaves it too, and leaves again; whatever follows is logged and passes
    lab.fence.check(group_case(LEAVE, 3)?, &members).await?;
    lab.fence.check(group_case(LEAVE, 3)?, &members).await?;
    assert_eq!(reason_with(&lab, group_case(METADATA, 3)?, &members).await, None);
    assert_eq!(members.reads(), 0);
    Ok(())
}

async fn reason_with(lab: &Lab, target: Target, members: &Members) -> Option<DenyReason> {
    lab.fence
        .check(target, members)
        .await
        .err()
        .map(|denied| *denied.reason())
}

// --- the call floor: the accounts of one app, one call at a time ---

/// `lab`'s fence sharing `floor` with the other accounts of its app.
fn on_floor(lab: Lab, floor: &CallFloor) -> Lab {
    Lab {
        fence: lab.fence.sharing_calls(floor),
        ..lab
    }
}

/// Where a's test call's callbacks go: its endpoint's call agent.
const A_CALLBACKS: &str =
    "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/callAgent/agent-a";
const A_CONVERSATION_END: &str =
    "/v4/f/SyntheticEndpoint00000/callAgent/agent-a/05050505/conversation/conversationEnd/";

/// a's test call start, shaped like `tests/calls.rs`'s (the echo call's capture).
fn a_start_body() -> Vec<u8> {
    json!({
        "conversationRequest": {
            "conversationType": null,
            "suppressDialout": true,
            "links": {
                "conversationEnd": format!("{A_CALLBACKS}/05050505/conversation/conversationEnd/"),
                "conversationUpdate": format!("{A_CALLBACKS}/06060606/conversation/conversationUpdate/"),
            },
            "scenario": TEST_CALL_SCENARIO,
        },
        "participants": {"from": {"id": LAB_A, "displayName": "Lab A"}, "to": []},
        "groupChat": null,
        "callInvitation": {
            "callModalities": ["Audio"],
            "links": {
                "acceptance": format!("{A_CALLBACKS}/03030303/call/acceptance/"),
                "end": format!("{A_CALLBACKS}/04040404/call/end/"),
            },
            "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
        },
    })
    .to_string()
    .into_bytes()
}

/// a's fence on `floor`, its test call started and registered: a is in a call.
async fn a_in_a_call(floor: &CallFloor) -> TestResult<Lab> {
    let lab = on_floor(lab_of(Some(LAB_A))?, floor);
    let body = a_start_body();
    let start = match contacting(
        &Request::new("POST", CALL_START_URL).with_body(Some(&body)),
        &Policy::new().with_echo_callee(Mri::new(ECHO)),
    ) {
        Contact::Write(target) => target,
        other => return Err(format!("a's start is {other:?}").into()),
    };
    let permit = lab.fence.check(start, &FailClosed).await?;
    let pending = lab.fence.pend_start(&permit, &body).ok_or("a's start pends")?;
    let conversation = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticConversation9";
    let answer = json!({
        "conversationController": format!("{conversation}?i=10-0-0-2&e=1"),
        "links": {"leave": format!("{conversation}/leave?i=10-0-0-2&e=1")},
    });
    lab.fence.register_start(pending, answer.to_string().as_bytes())?;
    Ok(lab)
}

/// a's call ends: the conversation's end delivered at its start's callback.
fn a_s_call_ends(lab: &Lab) {
    let ended = Delivery {
        connection: "session-a/socket-1".to_owned(),
        id: 1,
        path: A_CONVERSATION_END.to_owned(),
        trouter_request: Some(TROUTER_REQUEST.to_owned()),
        chain_id: None,
        message_id: None,
        body: Vec::new(),
    };
    assert_eq!(lab.fence.observe_delivery(&ended), Observed::Ended);
}

/// The 1:1 invitation to a from c, with this delivery id.
fn invitation_to_a(id: i64) -> TestResult<Delivery> {
    let (delivery, mut gp) = invitation(0, id)?;
    *gp.pointer_mut("/callNotification/to/id").ok_or("to")? = Value::from(LAB_A);
    *gp.pointer_mut("/callNotification/from/id").ok_or("from")? = Value::from(LAB_C);
    Ok(invitation_delivery(&delivery.path, id, &gp))
}

/// The test call start of `caller` (a new call of `caller` and the echo service).
fn test_call(caller: &str) -> Target {
    Target::new(WriteKind::Call, Scope::New).with_added([Mri::new(caller), Mri::new(ECHO)])
}

#[tokio::test]
async fn on_a_shared_floor_an_invitation_isn_t_offered_while_another_account_is_in_a_call() -> TestResult {
    let floor = CallFloor::new();
    let a = a_in_a_call(&floor).await?;
    let b = on_floor(lab_b()?, &floor);
    let (delivery, gp) = invitation(0, 120)?;
    assert_eq!(b.fence.observe_delivery(&delivery), Observed::Ignored);
    assert!(matches!(
        respond(&b, &receipt(120), &FailClosed).await,
        DeliveryDecision::NotPending
    ));
    // a fence of its own (no floor) isn't held by a's call
    assert_eq!(lab_b()?.fence.observe_delivery(&delivery), Observed::Offered);
    // once a's call ended, the invitation delivered again is offered
    a_s_call_ends(&a);
    let again = invitation_delivery(&delivery.path, 121, &gp);
    assert_eq!(b.fence.observe_delivery(&again), Observed::Offered);
    Ok(())
}

#[tokio::test]
async fn an_invitation_ringing_or_answered_holds_the_floor_until_it_ends() -> TestResult {
    let (url, body) = attach_case()?;
    // b's invitation only offered (not attached yet) holds it: two accounts never ring at once, and a starts nothing
    let floor = CallFloor::new();
    let b = on_floor(lab_b()?, &floor);
    let a = on_floor(lab_of(Some(LAB_A))?, &floor);
    let (delivery, _) = invitation(0, 130)?;
    assert_eq!(b.fence.observe_delivery(&delivery), Observed::Offered);
    assert_eq!(a.fence.observe_delivery(&invitation_to_a(131)?), Observed::Ignored);
    assert_eq!(
        reason(&a, test_call(LAB_A)).await,
        Some(DenyReason::AnotherAccountInCall)
    );
    // let go before it rang (the call machine dropped it): a's next one is offered
    b.fence.let_go(&url);
    assert_eq!(a.fence.observe_delivery(&invitation_to_a(137)?), Observed::Offered);
    // b's invitation attached (its ring's end known): until its ring ends
    let floor = CallFloor::new();
    let b = on_floor(bound().await?, &floor);
    let a = on_floor(lab_of(Some(LAB_A))?, &floor);
    assert_eq!(a.fence.observe_delivery(&invitation_to_a(132)?), Observed::Ignored);
    let end = body["attach"]["links"]["end"].as_str().ok_or("end")?;
    let ended = Delivery {
        path: url::Url::parse(end)?.path().to_owned(),
        id: 133,
        body: b"{}".to_vec(),
        ..delivery.clone()
    };
    assert_eq!(b.fence.observe_delivery(&ended), Observed::RingEnded);
    assert_eq!(a.fence.observe_delivery(&invitation_to_a(134)?), Observed::Offered);
    // b's invitation answered: until b left its call
    let floor = CallFloor::new();
    let b = on_floor(answered().await?, &floor);
    let a = on_floor(lab_of(Some(LAB_A))?, &floor);
    assert_eq!(a.fence.observe_delivery(&invitation_to_a(135)?), Observed::Ignored);
    b.fence.check(case_write(LEAVE, |_| ())?, &FailClosed).await?;
    assert_eq!(a.fence.observe_delivery(&invitation_to_a(136)?), Observed::Offered);
    Ok(())
}

#[tokio::test]
async fn an_invitation_offered_is_still_taken_when_delivered_again() -> TestResult {
    let floor = CallFloor::new();
    let b = on_floor(lab_b()?, &floor);
    let (delivery, gp) = invitation(0, 140)?;
    assert_eq!(b.fence.observe_delivery(&delivery), Observed::Offered);
    // b's invitation rings: a starts no call meanwhile
    let a = on_floor(lab_of(Some(LAB_A))?, &floor);
    assert_eq!(
        reason(&a, test_call(LAB_A)).await,
        Some(DenyReason::AnotherAccountInCall)
    );
    // and b's invitation delivered again is taken, its receipt with it
    let again = invitation_delivery(&delivery.path, 141, &gp);
    assert_eq!(b.fence.observe_delivery(&again), Observed::Offered);
    assert!(matches!(
        respond(&b, &receipt(141), &FailClosed).await,
        DeliveryDecision::Permitted(_)
    ));
    Ok(())
}

#[tokio::test]
async fn a_start_a_join_or_an_answer_is_denied_while_another_account_on_the_floor_is_busy() -> TestResult {
    // a in a call, and b's invitation offered before b joined the floor (as an account added while it rang)
    let floor = CallFloor::new();
    let a = a_in_a_call(&floor).await?;
    let b = lab_b()?;
    let (delivery, _) = invitation(0, 150)?;
    assert_eq!(b.fence.observe_delivery(&delivery), Observed::Offered);
    let b = on_floor(b, &floor);
    // b's ring-time sends go on: its attach, its answer bound, its endpoint state
    let (url, body) = attach_case()?;
    let permit = b.fence.check(attach_target(&url, &body)?, &FailClosed).await?;
    let pending = b.fence.pend_answer(&permit).ok_or("the attach pends")?;
    let (_, _, answer) = progress_case()?;
    b.fence.bind_answer(pending, answer.to_string().as_bytes())?;
    b.fence.check(case_write(STATE, |_| ())?, &FailClosed).await?;
    // what enters a call doesn't: its answer, a start, a join
    let another = Some(DenyReason::AnotherAccountInCall);
    assert_eq!(reason(&b, case_write(ACCEPT, |_| ())?).await, another);
    assert_eq!(reason(&b, test_call(LAB_B)).await, another);
    assert_eq!(reason(&b, case_write(MEDIA_JOIN, |_| ())?).await, another);
    // and while b's invitation rings, a doesn't start another call either
    assert_eq!(reason(&a, test_call(LAB_A)).await, another);
    // each deny logged, by its reason
    let log = std::fs::read_to_string(&b.log)?;
    let denies: Vec<Value> = log
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Value>, _>>()?
        .into_iter()
        .filter(|entry| entry["reason"] == "another_account_in_call")
        .collect();
    assert_eq!(denies.len(), 3, "{log}");
    assert!(denies.iter().all(|entry| entry["verdict"] == "deny"));
    // a's call ended: b answers
    a_s_call_ends(&a);
    let permit = b.fence.check(case_write(ACCEPT, |_| ())?, &FailClosed).await?;
    assert_eq!(permit.target().kind(), WriteKind::CallAnswer);
    Ok(())
}

#[tokio::test]
async fn a_fence_dropped_leaves_the_floor() -> TestResult {
    let floor = CallFloor::new();
    let a = a_in_a_call(&floor).await?;
    let b = on_floor(lab_b()?, &floor);
    let (delivery, gp) = invitation(0, 160)?;
    assert_eq!(b.fence.observe_delivery(&delivery), Observed::Ignored);
    assert_eq!(
        reason(&b, test_call(LAB_B)).await,
        Some(DenyReason::AnotherAccountInCall)
    );
    // a's account removed (its fence dropped) with its call still live: it no longer holds the floor
    drop(a);
    let again = invitation_delivery(&delivery.path, 161, &gp);
    assert_eq!(b.fence.observe_delivery(&again), Observed::Offered);
    assert_eq!(reason(&b, test_call(LAB_B)).await, None);
    Ok(())
}

#[tokio::test]
async fn a_lab_fence_on_a_floor_judges_its_own_account_s_invitations_as_without_one() -> TestResult {
    // b's invitation attached (its ring's end known): a lab fence offers b the group call's too, on a floor or not
    // (the floor keeps apart the accounts, not one account's own invitations)
    for floored in [false, true] {
        let b = bound().await?;
        let b = if floored { on_floor(b, &CallFloor::new()) } else { b };
        let (group, _) = invitation(1, 170)?;
        assert_eq!(
            b.fence.observe_delivery(&group),
            Observed::Offered,
            "floored: {floored}"
        );
    }
    Ok(())
}
