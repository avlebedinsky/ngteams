//! The authorize URL, the scope rule, and the Entra token grants (code → tokens, refresh → tokens) against `wiremock`,
//! with the refresh token kept in a `SecretStore`.
//!
//! Every exchange comes from a fixture under `tests/fixtures/auth/` (one `{request, response}` per file, see
//! `docs/protocol/auth.md`). The tests take their inputs from the fixture's request (client ID, redirect, code, refresh
//! token, scope, endpoint path) and their expectations from its response, so a fixture rewritten synthetic-from-capture
//! (written by hand from a local reference copy with the same file name and shape, made-up values; captures never
//! enter the repo) drops in for a made-up one. The only value the tests supply themselves is the PKCE verifier, since
//! a fixture's verifier is a placeholder, and the code exchange's ID token: a fixture's is a placeholder too (the
//! fixture guard refuses committed JWTs), so the code tests build an unsigned one at run time with the claims each case
//! needs.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use teams_auth::id_token::IdTokenError;
use teams_auth::pkce::Pkce;
use teams_auth::refresh::{SignIn, refresh_token_key};
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore};
use teams_auth::token::{
    Authorization, EntraConfig, NATIVE_CLIENT_REDIRECT, SPACES_RESOURCE, Scope, TEAMS_NATIVE_CLIENT_ID, TokenError,
    Tokens,
};
use url::Url;
use wiremock::MockServer;

#[path = "support/token_fixture.rs"]
mod fixture;
use fixture::{ACCOUNT, Fixture, OCTETS, OID, TENANT, TestResult, authorization, claims_of, store_with};

impl Fixture {
    fn body(&self, key: &str) -> TestResult<&str> {
        Ok(self.body[key]
            .as_str()
            .ok_or_else(|| format!("response.body has no {key}"))?)
    }
}

fn assert_hidden(what: &impl Debug, secrets: &[String]) {
    let debug = format!("{what:?}");
    for secret in secrets {
        assert!(!debug.contains(secret.as_str()), "{secret} shows in {debug}");
    }
}

fn assert_error_hidden(error: &TokenError, secrets: &[String]) {
    assert_hidden(error, secrets);
    let display = error.to_string();
    for secret in secrets {
        assert!(!display.contains(secret.as_str()), "{secret} shows in {display}");
    }
}

/// The tokens are the fixture's answer; the expiry is `expires_in` after the request.
fn assert_tokens(tokens: &Tokens, fixture: &Fixture, sent: SystemTime) -> TestResult {
    let body = &fixture.body;
    assert_eq!(tokens.access_token.expose(), fixture.body("access_token")?);
    assert_eq!(
        tokens.id_token.as_ref().map(Secret::expose),
        body["id_token"].as_str(),
        "id_token"
    );
    assert_eq!(tokens.token_type, fixture.body("token_type")?);
    let scopes: Vec<&str> = fixture.body("scope")?.split_whitespace().collect();
    assert_eq!(tokens.scopes, scopes);
    let lifetime = Duration::from_secs(body["expires_in"].as_u64().ok_or("expires_in")?);
    let earliest = sent + lifetime;
    let latest = SystemTime::now() + lifetime;
    assert!(tokens.expires_at >= earliest && tokens.expires_at <= latest);
    assert_hidden(tokens, &fixture.secrets());
    Ok(())
}

async fn stored(store: &MemorySecretStore, account: &str) -> TestResult<Option<String>> {
    let secret = store.get(&refresh_token_key(account)).await?;
    Ok(secret.map(|secret| secret.expose().to_owned()))
}

// ---- authorize URL and scope rule ----

