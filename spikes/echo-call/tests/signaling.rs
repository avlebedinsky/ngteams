//! The signaling module on the Task 18 fixtures (`tests/fixtures/calling/echo/*.json`, synthetic, shaped like the
//! capture of the web client's test call; `docs/protocol/calling-echo.md`): the call start and the `add` built from
//! permitted requests and compared with the client's, the start's answer, the acceptance with the remote SDP, the TURN
//! credentials from `trap/tokens`. What the capture didn't have (progress, end, an incoming call) comes from the
//! hand-written fixtures in `tests/fixtures/synthetic/`, each marked `_synthetic`. And the incoming-call policy: an
//! incoming call is ignored and never answered.

use std::io;
use std::path::{Path, PathBuf};

use echo_call::signaling::{
    Acceptance, Action, CALL_START_URL, CallState, Callback, CallbackLinks, Conversation, Endpoint, Inbound,
    Invitation, NgcSdp, OutgoingCall, SDP_NGC, TrapTokens, WireRequest, add_request, call_start_body,
};
use fixture_guard::Guard;
use serde_json::{Value, json};
use teams_fence::{Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, Roster, Scope};
use teams_proto::{AddToCall, Authorized, ConversationUrl, Mutating as _, StartCall, TEST_CALL_SCENARIO};

/// The fixtures' placeholders for lab account a and the echo bot.
const LAB_A: &str = "8:orgid:<guid:a>";
const ECHO: &str = "28:<guid:echo>";
/// The endpoint's Trouter push URL and the call agent of the fixtures.
const PUSH_URL: &str = "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/";
const AGENT: &str = "<guid:2>";
/// The `add` among the captured requests.
const ADD: &str = "adding the echo bot to the call (a call follow-up: where the echo joins)";

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn task_18_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/calling/echo")
}

fn spike_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic")
}

fn read(path: &Path) -> TestResult<Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

fn answers() -> TestResult<Value> {
    read(&task_18_fixtures().join("answers.json"))
}

/// The body of the request case `name` in `requests.json`, and its URL.
fn request(name: &str) -> TestResult<(String, Value)> {
    let requests = read(&task_18_fixtures().join("requests.json"))?;
    let case = requests["cases"]
        .as_array()
        .ok_or("requests.json has no cases")?
        .iter()
        .find(|case| case["name"] == name)
        .ok_or_else(|| format!("no request case {name:?}"))?;
    let url = case["url"].as_str().ok_or("a case without a URL")?;
    Ok((url.to_owned(), case["body"].clone()))
}

fn test_call_start() -> TestResult<(String, Value)> {
    request("the test call start")
}

/// A delivery body from the spike's own hand-written fixtures.
fn spike_body(name: &str) -> TestResult<Value> {
    Ok(read(&spike_fixtures().join(name))?["body"].clone())
}

/// The links the fixtures' call registered: the path segment of every link is read back from the captured start (and
/// the `add`), so what is checked is how a link is put together.
fn fixture_links() -> TestResult<CallbackLinks> {
    let (_, start) = test_call_start()?;
    let (_, add) = request(ADD)?;
    let captured = |callback: Callback| -> Option<String> {
        let link = match callback {
            Callback::RosterUpdate => start.pointer("/conversationRequest/roster/rosterUpdate"),
            Callback::ConversationEnd | Callback::ConversationUpdate => {
                start["conversationRequest"]["links"].get(callback.name())
            }
            Callback::AddParticipantSuccess | Callback::AddParticipantFailure => add["links"].get(callback.name()),
            _ => start["callInvitation"]["links"].get(callback.name()),
        }?;
        let link = link.as_str()?;
        let rest = link.strip_prefix(&format!("{PUSH_URL}callAgent/{AGENT}/"))?;
        Some(rest.split('/').next()?.to_owned())
    };
    Ok(CallbackLinks::new(PUSH_URL, AGENT, |callback| {
        captured(callback).unwrap_or_else(|| "08080808".to_owned())
    }))
}

fn me() -> Endpoint {
    Endpoint {
        display_name: "Lab A".into(),
        endpoint_id: "<guid:3>".into(),
        participant_id: "<guid:4>".into(),
        language_id: "en-us".into(),
    }
}

fn fence() -> Fence {
    Fence::new(
        Roster::new([Mri::new(LAB_A)], [Mri::new(ECHO)]),
        FenceLog::from_writer(io::sink()),
    )
}

