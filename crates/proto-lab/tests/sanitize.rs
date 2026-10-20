//! The sanitizer and `proto-lab export`, offline, on a synthetic capture shaped like the recorder's (`cdp` events,
//! bodies, verdicts, WebRTC reports) and planted with every kind of secret and identity a Teams capture holds: secrets
//! are gone, identities are stable placeholders (the lab's by alias), sanitizing twice changes nothing, the fixture
//! guard finds nothing in the result, and an export the guard or the export's name check refuses writes nothing.
//!
//! Every secret and identity here is made up.

use std::fs;
use std::path::Path;

use base64::Engine as _;
use fixture_guard::{Guard, Kind};
use proto_lab::commands::export::{self, ExportError, Layout};
use proto_lab::config::Alias;
use proto_lab::sanitize::{
    CHECK_ROUNDS, Identities, KnownAccount, KnownService, LearnedNames, NameFinding, Sanitizer, check_names,
    sanitize_capture, sanitize_events,
};
use serde_json::{Value, json};

const TENANT: &str = "contoso-lab.com";
const LAB_A_UPN: &str = "lab-a@contoso-lab.com";
const LAB_A_GUID: &str = "0a1b2c3d-1111-4222-8333-444455556666";
const LAB_B_GUID: &str = "0b1b2c3d-1111-4222-8333-444455556666";
const COWORKER_UPN: &str = "jane.doe@contoso-lab.com";
const COWORKER_GUID: &str = "9f8e7d6c-5b4a-4321-8fed-cba987654321";
const TENANT_GUID: &str = "72f988bf-86f1-41af-91ab-2d7cd011db47";
const BOT_GUID: &str = "11112222-3333-4444-8555-666677778888";
const ECHO_MRI: &str = "28:22223333-4444-4555-8666-777788889999";
const JWT: &str = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJhdWQiOiJodHRwczovL2FwaS5zcGFjZXMuc2t5cGUuY29tIn0.c2lnbmF0dXJlLXNpZ25hdHVyZS1zaWduYXR1cmU";
const SKYPETOKEN: &str =
    "eyJhbGciOiJSUzI1NiIsImtpZCI6IjEwMiJ9.eyJza3lwZWlkIjoib3JnaWQ6bGFiIn0.c2t5cGUtc2lnbmF0dXJlLXZhbHVlLXZhbHVl";
const ID_TOKEN: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJub25lIn0.eyJvaWQiOiJsYWIiLCJ1cG4iOiJsYWIifQ.";
const REFRESH: &str = "0.AXoAopaqueRefreshTokenValue_Zm9vYmFyYmF6cXV4-1234567890abcdef";
const AUTH_CODE: &str = "1.AQoAauthorizationCodeOpaqueValue_abcdefghijklmnop-0987654321";
const COOKIE_VALUE: &str = "CookieSecretValue_1234567890";
const SET_COOKIE_VALUE: &str = "SetCookieSecretValue_0987654321";
const ICE_PWD: &str = "IcePasswordValue0123456789ab";
const TURN_SECRET: &str = "turn-credential-secret";
const TURN_USER: &str = "1729351234:lab-turn-user";
const SAS_SIG: &str = "SasSignatureValue%2Bxyz%3D";
const OPAQUE_PATH_TOKEN: &str = "AbCdEfGh1234567890IjKlMnOpQrStUvWxYz0987";
const THREAD_BODY: &str = "aBcDeF0123456789aBcDeF0123456789";
const MEETING_BODY: &str = "meeting_MjA4ZmVlNjYtYWJjZC00";
const PUBLIC_IP: &str = "203.0.113.77";
const PRIVATE_IP: &str = "192.168.1.23";
const IPV6: &str = "2001:db8:85a3::8a2e:370:7334";
const COWORKER_NAME: &str = "Jane Doe";
const TOPIC: &str = "Secret project kickoff";
/// A Teams meeting's code (its numeric meeting id) and its join passcode, as a `/meet/` link carries them.
const MEETING_CODE: &str = "1234567890123";
const MEETING_PASSCODE: &str = "Ab3dEf5gHi7jKl9mNo";
/// The meeting id and passcode of an invitation's text, and a dial-in conference id.
const INVITE_CODE: &str = "355 123 456 789";
const INVITE_PASSCODE: &str = "Qx7Rt2";
const DIAL_IN_ID: &str = "987654321";

fn identities() -> Identities {
    Identities {
        accounts: vec![
            KnownAccount {
                alias: Alias::A,
                upn: LAB_A_UPN.to_owned(),
                mri: Some(format!("8:orgid:{LAB_A_GUID}")),
            },
            KnownAccount {
                alias: Alias::B,
                upn: "lab-b@contoso-lab.com".to_owned(),
                mri: Some(format!("8:orgid:{LAB_B_GUID}")),
            },
        ],
        services: vec![KnownService {
            name: "echo".to_owned(),
            mri: ECHO_MRI.to_owned(),
        }],
        tenant_domain: Some(TENANT.to_owned()),
    }
}

fn cdp(method: &str, params: &Value) -> Value {
    json!({"type": "cdp", "session": "S1", "method": method, "params": params, "seq": 1, "ts_ms": 1})
}

/// A request to a chat service, with every credential header and identities in the URL and the body.
fn request_event() -> Value {
    let body = json!({
        "members": [{"id": format!("8:orgid:{LAB_A_GUID}"), "role": "Admin"}, {"id": format!("8:orgid:{COWORKER_GUID}"), "role": "User"}],
        "properties": {"topic": TOPIC},
    });
    cdp(
        "Network.requestWillBeSent",
        &json!({
            "requestId": "1000.1",
            "request": {
                "url": format!(
                    "https://teams.microsoft.com/api/mt/emea/beta/users/8%3Aorgid%3A{COWORKER_GUID}/?skypetoken={SKYPETOKEN}&tid={TENANT_GUID}"
                ),
                "method": "POST",
                "headers": {
                    "Authorization": format!("Bearer {JWT}"),
                    "X-Skypetoken": SKYPETOKEN,
                    "Authentication": format!("skypetoken={SKYPETOKEN}"),
                    "Cookie": format!("authtoken={COOKIE_VALUE}; theme=dark"),
                    "x-ms-client-request-id": BOT_GUID.to_uppercase(),
                    "Content-Type": "application/json",
                },
                "postData": body.to_string(),
                "hasPostData": true,
            },
        }),
    )
}

/// The token endpoint's form body and its JSON answer.
fn token_events() -> [Value; 2] {
    let form = format!(
        "client_id=5e3ce6c0-2b1f-4285-8d4b-75ee78787346&grant_type=authorization_code&code={AUTH_CODE}&redirect_uri=https%3A%2F%2Fteams.microsoft.com%2Fgo&refresh_token={REFRESH}&login_hint=jane.doe%40contoso-lab.com"
    );
    let answer = json!({
        "token_type": "Bearer",
        "expires_in": 3599,
        "access_token": JWT,
        "refresh_token": REFRESH,
        "id_token": ID_TOKEN,
        "client_info": "eyJ1aWQiOiJsYWIiLCJ1dGlkIjoibGFiIn0",
    });
    [
        json!({"type": "request_body", "session": "S1", "request_id": "1000.2", "postData": form}),
        json!({"type": "response_body", "session": "S1", "request_id": "1000.2", "body": answer.to_string(), "base64Encoded": false}),
    ]
}

/// A people lookup naming the coworker and the lab account, and keyed by MRI.
fn people_event() -> Value {
    let people = json!({
        "value": [
            {"mri": format!("8:orgid:{COWORKER_GUID}"), "displayName": COWORKER_NAME, "email": COWORKER_UPN, "objectId": COWORKER_GUID.to_uppercase(), "jobTitle": "Engineer", "businessPhones": ["+1 555 0100"]},
            {"mri": format!("8:orgid:{LAB_A_GUID}"), "displayName": "Lab Bot A", "userPrincipalName": LAB_A_UPN},
        ],
        "presence": {format!("8:orgid:{COWORKER_GUID}"): "Available"},
        "tenant": "contoso-lab.onmicrosoft.com",
        "files": "https://contoso-lab-my.sharepoint.com/personal/jane",
    });
    json!({"type": "response_body", "session": "S1", "request_id": "1000.3", "body": people.to_string(), "base64Encoded": false})
}

/// A Trouter-style frame: a text prefix, JSON, and a message body as JSON inside a string.
fn frame_event() -> Value {
    let message = json!({
        "content": "Hi Jane, please mail jane.doe@contoso-lab.com",
        "from": format!("8:orgid:{COWORKER_GUID}"),
        "conversationid": format!("19:{THREAD_BODY}@thread.v2"),
        "chat": format!("19:{LAB_A_GUID}_{COWORKER_GUID}@unq.gbl.spaces"),
        "meeting": format!("19:{MEETING_BODY}@thread.v2"),
        "imdisplayname": COWORKER_NAME,
    });
    let frame = format!("3:::{}", json!({"name": "message", "body": message.to_string()}));
    cdp(
        "Network.webSocketFrameReceived",
        &json!({"requestId": "2000.1", "response": {"opcode": 1, "mask": false, "payloadData": frame}}),
    )
}

/// What the WebRTC hooks report: a configuration with TURN credentials, an SDP and a candidate.
fn rtc_events() -> [Value; 2] {
    let sdp = format!(
        "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\nc=IN IP6 {IPV6}\r\na=ice-ufrag:abcd\r\na=ice-pwd:{ICE_PWD}\r\na=candidate:1 1 udp 2122260223 {PRIVATE_IP} 54321 typ host generation 0 ufrag abcd network-id 1\r\na=candidate:2 1 udp 1686052607 {PUBLIC_IP} 61000 typ srflx raddr {PRIVATE_IP} rport 54321\r\n"
    );
    [
        json!({"type": "rtc", "session": "S1", "context": 3, "payload": {
            "pc": 1, "event": "create", "ts": 1,
            "data": {"iceServers": [{"urls": [format!("turn:{PUBLIC_IP}:3478")], "username": TURN_USER, "credential": TURN_SECRET}]},
        }}),
        json!({"type": "rtc", "session": "S1", "context": 3, "payload": {
            "pc": 1, "event": "setLocalDescription", "ts": 2, "data": {"type": "offer", "sdp": sdp},
        }}),
    ]
}

