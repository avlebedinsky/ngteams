//! The calling stack under the fence (M0 Task 18, `docs/protocol/calling-echo.md`): the own-state of a run that calls
//! (the Trouter session's frames) and the mapping of the call's writes, on synthetic requests shaped like the
//! `echo-call` capture's (made-up ids and tokens).

use proto_lab::classify::{Classification, Classifier, OwnKind};
use proto_lab::fence_ws::{Frame, FrameClassifier, Payload};
use teams_fence::{Mri, Scope, Target, WriteKind};

const TROUTER: &str = "wss://go.trouter.teams.microsoft.com/v4/c/?tc=%7B%22cv%22%3A%220.0%22%7D&auth=true&epid=00000000-0000-4000-8000-000000000001";

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
fn the_trouter_authentication_is_own_state_of_a_calling_run_only() {
    let frame = text(TROUTER, AUTHENTICATE);
    assert_eq!(
        FrameClassifier::calling().classify(&frame),
        Classification::Own(OwnKind::TrouterSession)
    );
    assert_eq!(FrameClassifier::seeded().classify(&frame), Classification::Unknown);
}

#[test]
fn an_authentication_anywhere_but_the_trouter_socket_is_unknown() {
    for url in [
        "wss://go.trouter.teams.microsoft.com/v4/c/extra",
        "wss://go.trouter.teams.microsoft.com/v4/c//",
        "wss://go.trouter.teams.microsoft.com/v3/c/",
        "ws://go.trouter.teams.microsoft.com/v4/c/",
        "wss://go.trouter.teams.microsoft.com.example.com/v4/c/",
        "wss://augloop.office.com/v4/c/",
        "wss://go.trouter.teams.microsoft.com:444/v4/c/",
        "not a url",
    ] {
        assert_eq!(
            FrameClassifier::calling().classify(&text(url, AUTHENTICATE)),
            Classification::Unknown,
            "{url}"
        );
    }
}

#[test]
fn only_the_exact_authentication_shape_is_own_state() {
    let frames = [
        // another event
        r#"5:::{"name":"user.activity","args":[{"state":"active"}]}"#,
        // trailing data after the value
        &format!("{AUTHENTICATE} "),
        &format!("{AUTHENTICATE}{{}}"),
        // a message, not an event
        r#"3:::{"name":"user.authenticate","args":[{"headers":{"Authorization":"Bearer x"}}]}"#,
        // an event with an id or an endpoint
        r#"5:1::{"name":"user.authenticate","args":[{"headers":{"Authorization":"Bearer x"}}]}"#,
        r#"5::/x:{"name":"user.authenticate","args":[{"headers":{"Authorization":"Bearer x"}}]}"#,
        // two names, alike in some case
        r#"5:::{"name":"user.authenticate","Name":"x","args":[{"headers":{"Authorization":"Bearer x"}}]}"#,
        // a key beside name and args
        r#"5:::{"name":"user.authenticate","args":[{"headers":{"Authorization":"Bearer x"}}],"to":"8:orgid:x"}"#,
        // two args, or something beside the headers
        r#"5:::{"name":"user.authenticate","args":[{"headers":{"Authorization":"Bearer x"}},{}]}"#,
        r#"5:::{"name":"user.authenticate","args":[{"headers":{"Authorization":"Bearer x"},"body":"hi"}]}"#,
        // a header not on the reviewed list, or not a string
        r#"5:::{"name":"user.authenticate","args":[{"headers":{"Authorization":"Bearer x","X-Forward-To":"y"}}]}"#,
        r#"5:::{"name":"user.authenticate","args":[{"headers":{"Authorization":["Bearer x"]}}]}"#,
        // no authorization at all
        r#"5:::{"name":"user.authenticate","args":[{"headers":{"X-Ms-Test-User":"False"}}]}"#,
        // not JSON
        "5:::user.authenticate",
        "",
    ];
    for data in frames {
        assert_eq!(
            FrameClassifier::calling().classify(&text(TROUTER, data)),
            Classification::Unknown,
            "{data}"
        );
    }
}

