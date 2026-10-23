//! The lab account's **own availability** under the browser fence (M2 Task 18, `docs/protocol/trouter.md`, "Own
//! presence").
//!
//! A lab account's own availability is seen across the tenant, so no participant check can pass for it. The user
//! approved a narrow exception on 2026-10-21 (CLAUDE.md, "A lab account may set its own availability"): a lab account
//! may set its own availability, in the captured shapes, availability values only, every id it names the account
//! itself or its own registered endpoint. Nothing else of presence: never a status note, out-of-office, a work location
//! or a profile field, and never another account's presence.
//!
//! What this module recognizes, each only in the shape the captures showed, each naming the account itself (`me` in
//! the path, from the request's token) and one endpoint (the client's Trouter endpoint id, which is the `registrationId`
//! of the messaging stack's registration, `crate::calling::messaging_endpoint`, and the request's `x-ms-endpoint-id`):
//! - [`endpoint_put`]: UPS's presence endpoint, `PUT /ups/<geo>/v1/me/endpoints/` (403 of them in the captures of
//!   2026-10-20 to 2026-10-21, all denied then; 201 when allowed), with one of the captured availability and activity
//!   pairs ([`ENDPOINT_STATES`]);
//! - [`manual_status`]: the manual status, `PUT /ups/<geo>/v1/me/forceavailability/` (15 in `own-presence-manual-held`
//!   of 2026-10-23, all denied then): setting one of the captured availabilities ([`MANUAL_STATES`]), or the reset,
//!   the same `PUT` with no body at all, which clears it.
//!
//! The classifier gives each as [`Classification::OwnAvailability`] naming its endpoint; the session's fence lets it
//! through only when the endpoint is one it let this account register in this run ([`OwnEndpoints`]: enrolled once
//! the fence's verdict on the registration was `Allow`, which says the fence let it out, not that the registrar took
//! it), and logs it to `fence.log` as own state ([`OwnKind::OwnAvailability`]). Any other endpoint, or a request whose
//! body the fence can't read with certainty, makes it [`Classification::Unknown`]. The Trouter `user.activity` frame
//! is `crate::calling::trouter_frame`'s (`active` or `inactive` only), as before.
//!
//! Only a classifier built with [`crate::classify::Classifier::with_own_availability`] asks: the `own-presence`
//! scenarios. Every other run denies these requests as it did. Not recognized, and so denied everywhere: "Be right
//! back", "Appear offline", a status with a duration, a status note (`publishnote`), a work location, out-of-office,
//! the presence subscriptions, and any other shape of these two requests.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use serde_json::Value;
use url::Url;

use crate::arming::TEAMS_ORIGINS;
use crate::classify::{Classification, is_lower_guid, strict_json};
use crate::post_read::{HeaderRules, are_listed_headers};

/// The (availability, activity) pairs a captured endpoint `PUT` set: `Available`/`Available` (388 of the 403) while the
/// client runs, `Busy`/`InACall` (15, all in `echo-call`) while it is in a call. No other pair was seen.
pub const ENDPOINT_STATES: [(&str, &str); 2] = [("Available", "Available"), ("Busy", "InACall")];

/// The availabilities a captured manual status set (three tries each, `own-presence-manual-held` of 2026-10-23).
pub const MANUAL_STATES: [&str; 4] = ["Busy", "DoNotDisturb", "Away", "Available"];

/// How the endpoint reports its activity (`activityReporting`) and what it is (`deviceType`), as every capture had them.
const ACTIVITY_REPORTING: &str = "Transport";
const DEVICE_TYPE: &str = "Web";

/// The client components a captured endpoint `PUT` named (`x-ms-client-caller`): the push channel's state changing
/// (325), the client's presence sync (48), and a call's state (30).
pub const ENDPOINT_CALLERS: [&str; 4] = [
    "presence-sync-strategy:TrouterStateChanged",
    "presence-sync-strategy-client",
    "set-callstate-presence-worker-resolver:setCallStatePresence",
    "set-callstate-presence-worker-resolver:setCallStatePresence-serialized",
];

