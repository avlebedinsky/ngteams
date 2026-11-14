//! The account's channel sections: CSA's `conversationFolders`, one hierarchy per account. A change names the
//! hierarchy version it was made against and is refused when that isn't the current one, so two clients never
//! overwrite each other's sections.

use serde_json::{Value, json};

use super::{Answer, Error, repo};
use crate::db::Db;

fn fresh(tenant: &str, user: &str) -> Value {
    let id = format!("{tenant}~{user}~Favorites");
    json!({
        "folderHierarchyVersion": 0,
        "conversationFolders": [{
            "id": id, "sortType": "UserDefinedCustomOrder", "name": "Favorites", "folderType": "Favorites",
            "conversationFolderItems": [], "version": 0, "createdTime": 0, "isExpanded": true, "isDeleted": false,
        }],
        "conversationFolderOrder": [id],
        "migratedPinnedToFavorites": true,
        "isPartialResponse": false,
    })
}

/// The account's hierarchy and the version it is kept at (`None`: never changed).
pub(super) async fn read(db: &Db, tenant: &str, user: &str) -> Answer<(Value, Option<i64>)> {
    Ok(match repo::folders(db, user).await? {
        Some((version, body)) => (
            serde_json::from_str(&body).map_err(|_| Error::Database(corrupt()))?,
            Some(version),
        ),
        None => (fresh(tenant, user), None),
    })
}

fn corrupt() -> crate::db::DbError {
    crate::db::DbError::Query(diesel::result::Error::DeserializationError(
        "stored folders are invalid".into(),
    ))
}

const STALE: Error = Error::Code(axum::http::StatusCode::CONFLICT, "FolderHierarchyVersionMismatch");

fn folder<'a>(hierarchy: &'a mut Value, id: &str) -> Answer<&'a mut Value> {
    hierarchy["conversationFolders"]
        .as_array_mut()
        .and_then(|folders| folders.iter_mut().find(|folder| folder["id"] == id))
        .ok_or(Error::BAD_REQUEST)
}

fn items(folder: &mut Value) -> Answer<&mut Vec<Value>> {
    folder["conversationFolderItems"]
        .as_array_mut()
        .ok_or(Error::BAD_REQUEST)
}

fn apply(hierarchy: &mut Value, change: &Value, at: i64, user: &str) -> Answer<()> {
    for action in change["actions"].as_array().ok_or(Error::BAD_REQUEST)? {
        let item = action["itemId"].as_str().filter(|item| !item.is_empty());
        let item = item.ok_or(Error::BAD_REQUEST)?;
        let entry = json!({"conversationId": item, "createdTime": at, "lastUpdatedTime": at,
            "threadType": "topic"});
        match action["action"].as_str() {
            Some("AddFolder") => {
                let name = change["folderName"].as_str().map(str::trim).unwrap_or_default();
                if name.is_empty() || name.chars().count() > 80 {
                    return Err(Error::BAD_REQUEST);
                }
                let id = format!("{user}~{}", uuid::Uuid::new_v4().simple());
                hierarchy["conversationFolders"]
                    .as_array_mut()
                    .ok_or(Error::BAD_REQUEST)?
                    .push(json!({
                        "id": id, "sortType": "UserDefinedCustomOrder", "name": name, "folderType": "UserDefined",
                        "conversationFolderItems": [entry], "version": at, "createdTime": at,
                        "isExpanded": change["isExpanded"].as_bool().unwrap_or(true), "isDeleted": false,
                    }));
                hierarchy["conversationFolderOrder"]
                    .as_array_mut()
                    .ok_or(Error::BAD_REQUEST)?
                    .push(json!(id));
            }
            Some("AddItem") => {
                let folder = folder(hierarchy, action["folderId"].as_str().unwrap_or_default())?;
                let items = items(folder)?;
                items.retain(|kept| kept["conversationId"] != item);
                let index = action["itemIndex"]
                    .as_u64()
                    .and_then(|index| usize::try_from(index).ok())
                    .map_or(items.len(), |index| index.min(items.len()));
                items.insert(index, entry);
                folder["version"] = json!(at);
            }
            Some("RemoveItem") => {
                let folder = folder(hierarchy, action["folderId"].as_str().unwrap_or_default())?;
                items(folder)?.retain(|kept| kept["conversationId"] != item);
                folder["version"] = json!(at);
            }
            _ => return Err(Error::BAD_REQUEST),
        }
    }
    Ok(())
}

/// Applies a sections change (`actions`, or `folderIds` with `isExpanded`) made against the current hierarchy.
pub(super) async fn change(db: &Db, tenant: &str, user: &str, change: &Value, expanded: bool) -> Answer<Value> {
    let (mut hierarchy, kept) = read(db, tenant, user).await?;
    if change["folderHierarchyVersion"].as_i64() != Some(kept.unwrap_or(0)) {
        return Err(STALE);
    }
    let at = crate::chatsvc::now().max(kept.unwrap_or(0) + 1);
    if expanded {
        let open = change["isExpanded"].as_bool().ok_or(Error::BAD_REQUEST)?;
        for id in change["folderIds"].as_array().ok_or(Error::BAD_REQUEST)? {
            folder(&mut hierarchy, id.as_str().unwrap_or_default())?["isExpanded"] = json!(open);
        }
    } else {
        apply(&mut hierarchy, change, at, user)?;
    }
    hierarchy["folderHierarchyVersion"] = json!(at);
    if !repo::keep_folders(db, user, kept, at, &hierarchy.to_string()).await? {
        return Err(STALE);
    }
    Ok(hierarchy)
}

/// Takes a thread that is gone for the account out of its sections; a failure leaves a stale item the client skips.
pub(super) async fn forget(db: &Db, tenant: &str, user: &str, thread: &str) {
    let Ok((mut hierarchy, Some(kept))) = read(db, tenant, user).await else {
        return;
    };
    let mut changed = false;
    for folder in hierarchy["conversationFolders"].as_array_mut().into_iter().flatten() {
        if let Some(items) = folder["conversationFolderItems"].as_array_mut() {
            let before = items.len();
            items.retain(|kept| kept["conversationId"] != thread);
            changed |= items.len() != before;
        }
    }
    if changed {
        let at = crate::chatsvc::now().max(kept + 1);
        hierarchy["folderHierarchyVersion"] = json!(at);
        let _ = repo::keep_folders(db, user, Some(kept), at, &hierarchy.to_string()).await;
    }
}
