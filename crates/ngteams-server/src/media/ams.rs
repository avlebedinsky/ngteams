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
use crate::db::Db;
use crate::db::users::User;
use crate::storage::Storage;
use crate::teams::repo::joined;

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

fn key(object: &str, blob: &str) -> String {
    format!("ams/{object}/{blob}")
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
    if permissions.values().any(|rights| rights != &json!(["read"])) {
        return Err(Error::Invalid("A chat may only read an object."));
    }
    let chats: Vec<String> = permissions.keys().cloned().collect();
    if joined(&app.db, &caller.id, &chats).await?.len() != chats.len() {
        return Err(Error::Forbidden);
    }
    let object = repo::Object {
        id: format!("0-{}-d1-{}", app.config.region, uuid::Uuid::new_v4().simple()),
        owner_id: caller.id,
        kind: PICTURE.to_owned(),
        filename: filename.to_owned(),
        content_type: None,
        blob: None,
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
    store(&app.db, app.storage.as_ref(), &object, content_type, bytes, quota(&app)).await?;
    Ok(StatusCode::CREATED.into_response())
}

/// Stores `bytes` as what `object` holds. Each fill's bytes go under a key of their own, and one transaction then
/// names the key the object reads from with its type and size and charges the owner. So fills of one object at once
/// leave it describing and counting the bytes it serves, and a fill that fails or goes over the quota changes
/// nothing. A node that stops between the two leaves bytes nothing reads.
async fn store(
    db: &Db,
    storage: &dyn Storage,
    object: &repo::Object,
    content_type: &str,
    bytes: Bytes,
    quota: i64,
) -> Result<(), Error> {
    let size = i64::try_from(bytes.len()).map_err(|_| Error::TooLarge)?;
    let blob = uuid::Uuid::new_v4().simple().to_string();
    storage.put(&key(&object.id, &blob), bytes).await?;
    let filled = repo::fill_object(db, object, &blob, content_type, size, quota).await;
    let unread = match &filled {
        Ok(Some(replaced)) => replaced.blob.clone(),
        Ok(None) | Err(_) => Some(blob),
    };
    if let Some(unread) = unread
        && let Err(error) = storage.delete(&key(&object.id, &unread)).await
    {
        tracing::warn!(%error, "a picture's unread bytes weren't cleared");
    }
    filled?.map(|_| ()).ok_or(Error::Quota)
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
    let chats = repo::object_chats(&app.db, id).await?;
    if joined(&app.db, &caller.id, &chats).await?.is_empty() {
        return Err(Error::Forbidden);
    }
    Ok(object)
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
    // a fill may replace the bytes between the row's read and theirs: the row is then read once more
    for _ in 0..2 {
        let object = readable(&app, &caller, &id).await?;
        let (Some(content_type), Some(blob)) = (object.content_type, object.blob) else {
            break;
        };
        if let Some(bytes) = app.storage.get(&key(&id, &blob)).await? {
            let headers = [
                (CONTENT_TYPE, content_type.as_str()),
                (CACHE_CONTROL, "private, max-age=86400"),
            ];
            return Ok((headers, bytes).into_response());
        }
    }
    Err(Error::Missing)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use tokio::sync::{Notify, oneshot};

    use super::*;
    use crate::media::repo::testing::{db, object_of};
    use crate::storage::{Blob, Incoming, StorageError};

    /// A store whose put of `slow` bytes waits to be told whether it works.
    #[derive(Debug)]
    struct Held {
        blobs: Mutex<BTreeMap<String, Bytes>>,
        slow: usize,
        started: Notify,
        works: Mutex<Option<oneshot::Receiver<bool>>>,
    }

    impl Held {
        fn new(slow: usize) -> (Self, oneshot::Sender<bool>) {
            let (tell, works) = oneshot::channel();
            let store = Self {
                blobs: Mutex::default(),
                slow,
                started: Notify::new(),
                works: Mutex::new(Some(works)),
            };
            (store, tell)
        }

        fn blobs(&self) -> BTreeMap<String, Bytes> {
            self.blobs.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Storage for Held {
        async fn put(&self, key: &str, bytes: Bytes) -> Result<(), StorageError> {
            let works = self.works.lock().unwrap().take_if(|_| bytes.len() == self.slow);
            if let Some(works) = works {
                self.started.notify_one();
                if !works.await.unwrap() {
                    return Err(StorageError::TooLarge);
                }
            }
            self.blobs.lock().unwrap().insert(key.to_owned(), bytes);
            Ok(())
        }

        async fn get(&self, key: &str) -> Result<Option<Bytes>, StorageError> {
            Ok(self.blobs.lock().unwrap().get(key).cloned())
        }

        async fn delete(&self, key: &str) -> Result<(), StorageError> {
            self.blobs.lock().unwrap().remove(key);
            Ok(())
        }

        async fn put_stream(&self, _: &str, _: Incoming, _: u64) -> Result<u64, StorageError> {
            Err(StorageError::Key)
        }

        async fn open(&self, _: &str, _: Option<std::ops::Range<u64>>) -> Result<Option<Blob>, StorageError> {
            Err(StorageError::Key)
        }
    }

    /// One fill of ten bytes whose put is held while a fill of one byte runs whole; the held put then works or not.
    /// What the object describes, what the store holds for it, every blob in the store, and the owner's count.
    async fn overtaken(works: bool) -> (Option<String>, i64, Option<Bytes>, usize, i64, bool) {
        let (db, _dir) = db().await;
        let object = object_of(&db).await;
        let (storage, tell) = Held::new(10);
        let slow = store(&db, &storage, &object, "image/png", Bytes::from(vec![1; 10]), 0);
        let fast = async {
            storage.started.notified().await;
            store(&db, &storage, &object, "image/gif", Bytes::from(vec![2; 1]), 0)
                .await
                .unwrap();
            tell.send(works).unwrap();
        };
        let (slow, ()) = tokio::join!(slow, fast);
        let now = repo::object(&db, &object.id).await.unwrap().unwrap();
        let served = storage
            .get(&key(&object.id, now.blob.as_deref().unwrap()))
            .await
            .unwrap();
        let used = repo::used(&db, &object.owner_id).await.unwrap();
        (
            now.content_type,
            now.size,
            served,
            storage.blobs().len(),
            used,
            slow.is_ok(),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_fill_overtaken_by_another_leaves_the_object_describing_the_bytes_it_serves() {
        let (content_type, size, served, blobs, used, filled) = overtaken(true).await;
        assert!(filled);
        assert_eq!((content_type.as_deref(), size, used), (Some("image/png"), 10, 10));
        assert_eq!((served, blobs), (Some(Bytes::from(vec![1; 10])), 1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_fill_that_fails_after_another_filled_the_object_leaves_the_others_picture() {
        let (content_type, size, served, blobs, used, filled) = overtaken(false).await;
        assert!(!filled);
        assert_eq!((content_type.as_deref(), size, used), (Some("image/gif"), 1, 1));
        assert_eq!((served, blobs), (Some(Bytes::from(vec![2; 1])), 1));
    }
}
