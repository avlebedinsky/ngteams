//! Pictures in messages, files sent in chats and link previews, as the client's AMS, SharePoint and Graph requests
//! ask for them (`docs/server.md`, "Pictures, files and link previews").
//!
//! Every blob lives in [`crate::storage`], shared by all nodes; its row says who may read it. A picture is read by
//! the members of the chats it was made for, checked against the database on every read. A file is read by the
//! accounts its owner shared it with. A file's bytes and thumbnail go out only through short-lived links this server
//! signs ([`links`]), on its own origin: the client takes no file link from any other host, and no token of an
//! account ever reaches the store.

mod ams;
mod drive;
mod fetch;
mod graph;
mod people;
mod repo;
mod schema;
mod thumbs;
mod urlp;

use std::sync::Arc;

pub(crate) use fetch::Fetcher;

use axum::http::header::WWW_AUTHENTICATE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::json;

use crate::app::App;
use crate::auth::keys::KeyError;
use crate::db::DbError;
use crate::db::users::{self, User};
use crate::storage::StorageError;

/// The largest body of a read by POST.
const SMALL_BODY: usize = 64 * 1024;
/// How long a read may take, and how long a file's or a picture's bytes may take to arrive or to start leaving.
const SHORT: std::time::Duration = std::time::Duration::from_secs(30);
const LONG: std::time::Duration = std::time::Duration::from_secs(600);

/// The chat service's audience, which AMS takes too.
const IC3: &str = "https://ic3.teams.office.com";

pub(crate) fn router(app: &App) -> Router<Arc<App>> {
    let small = Router::new()
        .merge(graph::router())
        .route(
            &format!(
                "{}/beta/users/fetchShortProfile",
                crate::services::middle_tier_prefix(&app.config)
            ),
            axum::routing::post(people::short_profiles),
        )
        .layer(axum::extract::DefaultBodyLimit::max(SMALL_BODY))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            SHORT,
        ));
    let previews = if app.config.media.link_previews {
        urlp::router()
    } else {
        Router::new()
    };
    let long = ams::router(app).merge(drive::router()).merge(previews).layer(
        tower_http::timeout::TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, LONG),
    );
    small.merge(long)
}

#[derive(Debug)]
enum Error {
    Unauthorized,
    Forbidden,
    Missing,
    Invalid(&'static str),
    Unsupported,
    TooLarge,
    Quota,
    Upstream,
    Busy,
    Database(DbError),
    Storage(StorageError),
    Key(KeyError),
}

impl From<DbError> for Error {
    fn from(error: DbError) -> Self {
        Self::Database(error)
    }
}

impl From<StorageError> for Error {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::TooLarge => Self::TooLarge,
            StorageError::Body(_) => Self::Invalid("The body couldn't be read."),
            other => Self::Storage(other),
        }
    }
}

impl From<KeyError> for Error {
    fn from(error: KeyError) -> Self {
        Self::Key(error)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Unauthorized => {
                return (
                    StatusCode::UNAUTHORIZED,
                    [(WWW_AUTHENTICATE, "Bearer error=\"invalid_token\"")],
                )
                    .into_response();
            }
            Self::Forbidden => (StatusCode::FORBIDDEN, "accessDenied", "Access denied."),
            Self::Missing => (StatusCode::NOT_FOUND, "itemNotFound", "The item wasn't found."),
            Self::Invalid(message) => (StatusCode::BAD_REQUEST, "invalidRequest", message),
            Self::Unsupported => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupportedMediaType",
                "The bytes aren't a picture the server takes.",
            ),
            Self::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "tooLarge", "The body is too large."),
            Self::Quota => (
                StatusCode::INSUFFICIENT_STORAGE,
                "quotaLimitReached",
                "The account's storage is full.",
            ),
            Self::Upstream => (StatusCode::BAD_GATEWAY, "badGateway", "The link's site didn't answer."),
            Self::Busy => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(axum::http::header::RETRY_AFTER, "5")],
                )
                    .into_response();
            }
            Self::Database(error) => return unavailable(&error),
            Self::Storage(error) => return unavailable(&error),
            Self::Key(error) => return unavailable(&error),
        };
        (status, Json(json!({"error": {"code": code, "message": message}}))).into_response()
    }
}

fn unavailable(error: &dyn std::fmt::Display) -> Response {
    tracing::error!(%error, "media request failed");
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

/// The enabled account whose access token for `audience` the request carries.
async fn account(app: &App, headers: &HeaderMap, audience: &str) -> Result<User, Error> {
    let token = crate::auth::extract::bearer(headers).ok_or(Error::Unauthorized)?;
    let claims = app
        .issuer
        .verify_access(token, audience)
        .await
        .map_err(|_| Error::Unauthorized)?;
    users::by_id(&app.db, &claims.oid)
        .await?
        .filter(|user| !user.disabled)
        .ok_or(Error::Forbidden)
}

fn quota(app: &App) -> i64 {
    i64::try_from(app.config.media.user_quota_bytes).unwrap_or(i64::MAX)
}
