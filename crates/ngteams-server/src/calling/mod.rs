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
//! - **Who may do what** is the token's account against the call's parties, and for a group call the members of its
//!   chat too (a join, a roster-only subscription, an add); anyone else gets 404.
//! - **The sweeper** ([`sweep`]) ends a ring nobody answered and a call whose keep-alives stopped, on whichever
//!   serving node looks first, and drops the rows of calls that are over.
//! - **A call's events in its chat** (`Event/Call`: started, ended with its length, missed) are written through the
//!   chat service ([`crate::chatsvc::call_events`]): a 1:1 call's when the two parties have their chat, a group
//!   call's to its group chat.
//! - **What a call leaves each party** ([`history`]): its call log entry, and a missed call's activity item.
//!
//! - **A group call** ([`group`]) is a group chat's: every member it names rings with the call's media node's offer,
//!   the caller is in it at once with the node's answer, and each party answers, declines, joins and leaves alone;
//!   the call ends when nobody is in it. Its media goes through the node ([`media::MediaNodes`]), which the
//!   operation asks first and the step then records. Until a node is installed ([`Mixers`]) a group call's start is
//!   answered 501; a meeting and the test call are never served.

pub mod config;
mod group;
mod history;
mod media;
mod meeting;
mod ops;
mod repo;
mod state;
mod turn;
mod wire;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

pub use media::{MediaError, MediaHome, MediaNodes, Mixers, Sdp, Sources};
pub use repo::{OpenCall, open_calls};
pub use turn::rest_credential;

use crate::app::App;
use crate::auth::extract::Skype;
use crate::chatsvc::call_events::{self, Part};
use crate::db::{DbError, users};
use crate::trouter::Stack;
use repo::Key;
use state::{At, Call, Delivery, Kind, Out, Outcome, Phase, Refusal, Seat};
use wire::Links;

/// How long an ended call's row is kept, in seconds: a late request of it is answered as gone.
const KEPT_AFTER_END: i64 = 300;
/// How often a node looks for calls that are due.
const SWEEP: Duration = Duration::from_millis(500);
/// How many sweeps go by between two looks at the group calls' media nodes.
const SWEEPS_PER_MEDIA_LOOK: u32 = 4;
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
    /// The call's media node didn't do its part.
    Media(MediaError),
    Database(DbError),
}

