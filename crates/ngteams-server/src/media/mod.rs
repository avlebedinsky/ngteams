//! Pictures in messages, files sent in chats and link previews, as the client's AMS, SharePoint and Graph requests
//! ask for them (`docs/server.md`, "Pictures, files and link previews").
//!
//! Every blob lives in [`crate::storage`], shared by all nodes; its row says who may read it. A picture is read by
//! the members of the chats it was made for, checked against the database on every read. A file is read by the
//! accounts its owner shared it with. A file's bytes and thumbnail go out only through short-lived links this server
//! signs ([`drive`]), on its own origin: the client takes no file link from any other host, and no token of an
//! account ever reaches the store.

mod ams;
mod drive;
mod fetch;
mod graph;
mod repo;
mod schema;
mod thumbs;
mod urlp;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

pub(crate) use fetch::{Fetcher, is_public};
pub(crate) use thumbs::{decode, decoding};

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
const SHORT: Duration = Duration::from_secs(30);
const LONG: Duration = Duration::from_secs(600);

/// The chat service's audience, which AMS takes too.
const IC3: &str = <crate::auth::extract::Ic3 as crate::auth::extract::Audience>::AUDIENCE;

/// Where a file of `owner`'s drive named `name` opens: its sharing link.
pub(crate) fn file_url(app: &App, owner: &str, name: &str) -> Option<String> {
    drive::web_url(app, owner, name).ok()
}

pub(crate) fn router(app: &App) -> Router<Arc<App>> {
    let small = Router::new()
        .merge(graph::router())
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
                "The account's storage is full. Delete files or pictures it sent: a file stays in its sender's drive \
                 after its message is deleted. The server's administrator can delete them too.",
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

/// The enabled account whose access token for `audience` the request carries. A token from before the account's
/// last password reset is refused as an invalid one, as the push socket refuses it.
async fn account(app: &App, headers: &HeaderMap, audience: &str) -> Result<User, Error> {
    let token = crate::auth::extract::bearer(headers).ok_or(Error::Unauthorized)?;
    let claims = app
        .issuer
        .verify_access(token, audience)
        .await
        .map_err(|_| Error::Unauthorized)?;
    let user = users::by_id(&app.db, &claims.oid)
        .await?
        .filter(|user| !user.disabled)
        .ok_or(Error::Forbidden)?;
    if user.password_version == claims.pwd_ver {
        Ok(user)
    } else {
        Err(Error::Unauthorized)
    }
}

/// Removes bytes nothing reads any more. A store that fails leaves them: the log names their key.
async fn clear(storage: &dyn crate::storage::Storage, key: &str) {
    if let Err(error) = storage.delete(key).await {
        tracing::warn!(%error, key, "stored bytes nothing reads weren't cleared");
    }
}

/// The pictures one message's delete frees at most.
const RELEASED: usize = 64;

/// Frees those of `objects` that are `owner`'s pictures and that no message names any more, after a message that
/// named them was deleted. The delete stands when this fails: the owner, or the operator, can still delete the
/// object. Their bytes come off the owner's count here and leave the store in a task of its own, so a store out
/// of reach neither holds the delete's answer back nor loses the clearing to the request's deadline. A node that
/// stops waits for that task ([`Clearing`]).
pub(crate) async fn release(app: &Arc<App>, owner: &str, objects: &[String]) {
    let mut unread = Vec::new();
    for id in objects.iter().take(RELEASED) {
        match ams::release(&app.db, owner, id).await {
            Ok(key) => unread.extend(key),
            Err(error) => tracing::warn!(%error, "a picture no message shows wasn't freed"),
        }
    }
    if unread.is_empty() {
        return;
    }
    app.clearing.0.send_modify(|keys| keys.extend(unread.iter().cloned()));
    let app = Arc::clone(app);
    tokio::spawn(async move {
        for key in unread {
            clear(app.storage.as_ref(), &key).await;
            app.clearing.0.send_modify(|keys| {
                keys.remove(&key);
            });
        }
    });
}

/// The store keys [`release`] still clears. Nothing else names them once their rows are gone, so a node that stops
/// waits for them ([`Self::done`]).
#[derive(Debug, Default)]
pub(crate) struct Clearing(tokio::sync::watch::Sender<BTreeSet<String>>);

impl Clearing {
    /// Waits until no key is left, at most `limit`; the keys left then, each named in the log.
    pub(crate) async fn done(&self, limit: Duration) -> Vec<String> {
        let mut keys = self.0.subscribe();
        if tokio::time::timeout(limit, keys.wait_for(BTreeSet::is_empty))
            .await
            .is_ok()
        {
            return Vec::new();
        }
        let left: Vec<String> = self.0.borrow().iter().cloned().collect();
        for key in &left {
            tracing::warn!(
                key,
                "stored bytes nothing reads weren't cleared before the node stopped"
            );
        }
        left
    }
}

/// One thing an account keeps in the store, for the operator's commands.
pub(crate) struct Stored {
    pub kind: &'static str,
    pub id: String,
    pub name: String,
    pub size: i64,
    pub created_at: i64,
}

/// What `owner` keeps in the store: its files, then its pictures, oldest first.
pub(crate) async fn stored(db: &crate::db::Db, owner: &str) -> Result<Vec<Stored>, DbError> {
    let files = repo::items(db, owner).await?.into_iter().map(|item| Stored {
        kind: "file",
        id: item.id,
        name: item.name,
        size: item.size,
        created_at: item.created_at,
    });
    let pictures = repo::objects(db, owner).await?.into_iter().map(|object| Stored {
        kind: "picture",
        id: object.id,
        name: object.filename,
        size: object.size,
        created_at: object.created_at,
    });
    Ok(files.chain(pictures).collect())
}

/// Removes the file or the picture `id` of `owner` with its bytes; whether the owner had it.
pub(crate) async fn remove_stored(
    db: &crate::db::Db,
    storage: &dyn crate::storage::Storage,
    owner: &str,
    id: &str,
) -> Result<bool, DbError> {
    Ok(drive::remove(db, storage, owner, id).await? || ams::remove(db, storage, owner, id).await?)
}

fn quota(app: &App) -> i64 {
    i64::try_from(app.config.media.user_quota_bytes).unwrap_or(i64::MAX)
}
