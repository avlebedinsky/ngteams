//! The admin API's app routes (`/api/admin/v1/apps…`, `docs/server.md` has the shapes): register a bot from the
//! form, upload or replace a Teams app package, read and list apps, change who may use one, its state or its bot's
//! endpoint and mode, give its bot a new secret, delete it. Admins only, as the account routes ([`Admin`]); each
//! change is audited. A bot's secret is in the answer that made it and nowhere else.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::repo::{self, Record};
use super::{AppError, AuthMode, BotForm, Changes, Registry};
use crate::admin::Actor;
use crate::admin::api::{Admin, ApiError};
use crate::app::App;

/// The largest JSON body these routes take.
const BODY_LIMIT: usize = 16 * 1024;

pub(super) fn router() -> Router<Arc<App>> {
    let packages = Router::new()
        .route("/api/admin/v1/apps/package", post(upload))
        .route("/api/admin/v1/apps/{id}/package", put(replace))
        .layer(DefaultBodyLimit::max(super::MAX_PACKAGE));
    Router::new()
        .route("/api/admin/v1/apps", get(list).post(register))
        .route("/api/admin/v1/apps/{id}", get(read).patch(update).delete(remove))
        .route("/api/admin/v1/apps/{id}/secret", post(rotate))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .merge(packages)
}

fn refused(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::Refused {
        status,
        code,
        message: message.into(),
        retry_after: None,
    }
}

impl From<AppError> for ApiError {
    fn from(error: AppError) -> Self {
        let (status, code) = match error {
            AppError::Db(error) => return Self::from(error),
            AppError::Storage(_) | AppError::Random => {
                tracing::error!(%error, "admin API failed");
                return refused(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "ServerError",
                    "The server can't do this now.",
                );
            }
            AppError::NoSuchApp => (StatusCode::NOT_FOUND, "NotFound"),
            AppError::AppExists => (StatusCode::CONFLICT, "AppExists"),
            AppError::BotExists => (StatusCode::CONFLICT, "BotExists"),
            AppError::BotChanged => (StatusCode::BAD_REQUEST, "BotChanged"),
            AppError::NoBot => (StatusCode::BAD_REQUEST, "NoBot"),
            AppError::Name => (StatusCode::BAD_REQUEST, "InvalidName"),
            AppError::Endpoint => (StatusCode::BAD_REQUEST, "InvalidEndpoint"),
            AppError::Unsigned => (StatusCode::BAD_REQUEST, "UnsignedNotAllowed"),
            AppError::UnknownUser => (StatusCode::BAD_REQUEST, "UnknownUser"),
            AppError::Package(_) => (StatusCode::BAD_REQUEST, "InvalidPackage"),
            AppError::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "PackageTooLarge"),
        };
        refused(status, code, error.to_string())
    }
}

fn answer(status: StatusCode, body: &Value) -> Response {
    (status, [(CACHE_CONTROL, "no-store")], axum::Json(body)).into_response()
}

/// A JSON body: anything that isn't the request's shape is a 400.
fn body<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(bytes).map_err(|error| refused(StatusCode::BAD_REQUEST, "InvalidRequest", error.to_string()))
}

/// An app as the API shows it: never its bot's secret.
pub(crate) fn app_json(record: &Record) -> Value {
    let app = &record.app;
    let bot = record.bot.as_ref().map(|bot| {
        json!({
            "id": bot.id,
            "mri": format!("28:{}", bot.id),
            "endpoint": bot.endpoint,
            "auth_mode": bot.auth_mode,
            "scopes": record.scopes(),
        })
    });
    json!({
        "id": app.id,
        "version": app.version,
        "name": app.name,
        "full_name": app.full_name,
        "short_description": app.short_description,
        "full_description": app.full_description,
        "developer": app.developer,
        "accent_color": app.accent_color,
        "source": app.source,
        "enabled": app.enabled,
        "everyone": app.everyone,
        "users": record.users,
        "manifest": serde_json::from_str::<Value>(&app.manifest).unwrap_or(Value::Null),
        "bot": bot,
        "created_at": app.created_at,
        "updated_at": app.updated_at,
    })
}

