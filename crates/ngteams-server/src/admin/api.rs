//! The admin API (`/api/admin/v1/…`), the first-admin setup and the account's own endpoints (`/api/account/v1/…`),
//! all JSON (`docs/server.md` has the shapes). The admin routes take the server's access token for
//! `https://api.spaces.skype.com` and look the account up on every request: a demoted, disabled or deleted admin is
//! refused at once, whatever its token says.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, RETRY_AFTER};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::repo::{self, Entry};
use super::{Actor, AdminError, Changes, NewAccount};
use crate::app::App;
use crate::auth::extract::{Bearer, Spaces, Unauthorized};
use crate::auth::password::{self, PasswordError};
use crate::db::DbError;
use crate::db::users::{self, User, normalize_upn};

/// The largest request body these endpoints take.
const BODY_LIMIT: usize = 16 * 1024;
/// A page of accounts or audit entries, when the request names no size, and the largest.
const PAGE: u32 = 50;
const MAX_PAGE: u32 = 200;

/// The routes.
pub fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/api/admin/v1/setup", get(setup_state).post(setup))
        .route("/api/admin/v1/users", get(list).post(create))
        .route("/api/admin/v1/users/{id}", get(read).patch(update).delete(remove))
        .route("/api/admin/v1/users/{id}/password", post(reset_password))
        .route("/api/admin/v1/audit", get(audit))
        .route("/api/account/v1/me", get(me))
        .route("/api/account/v1/password", post(change_password))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// A refusal: `{"error": {"code", "message"}}`, or the 401 of a missing or invalid token.
#[derive(Debug)]
pub enum ApiError {
    Token(Unauthorized),
    Refused {
        status: StatusCode,
        code: &'static str,
        message: String,
        retry_after: Option<Duration>,
    },
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self::Refused {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }

    fn held(wait: Duration) -> Self {
        Self::Refused {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "TooManyAttempts",
            message: "Too many attempts; try again later.".to_owned(),
            retry_after: Some(wait),
        }
    }

    fn server(error: &dyn std::fmt::Display) -> Self {
        tracing::error!(%error, "admin API failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "ServerError",
            "The server can't do this now.",
        )
    }

    fn credentials() -> Self {
        Self::from(AdminError::Credentials)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Token(unauthorized) => unauthorized.into_response(),
            Self::Refused {
                status,
                code,
                message,
                retry_after,
            } => {
                let mut response = answer(status, &json!({"error": {"code": code, "message": message}}));
                if let Some(wait) = retry_after
                    && let Ok(value) = wait.as_secs().max(1).to_string().parse()
                {
                    response.headers_mut().insert(RETRY_AFTER, value);
                }
                response
            }
        }
    }
}

impl From<DbError> for ApiError {
    fn from(error: DbError) -> Self {
        Self::server(&error)
    }
}

impl From<AdminError> for ApiError {
    fn from(error: AdminError) -> Self {
        let (status, code) = match &error {
            AdminError::Db(_) | AdminError::Random | AdminError::Password(PasswordError::Hash) => {
                return Self::server(&error);
            }
            AdminError::Password(PasswordError::TooShort) => (StatusCode::BAD_REQUEST, "WeakPassword"),
            AdminError::Upn => (StatusCode::BAD_REQUEST, "InvalidUpn"),
            AdminError::Name => (StatusCode::BAD_REQUEST, "InvalidName"),
            AdminError::SamePassword => (StatusCode::BAD_REQUEST, "SamePassword"),
            AdminError::SetupCode => (StatusCode::FORBIDDEN, "SetupCode"),
            AdminError::Credentials => (StatusCode::FORBIDDEN, "InvalidCredentials"),
            AdminError::NoSuchUser => (StatusCode::NOT_FOUND, "NotFound"),
            AdminError::Exists => (StatusCode::CONFLICT, "UpnTaken"),
            AdminError::LastAdmin => (StatusCode::CONFLICT, "LastAdmin"),
            AdminError::AdminExists => (StatusCode::CONFLICT, "AdminExists"),
        };
        Self::new(status, code, error.to_string())
    }
}

/// A JSON answer no cache may keep.
fn answer(status: StatusCode, body: &Value) -> Response {
    (status, [(CACHE_CONTROL, "no-store")], Json(body)).into_response()
}

fn done() -> Response {
    (StatusCode::NO_CONTENT, [(CACHE_CONTROL, "no-store")]).into_response()
}

