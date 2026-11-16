//! A bot's side of authentication, at the paths the bot SDKs and MSAL compute:
//!
//! - the client-credentials grant, as MSAL sends it, under a tenant's authority (`POST /<tenant>/oauth2/v2.0/token`,
//!   the tenant being the server's id or `botframework.com`) and under `/adfs` (`POST /adfs/oauth2/token`), the one
//!   authority msal-python takes without asking Microsoft; with the `OpenID` metadata MSAL reads first beside each;
//! - the keys a bot checks an activity's token with: `/bot/v1/.well-known/openidconfiguration` and the JWKS it
//!   names, the same JWKS at `/<tenant>/discovery/v2.0/keys` (the Agents SDK computes that path) and at
//!   `/adfs/discovery/keys`.
//!
//! A refused grant takes Entra's JSON shape and code, so an SDK logs what Entra would have said. Failed secrets are
//! counted per bot by the sign-in [`crate::throttle::Throttle`], across nodes when the server has Redis; a wrong secret
//! while the bot is held off gets 429 with `Retry-After`, and the right one is never held off (a bot's id is public).
//! A wrong secret and an unknown bot are told the same.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, RETRY_AFTER};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Serialize;
use serde_json::{Value, json};

use crate::app::App;
use crate::auth::keys::KeyError;
use crate::auth::tokens::seconds_until;
use crate::db::{DbError, now, seconds};

/// The tenant name Bot Framework bots ask for by default.
const BOT_FRAMEWORK_TENANT: &str = "botframework.com";
/// The client authentication methods the grant takes.
const AUTH_METHODS: [&str; 2] = ["client_secret_post", "client_secret_basic"];

pub(super) fn router() -> Router<Arc<App>> {
    let tokens = Router::new()
        .route("/{tenant}/oauth2/v2.0/token", post(tenant_token))
        .route("/adfs/oauth2/token", post(adfs_token))
        .layer(DefaultBodyLimit::max(crate::app::AUTH_BODY_LIMIT));
    Router::new()
        .merge(tokens)
        .route("/{tenant}/v2.0/.well-known/openid-configuration", get(tenant_metadata))
        .route("/{tenant}/discovery/v2.0/keys", get(tenant_keys))
        .route("/adfs/.well-known/openid-configuration", get(adfs_metadata))
        .route("/adfs/discovery/keys", get(keys))
        .route("/bot/v1/.well-known/openidconfiguration", get(bot_metadata))
        .route("/bot/v1/.well-known/keys", get(keys))
}

/// An Entra-shaped refusal.
#[derive(Debug)]
struct Refusal {
    status: StatusCode,
    error: &'static str,
    code: u32,
    description: &'static str,
    retry_after: Option<Duration>,
}

impl Refusal {
    const fn new(status: StatusCode, error: &'static str, code: u32, description: &'static str) -> Self {
        Self {
            status,
            error,
            code,
            description,
            retry_after: None,
        }
    }

    const fn missing() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            900_144,
            "The request body must contain the parameters of its grant.",
        )
    }

    const fn tenant() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            90_002,
            "The tenant is not this server's.",
        )
    }

    const fn grant_type() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            70_003,
            "Only the client credentials grant is served here.",
        )
    }

    const fn scope() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_scope",
            70_011,
            "The scope must be the bots' resource with /.default.",
        )
    }

    const fn no_secret() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            7_000_216,
            "client_secret is required.",
        )
    }

    const fn credentials() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            7_000_215,
            "Invalid client secret provided.",
        )
    }

    const fn disabled() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "unauthorized_client",
            7_000_112,
            "The application is disabled.",
        )
    }

    const fn unsigned() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "unauthorized_client",
            7_000_112,
            "The application is registered without authentication and takes no token.",
        )
    }

    fn held(wait: Duration) -> Self {
        Self {
            retry_after: Some(wait),
            ..Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "temporarily_unavailable",
                50_196,
                "Too many failed requests for this client; try again later.",
            )
        }
    }

    const fn server() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            90_033,
            "The server can't issue a token now.",
        )
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let body = json!({
            "error": self.error,
            "error_description": format!("AADSTS{}: {}", self.code, self.description),
            "error_codes": [self.code],
            "timestamp": now(),
        });
        let mut response = (self.status, [(CACHE_CONTROL, "no-store")], axum::Json(body)).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert("www-authenticate", axum::http::HeaderValue::from_static("Basic"));
        }
        if let Some(wait) = self.retry_after
            && let Ok(value) = wait.as_secs().max(1).to_string().parse()
        {
            response.headers_mut().insert(RETRY_AFTER, value);
        }
        response
    }
}

impl From<DbError> for Refusal {
    fn from(error: DbError) -> Self {
        tracing::error!(%error, "bot token endpoint failed");
        Self::server()
    }
}

