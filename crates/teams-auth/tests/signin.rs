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

// --- an own server's password sign-in ---

use teams_auth::refresh::PasswordSignIn;
use wiremock::matchers::body_string_contains;

const OWN_KEY: &str = "https://chat.example.test/a@lab.example.test";

/// A token endpoint that takes the password grant and names `username` in its ID token.
async fn password_endpoint(username: &str) -> TestResult<(MockServer, TokenClient)> {
    let server = MockServer::start().await;
    let body: Value = json!({
        "token_type": "Bearer",
        "scope": "https://api.spaces.skype.com/.default",
        "expires_in": 3600,
        "access_token": "made-up-access-token",
        "refresh_token": "made-up-own-refresh-token",
        "id_token": id_token(username)?,
    });
    Mock::given(method("POST"))
        .and(path("/oauth2/v2.0/token"))
        .and(body_string_contains("grant_type=password"))
        .and(body_string_contains("username=a%40lab.example.test"))
        .and(body_string_contains("password=made-up+password"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let client = TokenClient::new(EntraConfig::teams_native().with_authority(format!("{}/", server.uri())))?;
    Ok((server, client))
}

fn own_sign_in(expected: Option<(&'static str, &'static str)>) -> PasswordSignIn<'static> {
    PasswordSignIn {
        account: OWN_KEY,
        username: ACCOUNT,
        expected,
    }
}

#[tokio::test]
async fn a_password_sign_in_stores_the_refresh_token_under_the_accounts_key() -> TestResult {
    let (_server, client) = password_endpoint(ACCOUNT).await?;
    let store = MemorySecretStore::new();
    let password = Secret::new("made-up password");
    let signed = client
        .password_sign_in(&store, own_sign_in(Some((TENANT, OID))), &password, &Scope::spaces())
        .await?;
    assert_eq!(
        (signed.account.object_id.as_str(), signed.account.tenant.as_str()),
        (OID, TENANT)
    );
    let stored = store.get(&refresh_token_key(OWN_KEY)).await?;
    assert_eq!(stored.as_ref().map(Secret::expose), Some("made-up-own-refresh-token"));
    // never under the bare user name, where a Microsoft 365 account of that UPN keeps its own
    assert!(store.get(&refresh_token_key(ACCOUNT)).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn a_password_sign_in_naming_another_account_stores_nothing() -> TestResult {
    let store = MemorySecretStore::new();
    let password = Secret::new("made-up password");
    // the server's ID token names somebody else
    let (_server, client) = password_endpoint("b@lab.example.test").await?;
    let other = client
        .password_sign_in(&store, own_sign_in(None), &password, &Scope::spaces())
        .await;
    assert!(
        matches!(
            other,
            Err(TokenError::WrongAccount {
                claim: "preferred_username",
                ..
            })
        ),
        "{other:?}"
    );
    // an earlier sign-in named another object id
    let (_server, client) = password_endpoint(ACCOUNT).await?;
    let renamed = client
        .password_sign_in(
            &store,
            own_sign_in(Some((TENANT, "00000000-0000-4000-8000-00000000000b"))),
            &password,
            &Scope::spaces(),
        )
        .await;
    assert!(
        matches!(renamed, Err(TokenError::WrongAccount { claim: "oid", .. })),
        "{renamed:?}"
    );
    // a wrong password: the server's refusal, the password never in the error
    let refused = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "The user name or password is incorrect.",
        })))
        .mount(&refused)
        .await;
    let client = TokenClient::new(EntraConfig::teams_native().with_authority(format!("{}/", refused.uri())))?;
    let wrong = client
        .password_sign_in(&store, own_sign_in(None), &password, &Scope::spaces())
        .await
        .expect_err("a wrong password");
    assert!(matches!(wrong, TokenError::InvalidGrant(_)));
    assert!(!format!("{wrong} {wrong:?}").contains("made-up password"));
    assert!(store.get(&refresh_token_key(OWN_KEY)).await?.is_none());
    Ok(())
}
