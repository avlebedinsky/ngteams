//! Graph, as far as the served features need it: `GET /graph/v1.0/me`, the account the token is for. The client
//! reads it before a write that changes only the account's own state, to check that it writes as itself.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::app::App;
use crate::auth::extract::{Audience, Bearer};
use crate::db::users;

/// Graph's audience.
#[derive(Debug)]
pub struct Graph;

impl Audience for Graph {
    const AUDIENCE: &'static str = "https://graph.microsoft.com";
}

/// The account a Graph token is for.
pub async fn me(State(app): State<Arc<App>>, bearer: Bearer<Graph>) -> Response {
    match users::by_id(&app.db, &bearer.claims.oid).await {
        Ok(Some(user)) if !user.disabled => Json(json!({
            "@odata.context": format!("{}/graph/v1.0/$metadata#users/$entity", app.config.origin()),
            "id": user.id, "userPrincipalName": user.upn, "displayName": user.display_name, "mail": user.upn,
        }))
        .into_response(),
        Ok(_) => StatusCode::FORBIDDEN.into_response(),
        Err(error) => {
            tracing::error!(%error, "graph database failure");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}