impl From<KeyError> for Refusal {
    fn from(error: KeyError) -> Self {
        tracing::error!(%error, "bot token endpoint failed");
        Self::server()
    }
}

/// A bot's own token: the server's `EdDSA` JWT for the bots' resource, naming the bot as Entra names an app.
#[derive(Serialize)]
struct BotClaims<'a> {
    aud: &'a str,
    iss: &'a str,
    iat: i64,
    nbf: i64,
    exp: i64,
    sub: &'a str,
    appid: &'a str,
    azp: &'a str,
    tid: &'a str,
    idtyp: &'static str,
    ver: &'static str,
    uti: String,
}

/// Whether `tenant` names this server: its id, or the Bot Framework's tenant name.
fn ours(app: &App, tenant: &str) -> bool {
    tenant.eq_ignore_ascii_case(app.issuer.tenant()) || tenant.eq_ignore_ascii_case(BOT_FRAMEWORK_TENANT)
}

/// The issuer the server's own tokens carry (`crate::auth::tokens::Issuer`).
fn token_issuer(app: &App) -> String {
    format!("{}/{}/v2.0", app.config.origin(), app.issuer.tenant())
}

fn json_answer(body: &Value) -> Response {
    ([(CACHE_CONTROL, "max-age=86400, private")], axum::Json(body)).into_response()
}

