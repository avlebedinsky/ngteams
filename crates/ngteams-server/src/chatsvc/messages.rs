use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State, rejection::JsonRejection};
use axum::http::{StatusCode, header::LOCATION};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};

use super::{Caller, Error, channels, events, member_of, message_repo, reader_of, reads, repo, rich, streams, wire};
use crate::app::App;
use crate::db::DbError;

type Params = HashMap<String, String>;

/// The properties a post or an edit sets from what it sent.
pub(super) const MESSAGE_PROPERTIES: [&str; 6] = ["mentions", "cards", "links", "files", "formatVariant", "qtdMsgs"];

/// What the service adds beside them: whether every quote names a message of the chat ([`draft`]).
pub(super) const VALID_QUOTES: &str = "hasValidMsgReferences";

pub(super) async fn post(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, Error> {
    let (chat, post) = channels::split(reads::target(&path)?)?;
    match chat {
        streams::NOTES => streams::ensure(&app.db, chat, &caller.id).await?,
        streams::FEED | streams::MENTIONS | streams::CALL_LOG => return Err(Error::Denied),
        _ => crate::bots::open_chat(&app, chat, &caller.id).await?,
    }
    let (found, _) = member_of(&app, &caller, chat).await?;
    let Json(body) = body.map_err(|_| Error::BadRequest)?;
    let place = channels::place(&app.db, chat).await?;
    if post.is_some() && place.is_none() {
        return Err(Error::BadRequest);
    }
    if body["messagetype"].as_str() == Some("Control/Typing") {
        return typing(&app, &caller, chat, &found.id, &body).await;
    }
    let mut draft = draft(&app, &caller, chat, &found.id, &body).await?;
    if let Some(place) = &place {
        if !channels::writable(place) {
            return Err(Error::Denied);
        }
        if let Some(post) = post
            && !message_repo::is_post(&app.db, chat, post).await?
        {
            return Err(Error::BadRequest);
        }
        draft.root_id = Some(post.unwrap_or(0));
    }
    let posted = message_repo::insert(&app.db, &draft, &wire::base(&app))
        .await?
        .ok_or(Error::Denied)?;
    let row = &posted.row;
    wake(&app, &posted).await;
    if posted.inserted {
        events::tell(&app, "NewMessage", &row.value()?, &posted.readers, &posted.told).await;
    }
    Ok((
        StatusCode::CREATED,
        [(
            LOCATION,
            format!(
                "{}/v1/users/ME/conversations/{chat}/messages/{}",
                wire::base(&app),
                row.id
            ),
        )],
        Json(json!({"OriginalArrivalTime": row.id})),
    )
        .into_response())
}

/// Wakes the senders a post queued something for, after its commit.
async fn wake(app: &App, posted: &message_repo::Posted) {
    if posted.queued {
        crate::bots::wake(app).await;
    }
    if posted.hooked {
        crate::outgoing::wake(app).await;
    }
}

/// The message a post or an edit `sent` to `chat` (`chat_id` its row) stores, its quotes (`qtdMsgs`) checked against
/// the messages of the chat the writer may read.
pub(super) async fn draft(
    app: &App,
    caller: &Caller,
    chat: &str,
    chat_id: &str,
    sent: &Value,
) -> Result<message_repo::Message, Error> {
    let kind = sent["messagetype"].as_str().ok_or(Error::BadRequest)?;
    if !(kind == "Text" || kind == "RichText" || kind.starts_with("RichText/")) {
        return Err(Error::BadRequest);
    }
    let content = sent["content"].as_str().ok_or(Error::BadRequest)?;
    if sent
        .get("from")
        .is_some_and(|from| from.as_str() != Some(caller.mri().as_str()))
    {
        return Err(Error::Denied);
    }
    let mut properties = Map::new();
    if let Some(sent) = sent.get("properties") {
        let sent = sent.as_object().ok_or(Error::BadRequest)?;
        for key in MESSAGE_PROPERTIES {
            if let Some(value) = sent.get(key) {
                properties.insert(key.into(), value.clone());
            }
        }
    }
    if let Some(mentions) = properties.get("mentions") {
        let mentions = rich::mentions(mentions).map_err(|_| Error::BadRequest)?;
        properties.insert("mentions".into(), mentions);
    }
    let quotes = properties
        .get("qtdMsgs")
        .map(rich::Quotes::parse)
        .transpose()
        .map_err(|_| Error::BadRequest)?;
    let mut body = json!({
        "type": "Message", "conversationid": chat,
        "conversationLink": format!("{}/v1/users/ME/conversations/{chat}", wire::base(app)),
        "from": format!("{}/v1/users/ME/contacts/{}", wire::base(app), caller.mri()),
        "fromDisplayNameInToken": caller.name, "imdisplayname": caller.name,
        "messagetype": kind, "content": if kind == "Text" { content.to_owned() } else { rich::content(content) }, "properties": properties,
    });
    for key in ["contenttype", "clientmessageid", "amsreferences"] {
        if let Some(value) = sent.get(key) {
            body[key] = value.clone();
        }
    }
    let client_id = match sent.get("clientmessageid") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or(Error::BadRequest)?
                .to_owned(),
        ),
    };
    if let Some(quotes) = quotes {
        let valid = message_repo::readable(&app.db, chat_id, &caller.id, &quotes.ids()).await?;
        let (quotes, all) = quotes.marked(&valid);
        body["properties"]["qtdMsgs"] = quotes;
        body["properties"][VALID_QUOTES] = all.into();
    }
    Ok(message_repo::Message {
        chat_id: chat_id.into(),
        id: 0,
        owner_id: caller.id.clone(),
        client_id,
        version: 0,
        body: body.to_string(),
        root_id: None,
    })
}