#[test]
fn authorize_url_carries_the_pkce_request() -> TestResult {
    let config = EntraConfig::teams_native();
    let pkce = Pkce::from_bytes(&OCTETS);
    let challenge = pkce.challenge().to_owned();
    let verifier = pkce.verifier().expose().to_owned();
    let authorization = Authorization::with_pkce(&config, ACCOUNT, Scope::spaces(), pkce, "state-123".into());

    let url = Url::parse(authorization.url())?;
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("login.microsoftonline.com"));
    assert_eq!(url.path(), "/organizations/oauth2/v2.0/authorize");
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let params: BTreeMap<&str, &str> = pairs
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    assert_eq!(params.len(), pairs.len(), "no repeated parameter");
    let expected = BTreeMap::from([
        ("client_id", TEAMS_NATIVE_CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", NATIVE_CLIENT_REDIRECT),
        (
            "scope",
            "https://api.spaces.skype.com/.default openid profile offline_access",
        ),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", "state-123"),
        ("login_hint", ACCOUNT),
    ]);
    assert_eq!(params, expected);
    assert!(!authorization.url().contains(&verifier), "the verifier stays local");
    assert_eq!(authorization.account(), ACCOUNT);
    assert_eq!(authorization.state(), "state-123");
    assert_eq!(authorization.redirect_uri(), NATIVE_CLIENT_REDIRECT);
    Ok(())
}

#[test]
fn authorize_url_follows_the_configured_authority() -> TestResult {
    let config = EntraConfig::teams_native()
        .with_authority("http://127.0.0.1:9/tenant-x")
        .with_redirect_uri("msauth.com.microsoft.teams2://auth");
    let authorization = Authorization::new(&config, ACCOUNT, Scope::spaces())?;
    let url = Url::parse(authorization.url())?;
    assert_eq!(url.path(), "/tenant-x/oauth2/v2.0/authorize");
    let params: BTreeMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(params["redirect_uri"], "msauth.com.microsoft.teams2://auth");
    assert_eq!(params["code_challenge"], authorization.pkce().challenge());
    assert_eq!(params["state"], authorization.state());
    Ok(())
}

#[test]
fn fresh_authorizations_differ() -> TestResult {
    let config = EntraConfig::teams_native();
    let first = Authorization::new(&config, ACCOUNT, Scope::spaces())?;
    let second = Authorization::new(&config, ACCOUNT, Scope::spaces())?;
    assert_ne!(first.state(), second.state());
    assert_ne!(first.pkce().challenge(), second.pkce().challenge());
    assert_hidden(&first, &[first.pkce().verifier().expose().to_owned()]);
    Ok(())
}

#[test]
fn scope_rule_appends_default_unless_the_resource_names_a_scope() {
    let rest = "openid profile offline_access";
    for (resource, scope) in [
        (SPACES_RESOURCE, "https://api.spaces.skype.com/.default"),
        ("https://graph.microsoft.com", "https://graph.microsoft.com/.default"),
        ("https://outlook.office.com/", "https://outlook.office.com//.default"),
        (
            "https://outlook.office.com/search",
            "https://outlook.office.com/search/.default",
        ),
        (
            "6bc3b958-689b-49f5-9006-36d165f30e00",
            "6bc3b958-689b-49f5-9006-36d165f30e00/.default",
        ),
        (
            "https://graph.microsoft.com/.default",
            "https://graph.microsoft.com/.default",
        ),
        (
            "https://substrate.office.com/M365.Access",
            "https://substrate.office.com/M365.Access",
        ),
        (
            "https://graph.microsoft.com/Files.ReadWrite",
            "https://graph.microsoft.com/Files.ReadWrite",
        ),
    ] {
        assert_eq!(
            Scope::for_resource(resource).as_str(),
            format!("{scope} {rest}"),
            "{resource}"
        );
    }
    assert_eq!(Scope::spaces(), Scope::for_resource(SPACES_RESOURCE));
}

#[test]
fn defaults_match_the_fixtures() -> TestResult {
    let code = Fixture::load("entra-token-code")?;
    assert_eq!(code.form("client_id")?, TEAMS_NATIVE_CLIENT_ID);
    assert_eq!(code.form("redirect_uri")?, NATIVE_CLIENT_REDIRECT);
    assert_eq!(code.scope()?, Scope::spaces());
    let refresh = Fixture::load("entra-token-refresh")?;
    assert_eq!(refresh.scope()?, Scope::for_resource("https://graph.microsoft.com"));
    let endpoint = EntraConfig::teams_native().token_endpoint();
    assert_eq!(Url::parse(&endpoint)?.path(), code.path);
    Ok(())
}

// ---- code → tokens ----

