//! Kept call-registry cases moved from teams-fence: offers, ended rings and callback bindings.
use super::*;
use base64::Engine as _;
use serde_json::{Value, json};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const CHAT_DELIVERIES: &str = include_str!("../../../../tests/fixtures/calling/chat/deliveries.json");
const CHAT_FOLLOW_UPS: &str = include_str!("../../../../tests/fixtures/calling/chat/follow_ups.json");
use teams_proto::{CALL_START_URL, TEST_CALL_SCENARIO};
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

const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GROUP: &str = "0123456789abcdef0123456789abcdef";
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
fn attach_case() -> TestResult<(String, Value)> {
    let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS))?;
    let case = &fixture["cases"][1];
    Ok((case["url"].as_str().ok_or("url")?.to_owned(), case["body"].clone()))
}

struct Write(Url, Value);
impl Request for Write {
    fn service(&self) -> teams_proto::Service {
        teams_proto::Service::Calling
    }
    fn method(&self) -> teams_proto::Method {
        teams_proto::Method::Post
    }
    fn endpoint(&self) -> teams_proto::Endpoint {
        teams_proto::Endpoint::new([] as [&str; 0])
    }
    fn body(&self) -> Option<Value> {
        Some(self.1.clone())
    }
    fn call_url(&self) -> Option<&Url> {
        Some(&self.0)
    }
}
fn write(url: &str, body: Value) -> Write {
    Write(Url::parse(url).unwrap(), body)
}
fn starting(tracker: &Tracker) -> Attempt {
    tracker.before(&write(CALL_START_URL, serde_json::from_slice(&start_body()).unwrap()))
}
fn attached(tracker: &Tracker) -> (Delivery, Value, String) {
    let (invited, gp) = invitation(0, 1).unwrap();
    assert_eq!(tracker.observe_delivery(&invited), Observed::Offered);
    let (url, body) = attach_case().unwrap();
    tracker.before(&write(&url, body.clone()));
    (invited, gp, url)
}
#[test]
fn an_invitation_is_offered_only_to_its_account_and_not_from_itself() {
    let (invited, mut gp) = invitation(0, 1).unwrap();
    assert_eq!(
        Tracker::new(Mri::new(LAB_B)).observe_delivery(&invited),
        Observed::Offered
    );
    assert_eq!(
        Tracker::new(Mri::new(LAB_A)).observe_delivery(&invited),
        Observed::Ignored
    );
    gp["callNotification"]["from"]["id"] = json!(LAB_B);
    let self_call = invitation_delivery(&invited.path, 2, &gp);
    assert_eq!(
        Tracker::new(Mri::new(LAB_B)).observe_delivery(&self_call),
        Observed::Ignored
    );
    gp["callNotification"]["from"]["id"] = json!(COWORKER);
    assert_eq!(
        Tracker::new(Mri::new(LAB_B)).observe_delivery(&invitation_delivery(&invited.path, 3, &gp)),
        Observed::Offered,
        "caller is not a lab policy decision"
    );
}
#[test]
fn unreadable_or_wrong_kind_invitations_stay_ignored() {
    let (invited, gp) = invitation(0, 1).unwrap();
    let tracker = Tracker::new(Mri::new(LAB_B));
    for body in [b"not JSON".to_vec(), br#"{"evt":107,"gp":"not base64"}"#.to_vec()] {
        assert_eq!(
            tracker.observe_delivery(&Delivery {
                body,
                ..invited.clone()
            }),
            Observed::Ignored
        );
    }
    assert_eq!(
        tracker.observe_delivery(&invitation_delivery_as(&invited.path, 2, 109, &gp)),
        Observed::Ignored
    );
    let (group, gp) = invitation(1, 3).unwrap();
    assert_eq!(
        tracker.observe_delivery(&invitation_delivery_as(&group.path, 4, 107, &gp)),
        Observed::Ignored
    );
}
#[test]
fn redelivery_is_kept_but_an_ended_ring_never_rings_again() {
    let tracker = Tracker::new(Mri::new(LAB_B));
    let (invited, _, _) = attached(&tracker);
    assert!(tracker.holds_floor());
    assert_eq!(
        tracker.observe_delivery(&Delivery {
            id: 2,
            ..invited.clone()
        }),
        Observed::Offered
    );
    let (_, body) = attach_case().unwrap();
    let end = callbacks(&body)
        .into_iter()
        .find(|url| url.ends_with("/call/end/"))
        .unwrap();
    assert_eq!(
        tracker.observe_delivery(&delivery(3, Url::parse(&end).unwrap().path(), b"{}".to_vec())),
        Observed::RingEnded
    );
    assert!(!tracker.holds_floor());
    assert_eq!(tracker.observe_delivery(&invited), Observed::Ignored);
}
#[test]
fn conversation_end_and_machine_release_retire_the_offer() {
    for conversation_end in [false, true] {
        let tracker = Tracker::new(Mri::new(LAB_B));
        let (invited, _, attach) = attached(&tracker);
        if conversation_end {
            let (_, body) = attach_case().unwrap();
            let end = callbacks(&body)
                .into_iter()
                .find(|url| url.ends_with("/conversation/conversationEnd/"))
                .unwrap();
            assert_eq!(
                tracker.observe_delivery(&delivery(2, Url::parse(&end).unwrap().path(), b"{}".to_vec())),
                Observed::RingEnded
            );
        } else {
            tracker.let_go(&attach);
        }
        assert!(!tracker.holds_floor());
        assert_eq!(tracker.observe_delivery(&invited), Observed::Ignored);
    }
}
#[test]
fn the_account_offers_one_ringing_invitation_at_a_time() {
    let tracker = Tracker::new(Mri::new(LAB_B));
    let (_, _, attach) = attached(&tracker);
    let (mut later, mut gp) = invitation(0, 2).unwrap();
    let text = gp.to_string().replace("/forked/", "/forked/next");
    gp = serde_json::from_str(&text).unwrap();
    later = invitation_delivery(&later.path, 2, &gp);
    assert_eq!(tracker.observe_delivery(&later), Observed::Ignored);
    tracker.let_go(&attach);
    assert_eq!(tracker.observe_delivery(&later), Observed::Offered);
}
#[test]
fn a_start_acceptance_may_arrive_before_its_successful_http_answer() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    let pending = starting(&tracker);
    assert!(
        !tracker.holds_floor(),
        "pending start has the existing occupancy semantics"
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call-1"))),
        Observed::Acceptance
    );
    tracker.answered(pending, &Response::new(200, answer("conversation-1")));
    assert!(tracker.holds_floor());
    assert_eq!(
        tracker.observe_delivery(&delivery(2, END_PATH, b"{}".to_vec())),
        Observed::Ended
    );
    assert!(!tracker.holds_floor());
    assert_eq!(
        tracker.observe_delivery(&delivery(3, ACCEPTANCE_PATH, acceptance("call-1"))),
        Observed::Nothing
    );
}
#[test]
fn explicit_failed_or_unreadable_start_answers_bind_no_live_call() {
    for (status, body) in [(401, answer("c")), (200, b"{}".to_vec())] {
        let tracker = Tracker::new(Mri::new(LAB_A));
        tracker.answered(starting(&tracker), &Response::new(status, body));
        assert!(!tracker.holds_floor());
        assert_eq!(
            tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call"))),
            Observed::Nothing
        );
    }
}
#[test]
fn acceptance_links_must_name_one_call_and_the_bound_call() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    tracker.answered(starting(&tracker), &Response::new(200, answer("c")));
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, b"{}".to_vec())),
        Observed::AcceptanceUnbound
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(2, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Acceptance
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(3, ACCEPTANCE_PATH, acceptance("two"))),
        Observed::AcceptanceUnbound
    );
}
#[test]
fn a_live_calls_media_deliveries_bind_only_their_own_call_links() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    tracker.answered(starting(&tracker), &Response::new(200, answer("c")));
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Acceptance
    );
    let at = format!(
        "{}/08080808/call/mediaRenegotiation/",
        Url::parse(CALLBACKS).unwrap().path()
    );
    let body = |call: &str| {
        json!({"mediaNegotiation":{"links":{"mediaAnswer":format!("{PROXY}/{CC_HOST}/cc/v1/negotiations/{call}/1/a1/1/answer?i=1")}}}).to_string().into_bytes()
    };
    assert_eq!(
        tracker.observe_delivery(&delivery(2, &at, body("other"))),
        Observed::Nothing
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(3, &at, body("one"))),
        Observed::Renegotiation
    );
}

