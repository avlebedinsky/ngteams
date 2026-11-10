//! The `ab-call` and `abc-call` scenarios' offline parts: what a run holds back (a's starts in `start-held`, nothing
//! of a callee's), which forced denies each account's run expects, how held-back requests are counted, how a callee's
//! capture shows the call's invitation reaching its page and what its step sent, how a failed run sees the `leave` of a
//! page it hung up, and how a call the service ended at once because the chat already has a running call is read. All
//! ids, hosts and bodies here are made up.

// The full Linux browser/scenario harness is outside the Windows provisioning command set.
#![cfg(unix)]

use base64::Engine as _;
use proto_lab::capture_hold::{HeldRequest, PausedRequest};
use proto_lab::protocol::CALL_START_URL;
use proto_lab::scenario::ab_call::{
    ACCEPT_AUDIO_CSS, CallStep, CalleeAction, CalleeRole, DECLINE_CSS, DECLINE_SENDS, INVITATION_EVENTS, Modality,
    RingCheck, RingSends, RunCall, START_HELD, calling_socket_up, chat_shown, held_back, invitations, judge_step_sends,
    keep_alives, leaves, own_invitations, ring_check, ring_sends, running_call_conflict, sent_requests,
    start_held_rule, video_frames,
};
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn request(method: &str, url: &str) -> PausedRequest {
    PausedRequest {
        request_id: "1".to_owned(),
        method: method.to_owned(),
        url: url.to_owned(),
        body: Some(b"{}".to_vec()),
        network_id: None,
    }
}

const FORKED: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/forked/call-1/27/i1/1/attach?i=10-0-0-1";
const TROUTER: &str = "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c?tc=made-up";

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
fn a_callee_s_capture_counts_the_ring_time_sends_its_fence_let_through() {
    let verdict = |entry: serde_json::Value| entry.to_string();
    let text = [
        verdict(json!({"type": "verdict", "channel": "http", "method": "POST", "url": FORKED, "network_id": "1.1", "verdict": "allow"})),
        // the same request on the browser's session too
        verdict(json!({"type": "verdict", "channel": "http", "method": "POST", "url": FORKED, "network_id": "1.1", "verdict": "allow"})),
        verdict(json!({"type": "verdict", "channel": "http", "method": "POST", "url": FORKED, "network_id": "1.2", "verdict": "deny"})),
        verdict(json!({"type": "verdict", "channel": "http", "method": "POST", "url": FORKED.replace("/attach", "/progress"), "network_id": "1.3", "verdict": "allow"})),
        verdict(json!({"type": "verdict", "channel": "web_socket", "url": TROUTER, "frame": {"data": "3:::{\"id\":1}"}, "verdict": "allow"})),
        verdict(json!({"type": "verdict", "channel": "web_socket", "url": TROUTER, "frame": {"data": "5:::{}"}, "verdict": "allow"})),
        verdict(json!({"type": "verdict", "channel": "web_socket", "url": TROUTER, "frame": {"data": "3:::{\"id\":2}"}, "verdict": "deny"})),
    ]
    .join("\n");
    assert_eq!(
        ring_sends(&text),
        RingSends {
            attaches: 1,
            receipts: 1
        }
    );
}