async fn permitted_test_call() -> TestResult<Authorized<StartCall>> {
    let call = StartCall::test_call(Mri::new(LAB_A), Mri::new(ECHO));
    let permit = fence().check(call.target(), &teams_fence::FailClosed).await?;
    Ok(Authorized::new(call, permit)?)
}

/// What the fence would need before it permits an `add`: a complete read of the call's members. No real source gives
/// one yet (`docs/backlog/fence-call-follow-ups.md`); this stands in for it.
struct CallOfLabA;

impl MembershipSource for CallOfLabA {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(async { Membership::Complete(vec![Mri::new(LAB_A)]) })
    }
}

/// Whether every key of `ours`, at every level, is a key the captured `theirs` has there.
fn keys_within(ours: &Value, theirs: &Value) -> bool {
    match (ours, theirs) {
        (Value::Object(ours), Value::Object(theirs)) => ours
            .iter()
            .all(|(key, value)| theirs.get(key).is_some_and(|captured| keys_within(value, captured))),
        (Value::Array(ours), Value::Array(theirs)) => match theirs.first() {
            Some(captured) => ours.iter().all(|value| keys_within(value, captured)),
            None => ours.is_empty(),
        },
        _ => true,
    }
}

#[test]
fn every_fixture_is_synthetic_and_passes_the_fixture_guard() {
    for name in ["answers.json", "requests.json", "frames.json"] {
        let value = read(&task_18_fixtures().join(name)).unwrap();
        assert!(value["_synthetic"].is_string(), "{name} lacks the synthetic marker");
    }
    for entry in std::fs::read_dir(spike_fixtures()).unwrap() {
        let path = entry.unwrap().path();
        let marker = read(&path).unwrap()["_synthetic"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            marker.starts_with("SYNTHETIC") && marker.contains("Task 18"),
            "{} lacks the synthetic marker",
            path.display()
        );
    }
    let findings = Guard::new()
        .unwrap()
        .scan_paths(&[&spike_fixtures(), &task_18_fixtures()])
        .unwrap();
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn a_callback_link_is_the_push_url_the_agent_a_segment_of_its_own_and_its_name() {
    let links = fixture_links().unwrap();
    let (_, start) = test_call_start().unwrap();
    for callback in Callback::CALL {
        assert_eq!(
            links.url(callback),
            start["callInvitation"]["links"][callback.name()],
            "{callback:?}"
        );
    }
    assert_eq!(
        links.url(Callback::RosterUpdate),
        start["conversationRequest"]["roster"]["rosterUpdate"]
    );
    assert_eq!(
        links.path(Callback::Acceptance),
        answers().unwrap()["acceptance_callback"]["path"],
        "a delivery comes at the link's path"
    );

    let random = CallbackLinks::random(PUSH_URL, AGENT).unwrap();
    let segments: std::collections::BTreeSet<String> =
        Callback::ALL.into_iter().map(|callback| random.url(callback)).collect();
    assert_eq!(
        segments.len(),
        Callback::ALL.len(),
        "every link has a segment of its own"
    );
}

#[tokio::test]
async fn the_call_start_is_the_web_clients_test_call() {
    let call = permitted_test_call().await.unwrap();
    let (url, captured) = test_call_start().unwrap();
    let offer = NgcSdp::new(captured["callInvitation"]["mediaContent"]["blob"].as_str().unwrap()).unwrap();
    let body = call_start_body(&call, &me(), &offer, &fixture_links().unwrap());

    assert_eq!(CALL_START_URL, url);
    assert_eq!(
        body["participants"], captured["participants"],
        "no callee: `to` is empty"
    );
    assert_eq!(body["conversationRequest"]["scenario"], TEST_CALL_SCENARIO);
    assert_eq!(
        body["conversationRequest"]["links"],
        captured["conversationRequest"]["links"]
    );
    assert_eq!(
        body["conversationRequest"]["roster"],
        captured["conversationRequest"]["roster"]
    );
    assert_eq!(body["callInvitation"]["links"], captured["callInvitation"]["links"]);
    assert_eq!(body["callInvitation"]["mediaContent"]["contentType"], SDP_NGC);
    assert_eq!(
        body["callInvitation"]["mediaContent"]["blob"],
        captured["callInvitation"]["mediaContent"]["blob"]
    );
    assert_eq!(body["callInvitation"]["callModalities"], json!(["Audio"]));
    assert!(
        keys_within(&body, &captured),
        "every key sent is one the client sent: {body:#}"
    );
}

#[tokio::test]
async fn the_browser_fence_maps_the_native_start_to_the_permitted_target() {
    let call = permitted_test_call().await.unwrap();
    let (_, captured) = test_call_start().unwrap();
    let offer = NgcSdp::new(captured["callInvitation"]["mediaContent"]["blob"].as_str().unwrap()).unwrap();
    let body = serde_json::to_vec(&call_start_body(&call, &me(), &offer, &fixture_links().unwrap())).unwrap();
    let url = url::Url::parse(CALL_START_URL).unwrap();

    let mapped = proto_lab::calling::call_start("POST", &url, Some(&body), Some(&Mri::new(ECHO)));
    assert_eq!(mapped.as_ref(), Some(&call.request().target()));
    assert_eq!(TEST_CALL_SCENARIO, proto_lab::calling::TEST_CALL_SCENARIO);
}

#[test]
fn a_browser_sdp_is_not_the_teams_dialect() {
    let browser = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=rtpmap:111 opus/48000/2\r\n";
    assert!(NgcSdp::new(browser).is_err(), "str0m's or a browser's offer as is");
    assert!(
        NgcSdp::new("m=audio 50000 RTP/SAVP 111\r\n").is_err(),
        "no session part"
    );
    assert!(NgcSdp::new("v=0\r\ns=-\r\n").is_err(), "no media line");
    let answer = answers().unwrap()["acceptance_callback"]["body"]["callAcceptance"]["mediaContent"]["blob"].clone();
    assert!(NgcSdp::new(answer.as_str().unwrap()).is_ok());
}

#[test]
fn the_start_answer_names_the_conversation_its_add_and_its_leave() {
    let conversation = Conversation::from_answer(&answers().unwrap()["call_start_answer"]).unwrap();
    assert_eq!(conversation.controller.id(), "SyntheticConversation0");
    assert_eq!(conversation.scenario.as_deref(), Some(TEST_CALL_SCENARIO));
    let (add_url, _) = request(ADD).unwrap();
    assert_eq!(
        AddToCall::new(conversation.controller.clone(), Mri::new(ECHO))
            .url()
            .as_str(),
        add_url,
        "the client posts to …/add, not to the addParticipant link"
    );
    let (leave_url, _) = request("leaving the call (a call follow-up)").unwrap();
    assert_eq!(conversation.link("leave"), Some(leave_url.as_str()));
    assert!(conversation.link("getAllParticipants").is_some());

    assert_eq!(
        Conversation::from_answer(&json!({ "links": {} })),
        None,
        "no controller"
    );
}

#[test]
fn an_answer_whose_controller_has_no_conversation_id_is_not_a_conversation() {
    for controller in [
        "https://api.flightproxy.teams.microsoft.com",
        "https://api.flightproxy.teams.microsoft.com/",
        "https://api.flightproxy.teams.microsoft.com/conv/?i=0-0-0-0&e=1",
        "https://api.flightproxy.teams.microsoft.com:443",
        "http://api.flightproxy.teams.microsoft.com/conv/SyntheticConversation0",
        "https://api.flightproxy.teams.microsoft.com/conv/SyntheticConversation0#x",
        "SyntheticConversation0",
    ] {
        let answer = json!({ "conversationController": controller, "links": {} });
        assert_eq!(Conversation::from_answer(&answer), None, "{controller}");
    }
}

/// The invitation of the fixtures' `add`.
fn invitation() -> Invitation {
    Invitation {
        participant_id: "<guid:6>".into(),
        chat_thread: "19:<guid:a>_<guid:echo>@unq.gbl.spaces".into(),
        tenant_id: "<guid:7>".into(),
    }
}

async fn permitted_add(conversation: &ConversationUrl) -> TestResult<Authorized<AddToCall>> {
    let add = AddToCall::new(conversation.clone(), Mri::new(ECHO));
    let permit = fence().check(add.target(), &CallOfLabA).await?;
    Ok(Authorized::new(add, permit)?)
}

fn add_of(add: &Authorized<AddToCall>) -> TestResult<WireRequest> {
    add_request(add, &Mri::new(LAB_A), &me(), &invitation(), &fixture_links()?).ok_or_else(|| "no add".into())
}

#[tokio::test]
async fn the_add_is_the_web_clients_add_once_the_fence_could_permit_it() {
    let conversation = Conversation::from_answer(&answers().unwrap()["call_start_answer"]).unwrap();
    let add = permitted_add(&conversation.controller).await.unwrap();
    let WireRequest { url, body } = add_of(&add).unwrap();

    let (captured_url, mut captured) = request(ADD).unwrap();
    assert_eq!(url, captured_url, "posted where the client posts it");
    assert_eq!(
        body["links"]["addParticipantSuccess"],
        captured["links"]["addParticipantSuccess"]
    );
    assert!(body["links"]["addParticipantFailure"].is_string());
    let mut ours = body.clone();
    ours.as_object_mut().unwrap().remove("links");
    captured.as_object_mut().unwrap().remove("links");
    assert_eq!(ours, captured);
}

#[tokio::test]
async fn an_add_goes_only_to_the_call_its_permit_was_issued_for() {
    let call_x = ConversationUrl::parse(
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01.example.com/conv/CallX?i=0-0-0-0&e=1",
    )
    .unwrap();
    // the same conversation id on another conversation host
    let call_y = ConversationUrl::parse(
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-02.example.com/conv/CallX?i=0-0-0-0&e=1",
    )
    .unwrap();
    let add_x = permitted_add(&call_x).await.unwrap();
    assert_eq!(
        add_of(&add_x).unwrap().url,
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01.example.com/conv/CallX/add?i=0-0-0-0&e=1"
    );

    // the permit for call X, offered for an add to call Y
    let permit_x = fence()
        .check(AddToCall::new(call_x.clone(), Mri::new(ECHO)).target(), &CallOfLabA)
        .await
        .unwrap();
    let mismatch = Authorized::new(AddToCall::new(call_y.clone(), Mri::new(ECHO)), permit_x)
        .expect_err("a permit for call X carries no add to call Y");
    assert_eq!(
        mismatch.permitted().scope(),
        &Scope::Call {
            id: call_x.as_str().into()
        }
    );
    assert_eq!(
        mismatch.requested().scope(),
        &Scope::Call {
            id: call_y.as_str().into()
        }
    );
}

#[test]
fn the_acceptance_brings_the_remote_sdp_and_the_call_legs_links() {
    let delivery = &answers().unwrap()["acceptance_callback"];
    let links = fixture_links().unwrap();
    let Inbound::Accepted(acceptance) = Inbound::parse(&links, delivery["path"].as_str().unwrap(), &delivery["body"])
    else {
        panic!("expected the acceptance");
    };
    let Acceptance {
        answer,
        acknowledgement,
        call_leg,
    } = acceptance;
    assert!(answer.as_str().contains("m=audio 3478 RTP/SAVP"));
    let (acknowledge_url, _) = request("the acknowledgement of the call's acceptance (a call follow-up)").unwrap();
    assert_eq!(acknowledgement.as_deref(), Some(acknowledge_url.as_str()));
    assert!(call_leg.is_some_and(|leg| leg.contains("/cc/v1/active/")));

    let mut browser = delivery["body"].clone();
    browser["callAcceptance"]["mediaContent"]["blob"] = json!("v=0\r\ns=-\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\n");
    assert_eq!(
        Inbound::parse(&links, delivery["path"].as_str().unwrap(), &browser),
        Inbound::Unknown,
        "an answer that isn't the Teams dialect"
    );
}

#[test]
fn a_delivery_at_a_path_that_is_not_one_of_our_links_is_not_ours() {
    let delivery = &answers().unwrap()["acceptance_callback"];
    let links = fixture_links().unwrap();
    for path in [
        "/call/acceptance/",
        "/v4/f/SyntheticEndpoint00000/callAgent/<guid:2>/09090909/call/acceptance/",
        "/v4/f/SyntheticEndpoint00000/callAgent/<guid:9>/03030303/call/acceptance/",
    ] {
        assert_eq!(
            Inbound::parse(&links, path, &delivery["body"]),
            Inbound::NotOurs,
            "{path}"
        );
    }
    assert_eq!(
        Inbound::parse(&links, &links.path(Callback::Progress), &json!({})),
        Inbound::Progress
    );
    assert_eq!(
        Inbound::parse(&links, &links.path(Callback::End), &json!({})),
        Inbound::Ended { code: None }
    );
    assert_eq!(
        Inbound::parse(&links, &links.path(Callback::Acceptance), &json!({})),
        Inbound::Unknown,
        "an acceptance without media"
    );
}

#[test]
fn an_incoming_call_is_recognized_whatever_its_path() {
    let links = fixture_links().unwrap();
    let body = spike_body("incoming-call.json").unwrap();
    for path in [
        "/unconfirmed/incoming-call".to_owned(),
        links.path(Callback::Acceptance),
        String::new(),
    ] {
        assert_eq!(Inbound::parse(&links, &path, &body), Inbound::IncomingCall, "{path}");
    }
}

/// The deliveries of the test call, in order, each at its own link.
fn deliveries(links: &CallbackLinks) -> TestResult<Vec<(String, Value)>> {
    let acceptance = answers()?["acceptance_callback"]["body"].clone();
    Ok(vec![
        (links.path(Callback::Progress), spike_body("progress.json")?),
        (links.path(Callback::Acceptance), acceptance.clone()),
        // the service delivered the acceptance again when its response was denied (Task 18)
        (links.path(Callback::Acceptance), acceptance),
        (links.path(Callback::End), spike_body("end.json")?),
    ])
}

#[test]
fn the_outgoing_call_follows_its_callbacks() {
    let links = fixture_links().unwrap();
    let mut call = OutgoingCall::new(links.clone());
    let mut script = deliveries(&links).unwrap().into_iter();
    let mut next = |call: &mut OutgoingCall| {
        let (path, body) = script.next().unwrap();
        call.on_delivery(&path, &body)
    };
    assert_eq!(call.state(), CallState::Placing);
    assert_eq!(next(&mut call), Action::Nothing);
    assert_eq!(call.state(), CallState::Ringing);
    let Action::Accept(acceptance) = next(&mut call) else {
        panic!("the acceptance is to be applied and acknowledged");
    };
    assert!(acceptance.answer.as_str().contains("opus/48000/2") && acceptance.acknowledgement.is_some());
    assert_eq!(call.state(), CallState::Accepted);
    assert_eq!(
        next(&mut call),
        Action::Nothing,
        "a redelivered acceptance isn't applied twice"
    );
    assert_eq!(next(&mut call), Action::Hangup);
    assert_eq!(call.state(), CallState::Ended);
    let (path, body) = &deliveries(&links).unwrap()[1];
    assert_eq!(call.on_delivery(path, body), Action::Nothing, "nothing after the end");
}

#[test]
fn another_calls_deliveries_change_nothing() {
    let ours = fixture_links().unwrap();
    let other = CallbackLinks::random(PUSH_URL, AGENT).unwrap();
    let mut call = OutgoingCall::new(ours);
    for (path, body) in deliveries(&other).unwrap() {
        assert_eq!(call.on_delivery(&path, &body), Action::Nothing, "{path}");
        assert_eq!(call.state(), CallState::Placing);
    }
}

#[test]
fn an_incoming_call_is_ignored_in_every_state_and_never_answered() {
    let links = fixture_links().unwrap();
    let incoming = spike_body("incoming-call.json").unwrap();
    let mut call = OutgoingCall::new(links.clone());
    let mut script = deliveries(&links).unwrap().into_iter();
    for step in 0..5 {
        if step > 0 {
            let (path, body) = script.next().unwrap();
            call.on_delivery(&path, &body);
        }
        let state = call.state();
        for path in [
            "/unconfirmed/incoming-call".to_owned(),
            links.path(Callback::Acceptance),
        ] {
            assert_eq!(call.on_delivery(&path, &incoming), Action::Nothing, "step {step}");
            assert_eq!(call.state(), state, "an incoming call changes nothing");
        }
    }
    assert_eq!(
        call.ignored_incoming(),
        8,
        "counted while the call ran; ignored silently after it ended"
    );
}

#[test]
fn trap_tokens_give_the_turn_credentials() {
    let tokens = TrapTokens::from_json(&answers().unwrap()["trap_tokens"]).unwrap();
    assert_eq!(tokens.expires, 604_800, "seconds: 7 days");
    let [token] = tokens.tokens.as_slice() else {
        panic!("one credential");
    };
    assert_eq!(token.credentials.username, "<token:1>");
    assert_eq!(token.credentials.password, "<token:2>");
    assert_eq!(token.realm, "rtcmedia", "the quotes the service keeps are taken off");
    assert!(!format!("{tokens:?}").contains("<token:2>"), "Debug hides the password");

    for malformed in [
        json!({}),
        json!({ "tokens": [], "expires": 1 }),
        json!({ "tokens": [{ "realm": "r", "username": "u" }], "expires": 1 }),
        json!({ "tokens": [{ "realm": "r", "username": "u", "password": "p" }] }),
    ] {
        assert_eq!(TrapTokens::from_json(&malformed), None, "{malformed}");
    }
}
