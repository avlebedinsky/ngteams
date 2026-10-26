//! The calling stack under the fence (`docs/protocol/calling-echo.md`, `docs/protocol/trouter.md`): what sets up the
//! account's own endpoint reaches nobody and passes (its Trouter session's frames, its settings reads, either stack's
//! registration), a delivery's answer is denied, and the web client's test call start is a write judged by its
//! participants, on synthetic requests shaped like the `echo-call` and `ab-trouter` captures' (made-up ids and tokens).

use proto_lab::classify::{Classification, Classifier};
use proto_lab::fence_ws::{Frame, Payload, classify_frame};
use proto_lab::scenario::ab_trouter::hold_activity;
use teams_fence::{CallOperation, Mri, Scope, Target, WriteKind};

const TROUTER: &str = "wss://go.trouter.teams.microsoft.com/v4/c/?tc=%7B%22cv%22%3A%220.0%22%7D&auth=true&epid=00000000-0000-4000-8000-000000000001";

const REGIONAL: &str =
    "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c?tc=%7B%7D&epid=00000000-0000-4000-8000-000000000001";

/// The authentication on the regional socket: the reconnect parameters the service handed over, made up.
fn text(url: &str, data: &str) -> Frame {
    Frame {
        url: url.to_owned(),
        payload: Payload::Text(data.to_owned()),
    }
}

/// The Trouter authentication frame as the web client sends it, with a made-up token.
const AUTHENTICATE: &str = r#"5:::{"name":"user.authenticate","args":[{"headers":{"X-Ms-Test-User":"False","Authorization":"Bearer synthetic.token.value","X-MS-Migration":"True"}}]}"#;

// --- the Trouter session's frames ---

#[test]
fn the_trouter_session_s_own_frames_pass_on_either_socket() {
    for url in [TROUTER, REGIONAL] {
        for data in [
            AUTHENTICATE,
            r#"5:1+::{"name":"ping"}"#,
            r#"5:2+::{"name":"user.activity","args":[{"state":"active","cv":"SyntheticCv.0.1"}]}"#,
            r#"5:3+::{"name":"trouter.processed_message_loss","args":[{"droppedIndicators":[{"tag":"t","etag":"e"}]}]}"#,
            "0::",
            "2::",
        ] {
            assert_eq!(classify_frame(&text(url, data)), Classification::Allow, "{url} {data}");
        }
    }
}

#[test]
fn a_delivery_response_passes_and_an_acknowledgement_or_a_binary_frame_does_not() {
    // the endpoint's answer to something the service delivered goes back to the service that delivered it, a call's
    // receipts and acknowledgements among them: each passes the frame rule
    for data in [
        r#"3:::{"id":2029851735,"status":200,"headers":{"MS-CV":"SyntheticCv.0.1","trouter-request":"{\"id\":\"x\",\"src\":\"y\"}","trouter-client":"{\"cd\":2}"},"body":""}"#,
        r#"3:::{"id":-2129608987,"status":204,"headers":{},"body":""}"#,
        r#"3:::{"id":1,"status":200,"headers":{},"body":"{\"callAcceptanceAcknowledgement\":{}}"}"#,
    ] {
        for url in [TROUTER, REGIONAL] {
            assert_eq!(classify_frame(&text(url, data)), Classification::Allow, "{data}");
        }
    }
    for url in [TROUTER, REGIONAL] {
        assert_eq!(classify_frame(&text(url, "6:::1+[]")), Classification::Unknown);
    }
    let binary = Frame {
        url: REGIONAL.to_owned(),
        payload: Payload::Binary("AQID".to_owned()),
    };
    assert_eq!(classify_frame(&binary), Classification::Unknown);
}

#[test]
fn a_frame_on_a_calling_host_s_socket_is_unknown() {
    for url in [
        "wss://api.flightproxy.teams.microsoft.com/api/v2/ep/conv",
        "wss://pub-ent-zz-01.conv.skype.com/conv/x",
        "wss://api.fl.teams.microsoft.com/x",
    ] {
        assert_eq!(
            classify_frame(&text(url, AUTHENTICATE)),
            Classification::Unknown,
            "{url}"
        );
        assert_eq!(classify_frame(&text(url, "2::")), Classification::Unknown, "{url}");
    }
}

// --- the account's own setup: settings reads and either stack's registration ---

const AGGREGATE_SETTINGS: &str =
    r#"{"userPropertiesSettings":true,"voiceAdminSettings":true,"tenantSettingsV2":true,"clientSettings":true}"#;