/// Runs a code exchange for `ACCOUNT` (with `expect` applied to the authorization) against `fixture`, with
/// `already` stored as the account's refresh token beforehand when given.
async fn exchange(
    fixture: &Fixture,
    expect: impl FnOnce(Authorization) -> Authorization,
    already: Option<&str>,
) -> TestResult<(Result<SignIn, TokenError>, MemorySecretStore)> {
    let server = MockServer::start().await;
    let client = fixture.client(&server)?;
    let authorization = expect(authorization(client.config(), fixture)?);
    fixture.mount_code(&server, authorization.pkce()).await?;
    let store = MemorySecretStore::new();
    if let Some(refresh_token) = already {
        store
            .put(&refresh_token_key(ACCOUNT), &Secret::new(refresh_token))
            .await?;
    }
    let result = client
        .exchange_code(&store, &authorization, fixture.form("code")?)
        .await;
    Ok((result, store))
}

/// A code exchange that must fail without touching the store: an earlier refresh token stays, nothing else is added.
async fn refused_exchange(
    fixture: &Fixture,
    expect: impl FnOnce(Authorization) -> Authorization,
) -> TestResult<TokenError> {
    let earlier = "earlier-refresh-token-of-a";
    let (result, store) = exchange(fixture, expect, Some(earlier)).await?;
    let error = result.err().ok_or("the exchange should be refused")?;
    assert_error_hidden(&error, &fixture.secrets());
    assert_eq!(stored(&store, ACCOUNT).await?.as_deref(), Some(earlier));
    assert_eq!(stored(&store, "b@lab.example.test").await?, None);
    Ok(error)
}

#[tokio::test]
async fn code_is_exchanged_and_the_refresh_token_stored() -> TestResult {
    let fixture = Fixture::load("entra-token-code")?.with_id_token(&claims_of(ACCOUNT))?;

    let sent = SystemTime::now();
    let (result, store) = exchange(&fixture, |authorization| authorization, None).await?;
    let sign_in = result.expect("code redeemed");

    assert_tokens(&sign_in.tokens, &fixture, sent)?;
    assert_eq!(sign_in.account.username, ACCOUNT);
    assert_eq!(sign_in.account.object_id, OID);
    assert_eq!(sign_in.account.tenant, TENANT);
    assert_hidden(&sign_in, &fixture.secrets());
    assert_eq!(
        stored(&store, ACCOUNT).await?.as_deref(),
        Some(fixture.body("refresh_token")?)
    );
    Ok(())
}

#[tokio::test]
async fn the_account_check_ignores_case_and_takes_the_expected_ids() -> TestResult {
    let mut claims = claims_of(&ACCOUNT.to_ascii_uppercase());
    claims["upn"] = Value::String(ACCOUNT.into());
    claims["aud"] = json!(["another-client", TEAMS_NATIVE_CLIENT_ID]);
    let fixture = Fixture::load("entra-token-code")?.with_id_token(&claims)?;

    let (result, store) = exchange(
        &fixture,
        |authorization| {
            authorization
                .expect_tenant(TENANT.to_ascii_uppercase())
                .expect_object_id(OID)
        },
        Some("earlier-refresh-token-of-a"),
    )
    .await?;

    let sign_in = result.expect("the same account");
    assert_eq!(sign_in.account.username, ACCOUNT.to_ascii_uppercase());
    assert_eq!(
        stored(&store, ACCOUNT).await?.as_deref(),
        Some(fixture.body("refresh_token")?),
        "a new sign-in replaces the earlier refresh token"
    );
    Ok(())
}

#[tokio::test]
async fn another_signed_in_account_is_refused_and_stores_nothing() -> TestResult {
    let other = "b@lab.example.test";
    let fixture = Fixture::load("entra-token-code")?.with_id_token(&claims_of(other))?;

    let error = refused_exchange(&fixture, |authorization| authorization).await?;

    let TokenError::WrongAccount { claim, expected, found } = &error else {
        panic!("expected WrongAccount, got {error:?}");
    };
    assert_eq!(
        (*claim, expected.as_str(), found.as_str()),
        ("preferred_username", ACCOUNT, other)
    );
    Ok(())
}

