//! The sanitizer and `proto-lab export`, offline, on a synthetic capture shaped like the recorder's (`cdp` events,
//! bodies, verdicts, WebRTC reports) and planted with every kind of secret and identity a Teams capture holds: secrets
//! are gone, identities are stable placeholders (the lab's by alias), sanitizing twice changes nothing, the fixture
//! guard finds nothing in the result, and an export the guard refuses writes nothing.
//!
//! Every secret and identity here is made up.

use std::fs;
use std::path::Path;

use base64::Engine as _;
use fixture_guard::{Guard, Kind};
use proto_lab::commands::export::{self, ExportError, Layout};
use proto_lab::config::Alias;
use proto_lab::sanitize::{Identities, KnownAccount, KnownService, Sanitizer, sanitize_events};
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

/// A response's headers, its raw header text, a binary frame and a base64 body.
fn transport_events() -> [Value; 4] {
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
    ]
}

fn capture() -> Vec<Value> {
    let mut events = vec![request_event(), people_event(), frame_event()];
    events.extend(token_events());
    events.extend(rtc_events());
    events.extend(transport_events());
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
            Kind::Jwt,
            Kind::Mri,
            Kind::OauthToken,
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

// --- export ---

fn write_capture(dir: &Path, events: &[Value]) -> TestResult<std::path::PathBuf> {
    let capture = dir.join("capture");
    fs::create_dir_all(&capture)?;
    fs::write(capture.join("events.jsonl"), as_jsonl(events))?;
    Ok(capture)
}

#[test]
fn export_writes_the_sanitized_capture_under_the_fixture_name() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = dir.path().join("tests/fixtures");
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
    let error = export::write_fixture(&path, &text).unwrap_err();
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
    export::write_fixture(&path, &text).unwrap_err();
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"old\":true}\n");
    let names: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["leak.jsonl"]);
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
            export::fixture_path(fixtures, name).unwrap(),
            (fixtures.join(relative), layout),
            "{name}"
        );
    }
    let bad = [
        "", "/etc/x", "../x", "a/../x", "a//b", ".hidden", "a/.x", "x.txt", "a b", "a\\b", "x/",
    ];
    for name in bad {
        assert!(
            matches!(export::fixture_path(fixtures, name), Err(ExportError::BadName(_))),
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

#[test]
fn the_fixtures_dir_is_found_from_inside_the_repository() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::create_dir_all(dir.path().join("tests/fixtures")).unwrap();
    fs::create_dir_all(dir.path().join("crates/x/src")).unwrap();
    let found = export::find_fixtures_dir(&dir.path().join("crates/x/src")).unwrap();
    assert_eq!(found, dir.path().join("tests/fixtures"));
    let elsewhere = tempfile::tempdir().unwrap();
    assert!(matches!(
        export::find_fixtures_dir(elsewhere.path()),
        Err(ExportError::NoFixturesDir(_))
    ));
}
