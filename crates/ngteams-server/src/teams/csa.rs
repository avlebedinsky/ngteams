//! CSA: the account's teams with their channels and sections (`api/v3/teams/users/me`), the sections' changes, and
//! a team's tags.

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};

use super::repo::{self, Tag};
use super::{Actor, Answer, Csa, Error, Seat, body, folders, object_id, text, wire};
use crate::app::App;

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v3/teams/users/me", get(teams))
        .route(
            "/api/v1/teams/users/me/conversationFolders",
            get(sections).post(change_sections),
        )
        .route(
            "/api/v1/teams/users/me/conversationFolders/batchUpdateExpandedState",
            patch(expand_sections),
        )
        .route("/api/v1/teams/{team}/tagCards", get(tags))
        .route("/api/v1/teams/{team}/tagSettings", put(tag_settings))
        .route("/api/v1/teams/{team}/tags", post(create_tag))
        .route("/api/v1/teams/{team}/tags/{tag}", patch(edit_tag).delete(delete_tag))
}

async fn teams(State(app): State<Arc<App>>, actor: Actor<Csa>) -> Answer<Json<Value>> {
    let seats = repo::memberships(&app.db, &actor.id).await?;
    let team_ids: Vec<String> = seats.iter().map(|(team, _)| team.id.clone()).collect();
    let channels = repo::channels_of(&app.db, &team_ids).await?;
    let ids: Vec<String> = channels.iter().map(|channel| channel.id.clone()).collect();
    let joined = repo::joined(&app.db, &actor.id, &ids).await?;
    let own = repo::own_properties(&app.db, &actor.id, &ids).await?;
    let roles = repo::roles_of(&app.db, &ids).await?;
    let counts = repo::head_counts(&app.db, &team_ids).await?;
    let host = app.config.public_origin.host_str().unwrap_or("localhost");
    let listed: Vec<Value> = seats
        .iter()
        .map(|(team, member)| {
            let count = |owners: bool| -> i64 {
                counts
                    .iter()
                    .filter(|(id, owner, _)| *id == team.id && (!owners || *owner))
                    .map(|(_, _, count)| count)
                    .sum()
            };
            wire::View {
                tenant: &actor.tenant,
                host,
                team,
                member,
                channels: channels
                    .iter()
                    .filter(|channel| channel.team_id == team.id && joined.contains(&channel.id))
                    .collect(),
                own: &own,
                roles: &roles,
                user: &actor.id,
                people: count(false),
                owners: count(true),
            }
            .entry()
        })
        .collect();
    let (hierarchy, _) = folders::read(&app.db, &actor.tenant, &actor.id).await?;
    let now = crate::chatsvc::now();
    Ok(Json(json!({
        "teams": listed, "chats": [], "users": [], "privateFeeds": [], "engageCommunities": [],
        "conversationFolders": hierarchy,
        "metadata": {"syncToken": now.to_string(), "forwardSyncToken": null, "isPartialData": false,
            "hasMoreChats": false, "coldBootTime": now},
    })))
}

async fn sections(State(app): State<Arc<App>>, actor: Actor<Csa>) -> Answer<Json<Value>> {
    Ok(Json(folders::read(&app.db, &actor.tenant, &actor.id).await?.0))
}

async fn change_sections(
    State(app): State<Arc<App>>,
    actor: Actor<Csa>,
    sent: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let change = body(sent)?;
    Ok(Json(
        folders::change(&app.db, &actor.tenant, &actor.id, &change, false).await?,
    ))
}

async fn expand_sections(
    State(app): State<Arc<App>>,
    actor: Actor<Csa>,
    sent: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let change = body(sent)?;
    Ok(Json(
        folders::change(&app.db, &actor.tenant, &actor.id, &change, true).await?,
    ))
}

fn tag_settings_of(seat: &Seat) -> Value {
    serde_json::from_str(&seat.team.tag_settings).unwrap_or_else(|_| json!({}))
}

/// Whether the caller may change the team's tags: its owners, or everyone when the tag settings say so.
fn may_tag(seat: &Seat) -> Answer<()> {
    seat.active()?;
    if seat.member.owner || tag_settings_of(seat)["onlyOwnersCanUpdate"].as_bool() == Some(false) {
        Ok(())
    } else {
        Err(Error::FORBIDDEN)
    }
}