impl From<Refusal> for Failure {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<MediaError> for Failure {
    fn from(error: MediaError) -> Self {
        Self::Media(error)
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
            Self::Media(error) => media_failure(&error),
            Self::Database(error) => {
                tracing::error!(%error, "calling database failure");
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        };
        (status, Json(json!({"code": code}))).into_response()
    }
}

/// How a media node's failure is answered: a room that is gone as the call being gone, anything else as the service
/// being unavailable.
fn media_failure(error: &MediaError) -> (StatusCode, &'static str) {
    if matches!(error, MediaError::Gone) {
        return (StatusCode::GONE, "MediaGone");
    }
    tracing::warn!(%error, "a call's media node failed");
    (StatusCode::SERVICE_UNAVAILABLE, "MediaUnavailable")
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

/// Drops the media legs and the room a step let go of, publishes what it tells the parties, and writes the call's end
/// to its chat and what the call leaves each party in its own streams: its call log entry, and a missed call's
/// activity item to each callee who never took it ([`history::tell`]).
async fn carry_out(app: &App, call: &Call, out: Out) {
    release(app, call, &out).await;
    for delivery in &out.deliveries {
        publish(app, &call.id, delivery).await;
    }
    if let Some(outcome) = out.ended {
        tell_chat_ended(app, call, outcome).await;
        history::tell(app, call).await;
    }
}

/// Drops the legs and the room `out` let go of on the call's media node. One not dropped is logged: the media node
/// drops a room whose call is over by itself.
async fn release(app: &App, call: &Call, out: &Out) {
    let (Some(home), Some(nodes)) = (&call.media, app.mixers.get()) else {
        return;
    };
    for leg in &out.released {
        media::drop_leg(nodes.as_ref(), home, &call.id, leg).await;
    }
    if out.room_over {
        media::drop_room(nodes.as_ref(), home, &call.id).await;
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
    let part = |seat: Seat, seconds: i64| {
        let party = call.party(seat);
        Part {
            id: &party.id,
            name: &party.name,
            seconds,
        }
    };
    let parts = match (call.kind, outcome) {
        (Kind::Group, Outcome::Talked(seconds)) => {
            call.rostered().into_iter().map(|seat| part(seat, seconds)).collect()
        }
        (_, Outcome::Talked(seconds)) => vec![part(Seat::CALLER, seconds), part(Seat::CALLEE, seconds)],
        (_, Outcome::Missed) => vec![part(Seat::CALLER, 0)],
    };
    let caller = call.party(Seat::CALLER);
    let caller = (caller.id.as_str(), caller.name.as_str());
    if let Err(error) = call_events::ended(app, chat, caller, &call.id, &parts).await {
        tracing::warn!(%error, "a call's end wasn't written to its chat");
    }
}

/// Makes `step` on the call `key` names, as `caller`'s seat of it: read, stepped and stored unless another node moved
/// the row meanwhile, then read again. Someone who is no party of the call finds none.
async fn step<T>(
    app: &App,
    key: Key<'_>,
    caller: &Caller,
    step: impl Fn(&mut Call, Seat, At<'_>, &mut Out) -> Result<T, Refusal>,
) -> Result<T, Failure> {
    step_any(app, key, |call, at, out| {
        let seat = call.seat_of(&caller.id).ok_or(Refusal::Missing)?;
        step(call, seat, at, out)
    })
    .await
}

/// Makes `step` on the call `key` names, whoever asks: read, stepped and stored unless another node moved the row
/// meanwhile, then read again.
async fn step_any<T>(
    app: &App,
    key: Key<'_>,
    step: impl Fn(&mut Call, At<'_>, &mut Out) -> Result<T, Refusal>,
) -> Result<T, Failure> {
    let origin = app.config.origin();
    for _ in 0..ATTEMPTS {
        let kept = repo::find(&app.db, key).await?.ok_or(Refusal::Missing)?;
        let (mut call, mut out) = (kept.call.clone(), Out::default());
        let at = At {
            origin: &origin,
            now: now_ms(),
        };
        let answer = step(&mut call, at, &mut out)?;
        if call == kept.call || repo::store(&app.db, &call, kept.version).await? {
            carry_out(app, &call, out).await;
            return Ok(answer);
        }
    }
    Err(Failure::Busy)
}

/// A serving node's sweeper, until the task is dropped. Only a node that serves runs it: what a sweep tells the
/// parties reaches them through the node's fan-out, which a one-shot command's process ends with.
pub async fn keep_sweeping(app: Arc<App>) {
    let mut every = tokio::time::interval(SWEEP);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut round = 0_u32;
    loop {
        every.tick().await;
        sweep_round(&app, round).await;
        round = round.wrapping_add(1);
    }
}

/// The sweeper's round `round`: the calls that are due, and now and then the group calls' media nodes.
async fn sweep_round(app: &App, round: u32) {
    warn_failed(sweep(app).await, "the calls' sweep failed");
    if round.is_multiple_of(SWEEPS_PER_MEDIA_LOOK) {
        warn_failed(sweep_media(app).await, "the group calls' media nodes weren't looked at");
    }
}

fn warn_failed(result: Result<(), DbError>, what: &'static str) {
    if let Err(error) = result {
        tracing::warn!(%error, "{what}");
    }
}

/// Whether the media node `call`'s row names is known to be gone (one that can't be judged now isn't).
async fn media_gone(nodes: &dyn MediaNodes, call: &Call) -> bool {
    match &call.media {
        Some(home) => matches!(nodes.alive(home).await, Ok(false)),
        None => false,
    }
}

/// One look at the group calls' media nodes: a call whose node is gone (no heartbeat, or another life) ends for
/// everyone. A node that can't be judged now is looked at again later.
///
/// # Errors
/// A database failure.
pub async fn sweep_media(app: &App) -> Result<(), DbError> {
    let Some(nodes) = app.mixers.get() else {
        return Ok(());
    };
    for kept in repo::open_groups(&app.db).await? {
        if !media_gone(nodes.as_ref(), &kept.call).await {
            continue;
        }
        let home = kept
            .call
            .media
            .as_ref()
            .map(|home| home.node.as_str())
            .unwrap_or_default();
        tracing::info!(call = %kept.call.id, node = %home, "a group call's media node is gone: the call ends");
        let ended = step_any(
            app,
            Key::Id(&kept.call.id),
            |call, at, out| Ok(call.media_gone(at, out)),
        )
        .await;
        if let Err(Failure::Database(error)) = ended {
            return Err(error);
        }
    }
    Ok(())
}

/// The media node lost the leg `leg` of the call `call`: its party is taken out of the call (408 at its endpoint),
/// and the others get the roster.
pub(crate) async fn leg_lost(app: &App, call: &str, leg: &str) {
    let taken = step_any(app, Key::Id(call), |call, at, out| {
        if let Some(seat) = call.seat_of_leg(leg) {
            call.media_lost(seat, at, out);
        }
        Ok(())
    })
    .await;
    match taken {
        Ok(()) | Err(Failure::Refused(_)) => {}
        Err(error) => tracing::warn!(?error, "a party whose media was lost wasn't taken out of its call"),
    }
}

/// Whether the media node at `home` still has a use for the room of the call `call`: the call runs, in that room.
///
/// # Errors
/// A database failure.
pub(crate) async fn room_wanted(app: &App, call: &str, home: &MediaHome) -> Result<bool, DbError> {
    let kept = repo::find(&app.db, Key::Id(call)).await?;
    Ok(kept.is_some_and(|kept| kept.call.phase != Phase::Ended && kept.call.media.as_ref() == Some(home)))
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
    let kept = step(app, Key::Id(call), &caller, |call, seat, _, _| {
        call.more_links(seat, endpoint, links.clone());
        Ok(())
    })
    .await;
    if let Err(Failure::Database(error)) = kept {
        tracing::warn!(%error, "an acceptance's acknowledgement wasn't kept");
    }
}
