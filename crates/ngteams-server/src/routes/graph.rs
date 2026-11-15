//! Graph's people under `/graph/v1.0/` (`docs/protocol/graph.md`): `me` (the account the token is for, which the
//! client reads before a write that changes only the account's own state), `users/<id>` (a name, or the record a
//! `$select` asks for) and `users/<id>/photos/96x96/$value` (the picture an avatar is drawn from). Graph's file
//! reads are `crate::media`'s.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use crate::app::App;
use crate::auth::extract::Audience;
use crate::people::repo::{self, Person};
use crate::people::{Caller, unavailable};

/// Graph's audience.
#[derive(Debug)]
pub struct Graph;

impl Audience for Graph {
    const AUDIENCE: &'static str = "https://graph.microsoft.com";
}

/// The one picture size kept (`crate::people::set_picture`).
const PICTURE_SIZE: &str = "96x96";

pub(crate) fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/graph/v1.0/me", get(me))
        .route("/graph/v1.0/users/{id}", get(user))
        .route("/graph/v1.0/users/{id}/photos/{size}/$value", get(photo))
}

/// A refusal, in Graph's error shape.
fn refusal(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error": {"code": code, "message": message}}))).into_response()
}

fn no_user() -> Response {
    refusal(
        StatusCode::NOT_FOUND,
        "Request_ResourceNotFound",
        "The user doesn't exist.",
    )
}

/// A user's record, whole or the properties `select` names (a name it doesn't know is left out).
fn record(origin: &str, person: &Person, select: Option<&str>) -> Value {
    let (user, profile) = (&person.user, &person.profile);
    let whole = [
        ("id", json!(user.id)),
        ("displayName", json!(user.display_name)),
        ("userPrincipalName", json!(user.upn)),
        ("mail", json!(user.upn)),
        ("businessPhones", json!(profile.business_phone.iter().collect::<Vec<_>>())),
        ("givenName", json!(profile.given_name)),
        ("surname", json!(profile.surname)),
        ("jobTitle", json!(profile.job_title)),
        ("mobilePhone", json!(profile.mobile_phone)),
        ("officeLocation", json!(profile.office_location)),
        ("preferredLanguage", Value::Null),
    ];
    let wanted: Option<Vec<&str>> = select.map(|select| {
        select
            .split(',')
            .map(str::trim)
            .filter(|name| whole.iter().any(|(known, _)| known == name))
            .collect()
    });
    let entity = wanted
        .as_ref()
        .map_or_else(String::new, |names| format!("({})", names.join(",")));
    let mut fields = Map::new();
    fields.insert(
        "@odata.context".to_owned(),
        json!(format!("{origin}/graph/v1.0/$metadata#users{entity}/$entity")),
    );
    for (name, value) in whole {
        if wanted.as_ref().is_none_or(|names| names.contains(&name)) {
            fields.insert(name.to_owned(), value);
        }
    }
    Value::Object(fields)
}

async fn answer(app: &App, id: &str, query: &HashMap<String, String>) -> Response {
    match repo::person(&app.db, id).await {
        Ok(Some(person)) => {
            let select = query.get("$select").map(String::as_str);
            Json(record(&app.config.origin(), &person, select)).into_response()
        }
        Ok(None) => no_user(),
        Err(error) => unavailable(&error).into_response(),
    }
}

/// The account a Graph token is for.
async fn me(
    State(app): State<Arc<App>>,
    caller: Caller<Graph>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    answer(&app, &caller.id, &query).await
}

/// An account that wasn't deleted, by its object id.
async fn user(
    State(app): State<Arc<App>>,
    _caller: Caller<Graph>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    answer(&app, &id, &query).await
}

async fn photo(
    State(app): State<Arc<App>>,
    _caller: Caller<Graph>,
    Path((id, size)): Path<(String, String)>,
) -> Response {
    let found = if size == PICTURE_SIZE {
        crate::people::picture(&app.db, app.storage.as_ref(), &id).await
    } else {
        Ok(None)
    };
    match found {
        Ok(Some(bytes)) => (
            [(CONTENT_TYPE, "image/jpeg"), (CACHE_CONTROL, "private, max-age=300")],
            bytes,
        )
            .into_response(),
        Ok(None) => refusal(
            StatusCode::NOT_FOUND,
            "ImageNotFound",
            "The photo wasn't found.",
        ),
        Err(error) => unavailable(&error).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::users::User;
    use crate::people::Profile;

    #[test]
    fn a_record_is_whole_or_what_was_selected() {
        let person = Person {
            user: User {
                id: "i".into(),
                upn: "u@own.test".into(),
                display_name: "U".into(),
                password_hash: String::new(),
                password_version: 0,
                disabled: false,
                created_at: 0,
                updated_at: 0,
                is_admin: false,
                must_change_password: false,
                deleted_at: None,
            },
            profile: Profile {
                business_phone: Some("+1 555 0100".into()),
                ..Profile::default()
            },
        };
        let whole = record("https://o", &person, None);
        assert_eq!(whole.as_object().unwrap().len(), 12);
        assert_eq!(whole["businessPhones"], json!(["+1 555 0100"]));
        assert_eq!(whole["@odata.context"], "https://o/graph/v1.0/$metadata#users/$entity");
        let name = record("https://o", &person, Some("displayName, id,unknown"));
        assert_eq!(
            name,
            json!({"@odata.context": "https://o/graph/v1.0/$metadata#users(displayName,id)/$entity", "id": "i",
                "displayName": "U"})
        );
    }
}
