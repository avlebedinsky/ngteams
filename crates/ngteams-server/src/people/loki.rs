//! Loki, the person card's service, under `/loki/` (`docs/protocol/loki.md`): its configuration, which names this
//! same base as the account's regional one, and the five reads of a person. Everything comes from the directory;
//! whom a person works with is only what the reader may already see: the other people of the threads both are in.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use super::repo::{self, Person};
use super::{Caller, unavailable};
use crate::app::App;
use crate::auth::extract::Audience;
use crate::db::DbError;
use crate::db::users::User;

/// Loki's audience.
#[derive(Debug)]
pub(crate) struct Loki;

impl Audience for Loki {
    const AUDIENCE: &'static str = "https://loki.delve.office.com";
}

/// The managers one card lists, nearest first.
const MAX_MANAGERS: usize = 8;
/// The direct reports one card lists.
const MAX_DIRECTS: usize = 256;
/// The people a working-with read lists: what Teams' card asks for, and the most it may ask for.
const WORKING_WITH: usize = 12;
const MAX_WORKING_WITH: usize = 50;
/// A time Loki gives for "no time".
const NO_TIME: &str = "0001-01-01T00:00:00";

pub(super) fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/loki/api/v1/livepersonacard/configuration", get(configuration))
        .route("/loki/api/v2/person", get(person))
        .route("/loki/api/v1/organization", get(organization))
        .route("/loki/api/v1/workingwith", get(working_with))
        .route("/loki/api/v1/schedule", get(schedule))
        .route("/loki/api/v1/oofstatus", get(out_of_office))
}

/// A refusal, in Loki's error shape.
#[derive(Debug)]
enum Refusal {
    /// Loki answers no request that doesn't name its client.
    ClientTypeMissing,
    PersonNotFound,
    Unavailable,
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::ClientTypeMissing => (StatusCode::BAD_REQUEST, "ClientTypeMissing"),
            Self::PersonNotFound => (StatusCode::NOT_FOUND, "PersonNotFound"),
            Self::Unavailable => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        (status, Json(json!({"error": {"code": code}}))).into_response()
    }
}

impl From<DbError> for Refusal {
    fn from(error: DbError) -> Self {
        unavailable(&error);
        Self::Unavailable
    }
}

fn client_named(headers: &HeaderMap) -> Result<(), Refusal> {
    if headers.contains_key("x-clienttype") {
        Ok(())
    } else {
        Err(Refusal::ClientTypeMissing)
    }
}

/// The person a read's query names (`aadObjectId`, and the same account in `teamsMri` when that is there), when
/// they can sign in.
async fn persona(app: &App, headers: &HeaderMap, query: &HashMap<String, String>) -> Result<Person, Refusal> {
    client_named(headers)?;
    let id = query.get("aadObjectId").ok_or(Refusal::PersonNotFound)?;
    if query.get("teamsMri").is_some_and(|mri| *mri != format!("8:orgid:{id}")) {
        return Err(Refusal::PersonNotFound);
    }
    repo::person(&app.db, id)
        .await?
        .filter(|person| !person.user.disabled)
        .ok_or(Refusal::PersonNotFound)
}

async fn configuration(
    State(app): State<Arc<App>>,
    caller: Caller<Loki>,
    headers: HeaderMap,
) -> Result<Json<Value>, Refusal> {
    client_named(&headers)?;
    Ok(Json(json!({
        "LokiUrl": format!("{}/loki/", app.config.origin()),
        "AadObjectId": caller.id,
        "TenantId": caller.tenant,
        "UserPrincipalName": caller.user.upn,
        "DisplayName": caller.user.display_name,
    })))
}