/// What a binary WebSocket frame the page sent holds: an email, a name and an MRI, as raw bytes.
fn binary_frame_bytes() -> Vec<u8> {
    let mut bytes = vec![0x82, 0x00];
    bytes.extend(format!("{COWORKER_UPN}\0{COWORKER_NAME}\08:orgid:{COWORKER_GUID}").bytes());
    bytes
}

/// A response's headers, its raw header text, a binary frame, a base64 body, and the fence's verdicts on a request and
/// on a binary frame.
fn transport_events() -> [Value; 5] {
    let base64_body = base64::engine::general_purpose::STANDARD.encode(format!(r#"{{"owner":"{COWORKER_UPN}"}}"#));
    [
        cdp(
            "Network.responseReceivedExtraInfo",
            &json!({
                "requestId": "1000.1",
                "headers": {"set-cookie": format!("skypetoken_asm={SET_COOKIE_VALUE}; Domain=.asm.skype.com; Path=/; Secure; HttpOnly\nauthtoken={COOKIE_VALUE}; Path=/")},
                "headersText": format!("HTTP/1.1 200 OK\r\nSet-Cookie: authtoken={COOKIE_VALUE}\r\n"),
            }),
        ),
        cdp(
            "Network.webSocketFrameSent",
            &json!({"requestId": "2000.1", "response": {"opcode": 2, "mask": true, "payloadData": "AQIDBA=="}}),
        ),
        json!({"type": "response_body", "session": "S1", "request_id": "1000.4", "body": base64_body, "base64Encoded": true}),
        json!({"type": "verdict", "channel": "http", "session": null, "method": "POST", "verdict": "deny", "cause": "unclassified",
            "url": format!("https://go.trouter.teams.microsoft.com/v4/a?sr=x&sig={SAS_SIG}&epid={BOT_GUID}"),
            "request": {"url": format!("https://example.invalid/v1/token/{OPAQUE_PATH_TOKEN}"), "headers": {}}}),
        json!({"type": "verdict", "channel": "web_socket", "session": "S1", "url": "wss://pub.example.invalid/v1",
            "socket": 1, "frame_seq": 3, "verdict": "deny", "cause": "unclassified",
            "frame": {"kind": "binary", "data": base64::engine::general_purpose::STANDARD.encode(binary_frame_bytes())}}),
    ]
}

/// A Teams meeting join link, with the meeting's code and its passcode.
fn meeting_link() -> String {
    format!("https://teams.microsoft.com/meet/{MEETING_CODE}?p={MEETING_PASSCODE}")
}

/// A calendar answer: a meeting with its join link, an invitation that spells out its id and passcode, a dial-in.
fn calendar_event() -> Value {
    let calendar = json!({
        "value": [{
            "onlineMeeting": {"joinUrl": meeting_link(), "conferenceId": DIAL_IN_ID},
            "body": {"content": format!(
                "<p>Meeting ID: <span>{INVITE_CODE}</span><br>Passcode: <b>{INVITE_PASSCODE}</b></p><p>Dial in: +1 323 555 0199,,{DIAL_IN_ID}#</p>"
            )},
        }],
    });
    json!({"type": "response_body", "session": "S1", "request_id": "1000.5", "body": calendar.to_string(), "base64Encoded": false})
}

fn capture() -> Vec<Value> {
    let mut events = vec![request_event(), people_event(), frame_event()];
    events.extend(token_events());
    events.extend(rtc_events());
    events.extend(transport_events());
    events.push(calendar_event());
    events.push(cdp(
        "Network.webSocketCreated",
        &json!({"requestId": "2000.2", "url": format!("wss://pub.example.invalid/{ECHO_MRI}?x=1")}),
    ));
    events
}

/// Helpers return errors; the tests unwrap them.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn sanitized() -> TestResult<Vec<Value>> {
    Ok(sanitize_events(&capture(), &identities())?)
}

fn as_jsonl(events: &[Value]) -> String {
    export::render(events, Layout::Jsonl)
}

/// A body (JSON in a string) of one sanitized event, parsed.
fn body_of(event: &Value) -> TestResult<Value> {
    let body = event["body"].as_str().ok_or("no body")?;
    Ok(serde_json::from_str(body)?)
}

#[test]
fn secrets_are_removed() {
    let text = as_jsonl(&sanitized().unwrap());
    let secrets = [
        JWT,
        SKYPETOKEN,
        ID_TOKEN,
        REFRESH,
        AUTH_CODE,
        COOKIE_VALUE,
        SET_COOKIE_VALUE,
        ICE_PWD,
        TURN_SECRET,
        TURN_USER,
        SAS_SIG,
        OPAQUE_PATH_TOKEN,
        "eyJ1aWQiOiJsYWIiLCJ1dGlkIjoibGFiIn0",
        // the raw header text is dropped whole
        "HTTP/1.1 200 OK",
        MEETING_CODE,
        MEETING_PASSCODE,
        INVITE_CODE,
        INVITE_PASSCODE,
        DIAL_IN_ID,
    ];
    for secret in secrets {
        assert!(!text.contains(secret), "{secret} survived");
    }
    // nothing of a JWT is left either
    assert!(!text.contains("eyJ"));
}

#[test]
fn identities_become_placeholders() {
    let text = as_jsonl(&sanitized().unwrap()).to_lowercase();
    let identities = [
        LAB_A_UPN,
        COWORKER_UPN,
        "jane.doe%40contoso-lab.com",
        LAB_A_GUID,
        LAB_B_GUID,
        COWORKER_GUID,
        TENANT_GUID,
        BOT_GUID,
        "22223333-4444-4555-8666-777788889999",
        THREAD_BODY,
        MEETING_BODY,
        PUBLIC_IP,
        PRIVATE_IP,
        IPV6,
        "2001:db8",
        TENANT,
        "contoso-lab.onmicrosoft",
        "contoso-lab-my.sharepoint",
        "jane",
        "doe",
        "lab bot",
        "secret project",
        "engineer",
        "555 0100",
    ];
    for identity in identities {
        assert!(!text.contains(&identity.to_lowercase()), "{identity} survived");
    }
}

#[test]
fn what_identifies_nothing_is_kept() {
    let events = sanitized().unwrap();
    let request = &events[0]["params"]["request"];
    assert_eq!(request["method"], "POST");
    assert!(
        request["url"]
            .as_str()
            .unwrap()
            .starts_with("https://teams.microsoft.com/api/mt/emea/beta/users/")
    );
    assert_eq!(request["headers"]["Content-Type"], "application/json");
    let token_answer = body_of(&events[4]).unwrap();
    assert_eq!(token_answer["expires_in"], 3599);
    assert_eq!(token_answer["token_type"], "Bearer");
    let form = events[3]["postData"].as_str().unwrap();
    assert!(form.contains("grant_type=authorization_code"), "{form}");
    assert!(
        form.contains("redirect_uri=https%3A%2F%2Fteams.microsoft.com%2Fgo"),
        "{form}"
    );
    let sdp = events[6]["payload"]["data"]["sdp"].as_str().unwrap();
    assert!(sdp.contains("IN IP4 127.0.0.1"), "loopback is no identity: {sdp}");
    assert!(sdp.contains("typ srflx"), "{sdp}");
    let people = body_of(&events[1]).unwrap();
    assert_eq!(
        people["presence"].as_object().unwrap().values().next().unwrap(),
        "Available"
    );
    assert_eq!(events[2]["method"], "Network.webSocketFrameReceived");
}

#[test]
fn credentials_keep_their_scheme_and_cookies_their_names() {
    let events = sanitized().unwrap();
    let headers = &events[0]["params"]["request"]["headers"];
    let authorization = headers["Authorization"].as_str().unwrap();
    assert!(authorization.starts_with("Bearer <token:"), "{authorization}");
    assert!(
        headers["Authentication"]
            .as_str()
            .unwrap()
            .starts_with("skypetoken=<token:")
    );
    let cookie = headers["Cookie"].as_str().unwrap();
    assert!(cookie.starts_with("authtoken=<token:"), "{cookie}");
    assert!(cookie.contains("; theme=<token:"), "{cookie}");
    let set_cookie = events[7]["params"]["headers"]["set-cookie"].as_str().unwrap();
    assert!(set_cookie.contains("skypetoken_asm=<token:"), "{set_cookie}");
    assert!(
        set_cookie.contains("Domain=.asm.skype.com; Path=/; Secure; HttpOnly"),
        "{set_cookie}"
    );
    assert_eq!(events[7]["params"]["headersText"], "<omitted:raw>");
}

/// The placeholder that replaced the first match of `pattern` in `text`.
fn placeholder_in(text: &str, pattern: &str) -> TestResult<String> {
    let regex = regex::Regex::new(pattern)?;
    let captures = regex.captures(text).ok_or_else(|| format!("no {pattern} in {text}"))?;
    Ok(captures[1].to_owned())
}

#[test]
fn the_same_identity_gets_the_same_placeholder_throughout_the_capture() {
    let events = sanitized().unwrap();
    // the token in the X-Skypetoken header, the Authentication header and the query is one token
    let headers = &events[0]["params"]["request"]["headers"];
    let skypetoken = headers["X-Skypetoken"].as_str().unwrap().to_owned();
    assert!(skypetoken.starts_with("<token:"));
    assert_eq!(
        headers["Authentication"].as_str().unwrap(),
        format!("skypetoken={skypetoken}")
    );
    assert!(
        events[0]["params"]["request"]["url"]
            .as_str()
            .unwrap()
            .contains(&format!("skypetoken={skypetoken}"))
    );
    // the bearer JWT is the token endpoint's access token
    let access = body_of(&events[4]).unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(headers["Authorization"].as_str().unwrap(), format!("Bearer {access}"));

    // the coworker: encoded MRI in a URL, MRIs in bodies and frames, the bare (uppercase) object id, a map key
    let url = events[0]["params"]["request"]["url"].as_str().unwrap();
    let coworker = placeholder_in(url, r"users/<mri:(\w+)>").unwrap();
    let people = body_of(&events[1]).unwrap();
    assert_eq!(people["value"][0]["mri"], format!("<mri:{coworker}>"));
    assert_eq!(people["value"][0]["objectId"], format!("<guid:{coworker}>"));
    assert!(
        people["presence"].get(format!("<mri:{coworker}>")).is_some(),
        "{people}"
    );
    let frame = events[2]["params"]["response"]["payloadData"].as_str().unwrap();
    assert!(frame.contains(&format!(r#"\"from\":\"<mri:{coworker}>\""#)), "{frame}");
    // their email in a body, a frame and an encoded form field
    let email = people["value"][0]["email"].as_str().unwrap().to_owned();
    assert!(email.starts_with("<user:"), "{email}");
    assert!(frame.contains(&email), "{frame}");
    assert!(
        events[3]["postData"]
            .as_str()
            .unwrap()
            .contains(&format!("login_hint={email}"))
    );
    // their name in a name field, a message body and the frame's own name field
    let name = people["value"][0]["displayName"].as_str().unwrap().to_owned();
    assert!(name.starts_with("<name:"), "{name}");
    assert!(frame.contains(&format!("Hi {name}, please mail {email}")), "{frame}");
    assert!(frame.contains(&format!(r#"\"imdisplayname\":\"{name}\""#)), "{frame}");
}

#[test]
fn the_lab_gets_its_aliases_and_services_their_names() {
    let events = sanitized().unwrap();
    let people = body_of(&events[1]).unwrap();
    assert_eq!(people["value"][1]["mri"], "<mri:a>");
    assert_eq!(people["value"][1]["userPrincipalName"], "<user:a>");
    let request: Value = serde_json::from_str(events[0]["params"]["request"]["postData"].as_str().unwrap()).unwrap();
    assert_eq!(request["members"][0]["id"], "<mri:a>");
    // a 1:1 chat id keeps its shape, with both object ids replaced
    let frame = events[2]["params"]["response"]["payloadData"].as_str().unwrap();
    assert!(frame.contains("19:<guid:a>_<guid:"), "{frame}");
    assert!(frame.contains("@unq.gbl.spaces"), "{frame}");
    assert!(frame.contains("19:<thread:"), "{frame}");
    assert!(frame.contains("@thread.v2"), "{frame}");
    let created = events.last().unwrap()["params"]["url"].as_str().unwrap();
    assert!(created.contains("/<mri:echo>?"), "{created}");
    assert_eq!(people["tenant"], "<domain:1>.onmicrosoft.com");
    assert!(
        people["files"]
            .as_str()
            .unwrap()
            .starts_with("https://<domain:1>-my.sharepoint.com/")
    );
}

#[test]
fn binary_content_is_dropped_and_base64_text_is_read() {
    let events = sanitized().unwrap();
    assert_eq!(events[8]["params"]["response"]["payloadData"], "<binary:4>");
    let decoded = &events[9];
    assert_eq!(decoded["base64Encoded"], false);
    let body = body_of(decoded).unwrap();
    assert!(body["owner"].as_str().unwrap().starts_with("<user:"), "{body}");
}

#[test]
fn a_binary_frame_in_a_verdict_is_dropped() {
    let events = sanitized().unwrap();
    let verdict = &events[11];
    assert_eq!(verdict["channel"], "web_socket");
    assert_eq!(verdict["frame"]["kind"], "binary");
    assert_eq!(
        verdict["frame"]["data"],
        format!("<binary:{}>", binary_frame_bytes().len())
    );
    // no piece of the frame's base64 is left anywhere
    let encoded = base64::engine::general_purpose::STANDARD.encode(binary_frame_bytes());
    let text = as_jsonl(&events);
    for piece in encoded.split(['+', '/', '=']).filter(|piece| piece.len() >= 8) {
        assert!(!text.contains(piece), "{piece} survived");
    }
    // a text frame is read through the text rules
    let text_frame = json!({"type": "verdict", "channel": "web_socket", "frame": {"kind": "text", "data": format!("hello {COWORKER_UPN}")}});
    let out = sanitize_events(&[text_frame], &Identities::default())
        .unwrap()
        .remove(0);
    assert_eq!(out["frame"]["data"], "hello <user:1>");
}

#[test]
fn sanitizing_is_idempotent() {
    let once = sanitized().unwrap();
    let twice = sanitize_events(&once, &identities()).unwrap();
    assert_eq!(as_jsonl(&twice), as_jsonl(&once));
    // and without the lab's identities too
    let unknown = sanitize_events(&once, &Identities::default()).unwrap();
    assert_eq!(as_jsonl(&unknown), as_jsonl(&once));
}

#[test]
fn sanitizing_is_deterministic() {
    assert_eq!(as_jsonl(&sanitized().unwrap()), as_jsonl(&sanitized().unwrap()));
}

#[test]
fn the_fixture_guard_finds_nothing_in_a_sanitized_capture() {
    let guard = Guard::new().unwrap();
    // the raw capture trips every rule the guard has
    let raw = as_jsonl(&capture());
    let mut kinds: Vec<Kind> = guard.scan_text(&raw).into_iter().map(|(_, kind)| kind).collect();
    kinds.sort_by_key(|kind| format!("{kind:?}"));
    kinds.dedup();
    assert_eq!(
        kinds,
        [
            Kind::Bearer,
            Kind::Email,
            Kind::Guid,
            Kind::Jwt,
            Kind::MeetingId,
            Kind::MeetingLink,
            Kind::Mri,
            Kind::OauthToken,
            Kind::Passcode,
            Kind::Signature,
            Kind::Skypetoken
        ]
    );
    assert_eq!(guard.scan_text(&as_jsonl(&sanitized().unwrap())), []);
    // also without the lab's identities
    let unknown = sanitize_events(&capture(), &Identities::default()).unwrap();
    assert_eq!(guard.scan_text(&as_jsonl(&unknown)), []);
}

#[test]
fn without_the_labs_identities_every_identity_is_numbered() {
    let events = sanitize_events(&capture(), &Identities::default()).unwrap();
    let text = as_jsonl(&events);
    for labelled in ["<user:a>", "<mri:a>", "<guid:a>", "<mri:echo>", "<domain:tenant>"] {
        assert!(!text.contains(labelled), "{labelled}");
    }
    assert!(!text.to_lowercase().contains(LAB_A_UPN));
}

#[test]
fn values_under_generic_keys_are_secrets_only_when_long() {
    let event = json!({
        "error": {"code": "InvalidAuthenticationToken", "message": "Access token has expired."},
        "state": "Connected",
        "key": "x",
        "redirect": format!("https://localhost/#code={AUTH_CODE}&state=short"),
        "nested": {"code": AUTH_CODE},
    });
    let out = sanitize_events(&[event], &Identities::default()).unwrap().remove(0);
    assert_eq!(out["error"]["code"], "InvalidAuthenticationToken");
    assert_eq!(out["error"]["message"], "Access token has expired.");
    assert_eq!(out["state"], "Connected");
    assert_eq!(out["key"], "x");
    assert_eq!(out["redirect"], "https://localhost/#code=<token:1>&state=short");
    assert_eq!(out["nested"]["code"], "<token:1>");
}

#[test]
fn ordinary_text_and_numbers_pass_unchanged() {
    let event = json!({
        "url": "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/48:notes/messages?view=msnp24Equivalent",
        "ua": "Mozilla/5.0 (X11; Linux x86_64) Chrome/155.0.8059.84 Safari/537.36",
        "time": "2026-10-19T12:30:45.123Z",
        "version": "26260.1717.5229.7049",
        "chunk": "main.a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6.js",
        "loopback": "ws://127.0.0.1:9222/devtools/browser",
        "count": 42,
        "flag": true,
    });
    let out = sanitize_events(std::slice::from_ref(&event), &Identities::default())
        .unwrap()
        .remove(0);
    assert_eq!(out, event);
}

#[test]
fn a_name_is_learned_from_any_event_and_replaced_in_every_other() {
    let events = [
        json!({"text": "Lunch with Alexandra Example-Name?"}),
        json!({"profile": {"givenName": "Alexandra", "surname": "Example-Name"}}),
    ];
    let out = sanitize_events(&events, &Identities::default()).unwrap();
    let text = out[0]["text"].as_str().unwrap();
    assert!(!text.contains("Alexandra") && !text.contains("Example-Name"), "{text}");
    assert_eq!(out[1]["profile"]["givenName"], "<name:1>");
    assert_eq!(text, "Lunch with <name:1> <name:2>?");
}

#[test]
fn a_name_in_a_base64_body_is_learned_too() {
    let profile = json!({"value": [{"displayName": "Marguerite Fakename"}]}).to_string();
    let body = json!({"type": "response_body", "body": base64::engine::general_purpose::STANDARD.encode(profile), "base64Encoded": true});
    let message = json!({"text": "Hi Marguerite Fakename, and marguerite again"});
    for events in [[body.clone(), message.clone()], [message, body]] {
        let out = sanitize_events(&events, &Identities::default()).unwrap();
        let text = as_jsonl(&out).to_lowercase();
        assert!(!text.contains("marguerite") && !text.contains("fakename"), "{text}");
    }
}

#[test]
fn a_name_after_an_encoded_separator_is_replaced() {
    let events = [
        json!({"displayName": "Jane Doe"}),
        json!({
            "url": "https://x.example.invalid/send?message=Hello%20Jane%20Doe&to=Doe",
            "twice": "https://x.example.invalid/r?u=%2Fsend%3Fm%3DHello%2520Jane%2520Doe",
            "escaped": r#"a frame that isn't JSON: {\"text\":\"Hi\nJane\tDoe\u0020Jane\""#,
        }),
    ];
    let out = sanitize_events(&events, &Identities::default()).unwrap();
    let text = as_jsonl(&out).to_lowercase();
    assert!(!text.contains("jane") && !text.contains("doe"), "{text}");
    assert_eq!(
        out[1]["url"],
        "https://x.example.invalid/send?message=Hello%20<name:1>&to=<name:1>"
    );
    // a name inside a longer word is still no name
    let word = sanitize_events(
        &[
            json!({"displayName": "Ann"}),
            json!({"text": "Annual planning, by Ann"}),
        ],
        &Identities::default(),
    )
    .unwrap();
    assert_eq!(word[1]["text"], "Annual planning, by <name:1>");
}

#[test]
fn an_encoded_email_keeps_what_comes_before_it() {
    let events = [json!({
        "plain": "sip:jane@contoso.com",
        "encoded": "sip%3Ajane%40contoso.com",
        "redirect": "?url=https%3A%2F%2Fteams.microsoft.com%2Fl%3Fusers%3Djane%2540contoso.com",
        "quoted": "%22jane%40contoso.com%22",
        "lab": "to%3Alab-a%40contoso-lab.com",
        "plus": "jane%2Bteams%40contoso.com",
    })];
    let out = sanitize_events(&events, &identities()).unwrap().remove(0);
    assert_eq!(out["plain"], "sip:<user:1>");
    assert_eq!(out["encoded"], "sip%3A<user:1>");
    assert_eq!(
        out["redirect"],
        "?url=https%3A%2F%2Fteams.microsoft.com%2Fl%3Fusers%3D<user:1>"
    );
    assert_eq!(out["quoted"], "%22<user:1>%22");
    assert_eq!(out["lab"], "to%3A<user:a>");
    // an encoded character that belongs in an address stays part of it
    let plus = out["plus"].as_str().unwrap();
    assert!(plus.starts_with("<user:") && !plus.contains("jane"), "{plus}");
}

#[test]
fn an_ice_username_fragment_is_a_token_like_the_ufrag_it_is() {
    let candidate = json!({"type": "rtc", "payload": {"event": "icecandidate", "data": {
        "candidate": "candidate:1 1 udp 2122260223 127.0.0.1 54321 typ host generation 0 ufrag Qx7Zk network-id 1",
        "usernameFragment": "Qx7Zk",
    }}});
    let out = sanitize_events(&[candidate], &Identities::default()).unwrap().remove(0);
    assert!(!out.to_string().contains("Qx7Zk"), "{out}");
    let data = &out["payload"]["data"];
    let token = data["usernameFragment"].as_str().unwrap();
    assert!(token.starts_with("<token:"), "{token}");
    assert!(
        data["candidate"].as_str().unwrap().contains(&format!("ufrag {token}")),
        "{data}"
    );
}

#[test]
fn one_sanitizer_keeps_its_labels_across_events() {
    let mut sanitizer = Sanitizer::new(&Identities::default()).unwrap();
    let first = sanitizer.sanitize(&json!({"to": COWORKER_UPN})).unwrap();
    let second = sanitizer
        .sanitize(&json!({"cc": COWORKER_UPN.to_uppercase(), "to": "x@contoso.example.com"}))
        .unwrap();
    assert_eq!(first["to"], "<user:1>");
    assert_eq!(second["cc"], "<user:1>");
    // reserved documentation domains stay
    assert_eq!(second["to"], "x@contoso.example.com");
}

// --- encoded text ---

/// A signed URL's signature, as storage services hand them out.
const SIGNATURE: &str = "abCD12+efGH34/ijKL56+mnOP78/qrST90+uvWX123A=";
const AMZ_SIGNATURE: &str = "3f1a9c0b7e2d4f6a8b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a";
/// A name with non-ASCII letters, learned from a plain `displayName`.
const ACCENTED_NAME: &str = "José García";
/// A name only ever seen percent-encoded, in a query.
const CYRILLIC_NAME: &str = "Дмитрий Иванов";
/// A display name in the "Last, First" form many tenants use.
const COMMA_NAME: &str = "Doe, Jane";
/// A name only ever seen in a form body, `+` for its space.
const FORM_ONLY_NAME: &str = "Odalys Quennell";
/// A dotted display name that ends in a two-letter label, which is no domain.
const DOTTED_NAME: &str = "mirela.wu";
/// A meeting whose passcode has no digit, its link, and the same passcode on its own in free text.
const ALPHA_CODE: &str = "2468013579135";
const ALPHA_PASSCODE: &str = "ZqNBBm";
/// A meeting whose passcode only shows one layer of decoding after its code: in raw text, `&` escaped as `\u0026`.
const LATE_CODE: &str = "3579246801357";
const LATE_PASSCODE: &str = "hQzRtW";
/// A French invitation, its labels as the bundle's locale strings spell them (a no-break space before the colon).
const FRENCH_CODE: &str = "366 222 333 444";
const FRENCH_PASSCODE: &str = "hTqWmz";

fn alpha_link() -> String {
    format!("https://teams.microsoft.com/meet/{ALPHA_CODE}?anon=true&p={ALPHA_PASSCODE}")
}

fn late_link() -> String {
    format!(r"https://teams.microsoft.com/meet/{LATE_CODE}?anon=true\u0026p={LATE_PASSCODE}")
}

fn french_invitation() -> String {
    format!(
        "<div>ID de la réunion\u{a0}: <span>{FRENCH_CODE}</span></div><div>Code secret\u{a0}: <span>{FRENCH_PASSCODE}</span></div>"
    )
}

/// Percent-encodes every byte but the unreserved ones (as `encodeURIComponent` does, and a little more).
fn enc(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// `text` percent-encoded `times` times.
fn enc_times(text: &str, times: usize) -> String {
    (0..times).fold(text.to_owned(), |text, _| enc(&text))
}

/// Form encoding: a space is `+`.
fn form_enc(text: &str) -> String {
    enc(text).replace("%20", "+")
}

/// `inner` as the target of `depth` redirects, each one encoding it once more.
fn wrapped(inner: &str, depth: usize) -> String {
    (1..=depth).fold(inner.to_owned(), |url, level| {
        format!("https://outer{level}.example.invalid/r?target={}&x=1", enc(&url))
    })
}

/// What is planted, under which key, and every text that may not survive in any encoding.
fn planted() -> Vec<(&'static str, String, Vec<String>)> {
    let pieces = |value: &str| -> Vec<String> {
        let mut all = vec![value.to_owned()];
        all.extend(
            value
                .split(['+', '/', '=', ' ', ',', '.', '@', ':'])
                .filter(|piece| piece.chars().count() >= 3)
                .map(str::to_owned),
        );
        all
    };
    let coworker_mri = format!("8:orgid:{COWORKER_GUID}");
    let thread = format!("19:{THREAD_BODY}@thread.v2");
    vec![
        ("sig", SIGNATURE.to_owned(), pieces(SIGNATURE)),
        ("X-Amz-Signature", AMZ_SIGNATURE.to_owned(), pieces(AMZ_SIGNATURE)),
        ("access_token", REFRESH.to_owned(), vec![REFRESH.to_owned()]),
        ("skypetoken", SKYPETOKEN.to_owned(), vec![SKYPETOKEN.to_owned()]),
        ("code", AUTH_CODE.to_owned(), vec![AUTH_CODE.to_owned()]),
        ("tid", TENANT_GUID.to_owned(), vec![TENANT_GUID.to_owned()]),
        (
            "mri",
            coworker_mri.clone(),
            vec![coworker_mri, COWORKER_GUID.to_owned()],
        ),
        (
            "upn",
            COWORKER_UPN.to_owned(),
            vec![COWORKER_UPN.to_owned(), "jane.doe".to_owned()],
        ),
        ("thread", thread.clone(), vec![thread, THREAD_BODY.to_owned()]),
        ("ip", PUBLIC_IP.to_owned(), vec![PUBLIC_IP.to_owned()]),
        ("q", format!("Hola {ACCENTED_NAME}"), pieces(ACCENTED_NAME)),
        ("displayName", CYRILLIC_NAME.to_owned(), pieces(CYRILLIC_NAME)),
        ("text", "Привет, Дмитрий".to_owned(), pieces(CYRILLIC_NAME)),
        // a "Last, First" display name (learned from JSON): its surname alone, the swapped order, a OneDrive path
        ("text", "thanks Doe".to_owned(), pieces(COMMA_NAME)),
        ("text", "Hi Jane Doe".to_owned(), pieces(COMMA_NAME)),
        (
            "path",
            "/personal/jane_doe_contoso_com/Documents".to_owned(),
            vec!["jane_doe".to_owned(), "jane".to_owned(), "doe".to_owned()],
        ),
        // a name only ever seen in form data (`displayName=Odalys+Quennell`), used elsewhere
        ("text", format!("Hi {FORM_ONLY_NAME}"), pieces(FORM_ONLY_NAME)),
        // a meeting link: its code and its passcode (a short value under a key no secret rule knows)
        (
            "joinUrl",
            meeting_link(),
            vec![MEETING_CODE.to_owned(), MEETING_PASSCODE.to_owned()],
        ),
        // a passcode of letters only, in a link and again on its own in free text
        (
            "joinUrl",
            alpha_link(),
            vec![ALPHA_CODE.to_owned(), ALPHA_PASSCODE.to_owned()],
        ),
        (
            "text",
            format!("use {ALPHA_PASSCODE} to join"),
            vec![ALPHA_PASSCODE.to_owned()],
        ),
        // a passcode that shows one layer of decoding after its code
        (
            "text",
            late_link(),
            vec![LATE_CODE.to_owned(), LATE_PASSCODE.to_owned()],
        ),
        // an invitation in French
        (
            "body",
            french_invitation(),
            vec![
                FRENCH_CODE.to_owned(),
                FRENCH_CODE.replace(' ', ""),
                FRENCH_PASSCODE.to_owned(),
            ],
        ),
        // the first name of a dotted display name (`mirela.wu`, learned from JSON)
        ("text", "Thanks Mirela".to_owned(), vec!["mirela".to_owned()]),
    ]
}

/// Every planted value in a query, in JSON in a query, and in a form, each as is and behind one and two redirects
/// that encode it again, plus the events names are learned from.
fn encoded_capture() -> Vec<Value> {
    let mut events = vec![
        json!({"type": "response_body", "body": json!({"displayName": ACCENTED_NAME}).to_string()}),
        json!({"type": "response_body", "body": json!({"value": [{"displayName": COMMA_NAME}]}).to_string()}),
        json!({"type": "request_body", "postData": format!("displayName={}&next=1", form_enc(FORM_ONLY_NAME))}),
        json!({"type": "response_body", "body": json!({"displayName": DOTTED_NAME}).to_string()}),
    ];
    for (key, value, _) in planted() {
        let query = format!("https://svc.example.invalid/p?{key}={}&next=1", enc(&value));
        let in_json = format!(
            "https://svc.example.invalid/p?context={}",
            enc(&json!({ key: value }).to_string())
        );
        let form = format!("{key}={}&next=1", form_enc(&value));
        for depth in 0..=2 {
            events.push(cdp(
                "Network.requestWillBeSent",
                &json!({"request": {"url": wrapped(&query, depth), "postData": wrapped(&form, depth)}}),
            ));
            events.push(cdp(
                "Network.requestWillBeSent",
                &json!({"request": {"url": wrapped(&in_json, depth)}}),
            ));
        }
    }
    events
}

/// `original` in every encoding a capture may hold it in, lowercased.
fn encodings(original: &str) -> Vec<String> {
    let forms = [
        original.to_owned(),
        enc(original),
        enc_times(original, 2),
        enc_times(original, 3),
        form_enc(original),
        enc(&form_enc(original)),
    ];
    forms.iter().map(|form| form.to_lowercase()).collect()
}

#[test]
fn nothing_planted_survives_in_any_encoding() {
    let sanitized = sanitize_capture(&encoded_capture(), &identities()).unwrap();
    let out = sanitized.events;
    // the export's own check for names finds nothing left either
    assert_eq!(check_names(&sanitized.names, &as_jsonl(&out)), []);
    let text = as_jsonl(&out).to_lowercase();
    for (key, _, originals) in planted() {
        for original in originals {
            for form in encodings(&original) {
                assert!(!text.contains(&form), "{key}: {form} survived");
            }
        }
    }
    assert_eq!(Guard::new().unwrap().scan_text(&as_jsonl(&out)), []);
    let again = sanitize_events(&out, &identities()).unwrap();
    assert_eq!(as_jsonl(&again), as_jsonl(&out), "sanitizing is idempotent here too");
}

#[test]
fn encoded_secrets_names_and_guids_become_placeholders_in_place() {
    let events = [
        json!({"displayName": ACCENTED_NAME}),
        json!({
            // a signed URL inside a query value, its signature encoded twice
            "signed": "?target=https%3A%2F%2Fstorage.example.invalid%2Fblob%3Fsig%3DabCD12%252BefGH34%252FijKL56%252BmnOP78%252FqrST90%252BuvWX123A%253D",
            // a GUID right after a percent escape
            "context": format!("context=%7B%22Tid%22%3A%22{TENANT_GUID}%22%7D"),
            // a learned name with its non-ASCII letters encoded
            "search": "?text=Jos%C3%A9%20Garc%C3%ADa",
            "legacy": "?text=Jos%E9",
        }),
    ];
    let out = sanitize_events(&events, &Identities::default()).unwrap().remove(1);
    assert_eq!(
        out["signed"],
        "?target=https%3A%2F%2Fstorage.example.invalid%2Fblob%3Fsig%3D<token:1>"
    );
    assert_eq!(out["context"], "context=%7B%22Tid%22%3A%22<guid:1>%22%7D");
    assert_eq!(out["search"], "?text=<name:1>");
    assert_eq!(out["legacy"], "?text=<name:1>");
}

#[test]
fn every_kind_of_escape_is_seen_through_and_the_rest_kept_as_it_came() {
    let events = [
        json!({"displayName": ACCENTED_NAME}),
        json!({
            "unicode": r"Hola Jos\u00e9 Garc\u00eda",
            "twice": r"Hola Jos\\u00e9",
            "html": "<p>Hola Jos&#233;&nbsp;Garc&#xED;a &amp; co</p>",
            "pair": r"\ud83d\ude00 Garc\u00EDa",
            "lone": r"\ud83d Garc\u00EDa",
            "path": r"C:\new\table\José",
            "untouched": "https://x.example.invalid/?q=caf%C3%A9%20au%20lait&r=%2Fhome%3Fa%3D1&e=%E9&s=\\u00e9\\n&h=&amp;",
            // placeholder-shaped text that came from escapes is no placeholder
            "fake": "%3Cname:%5Aebediah%3E",
        }),
        json!({"displayName": "Zebediah"}),
    ];
    let out = sanitize_events(&events, &Identities::default()).unwrap().remove(1);
    assert_eq!(out["unicode"], "Hola <name:1>");
    assert_eq!(out["twice"], "Hola <name:1>");
    assert_eq!(out["html"], "<p>Hola <name:1> &amp; co</p>");
    assert_eq!(out["pair"], r"\ud83d\ude00 <name:1>");
    assert_eq!(out["lone"], r"\ud83d <name:1>");
    assert_eq!(out["path"], r"C:\new\table\<name:1>");
    assert_eq!(
        out["untouched"],
        "https://x.example.invalid/?q=caf%C3%A9%20au%20lait&r=%2Fhome%3Fa%3D1&e=%E9&s=\\u00e9\\n&h=&amp;"
    );
    assert_eq!(out["fake"], "%3Cname:<name:2>%3E");
}

#[test]
fn an_encoded_delimiter_never_cuts_a_secret_short() {
    let event =
        json!({"url": "https://x.example.invalid/p?access_token=PartOneSecret%26PartTwoSecret%3DPartThreeSecret&n=1"});
    let out = sanitize_events(&[event], &Identities::default()).unwrap().remove(0);
    let url = out["url"].as_str().unwrap();
    for piece in ["PartOneSecret", "PartTwoSecret", "PartThreeSecret"] {
        assert!(!url.contains(piece), "{piece} survived: {url}");
    }
    assert!(url.ends_with("access_token=<token:1>&n=1"), "{url}");
}

#[test]
fn a_name_seen_only_in_an_encoded_field_is_learned() {
    let events = [
        json!({"url": format!("https://x.example.invalid/people?displayName={}", enc(CYRILLIC_NAME))}),
        json!({"text": "Привет, Дмитрий!"}),
        json!({"body": format!("q={}", form_enc(&json!({"givenName": "Zebulon"}).to_string()))}),
        json!({"text": "zebulon called"}),
    ];
    let out = sanitize_events(&events, &Identities::default()).unwrap();
    assert_eq!(out[1]["text"], "Привет, <name:1>!");
    let text = as_jsonl(&out).to_lowercase();
    for gone in ["дмитрий", "иванов", "zebulon", &enc("Дмитрий").to_lowercase()] {
        assert!(!text.contains(gone), "{gone} survived: {text}");
    }
}

#[test]
fn a_name_seen_only_in_form_data_is_learned_with_its_spaces() {
    let events = [
        json!({"type": "request_body", "postData": "displayName=Marguerite+Fakename&x=1"}),
        json!({"text": "Hi Marguerite Fakename", "query": "?q=Marguerite%20Fakename"}),
        // a `%2B` is a literal plus: `C++` stays together while the `+` between words is a space
        json!({"type": "request_body", "postData": "givenName=Ottoline%2B+Quarrington"}),
        json!({"text": "ottoline+ and Quarrington"}),
    ];
    let out = sanitize_events(&events, &Identities::default()).unwrap();
    assert_eq!(out[0]["postData"], "displayName=<name:1>&x=1");
    assert_eq!(out[1]["text"], "Hi <name:1>");
    assert_eq!(out[1]["query"], "?q=<name:1>");
    let text = as_jsonl(&out).to_lowercase();
    for gone in ["marguerite", "fakename", "ottoline", "quarrington"] {
        assert!(!text.contains(gone), "{gone} survived: {text}");
    }
}

#[test]
fn a_last_comma_first_name_is_replaced_word_by_word_and_swapped() {
    let events = [
        json!({"displayName": "Doe, Jane"}),
        json!({
            "text": "thanks Doe",
            "greeting": "Hi Jane Doe",
            "path": "https://contoso-my.example.invalid/personal/jane_doe_contoso_com/Documents",
            "query": "?q=Jane+Doe&r=jane%20doe",
        }),
    ];
    let out = sanitize_events(&events, &Identities::default()).unwrap();
    assert_eq!(out[0]["displayName"], "<name:1>");
    assert_eq!(out[1]["text"], "thanks <name:1>");
    assert_eq!(out[1]["greeting"], "Hi <name:1>");
    assert_eq!(
        out[1]["path"],
        "https://contoso-my.example.invalid/personal/<name:1>_contoso_com/Documents"
    );
    assert_eq!(out[1]["query"], "?q=<name:1>&r=<name:1>");
    // punctuation around a word of a name doesn't keep the word from being one
    let dotted = sanitize_events(
        &[
            json!({"displayName": "O'Hara-Quill (Contractor)"}),
            json!({"text": "ask quill or o'hara, the contractor"}),
        ],
        &Identities::default(),
    )
    .unwrap();
    assert_eq!(dotted[1]["text"], "ask <name:1> or o'<name:1>, the <name:1>");
}

#[test]
fn a_name_is_found_where_a_longer_variant_stops_short_and_in_keys() {
    let events = [
        json!({"displayName": "Doe, Jane"}),
        json!({
            // `Jane Doe` matches up to `Does`, which isn't the name: `Jane` still is one
            "text": "Jane Doesn't know, xJane Doe",
            // a map keyed by display name
            "reactions": {"Jane Doe": "like", "Doe, Jane": "heart", "other": "ok"},
        }),
    ];
    let out = sanitize_events(&events, &Identities::default()).unwrap();
    assert_eq!(out[1]["text"], "<name:1> Doesn't know, xJane <name:1>");
    // keys holding a name become its placeholder (two keys for one name become one)
    let keys: Vec<&String> = out[1]["reactions"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["<name:1>", "other"]);
}

// --- meetings ---

#[test]
fn a_meeting_links_code_and_passcode_become_placeholders() {
    let events = [
        json!({"joinUrl": meeting_link()}),
        json!({"links": {"join": format!("https://teams.live.com/meet/{MEETING_CODE}?anon=true&p={MEETING_PASSCODE}")}}),
        json!({"url": format!("https://teams.microsoft.com/v2/?meetingjoin=true#/meet/{MEETING_CODE}?p={MEETING_PASSCODE}&x=1")}),
        json!({"url": format!("https://teams.cloud.microsoft/lm/meet/{MEETING_CODE}?p={MEETING_PASSCODE}")}),
        // the routes that look like one are no meeting
        json!({"routes": ["/meet/meet-now", "/v2/meet/meet-now", "/l/meetup-join/"]}),
        // only the query encoded; in an HTML attribute; a legacy join link
        json!({"url": format!("https://teams.microsoft.com/meet/{MEETING_CODE}%3Fp%3D{MEETING_PASSCODE}")}),
        json!({"html": format!("<a href=\"https://teams.microsoft.com/meet/{MEETING_CODE}?anon=true&amp;p={MEETING_PASSCODE}\">Join</a>")}),
        json!({"url": format!("https://teams.microsoft.com/l/meetup-join/19%3ameeting_x%40thread.v2/0?context=%7b%7d&p={MEETING_PASSCODE}")}),
    ];
    let out = sanitize_events(&events, &Identities::default()).unwrap();
    assert_eq!(
        out[0]["joinUrl"],
        "https://teams.microsoft.com/meet/<meeting:1>?p=<token:1>"
    );
    assert_eq!(
        out[1]["links"]["join"],
        "https://teams.live.com/meet/<meeting:1>?anon=true&p=<token:1>"
    );
    assert_eq!(
        out[2]["url"],
        "https://teams.microsoft.com/v2/?meetingjoin=true#/meet/<meeting:1>?p=<token:1>&x=1"
    );
    assert_eq!(
        out[3]["url"],
        "https://teams.cloud.microsoft/lm/meet/<meeting:1>?p=<token:1>"
    );
    assert_eq!(out[4], events[4]);
    assert_eq!(
        out[5]["url"],
        "https://teams.microsoft.com/meet/<meeting:1>%3Fp%3D<token:1>"
    );
    assert_eq!(
        out[6]["html"],
        "<a href=\"https://teams.microsoft.com/meet/<meeting:1>?anon=true&amp;p=<token:1>\">Join</a>"
    );
    assert_eq!(
        out[7]["url"],
        "https://teams.microsoft.com/l/meetup-join/19%3a<thread:1>%40thread.v2/0?context=%7b%7d&p=<token:1>"
    );
    assert_eq!(Guard::new().unwrap().scan_text(&as_jsonl(&out)), []);
}

#[test]
fn a_meetings_code_and_passcode_are_found_under_their_keys_in_invitations_and_everywhere_else() {
    let events = [
        json!({"meetingData": {"meetingCode": MEETING_CODE, "passcode": MEETING_PASSCODE}}),
        // as numbers
        json!({"joinMeetingIdSettings": {"joinMeetingId": 1_234_567_890_123_u64, "passcode": 246_813}}),
        json!({"url": format!("https://rpe.example.invalid/c?meetingId={MEETING_CODE}&meetingPw={MEETING_PASSCODE}")}),
        json!({"body": format!("<p>Meeting ID: <span>{INVITE_CODE}</span><br>Passcode: <b>{INVITE_PASSCODE}</b></p>")}),
        json!({"audioConferencing": {"conferenceId": DIAL_IN_ID}}),
        // what was learned anywhere is replaced everywhere else, the code in any grouping
        json!({"text": format!(
            "join {MEETING_CODE} or {} with {INVITE_PASSCODE}; dial +1 323 555 0199,,{DIAL_IN_ID}# then 246813",
            INVITE_CODE.replace(' ', "")
        )}),
    ];
    let out = sanitize_events(&events, &Identities::default()).unwrap();
    assert_eq!(
        out[0]["meetingData"],
        json!({"meetingCode": "<meeting:1>", "passcode": "<token:1>"})
    );
    assert_eq!(
        out[1]["joinMeetingIdSettings"],
        json!({"joinMeetingId": "<meeting:1>", "passcode": "<token:2>"})
    );
    assert_eq!(
        out[2]["url"],
        "https://rpe.example.invalid/c?meetingId=<meeting:1>&meetingPw=<token:1>"
    );
    assert_eq!(
        out[3]["body"],
        "<p>Meeting ID: <span><meeting:2></span><br>Passcode: <b><token:3></b></p>"
    );
    assert_eq!(out[4]["audioConferencing"]["conferenceId"], "<meeting:3>");
    assert_eq!(
        out[5]["text"],
        "join <meeting:1> or <meeting:2> with <token:3>; dial +1 323 555 0199,,<meeting:3># then <token:2>"
    );
    assert_eq!(Guard::new().unwrap().scan_text(&as_jsonl(&out)), []);
}

#[test]
fn a_passcode_of_letters_only_is_learned_and_replaced_everywhere() {
    let events = [
        json!({"joinUrl": format!("https://teams.microsoft.com/meet/{MEETING_CODE}?p=aBcDeF")}),
        json!({"meetingData": {"passcode": "ZqNBBm"}}),
        json!({"body": "<p>Meeting ID: 355 123 456 789<br>Passcode: <b>kwbxpq</b></p>"}),
        json!({"content": "Use aBcDeF to join, or ZqNBBm, or KWBXPQ"}),
        // a status word under a passcode's key is redacted where it is, and nowhere else
        json!({"passcodePolicy": "required", "meetingPasscode": "enabled", "text": "required and enabled"}),
        // a learned passcode that is a host's label too: the host would be destroyed, so the export refuses
        json!({"url": "https://abcdef.example.test/x"}),
    ];
    let sanitized = sanitize_capture(&events, &Identities::default()).unwrap();
    assert_eq!(sanitized.name_in_host, [6]);
    let out = sanitized.events;
    assert_eq!(
        out[0]["joinUrl"],
        "https://teams.microsoft.com/meet/<meeting:1>?p=<token:1>"
    );
    assert_eq!(out[3]["content"], "Use <token:1> to join, or <token:2>, or <token:3>");
    assert_eq!(out[4]["text"], "required and enabled");
    assert_eq!(Guard::new().unwrap().scan_text(&as_jsonl(&out)), []);
    // the guard finds each passcode of letters only that the sanitizer would have left
    let guard = Guard::new().unwrap();
    for raw in [&events[1], &events[2]] {
        let kinds: Vec<Kind> = guard
            .scan_text(&raw.to_string())
            .into_iter()
            .map(|(_, kind)| kind)
            .collect();
        assert!(kinds.contains(&Kind::Passcode), "{raw}: {kinds:?}");
    }
}

#[test]
fn an_invitation_in_another_language_gives_up_its_code_and_passcode() {
    let legacy = "https://teams.microsoft.com/l/meetup-join/19%3ameeting_x%40thread.v2/0?context=%7b%7d";
    let events = [
        // German and Russian, their labels as the bundle's locale strings spell them
        json!({"body": "<p>Besprechungs-ID: 355 123 456 789<br>Passcode: QxbRtz</p>"}),
        json!({"body": "Идентификатор собрания: 377 111 222 333\nСекретный код: pqrstu"}),
        json!({"body": french_invitation()}),
        // labels no list has, in an invitation with a join link that holds neither code nor passcode
        json!({"body": format!(
            "<a href=\"{legacy}\">Join</a><br><span>Besprechungsnummer: 388 999 000 111</span><br><span>Kennung: <b>WxYzAb</b></span><br><span>Ort: Online</span>"
        )}),
        json!({"text": format!(
            "codes 355123456789 377111222333 {} 388999000111, passcodes QxbRtz pqrstu {FRENCH_PASSCODE} WxYzAb",
            FRENCH_CODE.replace(' ', "")
        )}),
        // a value that may be a word, given away only by the structure: replaced after a label, not as a word
        json!({"note": "Ort: Online", "status": "online"}),
    ];
    let out = sanitize_events(&events, &Identities::default()).unwrap();
    assert_eq!(
        out[0]["body"],
        "<p>Besprechungs-ID: <meeting:1><br>Passcode: <token:1></p>"
    );
    assert_eq!(
        out[1]["body"],
        "Идентификатор собрания: <meeting:2>\nСекретный код: <token:2>"
    );
    assert_eq!(
        out[2]["body"],
        "<div>ID de la réunion\u{a0}: <span><meeting:3></span></div><div>Code secret\u{a0}: <span><token:3></span></div>"
    );
    assert_eq!(
        out[3]["body"],
        "<a href=\"https://teams.microsoft.com/l/meetup-join/19%3a<thread:1>%40thread.v2/0?context=%7b%7d\">Join</a><br><span>Besprechungsnummer: <meeting:4></span><br><span>Kennung: <b><token:4></b></span><br><span>Ort: <token:5></span>"
    );
    assert_eq!(
        out[4]["text"],
        "codes <meeting:1> <meeting:2> <meeting:3> <meeting:4>, passcodes <token:1> <token:2> <token:3> <token:4>"
    );
    assert_eq!(out[5], json!({"note": "Ort: <token:5>", "status": "online"}));
    let guard = Guard::new().unwrap();
    assert_eq!(guard.scan_text(&as_jsonl(&out)), []);
    // the guard finds the localized labels' values the sanitizer would have left
    for raw in &events[..3] {
        let kinds: Vec<Kind> = guard
            .scan_text(&raw.to_string())
            .into_iter()
            .map(|(_, kind)| kind)
            .collect();
        assert!(kinds.contains(&Kind::MeetingId), "{raw}: {kinds:?}");
        assert!(kinds.contains(&Kind::Passcode), "{raw}: {kinds:?}");
    }
}

#[test]
fn a_passcode_a_layer_after_its_code_is_replaced_and_found_by_the_guard() {
    // raw text, not JSON: its `&` is escaped as `\u0026`
    let events = [json!({"text": format!("join at {} now", late_link())})];
    let out = sanitize_events(&events, &Identities::default()).unwrap();
    assert_eq!(
        out[0]["text"],
        r"join at https://teams.microsoft.com/meet/<meeting:1>?anon=true\u0026p=<token:1> now"
    );
    let guard = Guard::new().unwrap();
    assert_eq!(guard.scan_text(&as_jsonl(&out)), []);
    // a sanitizer that took only the code leaves a link the guard refuses, in each form of its `&`
    for left in [
        r"/meet/<meeting:1>?anon=true\u0026p=hQzRtW",
        r"/meet/<meeting:1>?anon=true\x26p=hQzRtW",
        "/meet/<meeting:1>?anon=true&#38;p=hQzRtW",
        "/meet/<meeting:1>?anon=true&#x26;p=hQzRtW",
    ] {
        let kinds: Vec<Kind> = guard.scan_text(left).into_iter().map(|(_, kind)| kind).collect();
        assert_eq!(kinds, [Kind::MeetingLink], "{left}");
    }
}

// --- names and hosts ---

#[test]
fn a_dotted_display_name_is_a_name_and_not_a_domain() {
    let events = [
        json!({"displayName": "jane.wu"}),
        json!({"displayName": "Mei.Li"}),
        json!({"displayName": "Ann Doe.CA"}),
        json!({"displayName": "Lab (lab.example.org)"}),
        json!({"text": "Thanks Jane, Mei, Ann and Doe; see https://www.example.org/x"}),
    ];
    let out = sanitize_capture(&events, &Identities::default()).unwrap();
    assert_eq!(
        out.events[4]["text"],
        "Thanks <name:1>, <name:2>, <name:3> and <name:3>; see https://www.example.org/x"
    );
    assert_eq!(out.name_in_host, Vec::<usize>::new());
    assert_eq!(check_names(&out.names, &as_jsonl(&out.events)), []);
    // the export's check knows the first name too
    let names = names_in(&[json!({"displayName": "jane.wu"})]).unwrap();
    assert_eq!(check_names(&names, "{\"text\":\"Thanks Jane\"}\n").len(), 1);
}

/// Every URL of `urls` in `text`, as often as `times`.
fn assert_urls_intact(text: &str, urls: &[&str], times: usize) {
    for url in urls {
        assert_eq!(text.matches(url).count(), times, "{url} was changed: {text}");
    }
}

#[test]
fn an_email_shaped_display_name_leaves_every_host_intact() {
    let urls = [
        "https://teams.microsoft.com/api/mt/emea/beta/users/",
        "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        "https://api.contoso.com/v1/x",
        "https://www.example.org/x",
        "wss://pub.example.com/v1",
    ];
    let events = vec![
        json!({"displayName": "jane.doe@example.com"}),
        json!({"displayName": "jane.doe@contoso.com"}),
        json!({"displayName": "Lab (lab.example.org)"}),
        json!({"displayName": format!("8:orgid:{COWORKER_GUID}")}),
        json!({"urls": urls, "cookie": "x=1; Domain=.teams.microsoft.com; Path=/"}),
        json!({"text": "ask jane.doe@example.com in the lab", "port": "8080"}),
    ];
    let out = sanitize_capture(&events, &Identities::default()).unwrap();
    let text = as_jsonl(&out.events);
    assert_urls_intact(&text, &urls, 1);
    assert!(text.contains("Domain=.teams.microsoft.com"), "{text}");
    assert_eq!(out.events[5]["text"], "ask <name:1> in the <name:3>");
    assert_eq!(out.name_in_host, Vec::<usize>::new());
    // the export's own check doesn't take a domain's labels for a name's words either
    assert_eq!(check_names(&out.names, &text), []);
    let names = names_in(&[json!({"displayName": "jane.doe@contoso.com"})]).unwrap();
    assert_eq!(check_names(&names, "{\"u\":\"https://api.contoso.com/v1\"}\n"), []);

    // and an export of it is written with every host as it was
    let dir = tempfile::tempdir().unwrap();
    let capture = write_capture(dir.path(), &events).unwrap();
    let exported = export::export(&capture, "hosts", &dir.path().join("fixtures"), &Identities::default()).unwrap();
    assert_urls_intact(&fs::read_to_string(exported.path).unwrap(), &urls, 1);
}

#[test]
fn a_name_word_in_a_public_protocol_host_is_kept() {
    let events = [
        json!({"displayName": "Microsoft Teams"}),
        json!({"displayName": "Login Live"}),
        json!({
            "url": "https://teams.microsoft.com/api/x?next=https%3A%2F%2Flogin.live.com%2Fy",
            "text": "open Microsoft Teams, teams rock; live long",
        }),
    ];
    let out = sanitize_capture(&events, &Identities::default()).unwrap();
    assert_eq!(
        out.events[2]["url"],
        "https://teams.microsoft.com/api/x?next=https%3A%2F%2Flogin.live.com%2Fy"
    );
    assert_eq!(out.events[2]["text"], "open <name:1>, <name:1> rock; <name:2> long");
    assert_eq!(out.name_in_host, Vec::<usize>::new());
    assert_eq!(check_names(&out.names, &as_jsonl(&out.events)), []);
}

#[test]
fn a_name_word_in_any_other_host_is_reported() {
    let events = [
        json!({"displayName": "Jane Quill"}),
        json!({"ok": "nothing here"}),
        json!({"url": "https://quill.example.test/a", "text": "Quill"}),
        // a lookalike of a public host is no public host
        json!({"url": "https://teams.microsoft.com.quill.example.test/b"}),
    ];
    let out = sanitize_capture(&events, &Identities::default()).unwrap();
    assert_eq!(out.name_in_host, [3, 4]);
    // replaced all the same: nothing is left of the name
    assert_eq!(out.events[2]["url"], "https://<name:1>.example.test/a");
}

// --- the export's check for names ---

/// The names found in `events` (a capture), as the export gets them.
fn names_in(events: &[Value]) -> TestResult<LearnedNames> {
    Ok(sanitize_capture(events, &Identities::default())?.names)
}

#[test]
fn the_name_check_finds_a_name_left_as_written() {
    let names = names_in(&[
        json!({"displayName": "Doe, Jane"}),
        json!({"postData": "givenName=Marguerite+Fakename"}),
    ])
    .unwrap();
    let text =
        "{\"ok\":\"<name:1> and <name:2>\"}\n{\"text\":\"thanks DOE\"}\n{\"path\":\"/personal/x_fakename_y/\"}\n";
    let found = check_names(&names, text);
    assert_eq!(
        found,
        [
            NameFinding::Name {
                line: 2,
                layer: 0,
                label: "<name:1>".to_owned()
            },
            NameFinding::Name {
                line: 3,
                layer: 0,
                label: "<name:2>".to_owned()
            },
        ]
    );
    // what it reports and how it prints never holds the name
    let shown = format!("{found:?} {} {names:?}", found[0]);
    for name in ["doe", "jane", "marguerite", "fakename"] {
        assert!(!shown.to_lowercase().contains(name), "{shown}");
    }
    // a sanitized capture holding those names passes
    let clean = sanitize_events(
        &[
            json!({"displayName": "Doe, Jane"}),
            json!({"postData": "givenName=Marguerite+Fakename"}),
            json!({"text": "thanks Doe, said Marguerite", "path": "/personal/jane_doe_contoso_com/"}),
        ],
        &Identities::default(),
    )
    .unwrap();
    assert_eq!(check_names(&names, &as_jsonl(&clean)), []);
}

#[test]
fn the_name_check_sees_through_every_layer_of_encoding() {
    let names = names_in(&[json!({"displayName": "Jane Doe"})]).unwrap();
    // every byte of `Jane` percent-encoded, then encoded twice more: only the third decoded layer shows it
    let deep = enc_times("%4A%61%6E%65", 2);
    let lines = [
        "{\"ok\":\"<name:1>\"}".to_owned(),
        format!("{{\"q\":\"?q={deep}\"}}"),
        // an HTML reference and a backslash escape
        "{\"html\":\"<p>Hi &#68;oe</p>\",\"js\":\"\\\\u0044oe\"}".to_owned(),
        // a letter escaped right before the name: the name is a word apart from it too
        "{\"q\":\"?q=%41Jane\"}".to_owned(),
        // placeholder-shaped text that only shows once decoded is no placeholder
        "{\"q\":\"%3Cname:%44oe%3E\"}".to_owned(),
    ];
    let found = check_names(&names, &lines.join("\n"));
    let at: Vec<(usize, usize)> = found
        .iter()
        .map(|finding| match finding {
            NameFinding::Name { line, layer, .. } => (*line, *layer),
            NameFinding::TooDeep { line } => panic!("line {line} too deep"),
        })
        .collect();
    assert_eq!(at, [(2, 3), (3, 1), (4, 1), (5, 1)]);
}

#[test]
fn a_line_too_deep_to_check_is_refused() {
    let names = names_in(&[json!({"displayName": "Jane Doe"})]).unwrap();
    let deep = |times: usize| enc_times(&enc("a b"), times);
    // within the bound: decoded to the end, nothing found
    assert_eq!(check_names(&names, &deep(CHECK_ROUNDS - 1)), []);
    assert_eq!(
        check_names(&names, &format!("x\n{}", deep(CHECK_ROUNDS))),
        [NameFinding::TooDeep { line: 2 }]
    );
    // nothing to look for, nothing to refuse
    assert_eq!(check_names(&LearnedNames::default(), &deep(CHECK_ROUNDS + 2)), []);
}

#[test]
fn placeholders_and_short_words_are_no_names_to_the_check() {
    // a group whose words are placeholder kinds, and a name of two short words
    let names = names_in(&[json!({"displayName": "Domain Users"}), json!({"displayName": "Li Wu"})]).unwrap();
    let text = "{\"to\":\"<user:a>\",\"tenant\":\"<domain:tenant>\",\"who\":\"<name:2>\",\"short\":\"li wu\"}\n";
    assert_eq!(check_names(&names, text), []);
    let text = "{\"text\":\"for all domain users\"}\n";
    assert_eq!(check_names(&names, text).len(), 1);
    // a short name is still replaced whole where it is a name field's value
    let out = sanitize_events(&[json!({"displayName": "Li Wu"})], &Identities::default()).unwrap();
    assert_eq!(out[0]["displayName"], "<name:1>");
}

// --- export ---

fn write_capture(dir: &Path, events: &[Value]) -> TestResult<std::path::PathBuf> {
    let capture = dir.join("capture");
    fs::create_dir_all(&capture)?;
    fs::write(capture.join("events.jsonl"), as_jsonl(events))?;
    Ok(capture)
}

#[test]
fn export_writes_the_sanitized_capture_under_the_reference_name() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = dir.path().join("reference");
    let capture = write_capture(dir.path(), &capture()).unwrap();
    let exported = export::export(&capture, "calls/echo", &fixtures, &identities()).unwrap();
    assert_eq!(exported.path, fixtures.join("calls/echo.jsonl"));
    assert_eq!(exported.events, capture_len());
    let written = fs::read_to_string(&exported.path).unwrap();
    assert_eq!(written, as_jsonl(&sanitized().unwrap()));
    assert_eq!(Guard::new().unwrap().scan_paths(&[&fixtures]).unwrap(), []);
    // nothing else is left in the fixtures dir (no staging file)
    let names: Vec<_> = fs::read_dir(fixtures.join("calls"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["echo.jsonl"]);

    // the events file itself works too, and a .json name gets one pretty array
    let exported = export::export(
        &capture.join("events.jsonl"),
        "auth/authz.json",
        &fixtures,
        &identities(),
    )
    .unwrap();
    let array: Vec<Value> = serde_json::from_str(&fs::read_to_string(&exported.path).unwrap()).unwrap();
    assert_eq!(array, sanitized().unwrap());
}

fn capture_len() -> usize {
    capture().len()
}

#[test]
fn a_fixture_the_guard_refuses_is_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tests/fixtures/leak.jsonl");
    let text = format!("{{\"ok\":1}}\n{{\"authorization\":\"Bearer {JWT}\"}}\n");
    let error = export::write_reference(&path, &text).unwrap_err();
    let ExportError::Refused(found) = &error else {
        panic!("expected a refusal, got {error}");
    };
    assert!(found.contains(&(2, Kind::Jwt)), "{found:?}");
    let message = error.to_string();
    assert!(message.contains("line 2: Jwt"), "{message}");
    assert!(!message.contains(JWT), "the refusal must not show the value");
    assert!(!dir.path().join("tests").exists(), "nothing may be written");

    // an existing fixture of that name stays as it was
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{\"old\":true}\n").unwrap();
    export::write_reference(&path, &text).unwrap_err();
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"old\":true}\n");
    let names: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["leak.jsonl"]);
}

#[test]
fn a_fixture_that_cant_be_put_in_place_leaves_no_staging_file() {
    let dir = tempfile::tempdir().unwrap();
    // a non-empty directory where the fixture should go: the rename fails
    let path = dir.path().join("taken.jsonl");
    fs::create_dir_all(path.join("inside")).unwrap();
    let error = export::write_reference(&path, "{\"ok\":1}\n").unwrap_err();
    assert!(matches!(error, ExportError::Io { .. }), "{error}");
    let mut names: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["taken.jsonl"]);
    assert!(path.join("inside").is_dir());
}

/// A capture naming people only in places the sanitizer reads: a "Last, First" display name, a name in form data.
fn named_capture() -> Vec<Value> {
    vec![
        json!({"type": "response_body", "body": json!({"value": [{"displayName": COMMA_NAME}]}).to_string()}),
        json!({"type": "request_body", "postData": format!("displayName={}&x=1", form_enc(FORM_ONLY_NAME))}),
        json!({"type": "rtc", "text": "thanks Doe", "path": "/personal/jane_doe_contoso_com/", "hi": "Hi Odalys Quennell"}),
    ]
}

#[test]
fn an_export_with_names_in_it_is_written_without_them() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = dir.path().join("fixtures");
    let capture = write_capture(dir.path(), &named_capture()).unwrap();
    let exported = export::export(&capture, "people", &fixtures, &Identities::default()).unwrap();
    let written = fs::read_to_string(&exported.path).unwrap().to_lowercase();
    for gone in ["doe", "jane", "odalys", "quennell"] {
        assert!(!written.contains(gone), "{gone} survived: {written}");
    }
    assert!(
        written.contains("<name:1>") && written.contains("<name:2>"),
        "{written}"
    );
}

#[test]
fn an_export_with_a_name_left_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = dir.path().join("fixtures");
    let capture = write_capture(dir.path(), &named_capture()).unwrap();
    let path = fixtures.join("people.jsonl");
    fs::create_dir_all(&fixtures).unwrap();
    fs::write(&path, "{\"old\":true}\n").unwrap();
    // a sanitizer that misses a surname: the real one, with the surname put back into its output
    let leaky = |events: &[Value]| {
        let mut sanitized = sanitize_capture(events, &Identities::default())?;
        sanitized.events[2]["text"] = json!("thanks Doe");
        Ok(sanitized)
    };
    let error = export::export_with(&capture, "people", &fixtures, leaky).unwrap_err();
    let ExportError::NameLeft(found) = &error else {
        panic!("expected a name left, got {error}");
    };
    assert_eq!(
        found,
        &[NameFinding::Name {
            line: 3,
            layer: 0,
            label: "<name:1>".to_owned()
        }]
    );
    let message = error.to_string();
    assert!(message.contains("line 3: <name:1>"), "{message}");
    assert!(
        !message.to_lowercase().contains("doe"),
        "the refusal must not show the name: {message}"
    );
    // the existing fixture stays as it was, and nothing else is written
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"old\":true}\n");
    let names: Vec<_> = fs::read_dir(&fixtures)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["people.jsonl"]);
    // the same seam with the real sanitizer writes the fixture: the check refuses only what is left
    export::export_with(&capture, "people", &fixtures, |events| {
        sanitize_capture(events, &Identities::default())
    })
    .unwrap();
    assert_ne!(fs::read_to_string(&path).unwrap(), "{\"old\":true}\n");
}

