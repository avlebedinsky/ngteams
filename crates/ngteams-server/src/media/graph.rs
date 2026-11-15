//! Graph's file reads under `/graph/v1.0/` (its people are `crate::routes::graph`'s): the account's drive, and a shared file by its sharing link
//! (`docs/protocol/graph.md`, "A shared file's thumbnail" and "A shared file's content"). A file answers only the
//! accounts it is shared with; its bytes and pictures are behind links signed for an hour ([`super::drive`]).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

use super::{Error, account, drive, repo, thumbs};
use crate::app::App;
use crate::db::users::User;

const GRAPH: &str = <crate::routes::graph::Graph as crate::auth::extract::Audience>::AUDIENCE;
/// The longest sharing token read.
const MAX_SHARE: usize = 4096;

pub(super) fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/graph/v1.0/me/drive", get(own_drive))
        .route("/graph/v1.0/shares/{share}/driveItem", get(item))
        .route("/graph/v1.0/shares/{share}/driveItem/thumbnails", get(thumbnails))
}

pub(super) async fn caller(app: &App, headers: &HeaderMap) -> Result<User, Error> {
    account(app, headers, GRAPH).await
}

async fn own_drive(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Value>, Error> {
    let caller = caller(&app, &headers).await?;
    Ok(Json(drive::drive(&app, &caller)))
}

/// The file a sharing token (`u!<the link, base64url>`) names, when `caller` may read it.
async fn shared(app: &App, caller: &User, share: &str) -> Result<repo::Item, Error> {
    let link = share
        .strip_prefix("u!")
        .filter(|encoded| encoded.len() <= MAX_SHARE)
        .and_then(|encoded| URL_SAFE_NO_PAD.decode(encoded).ok())
        .and_then(|link| String::from_utf8(link).ok())
        .ok_or(Error::Invalid("The sharing token can't be read."))?;
    let item = drive::shared(app, &link).await?;
    if !repo::grants(&app.db, &item.id).await?.contains(&caller.id) {
        return Err(Error::Forbidden);
    }
    Ok(item)
}

async fn item(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(share): Path<String>,
) -> Result<Json<Value>, Error> {
    let caller = caller(&app, &headers).await?;
    let item = shared(&app, &caller, &share).await?;
    let mut answer = drive::drive_item(&app, &item)?;
    answer["@microsoft.graph.downloadUrl"] = drive::download_link(&app, &caller, &item)?.into();
    Ok(Json(answer))
}

async fn thumbnails(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(share): Path<String>,
) -> Result<Json<Value>, Error> {
    let caller = caller(&app, &headers).await?;
    let item = shared(&app, &caller, &share).await?;
    let made = thumbs::of(&app, &item).await?;
    if made.sizes.is_empty() {
        return Ok(Json(json!({"value": []})));
    }
    let mut set = json!({"id": "0"});
    for (size, (width, height)) in &made.sizes {
        set[size] =
            json!({"url": drive::thumbnail_link(&app, &caller, &item, size)?, "width": width, "height": height});
    }
    Ok(Json(json!({"value": [set]})))
}