const EFFECTIVE_POLICIES: &str = r#"["teamsCallingPolicy","teamsMeetingPolicy","externalAccessPolicy"]"#;

/// The registration of the endpoint's Trouter URL, as the web client posts it (made-up ids).
const REGISTRATION: &str = r#"{"clientDescription":{"appId":"TeamsCDLWebWorker","aesKey":"","languageId":"en-US","platform":"chrome","templateKey":"TeamsCDLWebWorker_2.6","platformUIVersion":"1/1"},"registrationId":"00000000-0000-4000-8000-000000000002","nodeId":"","transports":{"TROUTER":[{"context":"","path":"https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticSessionId00000/","ttl":3600}]}}"#;

const REGISTRAR: [&str; 2] = [
    "https://teams.microsoft.com/registrar/prod/V2/registrations",
    "https://teams.cloud.microsoft/registrar/prod/V2/registrations",
];

#[test]
fn the_calling_stack_s_own_setup_reaches_nobody() {
    let calling = REGISTRATION.replace("TeamsCDLWebWorker", "SkypeSpacesWeb");
    for origin in ["https://teams.microsoft.com", "https://teams.cloud.microsoft"] {
        for (path, body) in [
            (
                "/api/mt/part/amer-03/beta/users/useraggregatesettings",
                AGGREGATE_SETTINGS,
            ),
            ("/api/mt/part/emea-01/beta/users/effectivePolicies", EFFECTIVE_POLICIES),
            // whatever the body: a settings read changes nothing anyone else sees
            ("/api/mt/part/emea-01/beta/users/effectivePolicies", r#"{"anything":1}"#),
            ("/registrar/prod/V2/registrations", REGISTRATION),
            ("/registrar/prod/V2/registrations", calling.as_str()),
        ] {
            let url = format!("{origin}{path}");
            assert_eq!(
                Classifier::new().classify("POST", &url, &[], Some(body.as_bytes())),
                Classification::Allow,
                "{url} {body}"
            );
        }
    }
    for url in REGISTRAR {
        assert_eq!(
            Classifier::new().classify("POST", url, &[], None),
            Classification::Allow,
            "{url}"
        );
    }
}

// --- the call start ---

const CONVERSATIONS: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/epconv";
const CALLER: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const ECHO: &str = "28:00000000-0000-4000-8000-0000000000e0";

/// A call start as the web client posts it (made-up ids and links, the SDP cut to a stub), with `to` and the
/// conversation's scenario filled in.
fn call_start(to: &str, scenario: &str) -> String {
    format!(
        r#"{{"conversationRequest":{{"conversationType":null,"subject":null,"suppressDialout":true,"roster":{{"type":"Delta","rosterUpdate":"https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/x/callAgent/y/1/conversation/rosterUpdate/"}},"properties":{{"allowConversationWithoutHost":true}},"links":{{}}{scenario}}},"contentSharing":null,"participants":{{"from":{{"id":"{CALLER}","displayName":"Lab A","endpointId":"00000000-0000-4000-8000-000000000001","participantId":"00000000-0000-4000-8000-000000000002","languageId":"en-us"}},"to":[{to}]}},"capabilities":null,"endpointCapabilities":1,"clientEndpointCapabilities":1,"endpointMetadata":{{}},"groupContext":null,"groupChat":null,"meetingInfo":null,"meetingData":null,"endpointState":{{}},"callInvitation":{{"callModalities":["Audio"],"links":{{}},"mediaContent":{{"blob":"v=0\r\n","contentType":"application/sdp-ngc-1.0"}}}},"debugContent":{{}},"participantPropertyBag":{{}}}}"#
    )
}

fn test_call() -> String {
    call_start("", r#","scenario":"UserInitiatedTestCall""#)
}

fn classify_start(classifier: &Classifier, body: &str) -> Classification {
    classifier.classify("POST", CONVERSATIONS, &[], Some(body.as_bytes()))
}

#[test]
fn the_test_call_start_is_a_call_with_the_caller_and_the_echo_service() {
    let classifier = Classifier::new().with_test_call_callee(Mri::new(ECHO));
    let expected = Target::new(WriteKind::Call, Scope::New).with_added([Mri::new(CALLER), Mri::new(ECHO)]);
    assert_eq!(
        classify_start(&classifier, &test_call()),
        Classification::Write(expected)
    );
}

#[test]
fn a_test_call_start_is_unknown_without_the_echo_service_s_mri() {
    // the start names nobody but the caller: without knowing whom the call is for, the fence can't judge it
    assert_eq!(
        classify_start(&Classifier::new(), &test_call()),
        Classification::Unknown
    );
}

#[test]
fn a_test_call_start_may_name_the_echo_service_and_nobody_else() {
    let classifier = Classifier::new().with_test_call_callee(Mri::new(ECHO));
    let expected = Target::new(WriteKind::Call, Scope::New).with_added([Mri::new(CALLER), Mri::new(ECHO)]);
    let test_call = r#","scenario":"UserInitiatedTestCall""#;
    for to in [
        format!(r#"{{"id":"{ECHO}"}}"#),
        format!(r#"{{"id":"{ECHO}","participantId":"00000000-0000-4000-8000-000000000003"}}"#),
    ] {
        assert_eq!(
            classify_start(&classifier, &call_start(&to, test_call)),
            Classification::Write(expected.clone()),
            "{to}"
        );
    }
}

#[test]
fn a_call_start_naming_anyone_but_the_echo_service_is_unknown() {
    // Only the test call was captured: a call to named people (a lab account included), with or without a scenario, is
    // a shape nobody reviewed
    let classifier = Classifier::new().with_test_call_callee(Mri::new(ECHO));
    let lab_b = r#"{"id":"8:orgid:00000000-0000-4000-8000-00000000000b","displayName":"B"}"#;
    let echo = format!(r#"{{"id":"{ECHO}"}}"#);
    let test_call = r#","scenario":"UserInitiatedTestCall""#;
    let off = [
        call_start(lab_b, ""),
        call_start(lab_b, test_call),
        call_start(&echo, ""),
        call_start(&format!("{echo},{lab_b}"), test_call),
        call_start(&format!("{echo},{echo}"), test_call),
        call_start(r#"{"id":"28:00000000-0000-4000-8000-0000000000e1"}"#, test_call),
        // the echo service, with something about it nobody reviewed
        call_start(&format!(r#"{{"id":"{ECHO}","displayName":"Echo"}}"#), test_call),
        call_start(&format!(r#"{{"id":"{ECHO}","participantId":1}}"#), test_call),
    ];
    for body in &off {
        assert_eq!(classify_start(&classifier, body), Classification::Unknown, "{body}");
    }
}

#[test]
fn a_call_start_off_the_reviewed_shape_is_unknown() {
    let classifier = Classifier::new().with_test_call_callee(Mri::new(ECHO));
    let callee = r#"{"id":"8:orgid:00000000-0000-4000-8000-00000000000b"}"#;
    let off = [
        // a test call that names callees too, or another scenario with nobody to call
        call_start(callee, r#","scenario":"UserInitiatedTestCall""#),
        call_start("", r#","scenario":"SomethingElse""#),
        call_start("", ""),
        // into an existing group, meeting or chat: not a new target
        test_call().replace(
            r#""groupContext":null"#,
            r#""groupContext":{"threadId":"19:x@thread.v2"}"#,
        ),
        test_call().replace(r#""groupChat":null"#, r#""groupChat":{"threadId":"19:x@thread.v2"}"#),
        test_call().replace(r#""meetingInfo":null"#, r#""meetingInfo":{"organizerId":"x"}"#),
        test_call().replace(r#""meetingData":null"#, r#""meetingData":{}"#),
        test_call().replace(r#""conversationType":null"#, r#""conversationType":"GroupCall""#),
        // sharing content into something: a target the fence never counts
        test_call().replace(
            r#""contentSharing":null"#,
            r#""contentSharing":{"threadId":"19:x@thread.v2"}"#,
        ),
        // a participant beside the caller and the callees
        test_call().replace(r#""to":[]"#, r#""to":[],"cc":[{"id":"8:orgid:x"}]"#),
        // a transfer or replacement of a call that exists: someone the fence never read
        test_call().replace(
            r#""callModalities":["Audio"]"#,
            r#""callModalities":["Audio"],"replaces":{"callId":"x"}"#,
        ),
        test_call().replace(
            r#""callModalities":["Audio"]"#,
            r#""callModalities":["Audio"],"transferor":{"id":"8:orgid:x"}"#,
        ),
        test_call().replace(
            r#""callModalities":["Audio"]"#,
            r#""callModalities":["Audio"],"clientTransferContext":"x""#,
        ),
        // a key nobody reviewed in the conversation request, the invitation, its media or the caller
        test_call().replace(r#""subject":null"#, r#""subject":null,"invitees":["8:orgid:x"]"#),
        test_call().replace(
            r#""callModalities":["Audio"]"#,
            r#""callModalities":["Audio"],"target":"8:orgid:x""#,
        ),
        test_call().replace(
            r#""blob":"v=0\r\n""#,
            r#""blob":"v=0\r\n","participants":["8:orgid:x"]"#,
        ),
        test_call().replace(
            r#""languageId":"en-us""#,
            r#""languageId":"en-us","onBehalfOf":"8:orgid:x""#,
        ),
        // a top-level key nobody reviewed
        test_call().replace(
            r#""contentSharing":null,"#,
            r#""contentSharing":null,"invitees":["8:orgid:x"],"#,
        ),
        // a callee that isn't an object with an id, the caller missing
        call_start(r#""8:orgid:x""#, ""),
        call_start(r#"{"displayName":"B"}"#, ""),
        test_call().replace(&format!(r#""id":"{CALLER}","#), ""),
        // keys alike in some case, trailing data, not JSON
        test_call().replace(r#""groupChat":null"#, r#""groupChat":null,"GroupChat":{}"#),
        format!("{}{{}}", test_call()),
        "{".to_owned(),
    ];
    for body in &off {
        assert_eq!(classify_start(&classifier, body), Classification::Unknown, "{body}");
    }
    for (method, url) in [
        ("PUT", CONVERSATIONS),
        ("POST", "https://api.flightproxy.teams.microsoft.com/api/v2/epconv/x"),
        ("POST", "http://api.flightproxy.teams.microsoft.com/api/v2/epconv"),
        // the captured start has no query: one it didn't show makes the request unknown
        ("POST", "https://api.flightproxy.teams.microsoft.com/api/v2/epconv?x=1"),
        ("POST", "https://api.flightproxy.teams.microsoft.com/api/v2/epconv?"),
    ] {
        assert_eq!(
            classifier.classify(method, url, &[], Some(test_call().as_bytes())),
            Classification::Unknown,
            "{method} {url}"
        );
    }
    // a lookalike host is no contacting service: whatever goes there reaches nobody of the tenant
    assert_eq!(
        classifier.classify(
            "POST",
            "https://api.flightproxy.teams.microsoft.com.example.com/api/v2/epconv",
            &[],
            Some(test_call().as_bytes())
        ),
        Classification::Allow
    );
}

// --- the captured call, from the synthetic fixtures ---

const REQUESTS: &str = include_str!("../../../tests/fixtures/calling/echo/requests.json");
const FRAMES: &str = include_str!("../../../tests/fixtures/calling/echo/frames.json");

const LAB_A: &str = "00000000-0000-4000-8000-00000000000a";
const ECHO_GUID: &str = "00000000-0000-4000-8000-0000000000e0";

/// The fixture text with its placeholders filled in: a's and b's object ids, the tenant's, the echo bot's id, and a
/// made-up GUID per number.
fn filled(text: &str) -> String {
    let mut text = text
        .replace("<guid:a>", LAB_A)
        .replace("<guid:b>", "00000000-0000-4000-8000-00000000000b")
        .replace("<guid:tenant>", "00000000-0000-4000-8000-0000000000f0")
        .replace("<guid:echo>", ECHO_GUID);
    for number in 1..=40 {
        text = text.replace(
            &format!("<guid:{number}>"),
            &format!("00000000-0000-4000-8000-{number:012}"),
        );
    }
    text
}

/// What a fixture case's `expected` names; `None` for an expectation the test doesn't know.
fn expected(name: &str) -> Option<Classification> {
    let participants = |list: &str| list.split(',').map(Mri::new).collect::<Vec<_>>();
    Some(match name.split_once(':') {
        _ if name == "read" => Classification::Read,
        _ if name == "unknown" => Classification::Unknown,
        _ if name == "allow" => Classification::Allow,
        Some(("chat_create", list)) => {
            Classification::Write(Target::new(WriteKind::ChatCreate, Scope::New).with_added(participants(list)))
        }
        Some(("call", list)) => {
            Classification::Write(Target::new(WriteKind::Call, Scope::New).with_added(participants(list)))
        }
        _ => return None,
    })
}

/// What a request fixture case's `expected` names, its URL `url` and body `body`: [`expected`]'s, or an operation on a
/// registered call (`call_operation|<operation>[|<sender>[|<invitee>…]]`), its scope that operation on `url`, with the
/// callback links its body hands out (an acknowledgement's or an `add`'s).
fn expected_at(name: &str, url: &str, body: &serde_json::Value) -> Option<Classification> {
    let Some(rest) = name.strip_prefix("call_operation|") else {
        return expected(name);
    };
    let mut parts = rest.split('|');
    let name = parts.next()?;
    let operation = CallOperation::ALL
        .into_iter()
        .find(|operation| operation.as_str() == name)?;
    let from = parts.next().map(Mri::new);
    let links = match operation {
        CallOperation::Acknowledge => body.pointer("/callAcceptanceAcknowledgement/links"),
        CallOperation::Add => body.get("links"),
        _ => None,
    };
    let callbacks: Box<[String]> = links
        .and_then(serde_json::Value::as_object)
        .map(|links| {
            links
                .values()
                .filter_map(|link| link.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let scope = Scope::CallOperation {
        operation,
        url: url::Url::parse(url).ok()?.to_string(),
        from,
        callbacks,
        joins: None,
    };
    Some(Classification::Write(
        Target::new(operation.kind(), scope).with_added(parts.map(Mri::new)),
    ))
}

#[test]
fn every_captured_request_of_the_test_call_is_what_the_fixture_says() {
    let fixture: serde_json::Value = serde_json::from_str(&filled(REQUESTS)).unwrap();
    let classifier = Classifier::new().with_test_call_callee(Mri::new(format!("28:{ECHO_GUID}")));
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        let body = (!case["body"].is_null()).then(|| case["body"].to_string());
        let got = classifier.classify(
            case["method"].as_str().unwrap(),
            case["url"].as_str().unwrap(),
            &[],
            body.as_deref().map(str::as_bytes),
        );
        let want = expected_at(
            case["expected"].as_str().unwrap(),
            case["url"].as_str().unwrap(),
            &case["body"],
        )
        .expect("an expectation the test knows");
        assert_eq!(got, want, "{}", case["name"]);
    }
}

#[test]
fn every_captured_frame_of_the_test_call_is_what_the_fixture_says() {
    let fixture: serde_json::Value = serde_json::from_str(FRAMES).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 8);
    for case in cases {
        let socket = fixture["sockets"][case["socket"].as_str().unwrap()].as_str().unwrap();
        let got = classify_frame(&text(socket, case["frame"].as_str().unwrap()));
        let want = expected(case["expected"].as_str().unwrap()).expect("an expectation the test knows");
        assert_eq!(got, want, "{}", case["name"]);
    }
}

/// The fixture's test call start, filled in.
fn captured_start() -> Option<serde_json::Value> {
    let fixture: serde_json::Value = serde_json::from_str(&filled(REQUESTS)).ok()?;
    let cases = fixture["cases"].as_array()?;
    let start = cases.iter().find(|case| case["url"] == CONVERSATIONS)?;
    Some(start["body"].clone())
}

/// The JSON pointer of every value inside `value` (`at` itself included), objects and arrays walked into.
fn pointers(value: &serde_json::Value, at: &str, found: &mut Vec<String>) {
    found.push(at.to_owned());
    match value {
        serde_json::Value::Object(object) => {
            for (key, inner) in object {
                let key = key.replace('~', "~0").replace('/', "~1");
                pointers(inner, &format!("{at}/{key}"), found);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, inner) in items.iter().enumerate() {
                pointers(inner, &format!("{at}/{index}"), found);
            }
        }
        _ => {}
    }
}

fn classify_captured(start: &serde_json::Value) -> Classification {
    let classifier = Classifier::new().with_test_call_callee(Mri::new(format!("28:{ECHO_GUID}")));
    classify_start(&classifier, &start.to_string())
}

#[test]
fn every_object_of_the_test_call_start_refuses_a_key_nobody_reviewed() {
    let start = captured_start().unwrap();
    assert!(matches!(classify_captured(&start), Classification::Write(_)));
    let mut found = Vec::new();
    pointers(&start, "", &mut found);
    let mut objects = 0;
    for pointer in &found {
        let mut changed = start.clone();
        let Some(object) = changed.pointer_mut(pointer).and_then(serde_json::Value::as_object_mut) else {
            continue;
        };
        object.insert("unreviewed".to_owned(), serde_json::json!("8:orgid:x"));
        objects += 1;
        assert_eq!(classify_captured(&changed), Classification::Unknown, "{pointer:?}");
    }
    // the start, the conversation request and its roster, properties and links, the participants and the caller, the
    // invitation and its links, media content, descriptions, channel parameters, and the rest
    assert!(objects >= 20, "{objects}");
}

#[test]
fn every_value_of_the_test_call_start_is_typed_as_captured() {
    let start = captured_start().unwrap();
    let mut found = Vec::new();
    pointers(&start, "", &mut found);
    for pointer in found.iter().filter(|pointer| !pointer.is_empty()) {
        let mut changed = start.clone();
        let value = changed.pointer_mut(pointer).unwrap();
        *value = match value {
            serde_json::Value::Null => serde_json::json!({"threadId": "19:x@thread.v2"}),
            serde_json::Value::Bool(_) => serde_json::json!("true"),
            serde_json::Value::Number(_) => serde_json::json!("1"),
            serde_json::Value::String(_) => serde_json::json!(1),
            serde_json::Value::Array(_) => serde_json::json!({}),
            serde_json::Value::Object(_) => serde_json::json!([]),
        };
        assert_eq!(classify_captured(&changed), Classification::Unknown, "{pointer:?}");
    }
}

// --- the messaging session and deliveries, from the synthetic fixtures (`tests/fixtures/trouter/`) ---

const SESSION: &str = include_str!("../../../tests/fixtures/trouter/session.json");
const REGISTRATIONS: &str = include_str!("../../../tests/fixtures/trouter/registration.json");
const DELIVERIES: &str = include_str!("../../../tests/fixtures/trouter/deliveries.json");

#[test]
fn every_frame_the_client_sends_in_the_captured_session_is_what_the_fixture_says() {
    let fixture: serde_json::Value = serde_json::from_str(SESSION).unwrap();
    let frames = fixture["frames"].as_array().unwrap();
    let sent: Vec<_> = frames.iter().filter(|frame| frame["direction"] == "out").collect();
    assert!(sent.len() >= 6 && frames.len() > sent.len());
    for case in sent {
        let socket = fixture["sockets"][case["socket"].as_str().unwrap()].as_str().unwrap();
        let frame = text(socket, case["frame"].as_str().unwrap());
        let want = expected(case["expected"].as_str().unwrap()).expect("an expectation the test knows");
        assert_eq!(classify_frame(&frame), want, "{}", case["name"]);
        // the quiet run holds back what the fixture marks held, and nothing else
        let held = case["expected_quiet"].as_str() == Some("held");
        assert_eq!(hold_activity().unwrap().matches(&frame), held, "{}", case["name"]);
    }
}

#[test]
fn every_captured_registration_is_what_the_fixture_says() {
    let fixture: serde_json::Value = serde_json::from_str(&filled(REGISTRATIONS)).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 3);
    for case in cases {
        let body = case["body"].to_string();
        let got = Classifier::new().classify(
            case["method"].as_str().unwrap(),
            case["url"].as_str().unwrap(),
            &[],
            Some(body.as_bytes()),
        );
        let want = expected(case["expected"].as_str().unwrap()).expect("an expectation the test knows");
        assert_eq!(got, want, "{}", case["name"]);
    }
}

#[test]
fn chat_deliveries_come_on_the_messaging_path_and_their_answers_pass() {
    let fixture: serde_json::Value = serde_json::from_str(&filled(DELIVERIES)).unwrap();
    let socket = fixture["socket"].as_str().unwrap();
    let cases = fixture["deliveries"].as_array().unwrap();
    assert!(cases.len() >= 5);
    for case in cases {
        let name = &case["name"];
        let delivery: serde_json::Value =
            serde_json::from_str(case["frame"].as_str().unwrap().strip_prefix("3:::").unwrap()).unwrap();
        assert!(delivery["id"].is_u64(), "{name}");
        assert_eq!(delivery["method"], "POST", "{name}");
        assert!(delivery["url"].as_str().unwrap().ends_with("/messaging"), "{name}");
        let control = delivery["headers"]["X-Trouter-Delivery-Control"].as_str().unwrap();
        assert!(
            control.starts_with("async;") && control.contains("flow=messaging"),
            "{name}"
        );
        let body: serde_json::Value = serde_json::from_str(delivery["body"].as_str().unwrap()).unwrap();
        assert_eq!(body["type"], "EventMessage", "{name}");
        assert_eq!(body["resourceType"], case["resource_type"], "{name}");
        // the web client's answer: a socket.io message, which the frame rule lets through
        let answer = text(socket, case["answer"].as_str().unwrap());
        let answered: serde_json::Value =
            serde_json::from_str(case["answer"].as_str().unwrap().strip_prefix("3:::").unwrap()).unwrap();
        assert_eq!(answered["id"], delivery["id"], "{name}");
        assert_eq!(classify_frame(&answer), Classification::Allow, "{name}");
    }
}
