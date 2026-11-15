//! Workflows with incoming webhooks, as Teams' Workflows uses Power Automate (`docs/protocol/workflows.md`): the
//! Flow API subset the client sends to `/workflows/` (environments, the template gallery, workflows, owners,
//! triggers, runs) and the Power Apps connection routes on `/workflow-connections/`, in the shapes the client reads.
//! The gallery holds only Teams' two webhook templates ("Send webhook alerts to a channel" and "… to a chat"): every
//! other template needs a Microsoft connector.
//!
//! A workflow belongs to the accounts that own it; only an owner reads or changes it, and anyone else learns only
//! that it isn't there. It posts to one channel or chat, which every owner it gains must see. Its webhook link
//! (`hook`) takes no account token: an unguessable secret in it is the credential, so the secret is never logged and
//! a new workflow (delete and make again) is the only way to replace it. What arrives there is posted as the
//! Workflows bot through the chat service, so sync and push carry it as any message.

mod connections;
mod definition;
mod flows;
mod gallery;
pub(crate) mod hook;
mod limit;
mod repo;
mod schema;

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, FromRequestParts};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::app::App;
use crate::auth::extract::{Audience, Bearer};
use crate::config::Config;
use crate::db::{DbError, users};
pub use limit::Limiter;

/// The Flow API's audience, as the client asks for it (with its trailing slash).
pub const FLOW_AUDIENCE: &str = "https://service.flow.microsoft.com/";
/// The Power Apps connection service's audience.
pub const POWER_APPS_AUDIENCE: &str = "https://service.powerapps.com/";
/// The Workflows app's bot: its app id is the account id its posts carry (`28:<id>`), its row the migration's.
pub const WORKFLOWS_BOT: &str = "358f0194-6b0e-4dd3-af35-c24fe8a9ec87";
/// The name its posts carry.
const BOT_NAME: &str = "Workflows";
/// The largest Flow API request body: a workflow's definition with its markup.
const BODY_LIMIT: usize = 256 * 1024;

/// `[workflows]`: whether the server serves Workflows, and its webhooks' limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WorkflowsConfig {
    /// Whether the Flow and Power Apps routes are served and their tokens issued: the client offers Workflows
    /// for an account only then.
    pub enabled: bool,
    /// The largest body a webhook takes, in bytes (Teams takes a message of about 28 KB).
    pub max_body_bytes: usize,
    /// Posts a workflow's webhook takes in a minute, across every node.
    pub rate_per_minute: u32,
    /// The runs kept per workflow for its run history.
    pub runs_kept: u32,
}

impl Default for WorkflowsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_body_bytes: 28 * 1024,
            rate_per_minute: 60,
            runs_kept: 100,
        }
    }
}

/// The audiences a token may be asked for besides `config.audiences`: the Flow and Power Apps services', while the
/// server serves them.
#[must_use]
pub fn audiences(config: &Config) -> Vec<String> {
    if config.workflows.enabled {
        vec![FLOW_AUDIENCE.to_owned(), POWER_APPS_AUDIENCE.to_owned()]
    } else {
        Vec::new()
    }
}

pub(crate) struct Flow;
impl Audience for Flow {
    const AUDIENCE: &'static str = FLOW_AUDIENCE;
}

pub(crate) struct PowerApps;
impl Audience for PowerApps {
    const AUDIENCE: &'static str = POWER_APPS_AUDIENCE;
}

/// The Flow API and the connection routes; nothing while Workflows is off. The webhooks are [`hook::router`].
pub(crate) fn router(config: &Config) -> Router<Arc<App>> {
    if !config.workflows.enabled {
        return Router::new();
    }
    Router::new()
        .nest("/workflows/providers/Microsoft.ProcessSimple", flows::routes())
        .nest(
            "/workflow-connections/providers/Microsoft.PowerApps",
            connections::routes(),
        )
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// The enabled account a request of audience `A` comes from, and its tenant.
pub(crate) struct Caller<A> {
    pub id: String,
    pub tenant: String,
    audience: std::marker::PhantomData<A>,
}

impl<A: Audience> FromRequestParts<Arc<App>> for Caller<A> {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        let bearer = Bearer::<A>::from_request_parts(parts, app)
            .await
            .map_err(IntoResponse::into_response)?;
        let user = users::by_id(&app.db, &bearer.claims.oid)
            .await
            .map_err(|error| Error::from(error).into_response())?;
        if user.is_none_or(|user| user.disabled) {
            return Err(Error::FORBIDDEN.into_response());
        }
        Ok(Self {
            id: bearer.claims.oid,
            tenant: bearer.claims.tid,
            audience: std::marker::PhantomData,
        })
    }
}

impl<A> Caller<A> {
    /// The one environment every account of the server has: Power Automate's default one's name.
    fn environment(&self) -> String {
        environment(&self.tenant)
    }

    /// `environment`, when it is the caller's.
    fn check(&self, environment: &str) -> Answer<()> {
        if environment.eq_ignore_ascii_case(&self.environment()) {
            Ok(())
        } else {
            Err(Error::Code(StatusCode::NOT_FOUND, "EnvironmentAccessDenied"))
        }
    }
}

/// The tenant's default environment's name.
fn environment(tenant: &str) -> String {
    format!("Default-{tenant}")
}

/// A refusal in Power Automate's error shape.
#[derive(Debug)]
pub(crate) enum Error {
    Code(StatusCode, &'static str),
    Database(DbError),
}

impl Error {
    const BAD_REQUEST: Self = Self::Code(StatusCode::BAD_REQUEST, "InvalidRequestContent");
    const FORBIDDEN: Self = Self::Code(StatusCode::FORBIDDEN, "Forbidden");
    const NOT_FOUND: Self = Self::Code(StatusCode::NOT_FOUND, "NotFound");
    const FLOW_NOT_FOUND: Self = Self::Code(StatusCode::NOT_FOUND, "FlowNotFound");
}

impl From<DbError> for Error {
    fn from(error: DbError) -> Self {
        Self::Database(error)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self {
            Self::Code(status, code) => {
                (status, Json(json!({"error": {"code": code, "message": code}}))).into_response()
            }
            Self::Database(error) => {
                tracing::error!(%error, "workflows database failure");
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
        }
    }
}

type Answer<T = Response> = Result<T, Error>;

/// A JSON body, or [`Error::BAD_REQUEST`].
fn body(body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>) -> Answer<serde_json::Value> {
    body.map(|Json(body)| body).map_err(|_| Error::BAD_REQUEST)
}

/// Unix milliseconds as Power Automate writes a time (`2026-11-14T09:30:00.1234567Z` in its own; RFC 3339 here).
fn iso(at: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Now, in Unix milliseconds.
fn now() -> i64 {
    crate::chatsvc::now()
}