/// `GET /<tenant>/v2.0/.well-known/openid-configuration`: where MSAL finds the token endpoint of the authority
/// `<origin>/<tenant>`.
async fn tenant_metadata(State(app): State<Arc<App>>, Path(tenant): Path<String>) -> Result<Response, Refusal> {
    if !ours(&app, &tenant) {
        return Err(Refusal::tenant());
    }
    let authority = format!("{}/{tenant}", app.config.origin());
    Ok(json_answer(&json!({
        "token_endpoint": format!("{authority}/oauth2/v2.0/token"),
        "token_endpoint_auth_methods_supported": AUTH_METHODS,
        "jwks_uri": format!("{authority}/discovery/v2.0/keys"),
        "response_modes_supported": ["query", "fragment", "form_post"],
        "subject_types_supported": ["pairwise"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "response_types_supported": ["code", "id_token", "code id_token", "id_token token"],
        "scopes_supported": ["openid", "profile", "email", "offline_access"],
        "issuer": token_issuer(&app),
        "request_uri_parameter_supported": false,
        "authorization_endpoint": format!("{authority}/oauth2/v2.0/authorize"),
        "end_session_endpoint": format!("{authority}/oauth2/v2.0/logout"),
        "http_logout_supported": true,
        "frontchannel_logout_supported": true,
        "claims_supported": ["sub", "iss", "aud", "exp", "iat", "nbf", "tid", "appid", "azp"],
        "tenant_region_scope": Value::Null,
        "cloud_instance_name": app.config.public_origin.host_str(),
    })))
}

/// `GET /adfs/.well-known/openid-configuration`: the same for the authority `<origin>/adfs`.
async fn adfs_metadata(State(app): State<Arc<App>>) -> Response {
    let authority = format!("{}/adfs", app.config.origin());
    json_answer(&json!({
        "issuer": authority,
        "authorization_endpoint": format!("{authority}/oauth2/authorize"),
        "token_endpoint": format!("{authority}/oauth2/token"),
        "jwks_uri": format!("{authority}/discovery/keys"),
        "token_endpoint_auth_methods_supported": AUTH_METHODS,
        "response_types_supported": ["code", "id_token", "code id_token", "id_token token"],
        "response_modes_supported": ["query", "fragment", "form_post"],
        "grant_types_supported": ["client_credentials"],
        "subject_types_supported": ["pairwise"],
        "scopes_supported": ["openid", "profile", "email", "offline_access"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "end_session_endpoint": format!("{authority}/oauth2/logout"),
    }))
}

/// `GET /bot/v1/.well-known/openidconfiguration`: what a bot checks an activity's token against, in the Bot
/// Framework's shape.
async fn bot_metadata(State(app): State<Arc<App>>) -> Response {
    let origin = app.config.origin();
    json_answer(&json!({
        "issuer": super::issuer(&app.config),
        "authorization_endpoint": format!("{origin}/bot/invalid"),
        "jwks_uri": format!("{origin}/bot/v1/.well-known/keys"),
        "id_token_signing_alg_values_supported": ["RS256"],
        "token_endpoint_auth_methods_supported": ["private_key_jwt"],
    }))
}

/// The JWKS of the bot signing keys. Not kept by caches long: a key another node just made must show.
async fn keys(State(app): State<Arc<App>>) -> Result<Response, Refusal> {
    let keys = app.bot_keys.published().await?;
    Ok(([(CACHE_CONTROL, "max-age=300, public")], axum::Json(keys)).into_response())
}

async fn tenant_keys(State(app): State<Arc<App>>, Path(tenant): Path<String>) -> Result<Response, Refusal> {
    if !ours(&app, &tenant) {
        return Err(Refusal::tenant());
    }
    keys(State(app)).await
}

async fn tenant_token(
    State(app): State<Arc<App>>,
    Path(tenant): Path<String>,
    headers: HeaderMap,
    Form(form): Form<BTreeMap<String, String>>,
) -> Result<Response, Refusal> {
    if !ours(&app, &tenant) {
        return Err(Refusal::tenant());
    }
    grant(&app, &headers, &form).await
}

async fn adfs_token(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Form(form): Form<BTreeMap<String, String>>,
) -> Result<Response, Refusal> {
    grant(&app, &headers, &form).await
}

/// The client id and secret: HTTP Basic (each part form-encoded, RFC 6749 §2.3.1) or the form's.
fn client(headers: &HeaderMap, form: &BTreeMap<String, String>) -> Result<(String, Option<String>), Refusal> {
    let basic = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
        .and_then(|(_, encoded)| STANDARD.decode(encoded.trim()).ok())
        .and_then(|decoded| String::from_utf8(decoded).ok());
    if let Some(basic) = basic {
        let (id, secret) = basic.split_once(':').ok_or_else(Refusal::missing)?;
        let decode = |part: &str| {
            url::form_urlencoded::parse(format!("x={part}").as_bytes())
                .next()
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default()
        };
        return Ok((decode(id), Some(decode(secret)).filter(|secret| !secret.is_empty())));
    }
    let field = |name: &str| form.get(name).filter(|value| !value.is_empty()).cloned();
    Ok((field("client_id").ok_or_else(Refusal::missing)?, field("client_secret")))
}

/// The client-credentials grant: a token for the bots' resource to a registered, enabled bot that proves its
/// secret.
async fn grant(app: &App, headers: &HeaderMap, form: &BTreeMap<String, String>) -> Result<Response, Refusal> {
    match form.get("grant_type").map(String::as_str) {
        Some("client_credentials") => {}
        Some("") | None => return Err(Refusal::missing()),
        Some(_) => return Err(Refusal::grant_type()),
    }
    let (client_id, secret) = client(headers, form)?;
    let client_id = client_id.trim().to_lowercase();
    let wanted = format!("{}/.default", app.config.bots.scope);
    if form
        .get("scope")
        .is_none_or(|scope| scope.split_whitespace().ne([wanted.as_str()]))
    {
        return Err(Refusal::scope());
    }
    let secret = secret.ok_or_else(Refusal::no_secret)?;
    let attempts = format!("bot:{client_id}");
    let registered = super::repo::credentials(&app.db, &client_id).await?;
    let given = super::digest(&secret);
    let proven = registered.as_ref().is_some_and(|bot| {
        aws_lc_rs::constant_time::verify_slices_are_equal(bot.secret_hash.as_bytes(), given.as_bytes()).is_ok()
    });
    // the bot's id is public, so a hold must not lock out the bot that has its secret
    let Some(bot) = registered.filter(|_| proven) else {
        return Err(match app.throttle.reserve(&attempts).await {
            Some(wait) => Refusal::held(wait),
            None => Refusal::credentials(),
        });
    };
    app.throttle.succeeded(&attempts).await;
    if !bot.enabled {
        return Err(Refusal::disabled());
    }
    if !bot.signed {
        return Err(Refusal::unsigned());
    }
    issue(app, &client_id)
}

fn issue(app: &App, bot: &str) -> Result<Response, Refusal> {
    let iat = now();
    let exp = iat + seconds(app.config.bots.token_lifetime());
    let issuer = token_issuer(app);
    let claims = BotClaims {
        aud: &app.config.bots.scope,
        iss: &issuer,
        iat,
        nbf: iat,
        exp,
        sub: bot,
        appid: bot,
        azp: bot,
        tid: app.issuer.tenant(),
        idtyp: "app",
        ver: "2.0",
        uti: uuid::Uuid::new_v4().simple().to_string(),
    };
    let token = app.issuer.keys().sign(&claims)?;
    let expires_in = seconds_until(exp);
    let body = json!({
        "token_type": "Bearer",
        "expires_in": expires_in,
        "ext_expires_in": expires_in,
        "access_token": token,
    });
    Ok(([(CACHE_CONTROL, "no-store")], axum::Json(body)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_database_or_key_is_a_server_error_in_entras_shape() {
        for refusal in [Refusal::from(DbError::Conflict), Refusal::from(KeyError::NoKey)] {
            let response = refusal.into_response();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        }
    }
}
