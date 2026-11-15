//! The Power Apps connection routes on `/workflow-connections/`: the account's connections in its environment, and
//! a Microsoft Teams connection made when the card editor opens. The server is its own Teams, so a connection is
//! connected at once: the client then needs no consent page and no on-behalf-of sign-in.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Answer, Caller, Error, PowerApps, repo};
use crate::app::App;

/// The connector the server has connections of.
const TEAMS: &str = "shared_teams";

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/connections", get(list))
        .route("/apis/{api}/connections/{name}", put(create))
}

/// The environment filter every connection route carries (`environment eq '<name>'`).
#[derive(Debug, Deserialize)]
struct Filter {
    #[serde(rename = "$filter")]
    filter: Option<String>,
}

impl Filter {
    fn environment(&self) -> Option<String> {
        let quoted = self.filter.as_deref()?.trim().strip_prefix("environment eq '")?;
        Some(quoted.strip_suffix('\'')?.replace("''", "'"))
    }
}

async fn list(
    State(app): State<Arc<App>>,
    caller: Caller<PowerApps>,
    Query(filter): Query<Filter>,
) -> Answer<Json<Value>> {
    let environment = filter.environment().ok_or(Error::BAD_REQUEST)?;
    caller.check(&environment)?;
    let value: Vec<Value> = repo::connections(&app.db, &caller.id)
        .await?
        .iter()
        .map(|connection| resource(connection, &environment))
        .collect();
    Ok(Json(json!({"value": value})))
}

async fn create(
    State(app): State<Arc<App>>,
    caller: Caller<PowerApps>,
    Path((api, name)): Path<(String, String)>,
    Query(filter): Query<Filter>,
) -> Answer<axum::response::Response> {
    let environment = filter.environment().ok_or(Error::BAD_REQUEST)?;
    caller.check(&environment)?;
    if api != TEAMS {
        return Err(Error::Code(StatusCode::NOT_FOUND, "ApiNotFound"));
    }
    if name.is_empty() || name.len() > 128 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(Error::BAD_REQUEST);
    }
    let wanted = repo::Connection {
        name,
        owner_id: caller.id.clone(),
        api,
        created_at: super::now(),
    };
    let kept = repo::put_connection(&app.db, &wanted).await?;
    if kept.owner_id != caller.id {
        return Err(Error::FORBIDDEN);
    }
    let status = if kept.created_at == wanted.created_at {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(resource(&kept, &environment))).into_response())
}

/// A connection as Power Apps answers it.
fn resource(connection: &repo::Connection, environment: &str) -> Value {
    let api = format!("/providers/Microsoft.PowerApps/apis/{}", connection.api);
    json!({
        "name": connection.name,
        "id": format!("{api}/connections/{}", connection.name),
        "type": "Microsoft.PowerApps/apis/connections",
        "properties": {
            "apiId": api,
            "displayName": "Microsoft Teams",
            "statuses": [{"status": "Connected"}],
            "connectionParameters": {},
            "createdTime": super::iso(connection.created_at),
            "lastModifiedTime": super::iso(connection.created_at),
            "environment": {"name": environment, "id": format!("/providers/Microsoft.PowerApps/environments/{environment}")},
            "createdBy": {"id": connection.owner_id, "type": "User"},
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_names_one_environment() {
        let filter = |text: &str| Filter {
            filter: Some(text.into()),
        };
        assert_eq!(
            filter("environment eq 'Default-x'").environment().as_deref(),
            Some("Default-x")
        );
        assert_eq!(filter("environment eq 'it''s'").environment().as_deref(), Some("it's"));
        assert_eq!(filter("environment eq 'open").environment(), None);
        assert_eq!(filter("name eq 'x'").environment(), None);
        assert_eq!(Filter { filter: None }.environment(), None);
    }
}
