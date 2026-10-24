//! The `ab-call` and `abc-call` scenarios' offline parts: what each run holds back (a's starts in `start-held`, every
//! callee's sends to the calling services and its delivery responses while it rings and once it acted), which forced
//! denies each account's run expects, how held-back requests are counted, and how a callee's capture shows the call's
//! invitation reaching its page. All ids, hosts and bodies here are made up.

use base64::Engine as _;
use proto_lab::fence_http::{Denial, DenyCause, PausedRequest};
use proto_lab::fence_ws::{Frame, Payload};
use proto_lab::scenario::ab_call::{
    ACCEPT_AUDIO_TID, ANSWER_HELD, CALLER_JOIN_HELD, CallStep, CalleeAction, DECLINE_HELD, DECLINE_TID,
    INVITATION_EVENTS, Modality, RING_SENDS, RingCheck, RunCall, START_HELD, callee_forced, callee_rules,
    caller_forced, caller_join_rule, held_back, invitations, own_invitations, ring_check, start_held_rule,
};
use proto_lab::scenario::ab_chat::EARLIER_RUN_PROBE;
use serde_json::json;
use teams_fence::CALL_START_URL;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn request(method: &str, url: &str) -> PausedRequest {
    PausedRequest {
        request_id: "1".to_owned(),
        method: method.to_owned(),
        url: url.to_owned(),
        headers: Vec::new(),
        body: Some(b"{}".to_vec()),
        body_unread: false,
        network_id: None,
    }
}

fn text(url: &str, data: &str) -> Frame {
    Frame {
        url: url.to_owned(),
        payload: Payload::Text(data.to_owned()),
    }
}

const FORKED: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/forked/call-1/27/i1/1/attach?i=10-0-0-1";
const TROUTER: &str = "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c?tc=made-up";

#[test]
fn the_caller_s_join_of_a_running_call_is_held_back_and_nothing_else() -> TestResult {
    let rule = caller_join_rule()?;
    assert_eq!(rule.label(), Some(CALLER_JOIN_HELD));
    let conversation =
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/Synthetic1";
    assert!(rule.matches(&request("POST", conversation)));
    assert!(rule.matches(&request("POST", &format!("{conversation}?i=10-0-0-1&e=1"))));
    for (method, url) in [
        ("POST", format!("{conversation}/leave")),
        ("PUT", format!("{conversation}/updateEndpointMetadata")),
        ("GET", conversation.to_owned()),
        ("POST", CALL_START_URL.to_owned()),
        ("POST", FORKED.to_owned()),
    ] {
        assert!(!rule.matches(&request(method, &url)), "{method} {url}");
    }
    Ok(())
}

#[test]
fn start_held_holds_back_every_call_start_and_nothing_else() -> TestResult {
    let rule = start_held_rule()?;
    assert_eq!(rule.label(), Some(START_HELD));
    assert!(rule.matches(&request("POST", CALL_START_URL)));
    assert!(rule.matches(&request("POST", &format!("{CALL_START_URL}?x=1"))));
    for (method, url) in [
        ("GET", CALL_START_URL.to_owned()),
        ("POST", format!("{CALL_START_URL}/x")),
        ("POST", FORKED.to_owned()),
        ("POST", CALL_START_URL.replace("flightproxy", "flightproxy2")),
    ] {
        assert!(!rule.matches(&request(method, &url)), "{method} {url}");
    }
    Ok(())
}