fn registered() -> Tracker {
    let tracker = Tracker::new(Mri::new(LAB_A));
    tracker.answered(starting(&tracker), &Response::new(200, answer("conversation-1")));
    tracker
}

fn media_delivery(call: &str) -> Delivery {
    let path = format!(
        "{}/08080808/call/mediaRenegotiation/",
        Url::parse(CALLBACKS).unwrap().path()
    );
    let body = json!({"mediaNegotiation": {"links": {
        "mediaAnswer": format!("{PROXY}/{CC_HOST}/cc/v1/negotiations/{call}/1/a1/1/answer?i=1")
    }}});
    delivery(20, &path, body.to_string().into_bytes())
}

#[test]
fn an_acceptance_off_the_callback_or_naming_two_calls_cannot_bind() {
    let tracker = registered();
    let elsewhere = ACCEPTANCE_PATH.replace("agent-1", "agent-2");
    assert_eq!(
        tracker.observe_delivery(&delivery(1, &elsewhere, acceptance("one"))),
        Observed::Nothing
    );
    let split = String::from_utf8(acceptance("one"))
        .unwrap()
        .replace("mcProxy/one", "mcProxy/two");
    assert_eq!(
        tracker.observe_delivery(&delivery(2, ACCEPTANCE_PATH, split.into_bytes())),
        Observed::AcceptanceUnbound
    );
    assert_eq!(tracker.observe_delivery(&media_delivery("one")), Observed::Nothing);
    for id in [3, 4] {
        assert_eq!(
            tracker.observe_delivery(&delivery(id, ACCEPTANCE_PATH, acceptance("one"))),
            Observed::Acceptance
        );
    }
    assert_eq!(
        tracker.observe_delivery(&media_delivery("one")),
        Observed::Renegotiation
    );
}