#[test]
fn an_export_that_would_put_a_name_in_a_host_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = dir.path().join("fixtures");
    let events = [
        json!({"displayName": "Jane Quill"}),
        json!({"url": "https://quill.example.test/a", "text": "thanks Quill"}),
    ];
    let capture = write_capture(dir.path(), &events).unwrap();
    let path = fixtures.join("hosts.jsonl");
    fs::create_dir_all(&fixtures).unwrap();
    fs::write(&path, "{\"old\":true}\n").unwrap();
    let error = export::export(&capture, "hosts", &fixtures, &Identities::default()).unwrap_err();
    let ExportError::NameInHost(events) = &error else {
        panic!("expected a name in a host, got {error}");
    };
    assert_eq!(events, &[2]);
    let message = error.to_string();
    assert!(message.contains("event 2"), "{message}");
    assert!(!message.to_lowercase().contains("quill"), "{message}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"old\":true}\n");
    let names: Vec<_> = fs::read_dir(&fixtures)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["hosts.jsonl"]);
}

#[test]
fn an_export_never_writes_a_meeting_passcode() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = dir.path().join("fixtures");
    let link = meeting_link();
    let events = [
        json!({"joinUrl": link}),
        json!({"url": format!("https://x.example.invalid/r?target={}", enc(&enc(&link)))}),
    ];
    let capture = write_capture(dir.path(), &events).unwrap();
    let exported = export::export(&capture, "meeting", &fixtures, &Identities::default()).unwrap();
    let written = fs::read_to_string(exported.path).unwrap();
    for gone in [MEETING_CODE, MEETING_PASSCODE] {
        assert!(!written.contains(gone), "{gone} was written: {written}");
    }
    assert!(
        written.contains("https://teams.microsoft.com/meet/<meeting:1>?p=<token:1>"),
        "{written}"
    );
    // a sanitizer that misses the passcode: the fixture guard refuses the export
    let leaky = |events: &[Value]| {
        let mut sanitized = sanitize_capture(events, &Identities::default())?;
        sanitized.events[0]["joinUrl"] = json!(meeting_link());
        Ok(sanitized)
    };
    let error = export::export_with(&capture, "leak", &fixtures, leaky).unwrap_err();
    assert!(
        matches!(&error, ExportError::Refused(found) if found.contains(&(1, Kind::MeetingLink))),
        "{error}"
    );
    assert!(!fixtures.join("leak.jsonl").exists());
}