/// The caller of a captured manual status that sets one, and of one that resets it.
pub const MANUAL_SET_CALLER: &str = "TeamsClient:setPresenceAvailability";
pub const MANUAL_RESET_CALLER: &str = "TeamsClient:setPresenceAvailability-reset";

/// The headers every captured endpoint `PUT` and manual status carried as the fence paused them (all alike), by name
/// in any case; any other makes the request unknown. Their values: pinned to the one value every capture showed
/// ([`PINNED_HEADERS`]), each of them there; `Origin` the request's own; `x-ms-client-caller` one of the request's
/// captured callers; `x-ms-endpoint-id`, which must be there, a lower-case GUID (the endpoint the request names); the
/// rest free, as for the presence read (`crate::post_read`): the account's own credentials, the page, the client's
/// version and its correlation ids.
const HEADERS: [&str; 17] = [
    "Accept",
    "Cookie",
    "Origin",
    "Referer",
    "User-Agent",
    "authorization",
    "behavioroverride",
    "content-type",
    "x-ms-client-caller",
    "x-ms-client-type",
    "x-ms-client-user-agent",
    "x-ms-client-version",
    "x-ms-correlation-id",
    "x-ms-endpoint-id",
    "x-ms-object-id",
    "x-ms-request-id",
    "x-ms-session-id",
];

/// The headers these requests carry only with the one value every capture showed, by lower-cased name; each must be
/// there (the reset too sent `Content-Type`).
const PINNED_HEADERS: [(&str, &str); 6] = [
    ("content-type", "application/json"),
    ("behavioroverride", "redirectAs404"),
    ("x-ms-client-type", "cdlworker"),
    ("x-ms-client-user-agent", "Teams-V2-Web"),
    ("x-ms-object-id", ""),
    ("x-ms-request-id", ""),
];

const RULES: HeaderRules = HeaderRules {
    names: &HEADERS,
    pinned: &PINNED_HEADERS,
};

/// The endpoint an endpoint `PUT` sets the account's availability on, when the request is one in its captured shape
/// (see the module docs), else `None`: `PUT` on the path `/ups/<geo>/v1/me/endpoints/` ([`captured_headers`] for the
/// rest of the request, a caller of [`ENDPOINT_CALLERS`]), and a body, read strictly, of exactly `id` (the
/// `x-ms-endpoint-id`), `availability` and `activity` (a pair of [`ENDPOINT_STATES`]), `activityReporting` `Transport`
/// and `deviceType` `Web`.
#[must_use]
pub fn endpoint_put(
    method: &str,
    url: &Url,
    headers: &[(String, Option<String>)],
    body: Option<&[u8]>,
) -> Option<String> {
    if method != "PUT" || !is_own_path(url, "endpoints") {
        return None;
    }
    let (caller, endpoint) = captured_headers(url, headers)?;
    if !ENDPOINT_CALLERS.contains(&caller) {
        return None;
    }
    let Value::Object(body) = strict_json(body?)? else {
        return None;
    };
    let text = |key: &str| body.get(key).and_then(Value::as_str);
    let state = (text("availability")?, text("activity")?);
    let shape = body.len() == 5
        && text("id") == Some(endpoint)
        && ENDPOINT_STATES.contains(&state)
        && text("activityReporting") == Some(ACTIVITY_REPORTING)
        && text("deviceType") == Some(DEVICE_TYPE);
    shape.then(|| endpoint.to_owned())
}

/// The endpoint a manual status names, when the request is one in its captured shape (see the module docs), else
/// `None`: `PUT` on the path `/ups/<geo>/v1/me/forceavailability/` ([`captured_headers`] for the rest), and either the
/// caller [`MANUAL_SET_CALLER`] with a body, read strictly, of exactly `availability`, one of [`MANUAL_STATES`], or the
/// caller [`MANUAL_RESET_CALLER`] with no body (`None`: whether the fence could read that the request has none is the
/// session fence's check, [`OwnEndpoints::resolve`]).
#[must_use]
pub fn manual_status(
    method: &str,
    url: &Url,
    headers: &[(String, Option<String>)],
    body: Option<&[u8]>,
) -> Option<String> {
    if method != "PUT" || !is_own_path(url, "forceavailability") {
        return None;
    }
    let (caller, endpoint) = captured_headers(url, headers)?;
    let shape = match (caller, body) {
        (MANUAL_SET_CALLER, Some(body)) => matches!(strict_json(body), Some(Value::Object(body)) if body.len() == 1
            && body.get("availability").and_then(Value::as_str).is_some_and(|value| MANUAL_STATES.contains(&value))),
        (MANUAL_RESET_CALLER, None) => true,
        _ => false,
    };
    shape.then(|| endpoint.to_owned())
}

