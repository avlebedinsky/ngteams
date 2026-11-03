//! Authz wire logging runs alone because the diagnostic logger is process-global.
//! All credentials and endpoint responses are synthetic loopback answers.

#![cfg(test)]

use serde_json::{Value, json};
use teams_auth::authz::{AuthzClient, AuthzError, AuthzKind};
use teams_auth::secrets::Secret;
use wiremock::matchers::{body_string, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const SPACES_TOKEN: &str = "authz-wire-spaces-value";

const WIRE_SKYPE: &str = "authz-wire-skype-value";
const WIRE_COOKIE: &str = "authz-wire-cookie-value";
const NESTED_SECRETS: [(&str, &str); 5] = [
    ("access_token", "authz-wire-access-value"),
    ("refresh_token", "authz-wire-refresh-value"),
    ("id_token", "authz-wire-id-value"),
    ("code", "authz-wire-code-value"),
    ("code_verifier", "authz-wire-verifier-value"),
];

fn wire_body(marker: &str) -> Value {
    let mut body = json!({
        "tokens": {"skypeToken": WIRE_SKYPE, "expiresIn": 3600},
        "region": "authz-wire-region",
        "partition": "authz-wire-partition",
        "regionGtms": {"chatService": "https://wire.example.test/chat"},
        "visible": marker,
        "description": format!("visible diagnostic with an echoed {}", SPACES_TOKEN),
        "nested": {},
    });
    for (name, secret) in NESTED_SECRETS {
        body["nested"][name] = json!(secret);
    }
    body
}

async fn wire_exchange(server: &MockServer, label: &str, status: u16) -> AuthzClient {
    let endpoint = format!("/{label}/authz");
    let mut body = wire_body(label);
    if status == 403 {
        body["errorCode"] = json!("WireDenied");
    }
    Mock::given(method("POST"))
        .and(path(endpoint.as_str()))
        .and(header("authorization", format!("Bearer {SPACES_TOKEN}")))
        .and(header("content-length", "0"))
        .and(body_string(""))
        .respond_with(
            ResponseTemplate::new(status)
                .insert_header("x-wire-metadata", "authz-wire-response")
                .insert_header("x-wire-echo", SPACES_TOKEN)
                .insert_header("set-cookie", format!("session={WIRE_COOKIE}"))
                .set_body_json(body),
        )
        .expect(1)
        .mount(server)
        .await;
    AuthzClient::with_endpoint(&format!("{}{endpoint}", server.uri())).unwrap()
}

fn assert_wire_exchange(text: &str, label: &str, status: u16) {
    let records: Vec<_> = text
        .lines()
        .filter(|line| line.contains(&format!("/{label}/authz")))
        .collect();
    assert_eq!(records.len(), 2, "one full request and response per exchange: {text}");
    assert!(records[0].contains("teams_wire::http: request POST"), "{text}");
    assert!(records[0].contains("authorization: [redacted]"), "{text}");
    assert!(records[0].contains("claimschallengecapable: true"), "{text}");
    assert!(records[0].contains("ms-teams-authz-type: TokenRefresh"), "{text}");
    assert!(records[0].contains("content-length: 0"), "{text}");
    assert!(records[1].contains(&format!("-> {status} in ")), "{text}");
    for value in [
        "x-wire-metadata: authz-wire-response",
        "x-wire-echo: [redacted]",
        "visible diagnostic with an echoed [redacted]",
        "set-cookie: [redacted]",
        "body {",
        label,
    ] {
        assert!(records[1].contains(value), "missing {value}: {text}");
    }
    for name in [
        "tokens",
        "access_token",
        "refresh_token",
        "id_token",
        "code",
        "code_verifier",
    ] {
        assert!(records[1].contains(&format!("\"{name}\":\"[redacted]\"")), "{text}");
    }
}

fn assert_wire_debug(text: &str, requests: &[Request]) {
    for (label, status) in [("authz-wire-ok", 200), ("authz-wire-refused", 403)] {
        assert_wire_exchange(text, label, status);
    }
    for request in requests {
        let correlation = request.headers.get("clientRequestId").unwrap().to_str().unwrap();
        assert!(text.contains(&format!("clientrequestid: {correlation}")), "{text}");
        assert!(text.contains(&format!("x-ms-request-id: {correlation}")), "{text}");
    }
    for secret in [SPACES_TOKEN, WIRE_SKYPE, WIRE_COOKIE]
        .into_iter()
        .chain(NESTED_SECRETS.map(|(_, value)| value))
    {
        assert!(!text.contains(secret), "credential reached the log: {secret}");
    }
}

#[tokio::test]
async fn authz_logs_full_success_and_refusal_redacted_at_debug_and_no_payload_at_info() {
    let dir = tempfile::tempdir().unwrap();
    let logger = teams_log::install(&teams_log::Config {
        dir: dir.path().into(),
        setting: teams_log::Level::Debug,
        env: None,
        rotation: teams_log::Rotation::default(),
    })
    .unwrap();
    let server = MockServer::start().await;
    let success = wire_exchange(&server, "authz-wire-ok", 200).await;
    let granted = success
        .exchange(&Secret::new(SPACES_TOKEN), AuthzKind::TokenRefresh)
        .await
        .unwrap();
    assert_eq!(granted.token.expose(), WIRE_SKYPE);
    assert_eq!(granted.region.name.as_deref(), Some("authz-wire-region"));
    let refused = wire_exchange(&server, "authz-wire-refused", 403).await;
    assert!(matches!(
        refused.exchange(&Secret::new(SPACES_TOKEN), AuthzKind::TokenRefresh).await,
        Err(AuthzError::Refused { status: 403, code: Some(code) }) if code == "WireDenied"
    ));
    assert!(logger.flush_written());
    let debug = std::fs::read_to_string(logger.path()).unwrap();
    assert_wire_debug(&debug, &server.received_requests().await.unwrap());

    assert_eq!(
        teams_log::apply_setting(teams_log::Level::Info),
        Some(teams_log::Level::Info)
    );
    let info = wire_exchange(&server, "authz-wire-info", 200).await;
    info.exchange(&Secret::new(SPACES_TOKEN), AuthzKind::TokenRefresh)
        .await
        .unwrap();
    assert!(logger.flush_written());
    let all = std::fs::read_to_string(logger.path()).unwrap();
    let info = &all[debug.len()..];
    for payload in [
        "teams_wire::http",
        "authz-wire-info",
        WIRE_SKYPE,
        WIRE_COOKIE,
        SPACES_TOKEN,
    ] {
        assert!(!info.contains(payload), "info contains authz payload: {info}");
    }
}