#[test]
fn a_binary_frame_on_the_trouter_socket_is_unknown() {
    let frame = Frame {
        url: TROUTER.to_owned(),
        payload: Payload::Binary("NTo6Ont9".to_owned()),
    };
    assert_eq!(FrameClassifier::calling().classify(&frame), Classification::Unknown);
}

/// The regional Trouter socket the client moves to, as the service names it.
const REGIONAL: &str =
    "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c?tc=%7B%7D&epid=00000000-0000-4000-8000-000000000001";

/// The authentication on the regional socket: the reconnect parameters the service handed over, made up.
const AUTHENTICATE_REGIONAL: &str = r#"5:::{"name":"user.authenticate","args":[{"headers":{"X-Ms-Test-User":"False","Authorization":"Bearer synthetic.token.value","X-MS-Migration":"True"},"connectparams":{"issuer":"","scae":"1","sig":"","sr":"SyntheticSessionId00000","sp":"pub-ent-zz-01","se":"1700000600000","st":"1700000000000"}}]}"#;

#[test]
fn the_regional_trouter_socket_takes_the_same_session_frames() {
    for (url, data) in [
        (REGIONAL, AUTHENTICATE_REGIONAL),
        (REGIONAL, AUTHENTICATE),
        (TROUTER, AUTHENTICATE_REGIONAL),
        (TROUTER, r#"5:1+::{"name":"ping"}"#),
        (REGIONAL, r#"5:27+::{"name":"ping"}"#),
        (TROUTER, "0::"),
        // the acknowledgement of the service's report of messages lost while the socket was away
        (
            REGIONAL,
            r#"5:2+::{"name":"trouter.processed_message_loss","args":[{"droppedIndicators":[{"tag":"","etag":"2000-01-01T00:00:00.0000000Z"},{"tag":"messaging","etag":"2000-01-01T00:00:00.0000001Z"}]}]}"#,
        ),
        // the client's own activity, which keeps the account "Available"
        (
            REGIONAL,
            r#"5:4+::{"name":"user.activity","args":[{"state":"active","cv":"SyntheticCorrelation0.0.1"}]}"#,
        ),
        (
            TROUTER,
            r#"5:5+::{"name":"user.activity","args":[{"state":"inactive","cv":"SyntheticCorrelation0.0.2"}]}"#,
        ),
    ] {
        assert_eq!(
            FrameClassifier::calling().classify(&text(url, data)),
            Classification::Own(OwnKind::TrouterSession),
            "{url} {data}"
        );
    }
}

#[test]
fn other_session_frames_are_unknown() {
    for (url, data) in [
        // a Trouter-looking host elsewhere, or two labels deep
        (
            "wss://pub.trouter.teams.microsoft.com.example.com/v4/c",
            r#"5:1+::{"name":"ping"}"#,
        ),
        ("wss://a.b.trouter.teams.microsoft.com/v4/c", r#"5:1+::{"name":"ping"}"#),
        ("wss://trouter.teams.microsoft.com/v4/c", r#"5:1+::{"name":"ping"}"#),
        // a ping with more in it, without an id, or with a bad id
        (TROUTER, r#"5:1+::{"name":"ping","args":[1]}"#),
        (TROUTER, r#"5:::{"name":"ping"}"#),
        (TROUTER, r#"5:x+::{"name":"ping"}"#),
        (TROUTER, r#"5:1+:/x:{"name":"ping"}"#),
        // a disconnect of an endpoint, or with data
        (TROUTER, "0::/x"),
        (TROUTER, "0:: "),
        // reconnect parameters outside the reviewed list, or not strings
        (
            TROUTER,
            r#"5:::{"name":"user.authenticate","args":[{"headers":{"Authorization":"Bearer x"},"connectparams":{"to":"8:orgid:x"}}]}"#,
        ),
        (
            TROUTER,
            r#"5:::{"name":"user.authenticate","args":[{"headers":{"Authorization":"Bearer x"},"connectparams":{"sr":1}}]}"#,
        ),
        // a loss report acknowledged with more than its indicators, or an indicator with more than a tag and an etag
        (
            TROUTER,
            r#"5:2+::{"name":"trouter.processed_message_loss","args":[{"droppedIndicators":[],"to":"x"}]}"#,
        ),
        (
            TROUTER,
            r#"5:2+::{"name":"trouter.processed_message_loss","args":[{"droppedIndicators":[{"tag":"","etag":"","body":"hi"}]}]}"#,
        ),
        // an indicator without its etag, an activity without its correlation id
        (
            TROUTER,
            r#"5:2+::{"name":"trouter.processed_message_loss","args":[{"droppedIndicators":[{"tag":""}]}]}"#,
        ),
        (TROUTER, r#"5:4+::{"name":"user.activity","args":[{"state":"active"}]}"#),
        // reconnect parameters short of the ones the service hands out
        (
            REGIONAL,
            r#"5:::{"name":"user.authenticate","args":[{"headers":{"Authorization":"Bearer x"},"connectparams":{"sr":"x"}}]}"#,
        ),
        // an activity with a state nobody reviewed (a status message, a presence it sets), or more in it
        (
            TROUTER,
            r#"5:4+::{"name":"user.activity","args":[{"state":"busy","cv":"x"}]}"#,
        ),
        (
            TROUTER,
            r#"5:4+::{"name":"user.activity","args":[{"state":"active","cv":"x","note":"out today"}]}"#,
        ),
        (
            TROUTER,
            r#"5:4+::{"name":"user.activity","args":[{"state":"active","cv":"x"},{}]}"#,
        ),
        // an event nobody reviewed
        (
            TROUTER,
            r#"5:4+::{"name":"user.message","args":[{"to":"8:orgid:x","text":"hi"}]}"#,
        ),
        // a message with content, or an acknowledgement
        (TROUTER, r#"3:::{"id":1,"status":200,"headers":{},"body":"x"}"#),
        (TROUTER, "6:::1+[]"),
    ] {
        assert_eq!(
            FrameClassifier::calling().classify(&text(url, data)),
            Classification::Unknown,
            "{url} {data}"
        );
    }
}

// --- the account's own settings, read by POST ---

const AGGREGATE_SETTINGS: &str =
    r#"{"userPropertiesSettings":true,"voiceAdminSettings":true,"tenantSettingsV2":true,"clientSettings":true}"#;
const EFFECTIVE_POLICIES: &str = r#"["teamsCallingPolicy","teamsMeetingPolicy","externalAccessPolicy"]"#;

#[test]
fn reading_the_account_s_own_settings_and_policies_is_own_state_of_a_calling_run_only() {
    for origin in ["https://teams.microsoft.com", "https://teams.cloud.microsoft"] {
        for (path, body) in [
            (
                "/api/mt/part/amer-03/beta/users/useraggregatesettings",
                AGGREGATE_SETTINGS,
            ),
            ("/api/mt/part/emea-01/beta/users/effectivePolicies", EFFECTIVE_POLICIES),
        ] {
            let url = format!("{origin}{path}");
            assert_eq!(
                Classifier::calling().classify("POST", &url, Some(body.as_bytes())),
                Classification::Own(OwnKind::SettingsRead),
                "{url}"
            );
            assert_eq!(
                Classifier::mapped().classify("POST", &url, Some(body.as_bytes())),
                Classification::Unknown,
                "{url}"
            );
        }
    }
}

#[test]
fn a_settings_read_off_the_reviewed_shape_is_unknown() {
    let settings = "https://teams.microsoft.com/api/mt/part/amer-03/beta/users/useraggregatesettings";
    let policies = "https://teams.microsoft.com/api/mt/part/amer-03/beta/users/effectivePolicies";
    for (method, url, body) in [
        // a value that isn't a flag, or a list of anything but names
        ("POST", settings, r#"{"clientSettings":{"set":1}}"#),
        ("POST", settings, r#"{"clientSettings":"x"}"#),
        ("POST", settings, EFFECTIVE_POLICIES),
        (
            "POST",
            policies,
            r#"["teamsCallingPolicy",{"allowPrivateCalling":true}]"#,
        ),
        ("POST", policies, AGGREGATE_SETTINGS),
        ("POST", policies, "[]"),
        ("POST", settings, "{}"),
        // a setting group or a policy nobody reviewed
        ("POST", settings, r#"{"clientSettings":true,"updateUserSettings":true}"#),
        ("POST", policies, r#"["teamsCallingPolicy","setTeamsCallingPolicy"]"#),
        // keys alike in some case, trailing data
        ("POST", settings, r#"{"clientSettings":true,"ClientSettings":true}"#),
        ("POST", policies, r#"["x"] ["y"]"#),
        // another user's, another method, another path, another origin, another partition shape
        (
            "POST",
            "https://teams.microsoft.com/api/mt/part/amer-03/beta/users/8:orgid:x/useraggregatesettings",
            AGGREGATE_SETTINGS,
        ),
        ("PUT", settings, AGGREGATE_SETTINGS),
        (
            "POST",
            "https://teams.microsoft.com/api/mt/part/amer-03/beta/users/useraggregatesettings/x",
            AGGREGATE_SETTINGS,
        ),
        (
            "POST",
            "https://example.com/api/mt/part/amer-03/beta/users/useraggregatesettings",
            AGGREGATE_SETTINGS,
        ),
        (
            "POST",
            "https://teams.microsoft.com/api/mt/part/amer/beta/users/effectivePolicies",
            EFFECTIVE_POLICIES,
        ),
        (
            "POST",
            "https://teams.microsoft.com/api/mt/part/../beta/users/effectivePolicies",
            EFFECTIVE_POLICIES,
        ),
        // the captured reads have no query: one they didn't show makes the request unknown
        (
            "POST",
            "https://teams.microsoft.com/api/mt/part/amer-03/beta/users/useraggregatesettings?x=1",
            AGGREGATE_SETTINGS,
        ),
        (
            "POST",
            "https://teams.cloud.microsoft/api/mt/part/amer-03/beta/users/effectivePolicies?",
            EFFECTIVE_POLICIES,
        ),
    ] {
        assert_eq!(
            Classifier::calling().classify(method, url, Some(body.as_bytes())),
            Classification::Unknown,
            "{method} {url} {body}"
        );
    }
    assert_eq!(
        Classifier::calling().classify("POST", settings, None),
        Classification::Unknown
    );
}

// --- the endpoint's registration ---

/// The registration of the endpoint's Trouter URL, as the web client posts it (made-up ids).
const REGISTRATION: &str = r#"{"clientDescription":{"appId":"TeamsCDLWebWorker","aesKey":"","languageId":"en-US","platform":"chrome","templateKey":"TeamsCDLWebWorker_2.6","platformUIVersion":"1/1"},"registrationId":"00000000-0000-4000-8000-000000000002","nodeId":"","transports":{"TROUTER":[{"context":"","path":"https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticSessionId00000/","ttl":3600}]}}"#;

const REGISTRAR: [&str; 2] = [
    "https://teams.microsoft.com/registrar/prod/V2/registrations",
    "https://teams.cloud.microsoft/registrar/prod/V2/registrations",
];

#[test]
fn registering_the_endpoint_s_trouter_url_is_own_state_of_a_calling_run_only() {
    for url in REGISTRAR {
        assert_eq!(
            Classifier::calling().classify("POST", url, Some(REGISTRATION.as_bytes())),
            Classification::Own(OwnKind::EndpointRegistration),
            "{url}"
        );
        assert_eq!(
            Classifier::mapped().classify("POST", url, Some(REGISTRATION.as_bytes())),
            Classification::Unknown,
            "{url}"
        );
    }
}

#[test]
fn a_registration_off_the_reviewed_shape_is_unknown() {
    let off = [
        // a transport that isn't the endpoint's own Trouter URL
        REGISTRATION.replace("pub-ent-zz-01-f.trouter.teams.microsoft.com", "example.com"),
        REGISTRATION.replace("https://pub-ent", "http://pub-ent"),
        REGISTRATION.replace(r#""TROUTER""#, r#""WEBHOOK""#),
        // a second transport kind, or a field nobody reviewed
        REGISTRATION.replace(r#""transports":{"#, r#""transports":{"SMS":[],"#),
        REGISTRATION.replace(r#""nodeId":"""#, r#""nodeId":"","members":["8:orgid:x"]"#),
        // no transport at all
        REGISTRATION.replace(
            r#"[{"context":"","path":"https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticSessionId00000/","ttl":3600}]"#,
            "[]",
        ),
        // keys alike in some case, trailing data
        REGISTRATION.replace(r#""nodeId":"""#, r#""nodeId":"","NodeId":"""#),
        format!("{REGISTRATION}{{}}"),
        // a push URL on another port, with a query or a fragment, off the push path, with user info
        REGISTRATION.replace(":3443/v4/f/", ":444/v4/f/"),
        REGISTRATION.replace("SyntheticSessionId00000/", "SyntheticSessionId00000/?to=x"),
        REGISTRATION.replace("SyntheticSessionId00000/", "SyntheticSessionId00000/#x"),
        REGISTRATION.replace("/v4/f/SyntheticSessionId00000/", "/v4/x/SyntheticSessionId00000/"),
        REGISTRATION.replace("/v4/f/SyntheticSessionId00000/", "/v4/f/a/b/"),
        REGISTRATION.replace("https://pub-ent", "https://user@pub-ent"),
        // a transport's fields of another type or missing
        REGISTRATION.replace(r#""ttl":3600"#, r#""ttl":"3600""#),
        REGISTRATION.replace(r#""context":"","#, ""),
        // a client description with more or less in it
        REGISTRATION.replace(r#""appId":"TeamsCDLWebWorker","#, ""),
        REGISTRATION.replace(r#""platform":"chrome","#, r#""platform":"chrome","userId":"8:orgid:x","#),
        // no node id
        REGISTRATION.replace(r#""nodeId":"","#, ""),
    ];
    for body in &off {
        assert_eq!(
            Classifier::calling().classify("POST", REGISTRAR[0], Some(body.as_bytes())),
            Classification::Unknown,
            "{body}"
        );
    }
    // another method, path or origin
    for (method, url) in [
        ("PUT", REGISTRAR[0]),
        ("DELETE", REGISTRAR[0]),
        ("POST", "https://teams.microsoft.com/registrar/prod/V2/registrations/x"),
        ("POST", "https://teams.microsoft.com/registrar/prod/V3/registrations"),
        ("POST", "https://example.com/registrar/prod/V2/registrations"),
        // the captured registration has no query: one it didn't show makes the request unknown
        (
            "POST",
            "https://teams.microsoft.com/registrar/prod/V2/registrations?x=1",
        ),
        ("POST", "https://teams.cloud.microsoft/registrar/prod/V2/registrations?"),
    ] {
        assert_eq!(
            Classifier::calling().classify(method, url, Some(REGISTRATION.as_bytes())),
            Classification::Unknown,
            "{method} {url}"
        );
    }
    assert_eq!(
        Classifier::calling().classify("POST", REGISTRAR[0], None),
        Classification::Unknown
    );
}

// --- the Trouter session's delivery responses ---

#[test]
fn a_delivery_response_is_unknown_even_with_an_empty_body() {
    // the endpoint's answer to something the service delivered goes back to whoever sent it; which deliveries an empty
    // success only acknowledges (and which it might answer, an incoming call's) isn't established, so none passes
    for data in [
        r#"3:::{"id":2029851735,"status":200,"headers":{"MS-CV":"SyntheticCv.0.1","trouter-request":"{\"id\":\"x\",\"src\":\"y\"}","trouter-client":"{\"cd\":2}"},"body":""}"#,
        r#"3:::{"id":-2129608987,"status":204,"headers":{},"body":""}"#,
        r#"3:::{"id":1,"status":200,"headers":{},"body":"{\"callAcceptanceAcknowledgement\":{}}"}"#,
    ] {
        assert_eq!(
            FrameClassifier::calling().classify(&text(REGIONAL, data)),
            Classification::Unknown,
            "{data}"
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
    classifier.classify("POST", CONVERSATIONS, Some(body.as_bytes()))
}

#[test]
fn the_test_call_start_is_a_call_with_the_caller_and_the_echo_service() {
    let classifier = Classifier::calling().with_test_call_callee(Mri::new(ECHO));
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
        classify_start(&Classifier::calling(), &test_call()),
        Classification::Unknown
    );
    // and a run that doesn't call maps no call at all
    assert_eq!(
        classify_start(&Classifier::mapped(), &test_call()),
        Classification::Unknown
    );
}

#[test]
fn a_test_call_start_may_name_the_echo_service_and_nobody_else() {
    let classifier = Classifier::calling().with_test_call_callee(Mri::new(ECHO));
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
    let classifier = Classifier::calling().with_test_call_callee(Mri::new(ECHO));
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
    let classifier = Classifier::calling().with_test_call_callee(Mri::new(ECHO));
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
        (
            "POST",
            "https://api.flightproxy.teams.microsoft.com.example.com/api/v2/epconv",
        ),
        ("POST", "http://api.flightproxy.teams.microsoft.com/api/v2/epconv"),
        // the captured start has no query: one it didn't show makes the request unknown
        ("POST", "https://api.flightproxy.teams.microsoft.com/api/v2/epconv?x=1"),
        ("POST", "https://api.flightproxy.teams.microsoft.com/api/v2/epconv?"),
    ] {
        assert_eq!(
            classifier.classify(method, url, Some(test_call().as_bytes())),
            Classification::Unknown,
            "{method} {url}"
        );
    }
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
        Some(("own", "settings_read")) => Classification::Own(OwnKind::SettingsRead),
        Some(("own", "endpoint_registration")) => Classification::Own(OwnKind::EndpointRegistration),
        Some(("own", "trouter_session")) => Classification::Own(OwnKind::TrouterSession),
        Some(("chat_create", list)) => {
            Classification::Write(Target::new(WriteKind::ChatCreate, Scope::New).with_added(participants(list)))
        }
        Some(("call", list)) => {
            Classification::Write(Target::new(WriteKind::Call, Scope::New).with_added(participants(list)))
        }
        _ => return None,
    })
}

#[test]
fn every_captured_request_of_the_test_call_is_what_the_fixture_says() {
    let fixture: serde_json::Value = serde_json::from_str(&filled(REQUESTS)).unwrap();
    let classifier = Classifier::calling().with_test_call_callee(Mri::new(format!("28:{ECHO_GUID}")));
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        let body = (!case["body"].is_null()).then(|| case["body"].to_string());
        let got = classifier.classify(
            case["method"].as_str().unwrap(),
            case["url"].as_str().unwrap(),
            body.as_deref().map(str::as_bytes),
        );
        let want = expected(case["expected"].as_str().unwrap()).expect("an expectation the test knows");
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
        let got = FrameClassifier::calling().classify(&text(socket, case["frame"].as_str().unwrap()));
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
    let classifier = Classifier::calling().with_test_call_callee(Mri::new(format!("28:{ECHO_GUID}")));
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

// --- the messaging set: the Trouter session of a run that chats (M1 Task 14, `docs/protocol/trouter.md`) ---

/// The calling stack's registration: [`REGISTRATION`]'s shape, its app and template `SkypeSpacesWeb` (made-up ids).
fn calling_registration() -> String {
    REGISTRATION.replace("TeamsCDLWebWorker", "SkypeSpacesWeb")
}

#[test]
fn the_messaging_set_lets_the_trouter_session_out() {
    for (url, data) in [
        (TROUTER, AUTHENTICATE),
        (REGIONAL, AUTHENTICATE_REGIONAL),
        (REGIONAL, r#"5:3+::{"name":"ping"}"#),
        (REGIONAL, "0::"),
        (
            REGIONAL,
            r#"5:2+::{"name":"trouter.processed_message_loss","args":[{"droppedIndicators":[{"tag":"messaging","etag":"2000-01-01T00:00:00.0000001Z"}]}]}"#,
        ),
        // the client's own activity: the account shows online while it runs (accepted by the user, 2026-10-19)
        (
            REGIONAL,
            r#"5:4+::{"name":"user.activity","args":[{"state":"active","cv":"SyntheticCorrelation0.0.1"}]}"#,
        ),
        (
            REGIONAL,
            r#"5:5+::{"name":"user.activity","args":[{"state":"inactive","cv":"SyntheticCorrelation0.0.2"}]}"#,
        ),
    ] {
        assert_eq!(
            FrameClassifier::messaging().classify(&text(url, data)),
            Classification::Own(OwnKind::TrouterSession),
            "{url} {data}"
        );
        assert_eq!(
            FrameClassifier::seeded().classify(&text(url, data)),
            Classification::Unknown,
            "{url} {data}"
        );
    }
}

#[test]
fn the_messaging_set_answers_no_delivery() {
    // a chat delivery's acknowledgement goes back to the service like any other response: it stays unknown unless the
    // user approves answering reviewed chat deliveries (`docs/protocol/trouter.md`)
    for data in [
        r#"3:::{"id":1234567,"status":200,"headers":{"MS-CV":"SyntheticCv.0.1","trouter-request":"{\"id\":\"x\",\"src\":\"y\"}","trouter-client":"{\"cd\":2}"},"body":""}"#,
        r#"3:::{"id":1,"status":204,"headers":{},"body":""}"#,
        r#"5:6+::{"name":"user.message","args":[{"to":"8:orgid:x","text":"hi"}]}"#,
        r#"5:4+::{"name":"user.activity","args":[{"state":"busy","cv":"x"}]}"#,
    ] {
        assert_eq!(
            FrameClassifier::messaging().classify(&text(REGIONAL, data)),
            Classification::Unknown,
            "{data}"
        );
    }
    let binary = Frame {
        url: REGIONAL.to_owned(),
        payload: Payload::Binary("NTo6Ont9".to_owned()),
    };
    assert_eq!(FrameClassifier::messaging().classify(&binary), Classification::Unknown);
}

#[test]
fn the_messaging_set_registers_the_messaging_stack_only() {
    for url in REGISTRAR {
        assert_eq!(
            Classifier::messaging().classify("POST", url, Some(REGISTRATION.as_bytes())),
            Classification::Own(OwnKind::EndpointRegistration),
            "{url}"
        );
        // never the calling stack's endpoint: that one makes the account reachable for calls
        assert_eq!(
            Classifier::messaging().classify("POST", url, Some(calling_registration().as_bytes())),
            Classification::Unknown,
            "{url}"
        );
    }
    let template = r#""templateKey":"TeamsCDLWebWorker_2.6""#;
    let app = r#""appId":"TeamsCDLWebWorker""#;
    let off = [
        // the messaging app with the calling stack's template, or the other way round
        REGISTRATION.replace(template, r#""templateKey":"SkypeSpacesWeb_2.6""#),
        REGISTRATION.replace(app, r#""appId":"SkypeSpacesWeb""#),
        // an app or a template that only starts like the messaging stack's
        REGISTRATION.replace(app, r#""appId":"TeamsCDLWebWorkerCalls""#),
        REGISTRATION.replace(template, r#""templateKey":"TeamsCDLWebWorkerX""#),
        REGISTRATION.replace(template, r#""templateKey":"TeamsCDLWebWorker_""#),
        REGISTRATION.replace(template, r#""templateKey":"TeamsCDLWebWorker_2.6/x""#),
        // the messaging stack's registration off the reviewed shape
        REGISTRATION.replace("pub-ent-zz-01-f.trouter.teams.microsoft.com", "example.com"),
        REGISTRATION.replace(r#""nodeId":"""#, r#""nodeId":"","members":["8:orgid:x"]"#),
        REGISTRATION.replace(r#""nodeId":"""#, r#""nodeId":"","NodeId":"""#),
    ];
    for body in &off {
        assert_eq!(
            Classifier::messaging().classify("POST", REGISTRAR[0], Some(body.as_bytes())),
            Classification::Unknown,
            "{body}"
        );
    }
    assert_eq!(
        Classifier::messaging().classify("PUT", REGISTRAR[0], Some(REGISTRATION.as_bytes())),
        Classification::Unknown
    );
    // the captured registration has no query: one it didn't show makes the request unknown
    for query in ["?x=1", "?"] {
        assert_eq!(
            Classifier::messaging().classify(
                "POST",
                &format!("{}{query}", REGISTRAR[0]),
                Some(REGISTRATION.as_bytes())
            ),
            Classification::Unknown,
            "{query}"
        );
    }
    assert_eq!(
        Classifier::messaging().classify("POST", REGISTRAR[0], None),
        Classification::Unknown
    );
}

#[test]
fn the_messaging_set_maps_chat_writes_and_nothing_of_calling() {
    let chat =
        "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19%3Aa_b%40unq.gbl.spaces/messages";
    let message = r#"{"content":"hi","messagetype":"RichText/Html","contenttype":"Text"}"#;
    let mapped = Classifier::mapped().classify("POST", chat, Some(message.as_bytes()));
    assert!(matches!(mapped, Classification::Write(_)), "{mapped:?}");
    assert_eq!(
        Classifier::messaging().classify("POST", chat, Some(message.as_bytes())),
        mapped
    );
    // the calling stack's own-state and its writes stay out of a run that only chats
    let settings = "https://teams.microsoft.com/api/mt/part/amer-03/beta/users/useraggregatesettings";
    assert_eq!(
        Classifier::messaging().classify("POST", settings, Some(AGGREGATE_SETTINGS.as_bytes())),
        Classification::Unknown
    );
    let start = test_call();
    assert_eq!(
        Classifier::messaging().with_test_call_callee(Mri::new(ECHO)).classify(
            "POST",
            CONVERSATIONS,
            Some(start.as_bytes())
        ),
        Classification::Unknown
    );
}

#[test]
fn the_messaging_set_without_activity_holds_back_only_the_activity() {
    let quiet = FrameClassifier::messaging().without_activity();
    for data in [
        r#"5:4+::{"name":"user.activity","args":[{"state":"active","cv":"SyntheticCorrelation0.0.1"}]}"#,
        r#"5:5+::{"name":"user.activity","args":[{"state":"inactive","cv":"SyntheticCorrelation0.0.2"}]}"#,
    ] {
        assert_eq!(quiet.classify(&text(REGIONAL, data)), Classification::Unknown, "{data}");
    }
    for data in [AUTHENTICATE_REGIONAL, r#"5:3+::{"name":"ping"}"#, "0::"] {
        assert_eq!(
            quiet.classify(&text(REGIONAL, data)),
            Classification::Own(OwnKind::TrouterSession),
            "{data}"
        );
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
        let want_quiet = case["expected_quiet"].as_str().map_or(want.clone(), |name| {
            expected(name).expect("an expectation the test knows")
        });
        assert_eq!(FrameClassifier::messaging().classify(&frame), want, "{}", case["name"]);
        assert_eq!(
            FrameClassifier::messaging().without_activity().classify(&frame),
            want_quiet,
            "{}",
            case["name"]
        );
        // nothing of the session is own-state without the Trouter set
        assert_eq!(FrameClassifier::seeded().classify(&frame), Classification::Unknown);
    }
}

#[test]
fn every_captured_registration_is_what_the_fixture_says() {
    let fixture: serde_json::Value = serde_json::from_str(&filled(REGISTRATIONS)).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 3);
    for case in cases {
        let body = case["body"].to_string();
        let got = Classifier::messaging().classify(
            case["method"].as_str().unwrap(),
            case["url"].as_str().unwrap(),
            Some(body.as_bytes()),
        );
        let want = expected(case["expected"].as_str().unwrap()).expect("an expectation the test knows");
        assert_eq!(got, want, "{}", case["name"]);
        // a run that calls registers either stack
        assert_eq!(
            Classifier::calling().classify(
                case["method"].as_str().unwrap(),
                case["url"].as_str().unwrap(),
                Some(body.as_bytes())
            ),
            Classification::Own(OwnKind::EndpointRegistration),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn chat_deliveries_come_on_the_messaging_path_and_their_answers_stay_unknown() {
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
        // the web client's answer: unknown to the messaging set, and to the calling set too
        let answer = text(socket, case["answer"].as_str().unwrap());
        let answered: serde_json::Value =
            serde_json::from_str(case["answer"].as_str().unwrap().strip_prefix("3:::").unwrap()).unwrap();
        assert_eq!(answered["id"], delivery["id"], "{name}");
        assert_eq!(
            FrameClassifier::messaging().classify(&answer),
            Classification::Unknown,
            "{name}"
        );
        assert_eq!(
            FrameClassifier::calling().classify(&answer),
            Classification::Unknown,
            "{name}"
        );
    }
}

#[test]
fn the_calling_set_still_registers_both_stacks() {
    for body in [REGISTRATION.to_owned(), calling_registration()] {
        assert_eq!(
            Classifier::calling().classify("POST", REGISTRAR[0], Some(body.as_bytes())),
            Classification::Own(OwnKind::EndpointRegistration),
            "{body}"
        );
    }
}
