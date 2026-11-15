//! The media node's internal API (signaling node → media node), on a listener of its own: rooms and legs by call and
//! leg id. Every request carries a bearer token the deployment's own keys signed for [`AUDIENCE`] (every node reads
//! the same keys from the database) and the node life (`epoch`) the call's row names: another life's room is gone
//! (410).

use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use jsonwebtoken::{Algorithm, Validation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::oneshot;

use super::leg::LegError;
use super::node::{Command, NodeError};
use crate::app::App;
use crate::calling::{Sdp, Sources};

/// The audience of the internal API's tokens.
const AUDIENCE: &str = "ngteams-media";
/// How long an internal token holds.
const TOKEN_LIFETIME: Duration = Duration::from_secs(60);
/// The header naming the node life a request is meant for.
pub(crate) const EPOCH: &str = "ngteams-media-epoch";
/// The largest request (an offer with its candidates is a few KiB).
const BODY_LIMIT: usize = 256 * 1024;

#[derive(Serialize, Deserialize)]
struct Claims {
    aud: String,
    iss: String,
    exp: i64,
}

fn issuer(app: &App) -> String {
    format!("{}/media", app.config.origin())
}

/// A token for a request of the internal API.
pub(crate) fn token(app: &App) -> Option<String> {
    let claims = Claims {
        aud: AUDIENCE.to_owned(),
        iss: issuer(app),
        exp: crate::db::now() + crate::db::seconds(TOKEN_LIFETIME),
    };
    app.issuer.keys().sign(&claims).ok()
}

async fn authorized(app: &App, headers: &HeaderMap) -> bool {
    let Some(token) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.set_audience(&[AUDIENCE]);
    validation.set_issuer(&[issuer(app)]);
    validation.set_required_spec_claims(&["exp", "aud", "iss"]);
    app.issuer.keys().verify::<Claims>(token, &validation).await.is_ok()
}

/// What the API's handlers share.
#[derive(Debug)]
pub(crate) struct Internal {
    pub app: Arc<App>,
    pub epoch: u64,
    pub commands: Sender<Command>,
}

/// Why a request failed.
enum Refused {
    Unauthorized,
    Gone,
    Node(NodeError),
    Stopped,
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Gone => StatusCode::GONE,
            Self::Stopped | Self::Node(NodeError::Full) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Node(NodeError::Missing) => StatusCode::NOT_FOUND,
            Self::Node(NodeError::Leg(LegError::Sdp(_))) => StatusCode::BAD_REQUEST,
            Self::Node(NodeError::Leg(LegError::NoOffer | LegError::Waiting)) => StatusCode::CONFLICT,
            Self::Node(NodeError::Leg(LegError::TooOften)) => StatusCode::TOO_MANY_REQUESTS,
            Self::Node(NodeError::Leg(LegError::Codec(_))) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        status.into_response()
    }
}

impl From<NodeError> for Refused {
    fn from(error: NodeError) -> Self {
        Self::Node(error)
    }
}

type Shared = State<Arc<Internal>>;

impl Internal {
    /// The request is authorized and meant for this node's life.
    async fn admit(&self, headers: &HeaderMap) -> Result<(), Refused> {
        if !authorized(&self.app, headers).await {
            return Err(Refused::Unauthorized);
        }
        let epoch = headers
            .get(EPOCH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        if epoch != Some(self.epoch) {
            return Err(Refused::Gone);
        }
        Ok(())
    }

    /// Sends the media thread the command `make` makes, and waits for its reply.
    async fn ask<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Command) -> Result<T, Refused> {
        let (reply, replied) = oneshot::channel();
        self.commands.send(make(reply)).map_err(|_| Refused::Stopped)?;
        replied.await.map_err(|_| Refused::Stopped)
    }
}

/// The internal API's routes.
pub(crate) fn router(internal: Arc<Internal>) -> Router {
    Router::new()
        .route("/health/live", get(|| async { StatusCode::NO_CONTENT }))
        .route("/internal/mixer/v1/rooms/{call}", get(room).delete(delete_room))
        .route(
            "/internal/mixer/v1/rooms/{call}/legs/{leg}",
            put(put_leg).delete(delete_leg),
        )
        .route("/internal/mixer/v1/rooms/{call}/legs/{leg}/answer", post(answer))
        .route("/internal/mixer/v1/rooms/{call}/legs/{leg}/offer", post(offer))
        .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(internal)
}

#[derive(Deserialize)]
pub(crate) struct PutLeg {
    pub sources: Sources,
    pub offer: Option<Sdp>,
}

#[derive(Deserialize)]
pub(crate) struct Answer {
    pub answer: Sdp,
}

#[derive(Deserialize)]
pub(crate) struct Offer {
    pub offer: Sdp,
}

async fn put_leg(
    State(internal): Shared,
    Path((call, leg)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<PutLeg>,
) -> Result<Response, Refused> {
    internal.admit(&headers).await?;
    let answering = body.offer.is_some();
    let sdp = internal
        .ask(|reply| Command::Put {
            call,
            leg,
            sources: body.sources,
            offer: body.offer,
            reply,
        })
        .await??;
    let kind = if answering { "answer" } else { "offer" };
    Ok(Json(json!({kind: sdp, "epoch": internal.epoch})).into_response())
}

async fn answer(
    State(internal): Shared,
    Path((call, leg)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Answer>,
) -> Result<StatusCode, Refused> {
    internal.admit(&headers).await?;
    internal
        .ask(|reply| Command::Answer {
            call,
            leg,
            answer: body.answer,
            reply,
        })
        .await??;
    Ok(StatusCode::NO_CONTENT)
}

async fn offer(
    State(internal): Shared,
    Path((call, leg)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Offer>,
) -> Result<Response, Refused> {
    internal.admit(&headers).await?;
    let answer = internal
        .ask(|reply| Command::Renegotiate {
            call,
            leg,
            offer: body.offer,
            reply,
        })
        .await??;
    Ok(Json(json!({"answer": answer})).into_response())
}

async fn delete_leg(
    State(internal): Shared,
    Path((call, leg)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, Refused> {
    internal.admit(&headers).await?;
    internal.ask(|reply| Command::DeleteLeg { call, leg, reply }).await??;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_room(
    State(internal): Shared,
    Path(call): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, Refused> {
    internal.admit(&headers).await?;
    internal.ask(|reply| Command::DeleteRoom { call, reply }).await??;
    Ok(StatusCode::NO_CONTENT)
}

async fn room(State(internal): Shared, Path(call): Path<String>, headers: HeaderMap) -> Result<Response, Refused> {
    internal.admit(&headers).await?;
    let legs = internal
        .ask(|reply| Command::Room { call, reply })
        .await?
        .ok_or(Refused::Node(NodeError::Missing))?;
    Ok(Json(json!({"legs": legs})).into_response())
}
