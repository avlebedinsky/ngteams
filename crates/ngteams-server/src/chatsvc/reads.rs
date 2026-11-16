use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde_json::{Value, json};

use super::{Caller, Error, channels, member_of, repo, streams, wire};
use crate::app::App;

type Params = HashMap<String, String>;

pub(super) fn target(path: &Params) -> Result<&str, Error> {
    path.get("chat").map(String::as_str).ok_or(Error::BadRequest)
}

pub(super) async fn thread(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
) -> Result<Json<Value>, Error> {
    let id = target(&path)?;
    streams::no_thread(id)?;
    let (chat, members) = member_of(&app, &caller, id).await?;
    if let Some(place) = channels::place(&app.db, id).await? {
        let roles = channels::roles(&app.db, &place).await?;
        return Ok(Json(channels::thread(
            &app,
            &caller.tenant,
            &chat,
            &place,
            &caller.id,
            &roles,
        )));
    }
    let mut thread = wire::thread(&app, &caller, &chat, &members);
    crate::calendar::describe_thread(&app, &chat.id, &mut thread["properties"]).await?;
    Ok(Json(thread))
}

pub(super) async fn members(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    Query(query): Query<Params>,
) -> Result<Json<Value>, Error> {
    let id = target(&path)?;
    streams::no_thread(id)?;
    let (chat, members) = member_of(&app, &caller, id).await?;
    if let Some(place) = channels::place(&app.db, &chat.id).await? {
        let roles = channels::roles(&app.db, &place).await?;
        let select = query.get("selectMemberRoles").map(String::as_str);
        return Ok(Json(channels::members(&app, &chat, &members, &roles, select)));
    }
    Ok(Json(json!({
        "eTag": format!("\"{}\"", chat.version), "isDeleted": false,
        "members": members.iter().map(|member| wire::member(&app, member, false)).collect::<Vec<_>>(),
        "rosterVersion": 0, "totalMemberCount": members.len(),
    })))
}

pub(super) async fn conversations(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    Query(query): Query<Params>,
) -> Result<Json<Value>, Error> {
    if !path
        .get("owner")
        .is_some_and(|owner| owner == "ME" || *owner == caller.mri())
    {
        return Err(Error::Denied);
    }
    streams::ensure_listed(&app.db, &caller.id).await?;
    let current = repo::watermark(&app.db).await?;
    let since = number(&query, "syncState", 0)?;
    let until = number(&query, "until", current)?;
    let after = number(&query, "after", since)?;
    if until > current || since > until || after < since || after > until {
        return Err(Error::BadRequest);
    }
    let size = number(&query, "pageSize", 200)?.clamp(1, 200);
    let chats = repo::list(&app.db, &caller.id, after, until, size + 1).await?;
    let mut conversations = Vec::new();
    let mut bytes = 2;
    for chat in chats.iter().take(usize::try_from(size).unwrap_or(200)) {
        let mut entry = conversation(&app, &caller, chat).await?;
        let mut length = entry.to_string().len() + 1;
        if length + 2 > wire::MAX_PAGE_BYTES
            && let Some(preview) = entry["lastMessage"].as_object_mut()
        {
            preview.remove("annotationsSummary");
            length = entry.to_string().len() + 1;
        }
        if !conversations.is_empty() && bytes + length > wire::MAX_PAGE_BYTES {
            break;
        }
        bytes += length;
        conversations.push(entry);
    }
    let link = format!("{}/v1/users/ME/conversations", wire::base(&app));
    let mut metadata = json!({"syncState": format!("{link}?syncState={until}"), "forwardLink": ""});
    if conversations.len() < chats.len()
        && let Some(last) = chats.get(conversations.len().saturating_sub(1))
    {
        metadata["forwardLink"] = json!(format!(
            "{link}?syncState={since}&until={until}&after={}&pageSize={size}",
            last.version
        ));
    }
    Ok(Json(json!({"conversations": conversations, "_metadata": metadata})))
}

async fn conversation(app: &App, caller: &Caller, chat: &repo::Chat) -> Result<Value, Error> {
    let mut entry = wire::conversation(app, caller, chat);
    if let Some(place) = channels::place(&app.db, &chat.id).await? {
        let roles = channels::roles(&app.db, &place).await?;
        channels::conversation(&mut entry, caller, chat, &place, &roles);
    }
    crate::calendar::describe_thread(app, &chat.id, &mut entry["threadProperties"]).await?;
    if let Some(message) = super::message_repo::latest(&app.db, &chat.id, &caller.id).await? {
        entry["lastMessage"] = message.value()?;
        if let Some(preview) = entry["lastMessage"].as_object_mut() {
            preview.remove("properties");
        }
        entry["properties"]["isemptyconversation"] = "False".into();
    }
    if let Some((id, version)) = super::message_repo::last_updated(&app.db, &chat.id, &caller.id).await? {
        entry["lastUpdatedMessageId"] = id.into();
        entry["lastUpdatedMessageVersion"] = version.into();
    }
    if let Some(properties) = entry["properties"].as_object_mut() {
        properties.extend(super::property_repo::conversation(&app.db, &chat.id, &caller.id).await?);
    }
    Ok(entry)
}

pub(super) fn number(query: &Params, name: &str, default: i64) -> Result<i64, Error> {
    match query.get(name) {
        None => Ok(default),
        Some(value) => value.parse().ok().filter(|value| *value >= 0).ok_or(Error::BadRequest),
    }
}

pub(super) fn missing_copy(_caller: Caller) -> std::future::Ready<Error> {
    std::future::ready(Error::Missing)
}