pub(super) async fn page(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    Query(query): Query<Params>,
) -> Result<Json<Value>, Error> {
    let target = reads::target(&path)?;
    let (chat, root) = channels::split(target)?;
    let found = reader_of(&app, &caller, chat).await?;
    let current = repo::watermark(&app.db).await?;
    let until = reads::number(&query, "until", current)?;
    let since = reads::number(&query, "syncState", 0)?;
    let before = reads::number(&query, "before", i64::MAX)?;
    if until > current || since > until {
        return Err(Error::BadRequest);
    }
    let size = reads::number(&query, "pageSize", 200)?.clamp(1, 200);
    let incremental = query.contains_key("syncState");
    let rows = message_repo::page(
        &app.db,
        &found.id,
        &caller.id,
        message_repo::Window {
            since,
            until,
            before,
            size: size + 1,
            incremental,
            root,
        },
    )
    .await?;
    let (messages, more) = bounded_messages(&rows, size)?;
    let link = format!("{}/v1/users/ME/conversations/{target}/messages", wire::base(&app));
    let mut metadata = json!({"syncState": format!("{link}?syncState={until}"),
        "lastCompleteSegmentStartTime": since, "lastCompleteSegmentEndTime": until});
    if more && let Some(last) = rows.get(messages.len().saturating_sub(1)) {
        metadata["lastCompleteSegmentStartTime"] = if incremental {
            last.id.max(since.saturating_add(1))
        } else {
            last.id
        }
        .into();
        if incremental {
            metadata["backwardLink"] = json!(format!(
                "{link}?syncState={since}&until={until}&before={}&pageSize={size}",
                last.id
            ));
        } else {
            metadata["backwardLink"] = json!(format!("{link}?until={until}&before={}&pageSize={size}", last.id));
        }
    }
    Ok(Json(
        json!({"messages": messages, "tenantId": caller.tenant, "_metadata": metadata}),
    ))
}

pub(super) async fn around(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    Query(query): Query<Params>,
) -> Result<Json<Value>, Error> {
    let chat = reads::target(&path)?;
    let found = reader_of(&app, &caller, chat).await?;
    let at = reads::number(&path, "time", 0)?;
    let size = reads::number(&query, "pageSize", 200)?.clamp(1, 200);
    if query.get("direction").map(String::as_str) != Some("BIDIRECTIONAL") {
        return Err(Error::BadRequest);
    }
    let rows = message_repo::around(&app.db, &found.id, &caller.id, at, size).await?;
    let (messages, more) = bounded_messages(&rows, size)?;
    let mut metadata = json!({"lastCompleteSegmentStartTime": 0, "lastCompleteSegmentEndTime": 0});
    if more && let Some(last) = rows.get(messages.len().saturating_sub(1)) {
        metadata["backwardLink"] = json!(format!(
            "{}/v1/users/ME/conversations/{chat}/messages?before={}&pageSize={size}",
            wire::base(&app),
            last.id
        ));
    }
    Ok(Json(
        json!({"messages": messages, "tenantId": caller.tenant, "_metadata": metadata}),
    ))
}

fn bounded_messages(rows: &[message_repo::Message], size: i64) -> Result<(Vec<Value>, bool), DbError> {
    let mut messages = Vec::new();
    let mut bytes = 2;
    for row in rows.iter().take(usize::try_from(size).unwrap_or(200)) {
        let length = row.body.len() + 1;
        if !messages.is_empty() && bytes + length > wire::MAX_PAGE_BYTES {
            break;
        }
        bytes += length;
        messages.push(row.value()?);
    }
    let more = messages.len() < rows.len();
    Ok((messages, more))
}

/// Typing is told to the people in the thread at its own moment of the clock, not to a roster read before it.
async fn typing(app: &App, caller: &Caller, chat: &str, stored: &str, body: &Value) -> Result<Response, Error> {
    if body["content"].as_str() != Some("") {
        return Err(Error::BadRequest);
    }
    let (at, people) = repo::tick(&app.db, stored, &caller.id).await?.ok_or(Error::Denied)?;
    let resource = json!({
        "type": "Message", "id": at.to_string(), "version": at.to_string(), "clientmessageid": at.to_string(),
        "conversationid": chat, "conversationLink": format!("{}/v1/users/ME/conversations/{chat}", wire::base(app)),
        "from": format!("{}/v1/users/ME/contacts/{}", wire::base(app), caller.mri()), "imdisplayname": "",
        "messagetype": "Control/Typing", "contenttype": "Application/Message", "content": "",
        "properties": {"importance": "", "subject": ""},
    });
    events::others(app, caller, &people, "NewMessage", &resource).await;
    crate::bots::typing(app, stored, &caller.id, at).await;
    Ok((StatusCode::CREATED, Json(json!({"OriginalArrivalTime": at}))).into_response())
}
