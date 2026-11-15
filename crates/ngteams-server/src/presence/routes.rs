//! The presence service's requests under `/ups/<geo>/v1/`, in the forms the client sends them
//! (`docs/protocol/endpoints.md`, "Presence"): the read, the endpoint's own availability, the manual status and the
//! subscription. Each takes the presence audience's token of an account that can sign in.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{AVAILABILITIES, MANUAL, PresenceError, SOURCE, Subscription};
use crate::app::App;
use crate::auth::extract::Audience;
use crate::people::Caller;
use crate::trouter::{Stack, endpoint_id, is_guid, push_url};

/// The presence service's audience.
#[derive(Debug)]
pub(crate) struct Ups;

impl Audience for Ups {
    const AUDIENCE: &'static str = "https://presence.teams.microsoft.com/";
}

/// The largest body a presence request takes.
const BODY_LIMIT: usize = 128 * 1024;
/// The MRIs one read or one subscription names: what the web client tracks at most.
const MAX_NAMES: usize = 650;
/// What ends the URI a subscription's deliveries go to, after its endpoint's push URL.
const PUSH_PATH: &str = "unifiedPresenceService";
/// The header that names the endpoint a request is from.
const ENDPOINT_ID: &str = "x-ms-endpoint-id";
/// The headers UPS answers a subscription without 400.
const MANDATORY: [&str; 2] = ["x-ms-correlation-id", "x-ms-client-version"];

pub(crate) fn router(app: &App) -> Router<Arc<App>> {
    let routes = Router::new()
        .route("/v1/presence/getpresence/", post(read))
        .route("/v1/me/endpoints/", put(report))
        .route("/v1/me/forceavailability/", put(force))
        .route("/v1/pubsub/subscriptions/{epid}", post(subscribe))
        .layer(DefaultBodyLimit::max(BODY_LIMIT));
    Router::new().nest(&crate::services::presence_prefix(&app.config), routes)
}

/// A refusal.
enum Refusal {
    Bad(String),
    TooMany,
    Unavailable(PresenceError),
}

impl From<PresenceError> for Refusal {
    fn from(error: PresenceError) -> Self {
        match error {
            PresenceError::TooManyEndpoints => Self::TooMany,
            other => Self::Unavailable(other),
        }
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Self::Bad(message) => (StatusCode::BAD_REQUEST, Json(json!({"message": message}))).into_response(),
            Self::TooMany => StatusCode::TOO_MANY_REQUESTS.into_response(),
            Self::Unavailable(error) => {
                tracing::error!(%error, "presence request failed");
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
        }
    }
}

fn bad(message: &str) -> Refusal {
    Refusal::Bad(message.to_owned())
}

fn parsed<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Result<T, Refusal> {
    serde_json::from_slice(body).map_err(|_| bad("The body isn't of the form the presence service takes."))
}

/// One item of a read's list: `{"mri"}`, the account's own sometimes with `"source": "ups"` beside it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Asked {
    mri: String,
    #[serde(default)]
    source: Option<String>,
}

async fn read(State(app): State<Arc<App>>, caller: Caller<Ups>, body: Bytes) -> Result<Json<Value>, Refusal> {
    let asked: Vec<Asked> = parsed(&body)?;
    if asked.is_empty() || asked.len() > MAX_NAMES {
        return Err(bad("A presence read names 1 to 650 accounts."));
    }
    let asked: Vec<(String, bool)> = asked
        .into_iter()
        .map(|item| (item.mri, item.source.is_some()))
        .collect();
    Ok(Json(Value::Array(app.presence.read(&caller.id, &asked).await?)))
}

