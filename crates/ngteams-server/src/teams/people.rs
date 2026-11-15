//! The people pickers: the accounts a team could add (the middle tier's `users/searchV2`) and a team's own people
//! (the user profile service's `profilesearch`, for a private channel's box). Both are reads sent as `POST`.

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use super::repo::{self, Person};
use super::{Actor, Answer, Error, Seat, body, mri};
use crate::app::App;
use crate::auth::extract::Spaces;
use crate::db::Db;

/// How many people a picker lists at most.
const LISTED: i64 = 15;

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/v1.0/teams/{team}/users/profilesearch", post(in_team))
}

/// The people as a picker lists them, each with the job title its profile has.
async fn listed(db: &Db, tenant: &str, people: &[Person]) -> Answer<Vec<Value>> {
    let ids: Vec<&str> = people.iter().map(|person| person.id.as_str()).collect();
    let titles = crate::people::job_titles(db, &ids).await?;
    Ok(people
        .iter()
        .map(|person| {
            let mut listed = json!({
                "mri": mri(&person.id), "objectId": person.id, "displayName": person.name, "email": person.upn,
                "userPrincipalName": person.upn, "tenantId": tenant, "type": "ADUser",
            });
            if let Some(title) = titles.get(&person.id) {
                listed["jobTitle"] = json!(title);
            }
            listed
        })
        .collect())
}

/// The keyword a search names: at least one character that isn't a space.
fn keyword(value: &Value) -> Answer<Option<&str>> {
    let keyword = value.as_str().ok_or(Error::BAD_REQUEST)?.trim();
    Ok((!keyword.is_empty()).then_some(keyword))
}

pub(super) async fn addable(
    State(app): State<Arc<App>>,
    actor: Actor<Spaces>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let input = body(input)?;
    let found = match keyword(&input)? {
        Some(keyword) => repo::people(&app.db, keyword, None, LISTED).await?,
        None => Vec::new(),
    };
    Ok(Json(json!({"value": listed(&app.db, &actor.tenant, &found).await?})))
}

async fn in_team(
    State(app): State<Arc<App>>,
    actor: Actor<Spaces>,
    Path(team): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let seat = Seat::of(&app.db, &actor.id, &team).await?;
    let input = body(input)?;
    let found = match keyword(&input["keyword"])? {
        Some(keyword) => repo::people(&app.db, keyword, Some(&seat.team.id), LISTED).await?,
        None => Vec::new(),
    };
    Ok(Json(json!(listed(&app.db, &actor.tenant, &found).await?)))
}
