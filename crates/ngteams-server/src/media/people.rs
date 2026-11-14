//! The two people reads the client's file sharing can't go without: the chat's people by MRI with their addresses
//! (the middle tier's `fetchShortProfile`), and one account by id (Graph's `users/<id>`), which the client checks
//! each recipient against before it shares. The people service proper serves the rest of both.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use serde_json::{Value, json};

use super::{Error, account, graph};
use crate::app::App;
use crate::db::users;

const SPACES: &str = "https://api.spaces.skype.com";
/// The MRIs one lookup names.
const MAX_NAMES: usize = 500;

pub(super) async fn user(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, Error> {
    graph::caller(&app, &headers).await?;
    let user = users::by_id(&app.db, &id)
        .await?
        .filter(|user| !user.disabled)
        .ok_or(Error::Missing)?;
    Ok(Json(json!({
        "id": user.id, "userPrincipalName": user.upn, "mail": user.upn, "displayName": user.display_name,
    })))
}

pub(super) async fn short_profiles(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(names): Json<Vec<String>>,
) -> Result<Json<Value>, Error> {
    account(&app, &headers, SPACES).await?;
    if names.len() > MAX_NAMES {
        return Err(Error::Invalid("Too many accounts in one lookup."));
    }
    let mut profiles = Vec::new();
    for name in &names {
        let Some(id) = name.strip_prefix("8:orgid:") else {
            continue;
        };
        if let Some(user) = users::by_id(&app.db, id).await?.filter(|user| !user.disabled) {
            profiles.push(json!({
                "mri": name, "objectId": user.id, "displayName": user.display_name, "email": user.upn,
                "userPrincipalName": user.upn, "userType": "Member", "tenantName": "", "type": "ADUser",
            }));
        }
    }
    Ok(Json(json!({
        "type": "Microsoft.SkypeSpaces.MiddleTier.Models.IUserIdentity", "value": profiles,
    })))
}