#[test]
fn fixture_names_stay_inside_the_fixtures_dir() {
    let fixtures = Path::new("/repo/tests/fixtures");
    let ok = [
        ("auth/authz", "auth/authz.jsonl", Layout::Jsonl),
        ("auth/authz.jsonl", "auth/authz.jsonl", Layout::Jsonl),
        ("calling/echo/offer.json", "calling/echo/offer.json", Layout::Json),
        ("load_v2-1", "load_v2-1.jsonl", Layout::Jsonl),
    ];
    for (name, relative, layout) in ok {
        assert_eq!(
            export::reference_path(fixtures, name).unwrap(),
            (fixtures.join(relative), layout),
            "{name}"
        );
    }
    let bad = [
        "", "/etc/x", "../x", "a/../x", "a//b", ".hidden", "a/.x", "x.txt", "a b", "a\\b", "x/",
    ];
    for name in bad {
        assert!(
            matches!(export::reference_path(fixtures, name), Err(ExportError::BadName(_))),
            "{name:?} was accepted"
        );
    }
}

#[test]
fn a_malformed_capture_is_refused_with_its_line() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("events.jsonl");
    fs::write(&file, format!("{{\"a\":1}}\n\nnot json {JWT}\n")).unwrap();
    let error = export::export(&file, "x", &dir.path().join("fixtures"), &Identities::default()).unwrap_err();
    assert!(matches!(error, ExportError::Malformed { line: 3, .. }), "{error}");
    assert!(!error.to_string().contains(JWT));
    assert!(!dir.path().join("fixtures").exists());
}

