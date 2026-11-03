//! The in-process window handoff: request privacy, redirect/state validation and account-bound redemption.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use teams_auth::redirect::RedirectError;
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore};
use teams_auth::signin::{SignInAnswer, SignInRequest, WindowError, WindowSignInError, code_from_answer, sign_in_with};
use teams_auth::token::{
    Authorization, EntraConfig, NATIVE_CLIENT_REDIRECT, Scope, TEAMS_NATIVE_CLIENT_ID, TokenClient, TokenError,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const ACCOUNT: &str = "a@lab.example.test";
const CODE: &str = "made-up-authorization-code-0123456789";
const OID: &str = "00000000-0000-4000-8000-00000000000a";
const TENANT: &str = "00000000-0000-4000-8000-0000000000ff";

fn authorization() -> TestResult<Authorization> {
    Ok(Authorization::new(
        &EntraConfig::teams_native(),
        ACCOUNT,
        Scope::spaces(),
    )?)
}

fn redirect_to(state: &str) -> String {
    format!("{NATIVE_CLIENT_REDIRECT}?code={CODE}&state={state}&session_state=made-up")
}

#[test]
fn the_request_and_answer_hide_state_and_code_in_debug() -> TestResult {
    let auth = authorization()?;
    let request = SignInRequest::for_authorization(&auth);
    assert_eq!(request.authorize, auth.url());
    assert_eq!(request.redirect_uri, NATIVE_CLIENT_REDIRECT);
    assert!(!format!("{request:?}").contains(auth.state()));
    let answer = SignInAnswer::Redirect(Secret::new(redirect_to(auth.state())));
    assert_eq!(format!("{answer:?}"), "Redirect(<redacted>)");
    let code = code_from_answer(&auth, answer)?;
    assert_eq!(code.expose(), CODE);
    Ok(())
}

#[test]
fn a_redirect_for_another_request_or_uri_gives_no_code() -> TestResult {
    let auth = authorization()?;
    for (url, wrong_state) in [
        (redirect_to("another-state"), true),
        (
            format!(
                "https://elsewhere.example.test/common/oauth2/nativeclient?code={CODE}&state={}",
                auth.state()
            ),
            false,
        ),
    ] {
        let error = code_from_answer(&auth, SignInAnswer::Redirect(Secret::new(url))).expect_err("wrong redirect");
        assert!(matches!(
            (&error, wrong_state),
            (WindowError::Redirect(RedirectError::StateMismatch), true)
                | (WindowError::Redirect(RedirectError::NotTheRedirect), false)
        ));
        let shown = format!("{error} {error:?}");
        for secret in [CODE, auth.state(), "nativeclient?"] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }
    Ok(())
}

#[test]
fn a_closed_refused_or_failed_window_gives_no_code() -> TestResult {
    let auth = authorization()?;
    assert!(matches!(
        code_from_answer(&auth, SignInAnswer::Closed),
        Err(WindowError::Closed)
    ));
    assert!(
        matches!(code_from_answer(&auth, SignInAnswer::Refused("sso.example.test".into())), Err(WindowError::Refused(host)) if host == "sso.example.test")
    );
    assert!(
        matches!(code_from_answer(&auth, SignInAnswer::Failed("page ended".into())), Err(WindowError::Failed(reason)) if reason == "page ended")
    );
    Ok(())
}

fn id_token(username: &str) -> TestResult<String> {
    let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"RS256","kid":"test"}"#);
    let claims = json!({
        "aud": TEAMS_NATIVE_CLIENT_ID,
        "oid": OID,
        "tid": TENANT,
        "preferred_username": username,
    });
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?);
    Ok(format!("{header}.{payload}.c2lnbmF0dXJlLW5vdC1jaGVja2Vk"))
}

async fn sign_in_as(username: &str) -> TestResult<(Result<(), WindowSignInError>, MemorySecretStore)> {
    let server = MockServer::start().await;
    let body: Value = json!({
        "token_type": "Bearer",
        "scope": "https://api.spaces.skype.com/.default",
        "expires_in": 3600,
        "access_token": "made-up-access-token",
        "refresh_token": "made-up-refresh-token",
        "id_token": id_token(username)?,
    });
    Mock::given(method("POST"))
        .and(path("/organizations/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let config = EntraConfig::teams_native().with_authority(format!("{}/organizations/", server.uri()));
    let client = TokenClient::new(config.clone())?;
    let authorization = Authorization::new(&config, ACCOUNT, Scope::spaces())?;
    let answer = SignInAnswer::Redirect(Secret::new(redirect_to(authorization.state())));
    let store = MemorySecretStore::new();
    let result = sign_in_with(&client, &store, &authorization, |_| answer)
        .await
        .map(|sign_in| assert_eq!(sign_in.account.username, username));
    Ok((result, store))
}

#[tokio::test]
async fn the_code_is_redeemed_for_the_account_the_id_token_names() -> TestResult {
    let (result, store) = sign_in_as(ACCOUNT).await?;
    result?;
    let stored = store.get(&refresh_token_key(ACCOUNT)).await?;
    assert_eq!(stored.as_ref().map(Secret::expose), Some("made-up-refresh-token"));
    Ok(())
}

#[tokio::test]
async fn another_account_signed_in_through_the_window_stores_nothing() -> TestResult {
    let (result, store) = sign_in_as("b@lab.example.test").await?;
    assert!(
        matches!(result, Err(WindowSignInError::Token(TokenError::WrongAccount { .. }))),
        "{result:?}"
    );
    assert!(store.get(&refresh_token_key(ACCOUNT)).await?.is_none());
    assert!(store.get(&refresh_token_key("b@lab.example.test")).await?.is_none());
    Ok(())
}