/// An account as the API shows it: never its password hash.
fn account_json(user: &User) -> Value {
    json!({
        "id": user.id,
        "mri": format!("8:orgid:{}", user.id),
        "upn": user.upn,
        "display_name": user.display_name,
        "enabled": !user.disabled,
        "admin": user.is_admin,
        "must_change_password": user.must_change_password,
        "created_at": user.created_at,
        "updated_at": user.updated_at,
    })
}

fn entry_json(entry: &Entry) -> Value {
    json!({
        "id": entry.id,
        "at": entry.at,
        "actor_id": entry.actor_id,
        "actor": entry.actor,
        "action": entry.action,
        "target_id": entry.target_id,
        "target": entry.target,
        "detail": serde_json::from_str::<Value>(&entry.detail).unwrap_or(Value::Null),
    })
}

/// The enabled account a request's access token names.
#[derive(Debug)]
pub struct Account(pub User);

/// The same, when it is an admin.
#[derive(Debug)]
pub struct Admin(pub User);

impl FromRequestParts<Arc<App>> for Account {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        let bearer = Bearer::<Spaces>::from_request_parts(parts, app)
            .await
            .map_err(ApiError::Token)?;
        match users::by_id(&app.db, &bearer.claims.oid).await? {
            Some(user) if !user.disabled => Ok(Self(user)),
            _ => Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "AccountDisabled",
                "This account can't use this server.",
            )),
        }
    }
}