async fn person(
    State(app): State<Arc<App>>,
    _caller: Caller<Loki>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, Refusal> {
    let Person { user, profile } = persona(&app, &headers, &query).await?;
    let source = "Organisation";
    Ok(Json(json!({"person": {
        "names": [{"value": {
            "displayName": user.display_name, "givenName": profile.given_name, "surname": profile.surname,
        }, "source": source}],
        "emailAddresses": [{"value": {"name": user.upn, "address": user.upn, "type": "Main"}, "source": source}],
        "imAddresses": [{"value": {"displayText": user.upn, "url": format!("sip:{}", user.upn)}, "source": source}],
        "workDetails": [{"value": {}, "source": source}],
        "enhancedWorkDetails": {},
        "userPrincipalName": user.upn,
        "aadObjectId": user.id,
        "isContact": false,
        "userType": "Member",
        "isAgenticUser": false,
    }})))
}

/// Someone a card names, with a working-with entry's keys.
fn entry(user: &User) -> Value {
    json!({
        "aadObjectId": user.id, "fullName": user.display_name, "email": user.upn, "userPrincipalName": user.upn,
        "profilePictureUrl": null,
    })
}

/// The managers above the account `id`, nearest first from `manager`, until the chain ends, loops, reaches one who
/// can't sign in, or is [`MAX_MANAGERS`] long.
async fn managers(app: &App, id: &str, manager: Option<String>) -> Result<Vec<Value>, DbError> {
    let mut seen = vec![id.to_owned()];
    let mut above = Vec::new();
    let mut next = manager;
    while let Some(id) = next.take() {
        if seen.contains(&id) || above.len() >= MAX_MANAGERS {
            break;
        }
        let Some(manager) = repo::person(&app.db, &id).await?.filter(|person| !person.user.disabled) else {
            break;
        };
        above.push(entry(&manager.user));
        next = manager.profile.manager_id;
        seen.push(id);
    }
    Ok(above)
}

async fn organization(
    State(app): State<Arc<App>>,
    _caller: Caller<Loki>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, Refusal> {
    let Person { user, profile } = persona(&app, &headers, &query).await?;
    let managers = managers(&app, &user.id, profile.manager_id).await?;
    let directs = repo::directs(&app.db, &user.id, MAX_DIRECTS).await?;
    Ok(Json(json!({
        "managers": managers,
        "directs": directs.iter().map(entry).collect::<Vec<_>>(),
        "dottedLineManagers": [],
    })))
}

async fn working_with(
    State(app): State<Arc<App>>,
    caller: Caller<Loki>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, Refusal> {
    let person = persona(&app, &headers, &query).await?;
    let limit = query
        .get("limit")
        .and_then(|limit| limit.parse::<usize>().ok())
        .map_or(WORKING_WITH, |limit| limit.clamp(1, MAX_WORKING_WITH));
    let with = repo::works_with(&app.db, &caller.id, &person.user.id, limit).await?;
    Ok(Json(json!({"value": with.iter().map(entry).collect::<Vec<_>>()})))
}

/// No calendar says when someone is free: the status is unknown, or out of office while their automatic replies
/// are on, and no working hours are named.
async fn schedule(
    State(app): State<Arc<App>>,
    _caller: Caller<Loki>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, Refusal> {
    let person = persona(&app, &headers, &query).await?;
    let status = if person.profile.out_of_office { "Oof" } else { "Unknown" };
    Ok(Json(json!({
        "nextAvailability": {"utcDateTime": NO_TIME, "currentStatus": status, "nextStatus": status},
    })))
}

async fn out_of_office(
    State(app): State<Arc<App>>,
    _caller: Caller<Loki>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, Refusal> {
    let person = persona(&app, &headers, &query).await?;
    let state = if person.profile.out_of_office {
        "AlwaysEnabled"
    } else {
        "Disabled"
    };
    Ok(Json(json!({
        "outOfOfficeState": state, "externalAudience": "Unknown", "emailAddress": person.user.upn,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_answers_with_its_status() {
        let status = |refusal: Refusal| refusal.into_response().status();
        assert_eq!(status(Refusal::ClientTypeMissing), StatusCode::BAD_REQUEST);
        assert_eq!(status(Refusal::PersonNotFound), StatusCode::NOT_FOUND);
        assert_eq!(status(DbError::Conflict.into()), StatusCode::SERVICE_UNAVAILABLE);
    }
}