/// Whether `url` is `https://<a Teams origin>/ups/<geo>/v1/me/<name>/` exactly: one of the web client's two origins, no
/// user info, the geo lowercase letters (`noam`), the trailing slash, no query or fragment.
fn is_own_path(url: &Url, name: &str) -> bool {
    let teams = TEAMS_ORIGINS.contains(&url.origin().ascii_serialization().as_str());
    let plain =
        url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none();
    let path = url
        .path()
        .strip_prefix("/ups/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(geo, rest)| {
            !geo.is_empty()
                && geo.bytes().all(|byte| byte.is_ascii_lowercase())
                && rest.strip_prefix("v1/me/").and_then(|rest| rest.strip_suffix('/')) == Some(name)
        });
    teams && plain && path
}

/// The caller and the endpoint of a request whose headers are the captured ones ([`HEADERS`], each once; every pinned
/// one there with its value, [`PINNED_HEADERS`]; `Origin`, if there, the request's own), with `x-ms-client-caller` and
/// `x-ms-endpoint-id` there (the endpoint a lower-case GUID); else `None`. A header whose value the fence didn't keep
/// has none of these values.
fn captured_headers<'h>(url: &Url, headers: &'h [(String, Option<String>)]) -> Option<(&'h str, &'h str)> {
    if !are_listed_headers(url, headers, &RULES) {
        return None;
    }
    let value = |wanted: &str| {
        headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .and_then(|(_, value)| value.as_deref())
    };
    if !PINNED_HEADERS.iter().all(|&(name, _)| value(name).is_some()) {
        return None;
    }
    let endpoint = value("x-ms-endpoint-id").filter(|endpoint| is_lower_guid(endpoint))?;
    Some((value("x-ms-client-caller")?, endpoint))
}

/// The endpoints one browser's fence let this account register in this run (the messaging stack's `registrationId`s),
/// and with them the decision on a request that sets the account's own availability. Each lab session's HTTP fence has
/// its own, so an endpoint one account registered never counts for another.
#[derive(Debug, Default)]
pub struct OwnEndpoints {
    registered: Mutex<HashSet<String>>,
}

impl OwnEndpoints {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What the fence makes of a request `classification` was given for: an own-availability request
    /// ([`Classification::OwnAvailability`]) becomes own state ([`crate::classify::OwnKind::OwnAvailability`]) when its
    /// endpoint is an enrolled one ([`Self::enrol`]) and the fence could read the request's body with certainty
    /// (`body_unread` false: [`crate::fence_http::PausedRequest::body_unread`]), and [`Classification::Unknown`]
    /// otherwise; anything else stays as it is.
    #[must_use]
    pub fn resolve(&self, classification: Classification, body_unread: bool) -> Classification {
        match classification {
            Classification::OwnAvailability { endpoint } if !body_unread && self.has(&endpoint) => {
                Classification::Own(crate::classify::OwnKind::OwnAvailability)
            }
            Classification::OwnAvailability { .. } => Classification::Unknown,
            other => other,
        }
    }

    /// Enrols the endpoint of a messaging registration the fence let out (call it only once its verdict was `Allow`):
    /// its `registrationId`, when `crate::calling::messaging_endpoint` reads one; nothing for any other request.
    pub fn enrol(&self, method: &str, url: &str, body: Option<&[u8]>) {
        let endpoint = Url::parse(url)
            .ok()
            .and_then(|url| crate::calling::messaging_endpoint(method, &url, body));
        if let Some(endpoint) = endpoint {
            self.registered
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(endpoint);
        }
    }

    fn has(&self, endpoint: &str) -> bool {
        self.registered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(endpoint)
    }
}
