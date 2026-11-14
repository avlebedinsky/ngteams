//! AMS under `/ams/`: the objects pictures pasted into messages live in (`docs/protocol/chat.md`, "Pictures sent").
//! An object is made for chats its maker is in, filled by its maker alone, and read by whoever is a member of one of
//! those chats when the read arrives.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};

use super::{Error, IC3, account, quota, repo};
use crate::app::App;
use crate::chatsvc;
use crate::db::users::User;

/// The largest body that makes an object.
const CREATE_BODY: usize = 16 * 1024;
/// The chats one object may be made for.
const MAX_CHATS: usize = 16;
/// The object type of a picture in a message.
const PICTURE: &str = "pish/image";
/// The views a picture is read by; the server keeps the bytes as sent and serves each view from them.
const VIEWS: [&str; 3] = ["imgo", "imgpsh_fullsize", "imgpsh_fullsize_anim"];
/// Days the media service says an object lives, as Teams' does. Nothing is dropped after them yet.
const EXPIRY_DAYS: i64 = 25;

pub(super) fn router(app: &App) -> Router<Arc<App>> {
    let picture = usize::try_from(app.config.media.max_picture_bytes).unwrap_or(usize::MAX);
    Router::new()
        .route("/ams/v1/objects", post(create))
        .route("/ams/v1/objects/", post(create))
        .route("/ams/v1/objects/{object}", get(read))
        .route("/ams/v1/objects/{object}/", get(read))
        .route("/ams/v1/objects/{object}/views/{view}", get(view))
        .layer(DefaultBodyLimit::max(CREATE_BODY))
        .merge(
            Router::new()
                .route("/ams/v1/objects/{object}/content/imgpsh", put(fill))
                .layer(DefaultBodyLimit::max(picture)),
        )
}

pub(super) fn key(object: &str) -> String {
    format!("ams/{object}")
}

async fn create(State(app): State<Arc<App>>, headers: HeaderMap, Json(body): Json<Value>) -> Result<Response, Error> {
    let caller = account(&app, &headers, IC3).await?;
    if body["type"].as_str() != Some(PICTURE) {
        return Err(Error::Invalid("Only pictures (pish/image) are stored."));
    }
    let filename = body["filename"]
        .as_str()
        .filter(|name| (1..=255).contains(&name.len()) && !name.chars().any(char::is_control))
        .ok_or(Error::Invalid("The object needs a file name."))?;
    let permissions = body["permissions"]
        .as_object()
        .filter(|chats| (1..=MAX_CHATS).contains(&chats.len()))
        .ok_or(Error::Invalid("The object needs the chats that may read it."))?;
    let mut chats = Vec::new();
    for (chat, rights) in permissions {
        if rights != &json!(["read"]) {
            return Err(Error::Invalid("A chat may only read an object."));
        }
        if !chatsvc::is_member(&app.db, chat, &caller.id).await? {
            return Err(Error::Forbidden);
        }
        chats.push(chat.clone());
    }
    let object = repo::Object {
        id: format!("0-{}-d1-{}", app.config.region, uuid::Uuid::new_v4().simple()),
        owner_id: caller.id,
        kind: PICTURE.to_owned(),
        filename: filename.to_owned(),
        content_type: None,
        size: 0,
        created_at: crate::db::now(),
    };
    repo::create_object(&app.db, &object, &chats).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": object.id}))).into_response())
}

async fn fill(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Result<Response, Error> {
    let caller = account(&app, &headers, IC3).await?;
    let object = repo::object(&app.db, &id).await?.ok_or(Error::Missing)?;
    if object.owner_id != caller.id {
        return Err(Error::Forbidden);
    }
    let content_type = picture_type(&bytes).ok_or(Error::Unsupported)?;
    let size = i64::try_from(bytes.len()).map_err(|_| Error::TooLarge)?;
    if !repo::fill_object(&app.db, &object, Some(content_type), size, quota(&app)).await? {
        return Err(Error::Quota);
    }
    if let Err(error) = app.storage.put(&key(&id), bytes).await {
        // the bytes aren't there: the object and its owner's count go back to what they were
        let filled = repo::Object { size, ..object.clone() };
        repo::fill_object(&app.db, &filled, object.content_type.as_deref(), object.size, 0).await?;
        return Err(error.into());
    }
    Ok(StatusCode::CREATED.into_response())
}

/// The media type of a picture's bytes, by their signature: the formats the client shows.
pub(super) fn picture_type(bytes: &[u8]) -> Option<&'static str> {
    match image::guess_format(bytes).ok()? {
        image::ImageFormat::Png => Some("image/png"),
        image::ImageFormat::Jpeg => Some("image/jpeg"),
        image::ImageFormat::Gif => Some("image/gif"),
        image::ImageFormat::WebP => Some("image/webp"),
        _ => None,
    }
}

/// The object when `caller` made it or is now in a chat it was made for.
async fn readable(app: &App, caller: &User, id: &str) -> Result<repo::Object, Error> {
    let object = repo::object(&app.db, id).await?.ok_or(Error::Missing)?;
    if object.owner_id == caller.id {
        return Ok(object);
    }
    for chat in repo::object_chats(&app.db, id).await? {
        if chatsvc::is_member(&app.db, &chat, &caller.id).await? {
            return Ok(object);
        }
    }
    Err(Error::Forbidden)
}

async fn read(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>) -> Result<Json<Value>, Error> {
    let caller = account(&app, &headers, IC3).await?;
    let object = readable(&app, &caller, &id).await?;
    let contents = match &object.content_type {
        Some(_) => json!({"imgpsh": {"length": object.size}}),
        None => json!({}),
    };
    Ok(Json(json!({
        "id": object.id, "type": object.kind, "filename": object.filename, "contents": contents,
        "creation_date": iso(object.created_at), "expiry_date": iso(object.created_at + EXPIRY_DAYS * 86_400),
        "geo": app.config.region, "tenant_id": app.issuer.tenant(),
    })))
}

fn iso(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

async fn view(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path((id, view)): Path<(String, String)>,
) -> Result<Response, Error> {
    let caller = account(&app, &headers, IC3).await?;
    if !VIEWS.contains(&view.as_str()) {
        return Err(Error::Missing);
    }
    let object = readable(&app, &caller, &id).await?;
    let content_type = object.content_type.ok_or(Error::Missing)?;
    let bytes = app.storage.get(&key(&id)).await?.ok_or(Error::Missing)?;
    Ok((
        [
            (CONTENT_TYPE, content_type.as_str()),
            (CACHE_CONTROL, "private, max-age=86400"),
        ],
        bytes,
    )
        .into_response())
}