/// The endpoint a request's header names: a GUID, as the client writes its `epid`.
fn endpoint_of(headers: &HeaderMap) -> Result<&str, Refusal> {
    headers
        .get(ENDPOINT_ID)
        .and_then(|value| value.to_str().ok())
        .filter(|epid| is_guid(epid))
        .ok_or_else(|| bad("Missing mandatory header 'x-ms-endpoint-id'"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reported {
    id: String,
    availability: String,
    activity: String,
    #[serde(default)]
    device_type: Option<String>,
}

/// A word of an activity or a device type: ASCII letters and digits.
fn is_word(text: &str) -> bool {
    (1..=64).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

async fn report(
    State(app): State<Arc<App>>,
    caller: Caller<Ups>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, Refusal> {
    let epid = endpoint_of(&headers)?;
    let reported: Reported = parsed(&body)?;
    let device = reported.device_type.as_deref();
    let known = reported.id == epid
        && AVAILABILITIES.contains(&reported.availability.as_str())
        && is_word(&reported.activity)
        && device.is_none_or(is_word);
    if !known {
        return Err(bad("The endpoint's availability isn't one the presence service takes."));
    }
    app.presence
        .report(&caller.id, epid, &reported.availability, &reported.activity, device)
        .await?;
    Ok(StatusCode::CREATED)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Forced {
    availability: String,
}

async fn force(
    State(app): State<Arc<App>>,
    caller: Caller<Ups>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, Refusal> {
    endpoint_of(&headers)?;
    // the reset is the same request with no body at all
    let manual = if body.is_empty() {
        None
    } else {
        let forced: Forced = parsed(&body)?;
        if !MANUAL.contains(&forced.availability.as_str()) {
            return Err(bad("The status isn't one the presence service takes."));
        }
        Some(forced.availability)
    };
    app.presence.force(&caller.id, manual.as_deref()).await?;
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Sourced {
    mri: String,
    source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Subscribing {
    trouter_uri: String,
    should_purge_previous_subscriptions: bool,
    subscriptions_to_add: Vec<Sourced>,
    subscriptions_to_remove: Vec<Sourced>,
}

fn statuses(items: &[Sourced], status: impl Fn(usize) -> i64) -> Vec<Value> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| json!({"mri": item.mri, "source": item.source, "status": status(index)}))
        .collect()
}

async fn subscribe(
    State(app): State<Arc<App>>,
    caller: Caller<Ups>,
    Path(epid): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, Refusal> {
    for name in MANDATORY {
        if !headers.contains_key(name) {
            return Err(Refusal::Bad(format!("Missing mandatory header '{name}'")));
        }
    }
    if endpoint_of(&headers)? != epid {
        return Err(bad("The subscription isn't for the endpoint that sent it."));
    }
    let request: Subscribing = parsed(&body)?;
    // deliveries go to the caller's own messaging endpoint and nowhere else
    let endpoint = endpoint_id(&caller.id, &epid, Stack::Messaging);
    let own = format!("{}{PUSH_PATH}", push_url(&app.config.origin(), &endpoint));
    let names = request.subscriptions_to_add.len() + request.subscriptions_to_remove.len();
    let sourced = request
        .subscriptions_to_add
        .iter()
        .chain(&request.subscriptions_to_remove)
        .all(|item| item.source == SOURCE);
    if request.trouter_uri != own || names > MAX_NAMES || !sourced {
        return Err(bad("The subscription isn't of the form the presence service takes."));
    }
    let mris = |items: &[Sourced]| items.iter().map(|item| item.mri.clone()).collect::<Vec<_>>();
    let (add, remove) = (
        mris(&request.subscriptions_to_add),
        mris(&request.subscriptions_to_remove),
    );
    let taken = app
        .presence
        .subscribe(&Subscription {
            oid: &caller.id,
            endpoint: &endpoint,
            purge: request.should_purge_previous_subscriptions,
            add: &add,
            remove: &remove,
        })
        .await?;
    Ok(Json(json!({
        "addStatuses": statuses(&request.subscriptions_to_add, |index| taken[index].status()),
        "removeStatuses": statuses(&request.subscriptions_to_remove, |_| 20200),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::BoardError;

    #[test]
    fn a_refusal_answers_with_its_status() {
        let redis = redis::RedisError::from((redis::ErrorKind::Io, "down"));
        let board = PresenceError::Board(BoardError::from(redis));
        assert!(board.to_string().contains("down"));
        let status = |refusal: Refusal| refusal.into_response().status();
        assert_eq!(status(board.into()), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            status(PresenceError::TooManyEndpoints.into()),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(status(bad("no")), StatusCode::BAD_REQUEST);
        assert!(is_word("InACall") && !is_word("") && !is_word("a b") && !is_word(&"x".repeat(65)));
    }
}