#[tokio::test]
async fn every_name_claim_must_match() -> TestResult {
    let mut claims = claims_of(ACCOUNT);
    claims["upn"] = Value::String("b@lab.example.test".into());
    let fixture = Fixture::load("entra-token-code")?.with_id_token(&claims)?;

    let error = refused_exchange(&fixture, |authorization| authorization).await?;

    assert!(
        matches!(&error, TokenError::WrongAccount { claim: "upn", .. }),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn an_id_token_without_a_name_claim_is_refused() -> TestResult {
    let mut claims = claims_of(ACCOUNT);
    claims.as_object_mut().ok_or("claims")?.remove("preferred_username");
    let fixture = Fixture::load("entra-token-code")?.with_id_token(&claims)?;

    let error = refused_exchange(&fixture, |authorization| authorization).await?;

    assert!(
        matches!(
            error,
            TokenError::IdToken(IdTokenError::MissingClaim("preferred_username"))
        ),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn another_tenant_or_object_id_is_refused() -> TestResult {
    let fixture = Fixture::load("entra-token-code")?.with_id_token(&claims_of(ACCOUNT))?;
    let other = "00000000-0000-4000-8000-0000000000ee";

    let tenant = refused_exchange(&fixture, |authorization| authorization.expect_tenant(other)).await?;
    let object = refused_exchange(&fixture, |authorization| authorization.expect_object_id(other)).await?;

    assert!(
        matches!(&tenant, TokenError::WrongAccount { claim: "tid", found, .. } if found == TENANT),
        "{tenant:?}"
    );
    assert!(
        matches!(&object, TokenError::WrongAccount { claim: "oid", found, .. } if found == OID),
        "{object:?}"
    );
    Ok(())
}

#[tokio::test]
async fn an_id_token_for_another_client_is_refused() -> TestResult {
    let mut claims = claims_of(ACCOUNT);
    claims["aud"] = Value::String("another-client".into());
    let fixture = Fixture::load("entra-token-code")?.with_id_token(&claims)?;

    let error = refused_exchange(&fixture, |authorization| authorization).await?;

    assert!(
        matches!(&error, TokenError::WrongAccount { claim: "aud", found, .. } if found == "another-client"),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_code_answer_without_an_id_token_is_refused() -> TestResult {
    let mut fixture = Fixture::load("entra-token-code")?;
    fixture.body.as_object_mut().ok_or("body")?.remove("id_token");

    let error = refused_exchange(&fixture, |authorization| authorization).await?;

    assert!(matches!(error, TokenError::IdToken(IdTokenError::Missing)), "{error:?}");
    Ok(())
}

#[tokio::test]
async fn an_id_token_that_is_not_a_jwt_is_refused() -> TestResult {
    // The fixture's own ID token is a placeholder, not a JWT.
    let fixture = Fixture::load("entra-token-code")?;

    let error = refused_exchange(&fixture, |authorization| authorization).await?;

    assert!(
        matches!(error, TokenError::IdToken(IdTokenError::Malformed)),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn an_id_token_without_an_object_id_is_refused() -> TestResult {
    let mut claims = claims_of(ACCOUNT);
    claims.as_object_mut().ok_or("claims")?.remove("oid");
    let fixture = Fixture::load("entra-token-code")?.with_id_token(&claims)?;

    let error = refused_exchange(&fixture, |authorization| authorization).await?;

    assert!(
        matches!(error, TokenError::IdToken(IdTokenError::MissingClaim("oid"))),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_code_answer_without_a_refresh_token_is_refused() -> TestResult {
    let mut fixture = Fixture::load("entra-token-code")?.with_id_token(&claims_of(ACCOUNT))?;
    fixture.body.as_object_mut().expect("an object").remove("refresh_token");
    let server = MockServer::start().await;
    let client = fixture.client(&server)?;
    let authorization = authorization(client.config(), &fixture)?;
    fixture.mount_code(&server, authorization.pkce()).await?;
    let store = MemorySecretStore::new();

    let error = client
        .exchange_code(&store, &authorization, fixture.form("code")?)
        .await
        .expect_err("no refresh token");

    assert!(matches!(error, TokenError::MissingField("refresh_token")), "{error:?}");
    assert_error_hidden(&error, &fixture.secrets());
    assert_eq!(stored(&store, ACCOUNT).await?, None);
    Ok(())
}

#[tokio::test]
async fn an_unregistered_redirect_is_reported_as_such() -> TestResult {
    let fixture = Fixture::load("entra-error-aadsts50011")?;
    let server = MockServer::start().await;
    let client = fixture.client(&server)?;
    let authorization = authorization(client.config(), &fixture)?;
    fixture.mount_code(&server, authorization.pkce()).await?;
    let store = MemorySecretStore::new();

    let error = client
        .exchange_code(&store, &authorization, fixture.form("code")?)
        .await
        .expect_err("redirect not registered");

    let TokenError::RedirectMismatch(entra) = &error else {
        panic!("expected RedirectMismatch, got {error:?}");
    };
    assert_eq!(entra.codes, [50011]);
    assert_eq!(entra.error, fixture.body("error")?);
    assert!(error.to_string().contains("AADSTS50011"), "{error}");
    assert_error_hidden(&error, &fixture.secrets());
    assert_eq!(stored(&store, ACCOUNT).await?, None);
    Ok(())
}

// ---- refresh → tokens ----

#[tokio::test]
async fn refresh_for_another_audience_stores_the_rotated_refresh_token() -> TestResult {
    let fixture = Fixture::load("entra-token-refresh")?;
    let server = MockServer::start().await;
    fixture.mount_exact(&server).await?;
    let store = store_with(fixture.form("refresh_token")?).await?;

    let sent = SystemTime::now();
    let tokens = fixture
        .client(&server)?
        .refresh(&store, ACCOUNT, &fixture.scope()?)
        .await
        .expect("refreshed");

    assert_tokens(&tokens, &fixture, sent)?;
    assert_ne!(
        fixture.body("refresh_token")?,
        fixture.form("refresh_token")?,
        "the fixture rotates"
    );
    assert_eq!(
        stored(&store, ACCOUNT).await?.as_deref(),
        Some(fixture.body("refresh_token")?)
    );
    Ok(())
}

#[tokio::test]
async fn refresh_without_a_new_refresh_token_keeps_the_old_one() -> TestResult {
    let mut fixture = Fixture::load("entra-token-refresh")?;
    fixture.body.as_object_mut().expect("an object").remove("refresh_token");
    let server = MockServer::start().await;
    fixture.mount_exact(&server).await?;
    let store = store_with(fixture.form("refresh_token")?).await?;

    fixture
        .client(&server)?
        .refresh(&store, ACCOUNT, &fixture.scope()?)
        .await
        .expect("refreshed");

    assert_eq!(
        stored(&store, ACCOUNT).await?.as_deref(),
        Some(fixture.form("refresh_token")?)
    );
    Ok(())
}

#[tokio::test]
async fn refresh_without_a_stored_refresh_token_sends_nothing() -> TestResult {
    let fixture = Fixture::load("entra-token-refresh")?;
    let server = MockServer::start().await;
    fixture.mount(&server, fixture.form.clone(), 0).await?;
    let store = MemorySecretStore::new();

    let error = fixture
        .client(&server)?
        .refresh(&store, ACCOUNT, &fixture.scope()?)
        .await
        .expect_err("not signed in");

    assert!(
        matches!(&error, TokenError::NotSignedIn { account } if account == ACCOUNT),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn refresh_tokens_are_kept_per_account() -> TestResult {
    let fixture = Fixture::load("entra-token-refresh")?;
    let server = MockServer::start().await;
    fixture.mount_exact(&server).await?;
    let store = store_with(fixture.form("refresh_token")?).await?;
    let other = "b@lab.example.test";
    store
        .put(&refresh_token_key(other), &Secret::new("refresh-token-of-b"))
        .await
        .expect("memory store");

    fixture
        .client(&server)?
        .refresh(&store, ACCOUNT, &fixture.scope()?)
        .await
        .expect("refreshed");

    assert_eq!(stored(&store, other).await?.as_deref(), Some("refresh-token-of-b"));
    Ok(())
}

// ---- error paths ----

/// Runs a refresh grant against an error fixture, with the fixture's refresh token stored, and checks that the error
/// leaks nothing and the stored refresh token is left alone.
async fn refresh_error(name: &str) -> TestResult<(TokenError, Fixture)> {
    let fixture = Fixture::load(name)?;
    let server = MockServer::start().await;
    fixture.mount_exact(&server).await?;
    let store = store_with(fixture.form("refresh_token")?).await?;

    let error = fixture
        .client(&server)?
        .refresh(&store, ACCOUNT, &fixture.scope()?)
        .await
        .err()
        .ok_or("the fixture is an error")?;

    assert_error_hidden(&error, &fixture.secrets());
    assert_eq!(
        stored(&store, ACCOUNT).await?.as_deref(),
        Some(fixture.form("refresh_token")?),
        "an error leaves the stored refresh token alone"
    );
    Ok((error, fixture))
}

#[tokio::test]
async fn an_expired_refresh_token_is_invalid_grant() -> TestResult {
    let (error, fixture) = refresh_error("entra-error-invalid-grant").await?;
    let TokenError::InvalidGrant(entra) = &error else {
        panic!("expected InvalidGrant, got {error:?}");
    };
    assert_eq!(entra.error, "invalid_grant");
    assert_eq!(entra.codes, [700_082]);
    assert_eq!(entra.correlation_id.as_deref(), fixture.body["correlation_id"].as_str());
    let display = error.to_string();
    assert!(display.contains("AADSTS700082"), "{display}");
    assert!(
        !display.contains("Trace ID"),
        "the description is cut before the trace: {display}"
    );
    Ok(())
}

#[tokio::test]
async fn a_conditional_access_block_is_its_own_error() -> TestResult {
    let (error, fixture) = refresh_error("entra-error-aadsts53003").await?;
    let TokenError::ConditionalAccess(entra) = &error else {
        panic!("expected ConditionalAccess, got {error:?}");
    };
    assert_eq!(entra.codes, [53003]);
    assert_eq!(entra.error, fixture.body("error")?);
    assert_eq!(entra.suberror.as_deref(), fixture.body["suberror"].as_str());
    assert!(error.to_string().contains("AADSTS53003"), "{error}");
    Ok(())
}

#[tokio::test]
async fn throttling_reports_retry_after_in_seconds() -> TestResult {
    let (error, _) = refresh_error("entra-error-429").await?;
    let TokenError::Throttled {
        retry_after,
        error: entra,
    } = &error
    else {
        panic!("expected Throttled, got {error:?}");
    };
    assert_eq!(*retry_after, Some(Duration::from_secs(30)));
    assert_eq!(entra.as_ref().map(|entra| entra.codes.clone()), Some(vec![50196]));
    Ok(())
}

#[tokio::test]
async fn throttling_reads_retry_after_as_an_http_date() -> TestResult {
    let mut fixture = Fixture::load("entra-error-429")?;
    let when = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(120));
    for (name, value) in &mut fixture.headers {
        if name.eq_ignore_ascii_case("retry-after") {
            value.clone_from(&when);
        }
    }
    let server = MockServer::start().await;
    fixture.mount_exact(&server).await?;
    let store = store_with(fixture.form("refresh_token")?).await?;

    let error = fixture
        .client(&server)?
        .refresh(&store, ACCOUNT, &fixture.scope()?)
        .await
        .expect_err("throttled");

    let TokenError::Throttled {
        retry_after: Some(wait),
        ..
    } = error
    else {
        panic!("expected Throttled with a wait, got {error:?}");
    };
    assert!(
        wait > Duration::from_secs(100) && wait <= Duration::from_secs(120),
        "{wait:?}"
    );
    Ok(())
}

#[tokio::test]
async fn throttling_without_retry_after_has_no_wait() -> TestResult {
    let mut fixture = Fixture::load("entra-error-429")?;
    fixture
        .headers
        .retain(|(name, _)| !name.eq_ignore_ascii_case("retry-after"));
    fixture.body = Value::String("throttled".into());
    let server = MockServer::start().await;
    fixture.mount_exact(&server).await?;
    let store = store_with(fixture.form("refresh_token")?).await?;

    let error = fixture
        .client(&server)?
        .refresh(&store, ACCOUNT, &fixture.scope()?)
        .await
        .expect_err("throttled");

    assert!(
        matches!(
            error,
            TokenError::Throttled {
                retry_after: None,
                error: None
            }
        ),
        "{error:?}"
    );
    Ok(())
}

/// An error fixture rewritten into another Entra answer.
async fn refresh_answer(status: u16, body: Value) -> TestResult<(TokenError, Fixture)> {
    let mut fixture = Fixture::load("entra-error-invalid-grant")?;
    fixture.status = status;
    fixture.body = body;
    let server = MockServer::start().await;
    fixture.mount_exact(&server).await?;
    let store = store_with(fixture.form("refresh_token")?).await?;
    let error = fixture
        .client(&server)?
        .refresh(&store, ACCOUNT, &fixture.scope()?)
        .await
        .err()
        .ok_or("an error answer")?;
    assert_error_hidden(&error, &fixture.secrets());
    Ok((error, fixture))
}

#[tokio::test]
async fn interaction_required_is_its_own_error() -> TestResult {
    let body = serde_json::json!({
        "error": "interaction_required",
        "error_description": "AADSTS50076: (synthetic) multi-factor authentication is required. Trace ID: x",
        "error_codes": [50076],
    });
    let (error, _) = refresh_answer(400, body).await?;
    assert!(
        matches!(&error, TokenError::InteractionRequired(entra) if entra.codes == [50076]),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn other_conditional_access_codes_are_recognised() -> TestResult {
    for code in [53000_u32, 53001, 530_032] {
        let body = serde_json::json!({
            "error": "interaction_required",
            "error_description": format!("AADSTS{code}: (synthetic) blocked by policy."),
        });
        let (error, _) = refresh_answer(400, body).await?;
        assert!(
            matches!(&error, TokenError::ConditionalAccess(entra) if entra.codes == [code]),
            "{code}: {error:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_echoed_secret_is_scrubbed_from_the_description() -> TestResult {
    let fixture = Fixture::load("entra-error-invalid-grant")?;
    let echoed = format!(
        "AADSTS70000: (synthetic) the grant {} is bad.",
        fixture.form("refresh_token")?
    );
    let body = serde_json::json!({"error": "invalid_grant", "error_description": echoed, "error_codes": [70000]});
    let (error, _) = refresh_answer(400, body).await?;
    let TokenError::InvalidGrant(entra) = &error else {
        panic!("expected InvalidGrant, got {error:?}");
    };
    assert!(entra.description.contains("<redacted>"), "{}", entra.description);
    Ok(())
}

#[tokio::test]
async fn an_unknown_entra_error_keeps_its_status() -> TestResult {
    let body = serde_json::json!({"error": "unauthorized_client", "error_codes": [700_016]});
    let (error, _) = refresh_answer(400, body).await?;
    assert!(
        matches!(&error, TokenError::Entra { status: 400, error: entra } if entra.error == "unauthorized_client"),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_non_json_answer_is_unexpected() -> TestResult {
    let (error, _) = refresh_answer(500, Value::String("<html>oops</html>".into())).await?;
    assert!(
        matches!(error, TokenError::UnexpectedResponse { status: 500 }),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_success_without_an_access_token_is_refused() -> TestResult {
    let mut body = Fixture::load("entra-token-refresh")?.body;
    body.as_object_mut().expect("an object").remove("access_token");
    let (error, _) = refresh_answer(200, body).await?;
    assert!(matches!(error, TokenError::MissingField("access_token")), "{error:?}");
    Ok(())
}

#[tokio::test]
async fn a_success_with_a_mistyped_field_does_not_echo_it() -> TestResult {
    let mut body = Fixture::load("entra-token-refresh")?.body;
    body["expires_in"] = Value::String("planted-secret-looking-value".into());
    let (error, _) = refresh_answer(200, body).await?;
    assert!(matches!(error, TokenError::MissingField("expires_in")), "{error:?}");
    assert!(!format!("{error} {error:?}").contains("planted-secret-looking-value"));
    Ok(())
}

#[tokio::test]
async fn expires_in_may_come_as_a_string() -> TestResult {
    let mut body = Fixture::load("entra-token-refresh")?.body;
    body["expires_in"] = Value::String("3599".into());
    let fixture = {
        let mut fixture = Fixture::load("entra-token-refresh")?;
        fixture.body = body;
        fixture
    };
    let server = MockServer::start().await;
    fixture.mount_exact(&server).await?;
    let store = store_with(fixture.form("refresh_token")?).await?;
    let sent = SystemTime::now();
    let tokens = fixture
        .client(&server)?
        .refresh(&store, ACCOUNT, &fixture.scope()?)
        .await
        .expect("refreshed");
    assert!(tokens.expires_at >= sent + Duration::from_secs(3599));
    Ok(())
}