/// `app_json` with the secret shown once.
fn made(record: &Record, secret: Option<String>) -> Value {
    let mut json = app_json(record);
    if let Some(secret) = secret {
        json["secret"] = secret.into();
    }
    json
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterBody {
    name: String,
    #[serde(default)]
    description: String,
    endpoint: Option<String>,
    #[serde(default)]
    auth_mode: AuthMode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBody {
    enabled: Option<bool>,
    everyone: Option<bool>,
    users: Option<Vec<String>>,
    endpoint: Option<String>,
    auth_mode: Option<AuthMode>,
}

/// `GET /api/admin/v1/apps`: every app, by name.
async fn list(State(app): State<Arc<App>>, Admin(_): Admin) -> Result<Response, ApiError> {
    let apps: Vec<Value> = repo::list(&app.db).await?.iter().map(app_json).collect();
    Ok(answer(StatusCode::OK, &json!({ "apps": apps })))
}

/// `POST /api/admin/v1/apps`: a bot from the form.
async fn register(State(app): State<Arc<App>>, Admin(admin): Admin, bytes: Bytes) -> Result<Response, ApiError> {
    let request: RegisterBody = body(&bytes)?;
    let form = BotForm {
        name: request.name,
        description: request.description,
        endpoint: request.endpoint,
        auth_mode: request.auth_mode,
    };
    let (record, secret) = super::register(Registry::of(&app), Actor::account(&admin), &form).await?;
    Ok(answer(StatusCode::CREATED, &made(&record, Some(secret))))
}

/// `POST /api/admin/v1/apps/package`: the app a Teams package holds.
async fn upload(State(app): State<Arc<App>>, Admin(admin): Admin, package: Bytes) -> Result<Response, ApiError> {
    let (record, secret) = super::add_package(Registry::of(&app), Actor::account(&admin), package).await?;
    Ok(answer(StatusCode::CREATED, &made(&record, secret)))
}

/// `PUT /api/admin/v1/apps/{id}/package`: a new package of the app.
async fn replace(
    State(app): State<Arc<App>>,
    Admin(admin): Admin,
    Path(id): Path<String>,
    package: Bytes,
) -> Result<Response, ApiError> {
    let record = super::replace_package(Registry::of(&app), Actor::account(&admin), &id, package).await?;
    Ok(answer(StatusCode::OK, &app_json(&record)))
}

/// `GET /api/admin/v1/apps/{id}`.
async fn read(State(app): State<Arc<App>>, Admin(_): Admin, Path(id): Path<String>) -> Result<Response, ApiError> {
    let record = repo::get(&app.db, &id).await?.ok_or(AppError::NoSuchApp)?;
    Ok(answer(StatusCode::OK, &app_json(&record)))
}

/// `PATCH /api/admin/v1/apps/{id}`: its state, who may use it, its bot's endpoint and mode, each when given.
async fn update(
    State(app): State<Arc<App>>,
    Admin(admin): Admin,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    let request: UpdateBody = body(&bytes)?;
    let changes = Changes {
        enabled: request.enabled,
        everyone: request.everyone,
        users: request.users,
        endpoint: request.endpoint,
        auth_mode: request.auth_mode,
    };
    let record = super::update(Registry::of(&app), Actor::account(&admin), &id, changes).await?;
    Ok(answer(StatusCode::OK, &app_json(&record)))
}

/// `POST /api/admin/v1/apps/{id}/secret`: a new secret for its bot, shown once; the old one stops working.
async fn rotate(State(app): State<Arc<App>>, Admin(admin): Admin, Path(id): Path<String>) -> Result<Response, ApiError> {
    let secret = super::rotate_secret(&app.db, Actor::account(&admin), &id).await?;
    Ok(answer(StatusCode::OK, &json!({ "secret": secret })))
}

/// `DELETE /api/admin/v1/apps/{id}`.
async fn remove(State(app): State<Arc<App>>, Admin(admin): Admin, Path(id): Path<String>) -> Result<Response, ApiError> {
    super::delete(Registry::of(&app), Actor::account(&admin), &id).await?;
    Ok((StatusCode::NO_CONTENT, [(CACHE_CONTROL, "no-store")]).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_of_the_server_is_a_500_that_says_nothing_of_it() {
        let failures = [
            AppError::Random,
            AppError::Storage(crate::storage::StorageError::Key),
            AppError::Db(crate::db::DbError::Conflict),
        ];
        for failure in failures {
            let response = ApiError::from(failure).into_response();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        }
        let too_large = ApiError::from(AppError::TooLarge).into_response();
        assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