fn card(tag: &Tag) -> Value {
    let members: Vec<String> = serde_json::from_str(&tag.members).unwrap_or_default();
    json!({
        "id": tag.id, "tagName": tag.name, "tagDescription": tag.description, "tagType": "standard",
        "memberCount": members.len(), "members": members, "version": tag.version,
    })
}

async fn tags(State(app): State<Arc<App>>, actor: Actor<Csa>, Path(team): Path<String>) -> Answer<Json<Value>> {
    let seat = Seat::of(&app.db, &actor.id, &team).await?;
    let tags = repo::tags(&app.db, &team).await?;
    Ok(Json(json!({
        "tagCards": tags.iter().map(card).collect::<Vec<_>>(),
        "teamTagSettings": tag_settings_of(&seat),
    })))
}

async fn tag_settings(
    State(app): State<Arc<App>>,
    actor: Actor<Csa>,
    Path(team): Path<String>,
    sent: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let seat = Seat::of(&app.db, &actor.id, &team).await?;
    seat.owner()?;
    seat.active()?;
    let sent = body(sent)?;
    let flag = |key: &str| sent[key].as_bool().ok_or(Error::BAD_REQUEST);
    let settings = json!({
        "onlyOwnersCanUpdate": flag("onlyOwnersCanUpdate")?,
        "disallowedSources": [],
        "enableAutomaticTags": flag("enableAutomaticTags")?,
    });
    let edit = repo::TeamEdit {
        tag_settings: Some(&settings.to_string()),
        ..repo::TeamEdit::NONE
    };
    repo::edit_team(&app.db, &team, &edit).await?;
    Ok(Json(settings))
}

async fn create_tag(
    State(app): State<Arc<App>>,
    actor: Actor<Csa>,
    Path(team): Path<String>,
    sent: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let seat = Seat::of(&app.db, &actor.id, &team).await?;
    may_tag(&seat)?;
    let sent = body(sent)?;
    let name = text(&sent["tagName"], 40, true)?;
    let people = repo::roster(&app.db, &team).await?;
    let members = sent["memberIds"]
        .as_array()
        .ok_or(Error::BAD_REQUEST)?
        .iter()
        .map(|member| {
            let mri = member.as_str().ok_or(Error::BAD_REQUEST)?;
            let id = object_id(mri).ok_or(Error::BAD_REQUEST)?;
            if people.iter().any(|person| person.user_id == id) {
                Ok(mri.to_owned())
            } else {
                Err(Error::BAD_REQUEST)
            }
        })
        .collect::<Answer<Vec<String>>>()?;
    let existing = repo::tags(&app.db, &team).await?;
    if existing
        .iter()
        .any(|tag| tag.name.to_lowercase() == name.to_lowercase())
    {
        return Err(Error::Code(StatusCode::CONFLICT, "TagNameAlreadyExists"));
    }
    let tag = Tag {
        id: uuid::Uuid::new_v4().to_string(),
        team_id: team,
        name,
        description: text(&sent["tagDescription"], 1024, false)?,
        members: json!(members).to_string(),
        version: 1,
    };
    repo::insert_tag(&app.db, &tag).await?;
    Ok(Json(card(&tag)))
}

async fn edit_tag(
    State(app): State<Arc<App>>,
    actor: Actor<Csa>,
    Path((team, id)): Path<(String, String)>,
    headers: HeaderMap,
    sent: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let seat = Seat::of(&app.db, &actor.id, &team).await?;
    may_tag(&seat)?;
    let sent = body(sent)?;
    let mut tag = repo::tag(&app.db, &team, &id).await?.ok_or(Error::NOT_FOUND)?;
    let read: Option<i64> = headers
        .get("if-match")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim_matches('"').parse().ok());
    let name = text(&sent["tagName"], 40, true)?;
    let description = text(&sent["tagDescription"], 1024, false)?;
    if read != Some(tag.version) || !repo::edit_tag(&app.db, &tag, &name, &description).await? {
        return Err(Error::Code(StatusCode::PRECONDITION_FAILED, "TagVersionMismatch"));
    }
    tag.name = name;
    tag.description = description;
    tag.version += 1;
    Ok(Json(card(&tag)))
}

async fn delete_tag(
    State(app): State<Arc<App>>,
    actor: Actor<Csa>,
    Path((team, id)): Path<(String, String)>,
) -> Answer {
    let seat = Seat::of(&app.db, &actor.id, &team).await?;
    may_tag(&seat)?;
    if repo::delete_tag(&app.db, &team, &id).await? {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Err(Error::NOT_FOUND)
    }
}
