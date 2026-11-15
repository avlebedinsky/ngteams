//! Calls: the flight proxy's calling surface (`/flightproxy/api/v2/`, `docs/protocol/calling.md`) and the TURN
//! credentials (`/trap/tokens`). The server does a call's signaling only: a 1:1 call's media goes between the two
//! clients through the TURN relay, the caller's offer in the callee's invitation and the callee's answer in the
//! caller's acceptance.
//!
//! - **A call is a row** ([`repo`]), so any node answers any request of it. An operation reads the row, makes its
//!   step ([`state`], pure) and stores it only if the row didn't move meanwhile; a node that lost reads again.
//! - **What a step tells the parties** goes to their calling endpoints through the fan-out, on `call:<endpoint>`:
//!   the node that holds the endpoint's calling socket writes it as a Trouter delivery ([`crate::trouter`]), an
//!   invitation at the endpoint's root, everything else at the callback the party handed out.
//! - **A party's callbacks** are taken only on calling endpoints the account itself registered, so no request makes
//!   the server deliver to someone else's socket.
//! - **Who may do what** is the token's account against the call's two parties; anyone else gets 404.
//! - **The sweeper** ([`sweep`]) ends a ring nobody answered and a call whose keep-alives stopped, on whichever
//!   node looks first, and drops the rows of calls that are over.
//! - **A call's events in its chat** (`Event/Call`: started, ended with its length, missed) are written through the
//!   chat service ([`crate::chatsvc::call_events`]), when the two parties have their chat.
//!
//! A group call needs the call's mixer; its start is answered 501 until the media node serves it ([`ops::start`]
//! is where it plugs in: the call's kind is told apart there, and a mixer's offer would take the place of the
//! caller's in [`wire::invitation`]).

pub mod config;
mod ops;
mod repo;
mod state;
mod turn;
mod wire;

use std::sync::{Arc, Weak};
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

pub use repo::{OpenCall, open_calls};
pub use turn::rest_credential;

use crate::app::App;
use crate::auth::extract::Skype;
use crate::chatsvc::call_events::{self, Part};
use crate::db::{DbError, users};
use crate::trouter::Stack;
use repo::Key;
use state::{At, Call, Delivery, Out, Outcome, Refusal, Side};
use wire::Links;

/// How long an ended call's row is kept, in seconds: a late request of it is answered as gone.
const KEPT_AFTER_END: i64 = 300;
/// How often a node looks for calls that are due.
const SWEEP: Duration = Duration::from_millis(500);
/// How many due calls one look takes.
const SWEEP_BATCH: i64 = 64;
/// How often a step is tried again when another node moved the call meanwhile.
const ATTEMPTS: usize = 8;
/// The largest calling request (an offer with its candidates is a few KiB).
const BODY_LIMIT: usize = 256 * 1024;

/// The topic the deliveries to the calling endpoint `endpoint` are published on.
#[must_use]
pub fn topic(endpoint: &str) -> String {
    format!("call:{endpoint}")
}

pub(crate) fn router() -> Router<Arc<App>> {
    let proxy = "/flightproxy/api/v2";
    let leg = format!("{proxy}/ep/{{host}}/cc/v1/{{kind}}/{{call}}/{{leg}}/{{at}}/{{number}}");
    Router::new()
        .route(&format!("{proxy}/epconv"), post(ops::start))
        .route(
            &format!("{proxy}/ep/{{host}}/conv/{{conversation}}"),
            post(ops::subscribe).delete(ops::end_for_all),
        )
        .route(
            &format!("{proxy}/ep/{{host}}/conv/{{conversation}}/{{op}}"),
            any(ops::conversation),
        )
        .route(&leg, any(ops::keep_alive))
        .route(&format!("{leg}/{{op}}"), any(ops::controller))
        .route("/trap/tokens", get(turn::tokens))
        .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT))
}

/// The account a calling request's skypetoken is for, when it may sign in.
pub(crate) struct Caller {
    id: String,
    name: String,
}

impl Caller {
    fn mri(&self) -> String {
        format!("8:orgid:{}", self.id)
    }
}