impl FromRequestParts<Arc<App>> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        let Account(user) = Account::from_request_parts(parts, app).await?;
        if user.is_admin {
            Ok(Self(user))
        } else {
            Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "NotAdmin",
                "Only an admin may do this.",
            ))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupBody {
    code: Option<String>,
    upn: String,
    #[serde(default)]
    display_name: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    upn: String,
    #[serde(default)]
    display_name: String,
    password: String,
    #[serde(default)]
    admin: bool,
    #[serde(default)]
    must_change_password: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBody {
    display_name: Option<String>,
    upn: Option<String>,
    enabled: Option<bool>,
    admin: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetBody {
    password: String,
    #[serde(default)]
    must_change_password: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeBody {
    upn: String,
    current_password: String,
    new_password: String,
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    after: String,
    limit: Option<u32>,
}

#[derive(Deserialize)]
struct AuditQuery {
    before: Option<i64>,
    limit: Option<u32>,
}

/// The page size a request asks for, within bounds.
fn page(limit: Option<u32>) -> usize {
    usize::try_from(limit.unwrap_or(PAGE).clamp(1, MAX_PAGE)).unwrap_or(1)
}

/// `rows` cut to `size`, and whether there were more.
fn cut<T>(mut rows: Vec<T>, size: usize) -> (Vec<T>, bool) {
    let more = rows.len() > size;
    rows.truncate(size);
    (rows, more)
}

/// One more than `size`, as a query's limit: the extra row tells whether another page follows.
fn probe(size: usize) -> i64 {
    i64::try_from(size).map_or(i64::MAX, |size| size.saturating_add(1))
}

/// `GET /api/admin/v1/setup`: whether the server still needs its first admin, and whether creating it takes the
/// setup code. Open to anyone: the client asks before it has an account.
async fn setup_state(State(app): State<Arc<App>>) -> Result<Response, ApiError> {
    let needs_admin = repo::admins(&app.db).await? == 0;
    let body = json!({
        "needs_admin": needs_admin,
        "code_required": needs_admin && app.config.setup.require_code,
    });
    Ok(answer(StatusCode::OK, &body))
}

/// `POST /api/admin/v1/setup`: creates the first admin. Attempts are limited per client address.
async fn setup(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<SetupBody>,
) -> Result<Response, ApiError> {
    let attempts = format!("setup:{}:{}", app.issuer.tenant(), peer.ip());
    if let Some(wait) = app.setup_throttle.reserve(&attempts).await {
        return Err(ApiError::held(wait));
    }
    let new = NewAccount {
        upn: &body.upn,
        display_name: &body.display_name,
        password: &body.password,
        admin: true,
        must_change_password: false,
    };
    let user = super::create_first_admin(&app.db, app.config.setup.require_code, body.code.as_deref(), &new).await?;
    Ok(answer(StatusCode::CREATED, &account_json(&user)))
}

/// `GET /api/admin/v1/users?q=&after=&limit=`: accounts by UPN; `next` is the `after` of the next page.
async fn list(
    State(app): State<Arc<App>>,
    Admin(_): Admin,
    Query(query): Query<ListQuery>,
) -> Result<Response, ApiError> {
    let size = page(query.limit);
    let rows = repo::list(&app.db, query.q.trim(), &normalize_upn(&query.after), probe(size)).await?;
    let (rows, more) = cut(rows, size);
    let next = more.then(|| rows.last().map(|user| user.upn.as_str())).flatten();
    let users: Vec<Value> = rows.iter().map(account_json).collect();
    Ok(answer(StatusCode::OK, &json!({"users": users, "next": next})))
}

/// `POST /api/admin/v1/users`.
async fn create(
    State(app): State<Arc<App>>,
    Admin(admin): Admin,
    Json(body): Json<CreateBody>,
) -> Result<Response, ApiError> {
    let new = NewAccount {
        upn: &body.upn,
        display_name: &body.display_name,
        password: &body.password,
        admin: body.admin,
        must_change_password: body.must_change_password,
    };
    let user = super::create_user(&app.db, Actor::account(&admin), &new).await?;
    Ok(answer(StatusCode::CREATED, &account_json(&user)))
}

/// `GET /api/admin/v1/users/{id}`.
async fn read(State(app): State<Arc<App>>, Admin(_): Admin, Path(id): Path<String>) -> Result<Response, ApiError> {
    let user = repo::get(&app.db, &id).await?.ok_or(AdminError::NoSuchUser)?;
    Ok(answer(StatusCode::OK, &account_json(&user)))
}

/// `PATCH /api/admin/v1/users/{id}`: the name, UPN, enabled state and admin role, each when given.
async fn update(
    State(app): State<Arc<App>>,
    Admin(admin): Admin,
    Path(id): Path<String>,
    Json(body): Json<UpdateBody>,
) -> Result<Response, ApiError> {
    let changes = Changes {
        display_name: body.display_name,
        upn: body.upn,
        enabled: body.enabled,
        admin: body.admin,
    };
    let user = super::update_user(&app.db, Actor::account(&admin), &id, changes).await?;
    Ok(answer(StatusCode::OK, &account_json(&user)))
}

/// `DELETE /api/admin/v1/users/{id}`.
async fn remove(
    State(app): State<Arc<App>>,
    Admin(admin): Admin,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    super::delete_user(&app.db, Actor::account(&admin), &id).await?;
    Ok(done())
}

/// `POST /api/admin/v1/users/{id}/password`.
async fn reset_password(
    State(app): State<Arc<App>>,
    Admin(admin): Admin,
    Path(id): Path<String>,
    Json(body): Json<ResetBody>,
) -> Result<Response, ApiError> {
    let actor = Actor::account(&admin);
    super::reset_user_password(&app.db, actor, &id, &body.password, body.must_change_password).await?;
    Ok(done())
}

/// `GET /api/admin/v1/audit?before=&limit=`: admin actions, newest first; `next` is the `before` of the next page.
async fn audit(
    State(app): State<Arc<App>>,
    Admin(_): Admin,
    Query(query): Query<AuditQuery>,
) -> Result<Response, ApiError> {
    let size = page(query.limit);
    let rows = repo::audit(&app.db, query.before.unwrap_or(i64::MAX), probe(size)).await?;
    let (rows, more) = cut(rows, size);
    let next = more.then(|| rows.last().map(|entry| entry.id)).flatten();
    let entries: Vec<Value> = rows.iter().map(entry_json).collect();
    Ok(answer(StatusCode::OK, &json!({"entries": entries, "next": next})))
}

/// `GET /api/account/v1/me`: the signed-in account as the database has it now (its role too, which a token only
/// names as of its minting).
async fn me(Account(user): Account) -> Response {
    answer(StatusCode::OK, &account_json(&user))
}

/// `POST /api/account/v1/password`: an account replaces its own password, proving the current one (no token: an
/// account that must change its password can't get one). Guesses count as sign-in attempts do. Signs the account
/// out everywhere.
async fn change_password(State(app): State<Arc<App>>, Json(body): Json<ChangeBody>) -> Result<Response, ApiError> {
    let upn = normalize_upn(&body.upn);
    if let Some(wait) = app.throttle.reserve(&upn).await {
        return Err(ApiError::held(wait));
    }
    let user = users::by_upn(&app.db, &upn).await?;
    let hash = user.as_ref().map(|user| user.password_hash.as_str());
    if !password::verify(&body.current_password, hash).await {
        return Err(ApiError::credentials());
    }
    app.throttle.succeeded(&upn).await;
    let user = user.filter(|user| !user.disabled).ok_or_else(ApiError::credentials)?;
    super::change_password(&app.db, &user, &body.current_password, &body.new_password).await?;
    Ok(done())
}
