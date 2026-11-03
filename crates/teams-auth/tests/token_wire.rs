//! Token endpoint wire logging runs alone because the diagnostic logger is process-global.
//! All credentials and endpoint responses are synthetic.

#![cfg(test)]

use std::fs;

use teams_log::{Config, Level, Rotation};

use serde_json::json;
use teams_auth::secrets::MemorySecretStore;
use teams_auth::token::TokenClient;
use wiremock::MockServer;

#[path = "support/token_fixture.rs"]
mod fixture;
use fixture::{ACCOUNT, Fixture, TestResult, authorization, claims_of, store_with};

const CODE: &str = "wire-code-value+percent%and&equals=";
const REFRESH: &str = "wire-refresh-input+percent%and&equals=";
const ACCESS: &str = "wire-access-response-value";
const ROTATED: &str = "wire-rotated-response-value";
const COOKIE: &str = "wire-cookie-response-value";
const HEADER_AUTH: &str = "wire-authorization-response-value";
const NESTED: &str = "wire-nested-verifier-value";

fn fixture(name: &str, label: &str) -> TestResult<Fixture> {
    let mut fixture = Fixture::load(name)?.with_id_token(&claims_of(ACCOUNT))?;
    fixture.path = format!("/{label}/oauth2/v2.0/token");
    fixture.headers.extend([
        ("x-wire-answer".into(), "wire-visible-response-header".into()),
        ("set-cookie".into(), format!("session={COOKIE}; Secure")),
        ("authorization".into(), format!("Bearer {HEADER_AUTH}")),
    ]);
    fixture.body["access_token"] = ACCESS.into();
    fixture.body["refresh_token"] = ROTATED.into();
    fixture.body["diagnostic"] = json!({"visible": "wire-visible-response-body", "code_verifier": NESTED});
    Ok(fixture)
}

async fn send_code() -> TestResult<Vec<String>> {
    let mut fixture = fixture("entra-token-code", "wire-code-grant")?;
    fixture.form.insert("code".into(), CODE.into());
    let server = MockServer::start().await;
    let client = fixture.client(&server)?;
    let authorization = authorization(client.config(), &fixture)?;
    let verifier = authorization.pkce().verifier().expose().to_owned();
    fixture.mount_code(&server, authorization.pkce()).await?;
    client
        .exchange_code(&MemorySecretStore::new(), &authorization, CODE)
        .await?;
    let mut secrets = fixture.secrets();
    secrets.push(verifier);
    Ok(secrets)
}

async fn send_refresh() -> TestResult<Vec<String>> {
    let mut fixture = fixture("entra-token-refresh", "wire-refresh-grant")?;
    fixture.form.insert("refresh_token".into(), REFRESH.into());
    let server = MockServer::start().await;
    fixture.mount_exact(&server).await?;
    let client: TokenClient = fixture.client(&server)?;
    client
        .refresh(&store_with(REFRESH).await?, ACCOUNT, &fixture.scope()?)
        .await?;
    Ok(fixture.secrets())
}

async fn send_echoed_error() -> TestResult {
    let mut fixture = Fixture::load("entra-error-invalid-grant")?;
    fixture.path = "/wire-error-grant/oauth2/v2.0/token".into();
    fixture.form.insert("refresh_token".into(), REFRESH.into());
    let encoded: String = url::form_urlencoded::byte_serialize(REFRESH.as_bytes()).collect();
    fixture.body["error_description"] =
        format!("AADSTS70000: wire-visible-error-text: {REFRESH}; encoded {encoded}. Trace ID: wire-error-trace")
            .into();
    let server = MockServer::start().await;
    fixture.mount_exact(&server).await?;
    let client = fixture.client(&server)?;
    assert!(matches!(
        client
            .refresh(&store_with(REFRESH).await?, ACCOUNT, &fixture.scope()?)
            .await,
        Err(teams_auth::token::TokenError::InvalidGrant(_))
    ));
    Ok(())
}

fn assert_exchanges(text: &str) {
    for (label, grant) in [
        ("wire-code-grant", "authorization_code"),
        ("wire-refresh-grant", "refresh_token"),
    ] {
        let request = text
            .lines()
            .find(|line| line.contains("request POST ") && line.contains(label))
            .unwrap_or_else(|| panic!("missing {label} request: {text}"));
        assert!(request.contains("content-type: application/x-www-form-urlencoded"));
        assert!(request.contains(&format!("grant_type={grant}")), "{request}");
        assert!(request.contains("client_id="), "{request}");
        assert!(request.contains("scope=https%3A%2F%2F"), "{request}");
        let response = text
            .lines()
            .find(|line| line.contains("response POST ") && line.contains(label))
            .unwrap_or_else(|| panic!("missing {label} response: {text}"));
        for kept in [
            "-> 200",
            "x-wire-answer: wire-visible-response-header",
            "wire-visible-response-body",
            "\"expires_in\":",
            "\"access_token\":\"[redacted]\"",
            "\"refresh_token\":\"[redacted]\"",
            "\"id_token\":\"[redacted]\"",
            "\"code_verifier\":\"[redacted]\"",
            "set-cookie: [redacted]",
            "authorization: [redacted]",
        ] {
            assert!(response.contains(kept), "missing {kept}: {response}");
        }
    }
    for kept in [
        "code=[redacted]",
        "code_verifier=[redacted]",
        "refresh_token=[redacted]",
    ] {
        assert!(text.contains(kept), "missing {kept}: {text}");
    }
}

#[tokio::test]
async fn both_token_grants_log_full_redacted_exchanges_only_at_debug() -> TestResult {
    let dir = tempfile::tempdir()?;
    let log = teams_log::install(&Config {
        dir: dir.path().into(),
        setting: Level::Debug,
        env: None,
        rotation: Rotation::default(),
    })?;
    let secrets = [send_code().await?, send_refresh().await?].concat();
    send_echoed_error().await?;
    assert!(log.flush_written());
    let debug = fs::read_to_string(log.path())?;
    assert_exchanges(&debug);
    assert!(
        debug.contains("wire-visible-error-text"),
        "noncredential error text stays visible"
    );
    assert!(
        debug.contains("Trace ID: wire-error-trace"),
        "the wire response keeps its complete description"
    );
    for secret in secrets.iter().map(String::as_str).chain([COOKIE, HEADER_AUTH, NESTED]) {
        assert!(!debug.contains(secret), "a credential reached the debug log");
        let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
        assert!(!debug.contains(&encoded), "an encoded credential reached the debug log");
    }
    log.apply_setting(Level::Info);
    send_code().await?;
    send_refresh().await?;
    send_echoed_error().await?;
    assert!(log.flush_written());
    let all = fs::read_to_string(log.path())?;
    let info = &all[debug.len()..];
    for absent in [
        "teams_wire::http",
        "wire-code-grant",
        "wire-refresh-grant",
        "wire-visible-response",
    ] {
        assert!(!info.contains(absent), "wire payload reached info: {info}");
    }
    Ok(())
}