impl FromRequestParts<Arc<App>> for Caller {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        let Skype(claims) = Skype::from_request_parts(parts, app)
            .await
            .map_err(IntoResponse::into_response)?;
        let user = users::by_id(&app.db, &claims.oid)
            .await
            .map_err(|error| Failure::from(error).into_response())?;
        let user = user
            .filter(|user| !user.disabled)
            .ok_or_else(|| Failure::Refused(Refusal::Forbidden).into_response())?;
        Ok(Self {
            id: user.id,
            name: user.display_name,
        })
    }
}

/// Why a calling request wasn't served.
#[derive(Debug)]
pub(crate) enum Failure {
    Refused(Refusal),
    /// A kind of call this server doesn't serve yet.
    NotServed,
    /// Other nodes kept moving the call.
    Busy,
    Database(DbError),
}

impl From<Refusal> for Failure {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<DbError> for Failure {
    fn from(error: DbError) -> Self {
        Self::Database(error)
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::Refused(Refusal::Bad) => (StatusCode::BAD_REQUEST, "BadRequest"),
            Self::Refused(Refusal::Forbidden) => (StatusCode::FORBIDDEN, "Forbidden"),
            Self::Refused(Refusal::Missing) => (StatusCode::NOT_FOUND, "NotFound"),
            Self::Refused(Refusal::Gone) => (StatusCode::GONE, "CallEnded"),
            Self::NotServed => (StatusCode::NOT_IMPLEMENTED, "NotServed"),
            Self::Busy => (StatusCode::SERVICE_UNAVAILABLE, "Busy"),
            Self::Database(error) => {
                tracing::error!(%error, "calling database failure");
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        };
        (status, Json(json!({"code": code}))).into_response()
    }
}

/// Unix milliseconds.
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn ring_seconds(app: &App) -> i64 {
    i64::try_from(app.config.calling.ring_timeout).unwrap_or(i64::MAX / 4)
}

/// The Trouter endpoints of the calling stack `user` registered.
async fn calling_endpoints(app: &App, user: &str) -> Result<Vec<String>, DbError> {
    let stack = Stack::Calling;
    let registered = crate::registrar::registered(&app.db, user, stack.app_id()).await?;
    Ok(registered
        .iter()
        .map(|epid| crate::trouter::endpoint_id(user, epid, stack))
        .collect())
}

/// The callbacks `body` hands out and the endpoint they are on: all on one calling endpoint the account itself
/// registered. `None` when it hands out none.
async fn own_links(app: &App, caller: &Caller, body: &Value) -> Result<Option<(String, Links)>, Failure> {
    let origin = app.config.origin();
    if wire::foreign_callback(&origin, body) {
        return Err(Refusal::Forbidden.into());
    }
    let links = wire::links_in(&origin, body);
    let mut endpoints = links
        .values()
        .filter_map(|link| wire::callback(&origin, link))
        .map(|(endpoint, _)| endpoint);
    let Some(endpoint) = endpoints.next().map(str::to_owned) else {
        return Ok(None);
    };
    if endpoints.any(|other| other != endpoint) {
        return Err(Refusal::Forbidden.into());
    }
    if !calling_endpoints(app, &caller.id).await?.contains(&endpoint) {
        return Err(Refusal::Forbidden.into());
    }
    Ok(Some((endpoint, links)))
}

/// [`own_links`], for an operation that needs its callbacks.
async fn needed_links(app: &App, caller: &Caller, body: &Value) -> Result<(String, Links), Failure> {
    own_links(app, caller, body)
        .await?
        .ok_or(Failure::Refused(Refusal::Bad))
}

/// Publishes what a step tells the parties, and writes the call's end to its chat.
async fn carry_out(app: &App, call: &Call, out: Out) {
    for delivery in &out.deliveries {
        publish(app, &call.id, delivery).await;
    }
    if let Some(outcome) = out.ended {
        tell_chat_ended(app, call, outcome).await;
    }
}

async fn publish(app: &App, call: &str, delivery: &Delivery) {
    let payload = json!({"call": call, "path": delivery.path, "body": delivery.body.to_string()});
    let published = app
        .fanout
        .publish(&topic(&delivery.endpoint), payload.to_string().into())
        .await;
    if let Err(error) = published {
        tracing::warn!(%error, "a call delivery wasn't published");
    }
}

async fn tell_chat_ended(app: &App, call: &Call, outcome: Outcome) {
    let Some(chat) = &call.chat else { return };
    let part = |side: Side, seconds: i64| {
        let party = call.party(side);
        Part {
            id: &party.id,
            name: &party.name,
            seconds,
        }
    };
    let parts = match outcome {
        Outcome::Talked(seconds) => vec![part(Side::Caller, seconds), part(Side::Callee, seconds)],
        Outcome::Missed => vec![part(Side::Caller, 0)],
    };
    let caller = (call.caller.id.as_str(), call.caller.name.as_str());
    if let Err(error) = call_events::ended(app, chat, caller, &call.id, &parts).await {
        tracing::warn!(%error, "a call's end wasn't written to its chat");
    }
}

/// Makes `step` on the call `key` names, as `caller`'s side of it: read, stepped and stored unless another node moved
/// the row meanwhile, then read again. Someone who is no party of the call finds none.
async fn step<T>(
    app: &App,
    key: Key<'_>,
    caller: &Caller,
    step: impl Fn(&mut Call, Side, At<'_>, &mut Out) -> Result<T, Refusal>,
) -> Result<T, Failure> {
    let origin = app.config.origin();
    for _ in 0..ATTEMPTS {
        let kept = repo::find(&app.db, key).await?.ok_or(Refusal::Missing)?;
        let side = kept.call.side_of(&caller.id).ok_or(Refusal::Missing)?;
        let (mut call, mut out) = (kept.call.clone(), Out::default());
        let at = At {
            origin: &origin,
            now: now_ms(),
        };
        let answer = step(&mut call, side, at, &mut out)?;
        if call == kept.call || repo::store(&app.db, &call, kept.version).await? {
            carry_out(app, &call, out).await;
            return Ok(answer);
        }
    }
    Err(Failure::Busy)
}

/// Starts the node's sweeper; it stops when the node's state is dropped.
pub(crate) fn spawn_sweeper(app: &Arc<App>) {
    let app = Arc::downgrade(app);
    tokio::spawn(async move {
        let mut every = tokio::time::interval(SWEEP);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            let Some(app) = Weak::upgrade(&app) else { return };
            if let Err(error) = sweep(&app).await {
                tracing::warn!(%error, "the calls' sweep failed");
            }
        }
    });
}

/// One look at the calls that are due: a ring nobody answered ends, a call without keep-alives is given up, an
/// ended call's row goes.
///
/// # Errors
/// A database failure.
pub async fn sweep(app: &App) -> Result<(), DbError> {
    let origin = app.config.origin();
    for (id, over) in repo::due(&app.db, crate::db::now(), SWEEP_BATCH).await? {
        if over {
            repo::delete(&app.db, &id).await?;
            continue;
        }
        let Some(kept) = repo::find(&app.db, Key::Id(&id)).await? else {
            continue;
        };
        let (mut call, mut out) = (kept.call, Out::default());
        let at = At {
            origin: &origin,
            now: now_ms(),
        };
        // a node that lost the row to another leaves the step to it
        if call.lapse(at, &mut out) && repo::store(&app.db, &call, kept.version).await? {
            carry_out(app, &call, out).await;
        }
    }
    Ok(())
}

/// A calling socket's client answered a delivery of the call `call` with `body`: the acknowledgement of an
/// acceptance hands out the caller's later callbacks (where the far end's renegotiation goes).
pub(crate) async fn answered(app: &App, call: &str, account: &str, endpoint: &str, body: &str) {
    let Ok(body) = serde_json::from_str::<Value>(body) else {
        return;
    };
    let Some(acknowledged) = body.get("callAcceptanceAcknowledgement") else {
        return;
    };
    let origin = app.config.origin();
    let links: Links = wire::links_in(&origin, acknowledged)
        .into_iter()
        .filter(|(_, link)| wire::callback(&origin, link).is_some_and(|(on, _)| on == endpoint))
        .collect();
    let caller = Caller {
        id: account.to_owned(),
        name: String::new(),
    };
    let kept = step(app, Key::Id(call), &caller, |call, side, _, _| {
        call.more_links(side, endpoint, links.clone());
        Ok(())
    })
    .await;
    if let Err(Failure::Database(error)) = kept {
        tracing::warn!(%error, "an acceptance's acknowledgement wasn't kept");
    }
}
