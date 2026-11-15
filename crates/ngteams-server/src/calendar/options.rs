//! A meeting's options, as its organizer's Controls read and save them (`docs/protocol/calling.md`, "More of a
//! running call"): `beta/meetings/v1/options/<tenant>/<organizer>/<chat>/0/0`. The options belong to a chat: a
//! meeting's, or a group chat's for its calls. The people in the chat read them; the organizer the path names
//! saves them, which for a meeting must be the meeting's own.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use serde_json::{Map, Value, json};

use super::{Answer, Error, Me, body, meeting, meeting_repo};
use crate::app::App;
use crate::chatsvc::service;
use crate::db::{Db, DbError};

/// Each option Controls shows: its name, its type, the values it takes, the first one a new meeting's.
const OPTIONS: [(&str, &str, &[&str]); 4] = [
    (
        "AutoAdmittedUsers",
        "List",
        &[
            "EveryoneInCompany",
            "Everyone",
            "EveryoneInSameAndFederatedCompany",
            "InvitedUsers",
            "OrganizerOnly",
        ],
    ),
    (
        "PresenterOption",
        "List",
        &["Everyone", "EveryoneInCompany", "SpecificPeople", "OrganizerOnly"],
    ),
    ("AllowMeetingChat", "List", &["Enabled", "Disabled", "InMeeting"]),
    (
        "AttendeeRestrictions",
        "BitMask",
        &[
            "UnRestricted",
            "DisableAudio",
            "DisableVideo",
            "DisableAudio,DisableVideo",
        ],
    ),
];

type Target = (String, String, String);

/// The chat a path names, when the caller is in it and the path's tenant and organizer are its own; whether the
/// caller is that organizer.
async fn chat_of(app: &App, me: &Me, (tenant, organizer, chat): &Target) -> Answer<bool> {
    let members = service::members(&app.db, chat).await?;
    let in_chat = |user: &str| members.iter().any(|member| member.eq_ignore_ascii_case(user));
    if !tenant.eq_ignore_ascii_case(app.issuer.tenant()) || !in_chat(&me.id) {
        return Err(Error::NOT_FOUND);
    }
    let named = match meeting::of_chat(&app.db, chat).await? {
        Some(meeting) => meeting.organizer_id.eq_ignore_ascii_case(organizer),
        None => chat.ends_with("@thread.v2") && in_chat(organizer),
    };
    if !named {
        return Err(Error::NOT_FOUND);
    }
    Ok(me.id.eq_ignore_ascii_case(organizer))
}

async fn kept(db: &Db, chat: &str) -> Result<Map<String, Value>, DbError> {
    let body = meeting_repo::options(db, chat).await?;
    Ok(body
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_default())
}

fn entry(name: &str, kind: &str, values: &[&str], set: &Map<String, Value>) -> Value {
    let current = set.get(name).and_then(Value::as_str).unwrap_or(values[0]);
    json!({"name": name, "type": kind, "currentValue": current, "values": values})
}

/// `GET beta/meetings/v1/options/<tenant>/<organizer>/<chat>/0/0`: every option with its value.
pub(super) async fn read(State(app): State<Arc<App>>, me: Me, Path(target): Path<Target>) -> Answer<Json<Value>> {
    chat_of(&app, &me, &target).await?;
    let set = kept(&app.db, &target.2).await?;
    let options: Vec<Value> = OPTIONS
        .iter()
        .map(|(name, kind, values)| entry(name, kind, values, &set))
        .collect();
    Ok(Json(json!({"options": options})))
}

/// `POST` to the same resource with `{"options": [{"name", "currentValue"}]}`: the organizer's change, answered
/// with each changed option as it now is.
pub(super) async fn save(
    State(app): State<Arc<App>>,
    me: Me,
    Path(target): Path<Target>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let input = body(input)?;
    if !chat_of(&app, &me, &target).await? {
        return Err(Error::FORBIDDEN);
    }
    let sent = input["options"].as_array().ok_or(Error::BAD_REQUEST)?;
    let mut changes = Vec::new();
    for option in sent {
        let known = OPTIONS
            .iter()
            .find(|(name, _, _)| option["name"].as_str() == Some(name))
            .ok_or(Error::BAD_REQUEST)?;
        let value = option["currentValue"]
            .as_str()
            .filter(|value| known.2.contains(value))
            .ok_or(Error::BAD_REQUEST)?;
        changes.push((known, value));
    }
    if changes.is_empty() {
        return Err(Error::BAD_REQUEST);
    }
    let chat = &target.2;
    let mut values = kept(&app.db, chat).await?;
    for ((name, _, _), value) in &changes {
        values.insert((*name).to_owned(), json!(value));
    }
    meeting_repo::set_options(&app.db, chat, &Value::Object(values.clone()).to_string()).await?;
    let options: Vec<Value> = changes
        .iter()
        .map(|((name, kind, allowed), _)| entry(name, kind, allowed, &values))
        .collect();
    Ok(Json(json!({"options": options})))
}
