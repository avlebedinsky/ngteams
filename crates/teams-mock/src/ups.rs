//! UPS's own-availability writes (M2 Task 19): `PUT /ups/<geo>/v1/me/endpoints/` (the account's presence endpoint,
//! available) and `PUT /ups/<geo>/v1/me/forceavailability/` (its manual status, set, or with no body cleared), in the
//! shapes the web client sent (`tests/fixtures/ups/endpoint-requests.json`, `manual-status-requests.json`), answered as
//! UPS answered them: `201` and `200`, with no body. Under the presence base [`crate::PRESENCE_BASE`]; the geo is
//! `noam`.
//!
//! Each must carry a token the mock handed out (the account it sets), `x-ms-endpoint-id` naming an endpoint that
//! account registered with the mock's registrar, and a body of exactly the captured keys (the endpoint's `id` the
//! header's, `availability`/`activity` one of the captured pairs, `activityReporting` `Transport`, `deviceType` `Web`;
//! a manual status one of the captured four). A request without such a token is a 401, any other shape or endpoint a
//! 400, any other path under `/ups/noam/` a 404, all with no body: UPS's answers to those weren't captured. Each write
//! taken is kept ([`crate::ChatServiceMock::own_availability`]); nobody is told.

use percent_encoding::percent_decode_str;
use serde_json::Value;
use wiremock::{Request, ResponseTemplate};

use crate::chatsvc::status;
use crate::world::State;

/// The captured (availability, activity) pairs of the endpoint `PUT`.
const ENDPOINT_STATES: [(&str, &str); 2] = [("Available", "Available"), ("Busy", "InACall")];

/// The captured manual statuses.
const MANUAL_STATES: [&str; 4] = ["Busy", "DoNotDisturb", "Away", "Available"];

/// One own-availability write the mock took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnAvailability {
    /// The account its token is.
    pub account: String,
    /// The endpoint it named (`x-ms-endpoint-id`).
    pub endpoint: String,
    pub change: AvailabilityChange,
}

/// What an own-availability write set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AvailabilityChange {
    /// The endpoint `PUT`: its availability and activity.
    Endpoint { availability: String, activity: String },
    /// A manual status set.
    Manual(String),
    /// The manual status cleared.
    Reset,
}

/// The answer to `request` when it is under the presence base; `None` for any other path.
pub(crate) fn own_availability(state: &mut State, request: &Request) -> Option<ResponseTemplate> {
    let segments: Vec<String> = request
        .url
        .path_segments()?
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect::<Option<_>>()?;
    let rest = match segments.as_slice() {
        [ups, geo, rest @ ..] if ups == "ups" && geo == "noam" => rest,
        _ => return None,
    };
    let header = |name: &str| request.headers.get(name).and_then(|value| value.to_str().ok());
    let Some(account) = header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|token| state.account_by_token(token))
        .map(|account| account.mri.clone())
    else {
        return Some(status(401));
    };
    let name = match rest {
        [v1, me, name, last] if v1 == "v1" && me == "me" && last.is_empty() => name.as_str(),
        _ => return Some(status(404)),
    };
    if request.method.as_str() != "PUT" || request.url.query().is_some() {
        return Some(status(404));
    }
    let endpoint = header("x-ms-endpoint-id")
        .filter(|endpoint| {
            state
                .registrations
                .iter()
                .any(|registration| registration.account == account && registration.registration_id == *endpoint)
        })
        .map(str::to_owned);
    let change = match name {
        "endpoints" => endpoint_change(&request.body, endpoint.as_deref()),
        "forceavailability" => manual_change(&request.body),
        _ => return Some(status(404)),
    };
    let (Some(endpoint), Some(change)) = (endpoint, change) else {
        return Some(status(400));
    };
    let answer = if matches!(change, AvailabilityChange::Endpoint { .. }) {
        201
    } else {
        200
    };
    state.availability.push(OwnAvailability {
        account,
        endpoint,
        change,
    });
    Some(status(answer))
}

/// The endpoint `PUT`'s change, when its body is exactly the captured keys and its `id` is `endpoint`.
fn endpoint_change(body: &[u8], endpoint: Option<&str>) -> Option<AvailabilityChange> {
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(body) else {
        return None;
    };
    let text = |key: &str| body.get(key).and_then(Value::as_str);
    let state = (text("availability")?, text("activity")?);
    let shape = body.len() == 5
        && endpoint.is_some()
        && text("id") == endpoint
        && ENDPOINT_STATES.contains(&state)
        && text("activityReporting") == Some("Transport")
        && text("deviceType") == Some("Web");
    shape.then(|| AvailabilityChange::Endpoint {
        availability: state.0.to_owned(),
        activity: state.1.to_owned(),
    })
}

/// The manual status's change: a body of exactly a captured `availability`, or none at all (the reset).
fn manual_change(body: &[u8]) -> Option<AvailabilityChange> {
    if body.is_empty() {
        return Some(AvailabilityChange::Reset);
    }
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(body) else {
        return None;
    };
    let availability = body.get("availability").and_then(Value::as_str)?;
    (body.len() == 1 && MANUAL_STATES.contains(&availability))
        .then(|| AvailabilityChange::Manual(availability.to_owned()))
}
