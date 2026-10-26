//! Offered invitations (`teams_fence`'s `calls::offer`, `docs/protocol/fence-targets.md`, "Offered invitations"): an
//! incoming call is offered only when it is readable, addressed to the fence's own account and called by a lab account.
//! Under the registry's rule (the personal build's, `Fence::tracking_calls` here) only its ring-time sends pass on it
//! (its delivery response, the callee's `attach`), each bound to it, a group call's only while its group chat reads
//! lab-only, and nothing passes once its ring ended. Under the lab's rule (at the end) only its answer and its decline
//! are judged, and a join by the chat it names. Shaped like the `ab-call` and
//! `abc-call` captures (`tests/fixtures/calling/chat/`), with made-up ids.

use std::path::PathBuf;

use base64::Engine as _;
use serde_json::{Value, json};
use teams_fence::{
    CallOperation, Contact, Delivery, DeliveryDecision, DeliveryDenyReason, DenyReason, FailClosed, Fence, FenceLog,
    Membership, MembershipFuture, MembershipSource, Mri, Observed, PartialReason, Policy, Request, Roster, Scope,
    Target, WriteKind, contacting,
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
            Scope::Conversation { id } if id == &format!("19:{GROUP}@thread.v2") => self.0.clone(),
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