#[test]
fn a_callee_holds_back_every_write_to_the_calling_services_and_every_delivery_response() -> TestResult {
    let (requests, frames) = callee_rules(RING_SENDS)?;
    assert!(requests.iter().all(|rule| rule.label() == Some(RING_SENDS)));
    assert_eq!(frames.label(), Some(RING_SENDS));
    let denied = |method: &str, url: &str| requests.iter().any(|rule| rule.matches(&request(method, url)));
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        for url in [
            FORKED,
            CALL_START_URL,
            "https://api.fl.teams.microsoft.com/api/v2/ep/x/conv/y/leave",
            "https://conv-zz-01-prod-aks.conv.skype.com/conv/y",
            "https://cc-zz-01-prod-aks.cc.skype.com:443/cc/v1/callParticipant/x/replacement",
        ] {
            assert!(denied(method, url), "{method} {url}");
        }
    }
    for (method, url) in [
        ("GET", FORKED),
        ("OPTIONS", FORKED),
        (
            "POST",
            "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/x/messages",
        ),
        (
            "POST",
            "https://flightproxy.teams.microsoft.com.example.com/api/v2/epconv",
        ),
        ("POST", "https://skype.com.example.com/x"),
    ] {
        assert!(!denied(method, url), "{method} {url}");
    }
    assert!(frames.matches(&text(TROUTER, r#"3:::{"id":-7,"status":200}"#)));
    assert!(frames.matches(&text(
        "wss://go.trouter.teams.microsoft.com:443/v4/c",
        r#"3:::{"id":1,"status":404}"#
    )));
    for frame in [
        text(TROUTER, r#"5:3+::{"name":"ping"}"#),
        text(TROUTER, "2::"),
        text("wss://example.com/v4/c", r#"3:::{"id":1}"#),
    ] {
        assert!(!frames.matches(&frame), "{frame:?}");
    }
    Ok(())
}

#[test]
fn each_account_expects_only_its_step_s_own_forced_denies() {
    assert_eq!(caller_forced(CallStep::StartHeld), [EARLIER_RUN_PROBE, START_HELD]);
    for step in [CallStep::Answer, CallStep::Decline, CallStep::Cancel, CallStep::Timeout] {
        assert_eq!(caller_forced(step), [EARLIER_RUN_PROBE, CALLER_JOIN_HELD], "{step:?}");
    }
    assert_eq!(
        callee_forced(CallStep::Answer),
        [EARLIER_RUN_PROBE, RING_SENDS, ANSWER_HELD]
    );
    assert_eq!(
        callee_forced(CallStep::Decline),
        [EARLIER_RUN_PROBE, RING_SENDS, DECLINE_HELD]
    );
    assert_eq!(callee_forced(CallStep::Cancel), [EARLIER_RUN_PROBE, RING_SENDS]);
    assert_eq!(callee_forced(CallStep::Timeout), [EARLIER_RUN_PROBE, RING_SENDS]);
    assert_eq!(CallStep::default(), CallStep::Answer);
    let names: Vec<&str> = [
        CallStep::StartHeld,
        CallStep::Answer,
        CallStep::Decline,
        CallStep::Cancel,
        CallStep::Timeout,
    ]
    .iter()
    .map(|step| step.as_str())
    .collect();
    assert_eq!(names, ["start-held", "answer", "decline", "cancel", "timeout"]);
}

#[test]
fn a_callee_s_click_is_held_back_under_a_label_its_run_expects() {
    assert_eq!(
        CallStep::Answer.callee_action(),
        Some(CalleeAction {
            label: ANSWER_HELD,
            button: ACCEPT_AUDIO_TID,
            what: "answer"
        })
    );
    assert_eq!(
        CallStep::Decline.callee_action(),
        Some(CalleeAction {
            label: DECLINE_HELD,
            button: DECLINE_TID,
            what: "decline"
        })
    );
    for step in [CallStep::StartHeld, CallStep::Cancel, CallStep::Timeout] {
        assert_eq!(step.callee_action(), None, "{step:?}");
    }
    // the label a callee's click is held back under is one its run expects
    for step in [CallStep::Answer, CallStep::Decline] {
        let action = step.callee_action().map(|action| action.label);
        assert!(
            action.is_some_and(|label| callee_forced(step).contains(&label)),
            "{step:?}"
        );
    }
}

#[test]
fn held_back_requests_count_once_per_network_request_under_their_label() {
    let forced = |label: &str, id: Option<&str>| Denial {
        label: Some(label.to_owned()),
        network_id: id.map(str::to_owned),
        ..Denial::new("POST", CALL_START_URL, DenyCause::Forced)
    };
    let denials = [
        // a request pauses on its target's session and the browser's: one request
        forced(START_HELD, Some("1.1")),
        forced(START_HELD, Some("1.1")),
        forced(START_HELD, Some("1.2")),
        forced(START_HELD, None),
        forced(RING_SENDS, Some("1.3")),
        Denial::new("POST", CALL_START_URL, DenyCause::Unclassified),
    ];
    assert_eq!(held_back(&denials, START_HELD), 3);
    assert_eq!(held_back(&denials, RING_SENDS), 1);
    assert_eq!(held_back(&denials, ANSWER_HELD), 0);
}

/// A capture line: the page received this socket.io message on a Trouter socket.
fn received(message: &serde_json::Value) -> String {
    received_at(message, PLACED + 1_000)
}

/// A capture line recorded at `ts_ms`: the page received this socket.io message on a Trouter socket.
fn received_at(message: &serde_json::Value, ts_ms: u64) -> String {
    json!({
        "type": "cdp",
        "method": "Network.webSocketFrameReceived",
        "params": {"requestId": "1", "response": {"payloadData": format!("3:::{message}")}},
        "seq": 1,
        "ts_ms": ts_ms,
    })
    .to_string()
}

/// When the run placed its call, in the tests (Unix ms).
const PLACED: u64 = 1_800_000_000_000;

fn invitation_body(event: u64, payload: &serde_json::Value) -> String {
    let gp = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
    json!({"evt": event, "gp": gp}).to_string()
}

#[test]
fn a_callee_s_capture_counts_each_call_invitation_its_page_received() {
    let notification = json!({"callNotification": {"from": {"id": "8:orgid:made-up"}, "links": {}}});
    let delivery = |id: i64, body: String| json!({"id": id, "method": "POST", "url": "/v4/f/x/", "body": body});
    let capture = [
        received(&delivery(1, invitation_body(INVITATION_EVENTS[0], &notification))),
        // the service delivers it again while it goes unanswered; a group call's
        received(&delivery(-2, invitation_body(INVITATION_EVENTS[0], &notification))),
        received(&delivery(7, invitation_body(INVITATION_EVENTS[1], &notification))),
        // another event, no call notification, a messaging delivery, an unreadable body
        received(&delivery(3, invitation_body(108, &notification))),
        received(&delivery(
            4,
            invitation_body(INVITATION_EVENTS[0], &json!({"other": {}})),
        )),
        received(&delivery(5, json!({"type": "EventMessage"}).to_string())),
        received(&delivery(6, json!({"evt": 107, "gp": "not base64!"}).to_string())),
        "not json".to_owned(),
        json!({"type": "verdict", "channel": "web_socket"}).to_string(),
    ]
    .join("\n");
    assert_eq!(invitations(&capture).len(), 3);
    assert!(invitations("").is_empty());
}

const CHAT_DELIVERIES: &str = include_str!("../../../tests/fixtures/calling/chat/deliveries.json");

#[test]
fn the_captured_invitation_s_shape_counts_as_one() -> TestResult {
    let fixture: serde_json::Value = serde_json::from_str(CHAT_DELIVERIES)?;
    let forms = fixture["invitations"].as_array().ok_or("invitations")?;
    assert_eq!(forms.len(), 2, "a 1:1 call's and a group call's");
    for invitation in forms {
        let body = &invitation["body"];
        let wire = invitation_body(body["evt"].as_u64().ok_or("evt")?, &body["gp"]);
        let message = json!({"id": -7, "method": "POST", "url": invitation["path"], "body": wire});
        assert_eq!(invitations(&received(&message)).len(), 1, "{}", invitation["name"]);
    }
    Ok(())
}

#[test]
fn each_call_is_placed_by_its_own_controls() {
    for (modality, tid) in [
        (Modality::Audio, "default-chat-call-audio-button"),
        (Modality::Audio, "audio-drop-in-dropdown-menu-item-audio-button"),
        (Modality::Video, "default-chat-call-video-button"),
        (Modality::Video, "chat-call-video-button"),
    ] {
        assert!(modality.control().contains(&format!("\"{tid}\"")), "{modality:?} {tid}");
    }
    assert!(!Modality::Audio.control().contains("video"));
    assert!(!Modality::Video.control().contains("audio-button\""));
}

/// The fixture's lab accounts a and b, c's made-up MRI, and the group chat.
const A: &str = "8:orgid:<guid:a>";
const B: &str = "8:orgid:<guid:b>";
const C: &str = "8:orgid:<guid:c>";
const GROUP: &str = "19:<group>@thread.v2";

/// The fixture's invitation `index` (0 a 1:1 call's, 1 a group call's): its event and its decoded `gp`.
fn fixture_invitation(index: usize) -> TestResult<(u64, serde_json::Value)> {
    let fixture: serde_json::Value = serde_json::from_str(CHAT_DELIVERIES)?;
    let body = &fixture["invitations"][index]["body"];
    Ok((body["evt"].as_u64().ok_or("evt")?, body["gp"].clone()))
}

/// An invitation delivery: its event, its `gp` and when the capture recorded it.
type Delivery<'a> = (u64, &'a serde_json::Value, u64);

/// A capture of these invitation deliveries.
fn capture_of(deliveries: &[Delivery<'_>]) -> String {
    deliveries
        .iter()
        .map(|(event, gp, ts)| {
            let message = json!({"id": -7, "method": "POST", "url": "/v4/f/x/", "body": invitation_body(*event, gp)});
            received_at(&message, *ts)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn one_to_one() -> RunCall {
    RunCall {
        caller: A.to_owned(),
        group_chat: None,
    }
}

fn group() -> RunCall {
    RunCall {
        caller: A.to_owned(),
        group_chat: Some(GROUP.to_owned()),
    }
}

/// `gp` with the value at `pointer` set to `value`.
fn with(gp: &serde_json::Value, pointer: &str, value: serde_json::Value) -> TestResult<serde_json::Value> {
    let mut changed = gp.clone();
    *changed.pointer_mut(pointer).ok_or(pointer.to_owned())? = value;
    Ok(changed)
}

#[test]
fn a_callee_clicks_only_once_the_run_s_own_call_s_invitation_and_nothing_else_reached_its_page() -> TestResult {
    let (direct, direct_gp) = fixture_invitation(0)?;
    let (mixer, group_gp) = fixture_invitation(1)?;
    // the run's 1:1 call, delivered again while it goes unanswered
    let delivered = invitations(&capture_of(&[
        (direct, &direct_gp, PLACED + 900),
        (direct, &direct_gp, PLACED + 2_900),
        (direct, &direct_gp, PLACED + 4_900),
    ]));
    assert_eq!(ring_check(&delivered, &one_to_one(), B, PLACED), RingCheck::Ready);
    assert_eq!(own_invitations(&delivered, &one_to_one(), B, PLACED), 3);
    // the run's group call, in the recorded chat
    let delivered = invitations(&capture_of(&[(mixer, &group_gp, PLACED + 900)]));
    assert_eq!(ring_check(&delivered, &group(), B, PLACED), RingCheck::Ready);
    // nothing yet
    assert_eq!(ring_check(&[], &one_to_one(), B, PLACED), RingCheck::Pending);
    assert_eq!(own_invitations(&[], &one_to_one(), B, PLACED), 0);
    Ok(())
}

#[test]
fn any_invitation_but_the_run_s_own_call_s_refuses_the_click() -> TestResult {
    let (direct, direct_gp) = fixture_invitation(0)?;
    let (mixer, group_gp) = fixture_invitation(1)?;
    let outsider = with(
        &direct_gp,
        "/callNotification/from/id",
        json!("8:orgid:not-a-lab-account"),
    )?;
    let other_call = with(
        &direct_gp,
        "/conversationInvitation/conversationController",
        json!("https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/Other"),
    )?;
    let cases: Vec<(&str, Vec<Delivery<'_>>, RunCall, &str)> = vec![
        (
            "from someone else",
            vec![(direct, &outsider, PLACED + 1)],
            one_to_one(),
            B,
        ),
        (
            "to another callee",
            vec![(direct, &direct_gp, PLACED + 1)],
            one_to_one(),
            C,
        ),
        (
            "to no known callee",
            vec![(direct, &direct_gp, PLACED + 1)],
            one_to_one(),
            "",
        ),
        (
            "an outsider's after the run's",
            vec![(direct, &direct_gp, PLACED + 1), (direct, &outsider, PLACED + 2)],
            one_to_one(),
            B,
        ),
        (
            "an outsider's that already went",
            vec![(direct, &outsider, PLACED + 1), (direct, &direct_gp, PLACED + 2)],
            one_to_one(),
            B,
        ),
        (
            "from before the call was placed",
            vec![(direct, &direct_gp, PLACED - 1)],
            one_to_one(),
            B,
        ),
        (
            "of two calls",
            vec![(direct, &direct_gp, PLACED + 1), (direct, &other_call, PLACED + 2)],
            one_to_one(),
            B,
        ),
        (
            "a group call's for a 1:1 call",
            vec![(mixer, &group_gp, PLACED + 1)],
            one_to_one(),
            B,
        ),
        (
            "a 1:1 call's for a group call",
            vec![(direct, &direct_gp, PLACED + 1)],
            group(),
            B,
        ),
    ];
    for (name, deliveries, call, callee) in cases {
        let delivered = invitations(&capture_of(&deliveries));
        assert!(
            matches!(ring_check(&delivered, &call, callee, PLACED), RingCheck::Refused(_)),
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn an_invitation_naming_anything_else_isn_t_the_run_s_call() -> TestResult {
    let (direct, direct_gp) = fixture_invitation(0)?;
    let (mixer, group_gp) = fixture_invitation(1)?;
    let outsider = json!("8:orgid:not-a-lab-account");
    let changes = [
        (direct, &direct_gp, one_to_one(), "/callNotification/to/id", json!(C)),
        (
            direct,
            &direct_gp,
            one_to_one(),
            "/callNotification/replaces",
            json!({"callId": "x"}),
        ),
        (direct, &direct_gp, one_to_one(), "/groupContext", json!({"x": 1})),
        (
            direct,
            &direct_gp,
            one_to_one(),
            "/conversationInvitation/conversationController",
            json!(null),
        ),
        (
            mixer,
            &group_gp,
            group(),
            "/groupChat/threadId",
            json!("19:other@thread.v2"),
        ),
        (
            mixer,
            &group_gp,
            group(),
            "/conversationInvitation/groupCallInitiator",
            outsider.clone(),
        ),
        (
            mixer,
            &group_gp,
            group(),
            "/conversationInvitation/groupCallInitiatorParticipant/id",
            outsider.clone(),
        ),
    ];
    for (event, gp, call, pointer, value) in changes {
        let edited = with(gp, pointer, value)?;
        let delivered = invitations(&capture_of(&[(event, &edited, PLACED + 1)]));
        assert_eq!(delivered.len(), 1, "{pointer}");
        assert!(!delivered[0].is_of(&call, B), "{pointer}");
        assert!(
            matches!(ring_check(&delivered, &call, B, PLACED), RingCheck::Refused(_)),
            "{pointer}"
        );
        assert_eq!(own_invitations(&delivered, &call, B, PLACED), 0, "{pointer}");
    }
    Ok(())
}

#[test]
fn an_invitation_the_capture_didn_t_time_can_t_be_the_run_s_call() -> TestResult {
    let (direct, direct_gp) = fixture_invitation(0)?;
    let message = json!({"id": 1, "method": "POST", "url": "/v4/f/x/", "body": invitation_body(direct, &direct_gp)});
    let untimed = json!({
        "type": "cdp",
        "method": "Network.webSocketFrameReceived",
        "params": {"response": {"payloadData": format!("3:::{message}")}},
    })
    .to_string();
    let delivered = invitations(&untimed);
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].at_ms, 0);
    assert!(matches!(
        ring_check(&delivered, &one_to_one(), B, PLACED),
        RingCheck::Refused(_)
    ));
    assert_eq!(own_invitations(&delivered, &one_to_one(), B, PLACED), 0);
    Ok(())
}

#[test]
fn a_callee_s_verdict_counts_only_the_run_s_own_call_s_invitations() -> TestResult {
    let (direct, direct_gp) = fixture_invitation(0)?;
    let outsider = with(
        &direct_gp,
        "/callNotification/from/id",
        json!("8:orgid:not-a-lab-account"),
    )?;
    let delivered = invitations(&capture_of(&[
        (direct, &direct_gp, PLACED - 5),
        (direct, &outsider, PLACED + 1),
        (direct, &direct_gp, PLACED + 2),
    ]));
    assert_eq!(delivered.len(), 3);
    assert_eq!(own_invitations(&delivered, &one_to_one(), B, PLACED), 1);
    // only an unrelated call reached the callee: the run's call didn't
    let unrelated = invitations(&capture_of(&[(direct, &outsider, PLACED + 1)]));
    assert_eq!(own_invitations(&unrelated, &one_to_one(), B, PLACED), 0);
    Ok(())
}
