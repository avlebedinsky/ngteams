//! The registrar: `POST /registrar/prod/V2/registrations` (`docs/protocol/trouter.md`, "Registration and its TTL").
//! A stack registers its own Trouter endpoint's push URL for a time (at most an hour, as the client asks); the
//! account is the token's. The registration is a row every node reads, and the account's sockets are told at once,
//! so the endpoint's deliveries start on whichever node holds its socket.
//!
//! An account registers only its own endpoints: the push URL must be the one this server gave the endpoint of that
//! `registrationId` (the socket's `epid`) on that stack.

mod repo;

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::Value;

pub use repo::{Registration, purge_expired, until};

use crate::app::App;
use crate::auth::extract::{Bearer, Ic3};
use crate::db::users;
use crate::trouter::{self, Control, Stack};

/// The longest a registration lasts, in seconds: what the client asks for.
pub const MAX_TTL: u64 = 3600;
/// The registrations an account may hold at a time.
pub const MAX_REGISTRATIONS: i64 = 64;
/// The largest registration the registrar reads.
const BODY_LIMIT: usize = 16 * 1024;

pub(crate) fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/registrar/prod/V2/registrations", post(register))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// What a registration's body names, when it is a stack's registration of a Trouter endpoint.
struct Asked<'a> {
    stack: Stack,
    registration_id: &'a str,
    path: &'a str,
    ttl: u64,
}

fn asked(body: &Value) -> Option<Asked<'_>> {
    let description = &body["clientDescription"];
    let app_id = description["appId"].as_str()?;
    let stack = Stack::of_app(app_id)?;
    description["templateKey"]
        .as_str()?
        .strip_prefix(app_id)?
        .strip_prefix('_')
        .filter(|version| !version.is_empty())?;
    let registration_id = body["registrationId"].as_str().filter(|id| trouter::is_guid(id))?;
    let transport = body["transports"]["TROUTER"]
        .as_array()
        .filter(|transports| transports.len() == 1)?
        .first()?;
    Some(Asked {
        stack,
        registration_id,
        path: transport["path"].as_str()?,
        ttl: transport["ttl"].as_u64().filter(|ttl| *ttl > 0)?,
    })
}

async fn register(
    State(app): State<Arc<App>>,
    bearer: Bearer<Ic3>,
    body: Result<Json<Value>, JsonRejection>,
) -> StatusCode {
    let Ok(Json(body)) = body else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(asked) = asked(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let oid = &bearer.claims.oid;
    let endpoint = trouter::endpoint_id(oid, asked.registration_id, asked.stack);
    if asked.path != trouter::push_url(&app.config.origin(), &endpoint) {
        return StatusCode::FORBIDDEN;
    }
    match users::by_id(&app.db, oid).await {
        Ok(Some(user)) if !user.disabled => {}
        Ok(_) => return StatusCode::FORBIDDEN,
        Err(error) => return unavailable(&error),
    }
    let until = crate::db::now() + i64::try_from(asked.ttl.min(MAX_TTL)).unwrap_or(0);
    let registration = Registration {
        user_id: oid.clone(),
        registration_id: asked.registration_id.to_owned(),
        app_id: asked.stack.app_id().to_owned(),
        path: asked.path.to_owned(),
        expires_at: until,
    };
    match repo::put(&app.db, &registration, MAX_REGISTRATIONS).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::TOO_MANY_REQUESTS,
        Err(error) => return unavailable(&error),
    }
    let told = trouter::tell(
        &app,
        &format!("8:orgid:{oid}"),
        &Control::Registered { endpoint, until },
    )
    .await;
    if let Err(error) = told {
        // taken all the same: the socket reads it at its next connect
        tracing::warn!(%error, "a registration's socket wasn't told");
    }
    StatusCode::ACCEPTED
}

fn unavailable(error: &crate::db::DbError) -> StatusCode {
    tracing::error!(%error, "registrar database failure");
    StatusCode::SERVICE_UNAVAILABLE
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn body() -> Value {
        json!({
            "clientDescription": {"appId": "TeamsCDLWebWorker", "templateKey": "TeamsCDLWebWorker_2.6"},
            "registrationId": "00000000-0000-4000-8000-000000000001",
            "transports": {"TROUTER": [{"context": "", "path": "https://x/trouter/v4/f/E/", "ttl": 3600}]},
        })
    }

    #[test]
    fn only_a_stacks_registration_of_one_trouter_endpoint_is_read() {
        let read = body();
        let good = asked(&read).unwrap();
        assert_eq!(good.stack, Stack::Messaging);
        assert_eq!(good.ttl, 3600);
        let broken: [fn(&mut Value); 8] = [
            |body| body["clientDescription"]["appId"] = json!("Other"),
            |body| body["clientDescription"]["templateKey"] = json!("TeamsCDLWebWorker_"),
            |body| body["clientDescription"]["templateKey"] = json!("SkypeSpacesWeb_2.6"),
            |body| body["registrationId"] = json!("not-a-guid"),
            |body| body["transports"]["TROUTER"] = json!([]),
            |body| body["transports"]["TROUTER"][0]["path"] = json!(7),
            |body| body["transports"]["TROUTER"][0]["ttl"] = json!(0),
            |body| body["transports"] = json!({}),
        ];
        for breaking in broken {
            let mut body = body();
            breaking(&mut body);
            assert!(asked(&body).is_none(), "{body}");
        }
    }
}