#[test]
fn each_account_expects_only_its_step_s_own_first_message_holds() {
    assert_eq!(CallStep::KeepAlive.as_str(), "keep-alive");
    assert_eq!(CallStep::Steps.as_str(), "steps");
    assert_eq!(CallStep::KeepAlive.callee_action(), CallStep::Answer.callee_action());
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
fn a_callee_clicks_its_step_s_button() {
    assert_eq!(
        CallStep::Answer.callee_action(),
        Some(CalleeAction {
            button: ACCEPT_AUDIO_CSS,
            what: "answer"
        })
    );
    assert_eq!(
        CallStep::Decline.callee_action(),
        Some(CalleeAction {
            button: DECLINE_CSS,
            what: "decline"
        })
    );
    for step in [CallStep::StartHeld, CallStep::Cancel, CallStep::Timeout] {
        assert_eq!(step.callee_action(), None, "{step:?}");
    }
}

#[test]
fn held_back_requests_count_once_per_network_request_under_their_label() {
    let forced = |label: &str, id: Option<&str>| HeldRequest {
        label: Some(label.to_owned()),
        network_id: id.map(str::to_owned),
        ..HeldRequest::new("POST", CALL_START_URL)
    };
    let denials = [
        // a request pauses on its target's session and the browser's: one request
        forced(START_HELD, Some("1.1")),
        forced(START_HELD, Some("1.1")),
        forced(START_HELD, Some("1.2")),
        forced(START_HELD, None),
        forced("another capture", Some("1.3")),
        HeldRequest::new("POST", CALL_START_URL),
    ];
    assert_eq!(held_back(&denials, START_HELD), 3);
    assert_eq!(held_back(&denials, "another capture"), 1);
    assert_eq!(held_back(&denials, "another label"), 0);
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
fn any_earlier_or_unrelated_invitation_refuses_the_click() -> TestResult {
    let (direct, direct_gp) = fixture_invitation(0)?;
    let outsider = with(
        &direct_gp,
        "/callNotification/from/id",
        json!("8:orgid:not-a-lab-account"),
    )?;
    for at in [PLACED - 60_000, PLACED - 1, PLACED + 1] {
        let deliveries = [(direct, &outsider, at), (direct, &direct_gp, PLACED + 900)];
        let delivered = invitations(&capture_of(&deliveries));
        assert!(
            matches!(ring_check(&delivered, &one_to_one(), B, PLACED), RingCheck::Refused(_)),
            "{at}"
        );
    }
    let deliveries = [
        (direct, &direct_gp, PLACED - 60_000),
        (direct, &direct_gp, PLACED + 900),
    ];
    assert!(matches!(
        ring_check(&invitations(&capture_of(&deliveries)), &one_to_one(), B, PLACED),
        RingCheck::Refused(_)
    ));
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

// --- once a callee acted: what goes to the fence, what stays held ---

const CC: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1";
const CONV: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/c1";

#[test]
fn a_page_s_calling_socket_is_up_once_connected_and_until_it_closes() {
    let event = |method: &str, params: serde_json::Value| {
        json!({"type": "cdp", "method": method, "params": params}).to_string()
    };
    let calling = "wss://go.trouter.teams.microsoft.com/v4/c?tc=%7B%22ua%22:%22SkypeSpaces%22%7D";
    let messaging = "wss://go.trouter.teams.microsoft.com/v4/c?tc=%7B%22ua%22:%22TeamsCDL%22%7D";
    let created = |id: &str, url: &str| event("Network.webSocketCreated", json!({"requestId": id, "url": url}));
    let connected = |id: &str| {
        event(
            "Network.webSocketFrameReceived",
            json!({"requestId": id, "response": {"payloadData": "5:1::{\"name\":\"trouter.connected\",\"args\":[]}"}}),
        )
    };
    let closed = |id: &str| event("Network.webSocketClosed", json!({"requestId": id}));
    assert!(!calling_socket_up(&created("1", calling)));
    assert!(calling_socket_up(&[created("1", calling), connected("1")].join("\n")));
    assert!(!calling_socket_up(
        &[created("2", messaging), connected("2")].join("\n")
    ));
    assert!(!calling_socket_up(
        &[created("1", calling), connected("1"), closed("1")].join("\n")
    ));
    assert!(calling_socket_up(
        &[
            created("1", calling),
            connected("1"),
            closed("1"),
            created("3", calling),
            connected("3")
        ]
        .join("\n")
    ));
}

#[test]
fn a_capture_s_decoded_video_is_the_most_frames_any_statistics_report_named() {
    let stats = |frames: u64, kind: &str| {
        json!({"type": "rtc", "payload": {"event": "stats", "data": [
            {"type": "inbound-rtp", "kind": kind, "framesDecoded": frames},
        ]}})
        .to_string()
    };
    assert_eq!(video_frames(""), 0);
    assert_eq!(
        video_frames(&[stats(10, "video"), stats(40, "video"), stats(500, "audio")].join("\n")),
        40
    );
}

// --- what a callee's step must have sent, and the join's click gate ---

const REJECT: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/incoming/call-1/27/t/2/reject?i=10-0-0-1";
const JOIN: &str =
    "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/c1?i=10-0-0-2&e=1";

fn verdict(method: &str, url: &str, network_id: &str, verdict: &str) -> String {
    json!({"type": "verdict", "channel": "http", "method": method, "url": url, "network_id": network_id, "verdict": verdict})
        .to_string()
}

#[test]
fn a_keep_alive_counts_only_when_its_fence_let_it_through_at_the_call_leg_s_own_link() {
    let leg = format!("{CC}/active/call-1/27/a4/3?i=10-0-0-1");
    let none = [
        verdict("POST", &leg, "2.1", "deny"),
        verdict("PUT", &leg, "2.2", "allow"),
        verdict(
            "POST",
            &format!("{CC}/active/call-1/27/a4/3/renegotiate?i=1"),
            "2.3",
            "allow",
        ),
        verdict(
            "POST",
            &format!("{CC}/active/call-1/27/a4/3/updateMediaDescriptions?i=1"),
            "2.4",
            "allow",
        ),
        verdict("POST", &format!("{CC}/active/call-1/27/a4?i=1"), "2.5", "allow"),
    ]
    .join("\n");
    assert_eq!(keep_alives(&none), 0);
    // one let through, seen on both layers, and the next one 2430 s later
    let sent = [
        none,
        verdict("POST", &leg, "2.6", "allow"),
        verdict("POST", &leg, "2.6", "allow"),
        verdict("POST", &format!("{CC}/active/call-1/27/a4/3"), "2.7", "allow"),
    ]
    .join("\n");
    assert_eq!(keep_alives(&sent), 2);
}

#[test]
fn a_callee_s_role_is_its_step_s() {
    assert_eq!(CalleeRole::of(CallStep::Decline, false), CalleeRole::Declines);
    assert_eq!(CalleeRole::of(CallStep::Decline, true), CalleeRole::Declines);
    assert_eq!(CalleeRole::of(CallStep::Join, true), CalleeRole::Joins);
    assert_eq!(CalleeRole::of(CallStep::Join, false), CalleeRole::Other);
    for step in [
        CallStep::Answer,
        CallStep::Video,
        CallStep::Dropped,
        CallStep::Cancel,
        CallStep::Timeout,
    ] {
        assert_eq!(CalleeRole::of(step, true), CalleeRole::Other, "{step:?}");
    }
}

#[test]
fn a_decline_passes_only_with_its_rejection_let_through() {
    // denied, to another link, or another method: no rejection went out
    let none = [
        verdict("DELETE", REJECT, "1.1", "deny"),
        verdict("DELETE", &REJECT.replace("/reject", "/progress"), "1.2", "allow"),
        verdict("POST", REJECT, "1.3", "allow"),
    ]
    .join("\n");
    assert!(judge_step_sends(CalleeRole::Declines, &none).is_err());
    // one let through, seen on both layers: one rejection
    let sent = [
        none,
        verdict("DELETE", REJECT, "1.4", "allow"),
        verdict("DELETE", REJECT, "1.4", "allow"),
    ]
    .join("\n");
    assert_eq!(sent_requests(&sent, "DELETE", DECLINE_SENDS), 1);
    assert_eq!(
        judge_step_sends(CalleeRole::Declines, &sent),
        Ok(Some("the capture recorded 1 rejection(s) through".to_owned()))
    );
    // nobody else's sends are judged here
    assert_eq!(judge_step_sends(CalleeRole::Other, ""), Ok(None));
}

fn reading(sends: u64, ours: u64, joins: u64) -> String {
    json!({"sends": sends, "ours": ours, "joins": joins, "where": []}).to_string()
}

#[test]
fn the_join_is_clicked_only_with_the_run_s_chat_shown_and_one_join() {
    assert_eq!(chat_shown(&reading(1, 1, 1)), Ok(()));
    for (name, refused) in [
        ("no join", reading(1, 1, 0)),
        ("two joins", reading(1, 1, 2)),
        ("another chat open", reading(1, 0, 1)),
        ("no chat open", reading(0, 0, 1)),
        ("two chats open, one the run's", reading(2, 1, 1)),
        ("no reading", "not json".to_owned()),
        ("an empty reading", "{}".to_owned()),
    ] {
        assert!(chat_shown(&refused).is_err(), "{name}");
    }
}

#[test]
fn a_join_passes_only_once_its_fence_let_it_through() {
    // a join denied, or another request on the conversation let through: no join
    let none = [
        verdict("POST", JOIN, "2.1", "deny"),
        verdict("POST", &JOIN.replace("/conv/c1?", "/conv/c1/leave?"), "2.2", "allow"),
        verdict("GET", JOIN, "2.3", "allow"),
    ]
    .join("\n");
    assert!(judge_step_sends(CalleeRole::Joins, &none).is_err());
    // the browser decides a request on two layers: one join
    let joined = [
        none,
        verdict("POST", JOIN, "2.4", "allow"),
        verdict("POST", JOIN, "2.4", "allow"),
    ]
    .join("\n");
    assert_eq!(
        judge_step_sends(CalleeRole::Joins, &joined),
        Ok(Some(
            "the capture recorded 1 join(s) of the running call through".to_owned()
        ))
    );
}

#[test]
fn a_page_that_hung_up_counts_the_leaves_its_fence_let_through() {
    let leave = format!("{CONV}/leave?i=10-0-0-2&e=1");
    let text = [
        verdict("POST", &leave, "3.1", "allow"),
        verdict("POST", &leave, "3.1", "allow"),
        verdict("POST", &leave, "3.2", "deny"),
        verdict("POST", &format!("{CONV}/updateEndpointMetadata"), "3.3", "allow"),
        verdict("POST", &leave.replace("flightproxy", "flightproxy2"), "3.4", "allow"),
    ]
    .join("\n");
    assert_eq!(leaves(&text), 1);
    assert_eq!(leaves(""), 0);
}

/// A capture line: the page received a call's conversation end, as the call agent delivers it, with this body.
fn conversation_end(body: &serde_json::Value) -> String {
    received(&json!({
        "id": -4,
        "method": "POST",
        "url": "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/05050505/conversation/conversationEnd/",
        "headers": {"trouter-request": "{\"id\":\"request-1\"}"},
        "body": body.to_string(),
    }))
}

#[test]
fn a_start_ended_at_once_by_a_running_call_in_its_chat_names_that_call() {
    let running = format!("{CONV}?i=10-0-0-2&e=1");
    let refused = json!({
        "code": 409,
        "subCode": 5704,
        "phrase": "Conversation cannot be created due to a failure to resolve ownership.",
        "conversationUrl": {"OriginalString": running, "Location": running},
        "threadId": "19:0123456789abcdef0123456789abcdef@thread.v2",
    });
    let text = [
        conversation_end(&json!({"code": 0, "phrase": "noError"})),
        conversation_end(&refused),
    ]
    .join("\n");
    assert_eq!(running_call_conflict(&text), Some(CONV.to_owned()));
    // naming no conversation, it is still the conflict
    let bare = conversation_end(&json!({"code": 409, "subCode": 5704}));
    assert_eq!(running_call_conflict(&bare), Some("?".to_owned()));
    // any other end, another sub-code, or the same body at another callback is none
    for other in [
        conversation_end(&json!({"code": 409, "subCode": 5700})),
        conversation_end(&json!({"code": 487, "subCode": 5704})),
        received(&json!({
            "id": 2,
            "url": "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/04040404/call/end/",
            "headers": {},
            "body": refused.to_string(),
        })),
    ] {
        assert_eq!(running_call_conflict(&other), None, "{other}");
    }
}