#[test]
fn an_end_before_the_start_answer_cannot_be_revived_by_the_late_answer() {
    for path in [END_PATH, CONVERSATION_END_PATH] {
        let tracker = Tracker::new(Mri::new(LAB_A));
        let pending = starting(&tracker);
        assert_eq!(
            tracker.observe_delivery(&delivery(1, path, b"{}".to_vec())),
            Observed::Ended
        );
        tracker.answered(pending, &Response::new(200, answer("conversation-1")));
        assert!(!tracker.holds_floor());
        assert_eq!(
            tracker.observe_delivery(&delivery(2, ACCEPTANCE_PATH, acceptance("one"))),
            Observed::Nothing
        );
    }
}

#[test]
fn a_retry_after_an_explicit_rejection_binds_the_new_start() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    tracker.answered(starting(&tracker), &Response::new(503, answer("not-taken")));
    tracker.answered(starting(&tracker), &Response::new(200, answer("taken")));
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Acceptance
    );
    assert!(tracker.holds_floor());
    assert_eq!(
        tracker.observe_delivery(&delivery(2, CONVERSATION_END_PATH, b"{}".to_vec())),
        Observed::Ended
    );
    assert!(!tracker.holds_floor());
}

#[test]
fn an_unknown_start_outcome_keeps_its_callbacks_until_the_end() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    let _unknown = starting(&tracker);
    // No answered hook: the request went out but no HTTP outcome is known.
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Acceptance
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(2, END_PATH, b"{}".to_vec())),
        Observed::Ended
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(3, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Nothing
    );
}

#[test]
fn a_start_that_never_went_out_cannot_bind_late_callbacks_or_be_revived() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    let pending = starting(&tracker);
    tracker.not_sent(pending);
    tracker.answered(pending, &Response::new(200, answer("late")));
    assert!(!tracker.holds_floor());
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Nothing
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(2, END_PATH, b"{}".to_vec())),
        Observed::Nothing
    );
}

#[test]
fn a_not_sent_notice_cannot_end_a_live_call_or_another_kind_of_attempt() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    let pending = starting(&tracker);
    tracker.answered(pending, &Response::new(200, answer("conversation-1")));
    for attempt in [
        pending,
        Attempt::Nothing,
        Attempt::Answer {
            offer: 0,
            operation: CallOperation::Attach,
        },
    ] {
        tracker.not_sent(attempt);
        assert!(tracker.holds_floor());
    }
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Acceptance
    );
    assert_eq!(
        tracker.observe_delivery(&media_delivery("one")),
        Observed::Renegotiation
    );
}