fn mode(path: &Path) -> TestResult<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
}

#[test]
fn export_writes_a_private_reference_copy_under_the_lab_data_dir() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("teams-lab");
    let reference = export::reference_dir(&data_dir);
    assert_eq!(reference, data_dir.join("reference"));
    // a reference dir that was there with a wider mode is made private too
    fs::create_dir_all(&reference).unwrap();
    fs::set_permissions(&reference, fs::Permissions::from_mode(0o755)).unwrap();
    let capture = write_capture(dir.path(), &capture()).unwrap();
    let exported = export::export(&capture, "calls/echo", &reference, &identities()).unwrap();
    assert_eq!(exported.path, reference.join("calls/echo.jsonl"));
    assert_eq!(mode(&exported.path).unwrap(), 0o600);
    assert_eq!(mode(&reference.join("calls")).unwrap(), 0o700);
    assert_eq!(mode(&reference).unwrap(), 0o700);
}

#[test]
fn export_refuses_a_destination_inside_a_git_work_tree() {
    let dir = tempfile::tempdir().unwrap();
    let capture = write_capture(dir.path(), &capture()).unwrap();
    // a repository (`.git` dir), a linked worktree (`.git` file), and a symlink from outside into a repository
    let repo = dir.path().join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(repo.join("sub")).unwrap();
    let worktree = dir.path().join("worktree");
    fs::create_dir_all(&worktree).unwrap();
    fs::write(worktree.join(".git"), "gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
    let outside = dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(repo.join("sub"), outside.join("link")).unwrap();
    let inside = [
        repo.join("tests/fixtures"),
        repo.clone(),
        worktree.join("deep/reference"),
        outside.join("link/reference"),
    ];
    for reference in &inside {
        let error = export::export(&capture, "calls/echo", reference, &identities()).unwrap_err();
        assert!(
            matches!(error, ExportError::InsideRepository(_)),
            "{reference:?}: {error}"
        );
        assert!(
            !reference.join("calls").exists(),
            "{reference:?}: nothing may be written"
        );
        let error = export::write_reference(&reference.join("x.jsonl"), "{\"ok\":1}\n").unwrap_err();
        assert!(
            matches!(error, ExportError::InsideRepository(_)),
            "{reference:?}: {error}"
        );
        assert!(!reference.join("x.jsonl").exists());
    }
    assert!(!repo.join("tests").exists() && !worktree.join("deep").exists() && !repo.join("sub/reference").exists());

    // this repository's own tests/fixtures is refused before the capture is even read
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let missing = dir.path().join("no-such-capture");
    let error = export::export(&missing, "never-written", &fixtures, &identities()).unwrap_err();
    assert!(matches!(error, ExportError::InsideRepository(_)), "{error}");
    assert!(!fixtures.join("never-written.jsonl").exists());

    // and the same export outside every repository is written
    let exported = export::export(&capture, "calls/echo", &outside.join("reference"), &identities()).unwrap();
    assert!(exported.path.is_file());
}