#[test]
fn echo_updates_keep_the_call_live_and_its_end_after_leave_retires_it() {
    let tracker = registered();
    let fixture = include_str!("../../../../tests/fixtures/calling/echo/deliveries.json")
        .replace("<guid:a>", GUID_A)
        .replace("<guid:echo>", ECHO.trim_start_matches("28:"));
    let fixture: Value = serde_json::from_str(&fixture).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let path = format!(
            "{}/{}",
            Url::parse(CALLBACKS).unwrap().path(),
            case["callback"].as_str().unwrap()
        );
        if case["observed"] == "ended" {
            tracker.before(&write(
                &format!("{}/leave?{ROUTE}", conversation("conversation-1")),
                Value::Null,
            ));
            assert!(!tracker.holds_floor(), "a leaving call releases the floor");
            assert_eq!(
                tracker.observe_delivery(&delivery(10, &path, case["body"].to_string().into_bytes())),
                Observed::Ended
            );
        } else {
            assert_eq!(
                tracker.observe_delivery(&delivery(10, &path, case["body"].to_string().into_bytes())),
                Observed::Nothing
            );
            assert!(tracker.holds_floor());
        }
    }
    assert_eq!(
        tracker.observe_delivery(&delivery(11, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Nothing
    );
}

#[test]
fn a_retarget_moves_media_binding_only_after_a_success_with_one_call_leg() {
    let tracker = registered();
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Acceptance
    );
    let acknowledged = format!(
        "{}/09090909/call/mediaAcknowledgement/",
        Url::parse(CALLBACKS).unwrap().path()
    );
    let active = |call: &str, suffix: &str| format!("{PROXY}/{CC_HOST}/cc/v1/active/{call}/1/a4/5{suffix}?i=1");
    for body in [
        json!({"mediaAcknowledgement":{"resultCategories":["Failure"]},"links":{"callLeg":active("two", "")}}),
        json!({"mediaAcknowledgement":{"resultCategories":["Success"]},"links":{"callLeg":active("two", ""),"mediaRenegotiation":active("three", "/renegotiate")}}),
        json!({"mediaAcknowledgement":{"resultCategories":["Success"]},"links":{"mediaRenegotiation":active("two", "/renegotiate")}}),
    ] {
        assert_eq!(
            tracker.observe_delivery(&delivery(2, &acknowledged, body.to_string().into_bytes())),
            Observed::Renegotiation
        );
        assert_eq!(tracker.observe_delivery(&media_delivery("two")), Observed::Nothing);
        assert_eq!(
            tracker.observe_delivery(&media_delivery("one")),
            Observed::Renegotiation
        );
    }
    // This synthetic form preserves the capture's duplicate callLeg: JSON.parse keeps the final value.
    let body = json!({"mediaAcknowledgement":{"resultCategories":["Success"]},"links":{
        "callLeg":active("two", ""),"mediaRenegotiation":active("two", "/renegotiate")
    }})
    .to_string()
    .replacen(r#""links":{"#, r#""links":{"callLeg":null,"#, 1);
    assert_eq!(
        tracker.observe_delivery(&delivery(
            3,
            &acknowledged.replace("agent-1", "agent-2"),
            body.clone().into_bytes()
        )),
        Observed::Nothing
    );
    assert_eq!(tracker.observe_delivery(&media_delivery("two")), Observed::Nothing);
    assert_eq!(
        tracker.observe_delivery(&delivery(4, &acknowledged, body.into_bytes())),
        Observed::Renegotiation
    );
    assert_eq!(
        tracker.observe_delivery(&media_delivery("two")),
        Observed::Renegotiation
    );
    assert_eq!(tracker.observe_delivery(&media_delivery("one")), Observed::Nothing);
}

fn follow_ups() -> Value {
    serde_json::from_str(&fill(CHAT_FOLLOW_UPS)).unwrap()
}
fn fixture_write(fixture: &Value, index: usize) -> Write {
    let case = &fixture["cases"][index];
    write(case["url"].as_str().unwrap(), case["body"].clone())
}
fn bound_offer(tracker: &Tracker) -> (Delivery, String, Value) {
    let (invited, _, attach) = attached(tracker);
    let fixture = follow_ups();
    let pending = tracker.before(&fixture_write(&fixture, 1));
    tracker.answered(pending, &Response::new(200, fixture["attach_answer"].to_string()));
    (invited, attach, fixture)
}
fn callee_negotiation(fixture: &Value, index: usize) -> Delivery {
    let callbacks = callbacks(&fixture["cases"][1]["body"]);
    let agent = callbacks
        .iter()
        .filter_map(|callback| parse_callback(callback))
        .find_map(|callback| agent_of(&callback))
        .unwrap();
    let case = &fixture["deliveries"][index];
    delivery(
        30,
        &format!("{}{}", agent.path, case["callback"].as_str().unwrap()),
        case["body"].to_string().into_bytes(),
    )
}

#[test]
fn a_ring_end_before_the_attach_answer_suppresses_the_late_offer() {
    let tracker = Tracker::new(Mri::new(LAB_B));
    let (invited, _, _) = attached(&tracker);
    let fixture = follow_ups();
    let pending = tracker.before(&fixture_write(&fixture, 1));
    let end = fixture["cases"][1]["body"]["attach"]["links"]["end"].as_str().unwrap();
    assert_eq!(
        tracker.observe_delivery(&delivery(2, Url::parse(end).unwrap().path(), b"{}".to_vec())),
        Observed::RingEnded
    );
    tracker.answered(pending, &Response::new(200, fixture["attach_answer"].to_string()));
    assert!(!tracker.holds_floor());
    assert_eq!(tracker.observe_delivery(&invited), Observed::Ignored);
    assert_eq!(
        tracker.observe_delivery(&callee_negotiation(&fixture, 0)),
        Observed::Nothing
    );
}

#[test]
fn a_rejected_attach_can_be_retried_and_an_answered_offer_is_not_let_go() {
    let tracker = Tracker::new(Mri::new(LAB_B));
    let (invited, _, attach) = attached(&tracker);
    let fixture = follow_ups();
    for status in [401, 503] {
        let pending = tracker.before(&fixture_write(&fixture, 1));
        tracker.answered(pending, &Response::new(status, fixture["attach_answer"].to_string()));
        assert_eq!(tracker.observe_delivery(&invited), Observed::Offered);
        assert!(tracker.holds_floor());
    }
    let pending = tracker.before(&fixture_write(&fixture, 1));
    tracker.answered(pending, &Response::new(200, fixture["attach_answer"].to_string()));
    assert_eq!(
        tracker.observe_delivery(&callee_negotiation(&fixture, 0)),
        Observed::Nothing,
        "not answered yet"
    );
    let pending = tracker.before(&fixture_write(&fixture, 4));
    tracker.answered(pending, &Response::new(200, fixture["accept_answer"].to_string()));
    tracker.let_go(&attach);
    assert!(tracker.holds_floor());
    for index in [0, 1] {
        let negotiated = callee_negotiation(&fixture, index);
        assert_eq!(tracker.observe_delivery(&negotiated), Observed::Renegotiation);
        let elsewhere = Delivery {
            path: negotiated.path.replace("callAgent/", "callAgent/other"),
            ..negotiated.clone()
        };
        assert_eq!(tracker.observe_delivery(&elsewhere), Observed::Nothing);
        let other = Delivery {
            body: String::from_utf8(negotiated.body.clone())
                .unwrap()
                .replace("SyntheticCall1", "SyntheticCall9")
                .into_bytes(),
            ..negotiated
        };
        assert_eq!(tracker.observe_delivery(&other), Observed::Nothing);
    }
    tracker.before(&fixture_write(&fixture, 8));
    assert!(!tracker.holds_floor());
    assert_eq!(
        tracker.observe_delivery(&callee_negotiation(&fixture, 0)),
        Observed::Nothing
    );
}

#[test]
fn a_new_leg_of_the_same_conversation_is_offered_after_decline_or_leave() {
    for leave in [false, true] {
        let tracker = Tracker::new(Mri::new(LAB_B));
        let (_, _, fixture) = bound_offer(&tracker);
        if leave {
            let pending = tracker.before(&fixture_write(&fixture, 4));
            tracker.answered(pending, &Response::new(200, fixture["accept_answer"].to_string()));
            tracker.before(&fixture_write(&fixture, 8));
        } else {
            tracker.before(&fixture_write(&fixture, 5));
        }
        assert!(!tracker.holds_floor());
        let (invited, gp) = invitation(0, 40).unwrap();
        let gp: Value = serde_json::from_str(&gp.to_string().replace("SyntheticCall1/", "SyntheticCall2/")).unwrap();
        let next = invitation_delivery(&invited.path, 41, &gp);
        assert_eq!(tracker.observe_delivery(&next), Observed::Offered);
        let fixture: Value =
            serde_json::from_str(&fixture.to_string().replace("SyntheticCall1/", "SyntheticCall2/")).unwrap();
        let pending = tracker.before(&fixture_write(&fixture, 1));
        tracker.answered(pending, &Response::new(200, fixture["attach_answer"].to_string()));
        let pending = tracker.before(&fixture_write(&fixture, 4));
        tracker.answered(pending, &Response::new(200, fixture["accept_answer"].to_string()));
        assert!(tracker.holds_floor());
        assert_eq!(
            tracker.observe_delivery(&callee_negotiation(&fixture, 0)),
            Observed::Renegotiation
        );
    }
}
