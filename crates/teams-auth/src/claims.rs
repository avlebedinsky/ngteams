//! Reading the claims of a token that is a JWT, without checking its signature, and the few claims of an access token
//! the lab may show: audience, expiry, scopes and tenant ([`TokenSummary`]).
//!
//! Nothing here returns the token or a piece of it. A token that isn't a JWT with a JSON object payload simply has no
//! claims.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value};

use crate::secrets::Secret;

/// The JSON object in a JWS compact token's payload part, if it is one.
pub(crate) fn payload(token: &str) -> Option<Map<String, Value>> {
    let mut parts = token.split('.');
    let (Some(_header), Some(payload), Some(_signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    match serde_json::from_slice(&bytes).ok()? {
        Value::Object(claims) => Some(claims),
        _ => None,
    }
}

/// A non-empty string claim.
pub(crate) fn text(claims: &Map<String, Value>, name: &str) -> Option<String> {
    claims
        .get(name)?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// A `NumericDate` claim (`exp`, `iat`): whole seconds since the epoch.
pub(crate) fn time(claims: &Map<String, Value>, name: &str) -> Option<SystemTime> {
    let seconds = claims.get(name)?.as_u64()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

/// What may be shown of an access token: its audience, expiry, scopes and tenant. Never the token.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenSummary {
    /// `aud`; several when the claim is a list.
    pub audience: Vec<String>,
    /// `exp`.
    pub expires_at: Option<SystemTime>,
    /// `scp`, split on spaces; app-only tokens have `roles` instead, which isn't read.
    pub scopes: Vec<String>,
    /// `tid`.
    pub tenant: Option<String>,
}

impl TokenSummary {
    /// The summary of `token`, or `None` when it isn't a JWT with a JSON claims object (an opaque token).
    #[must_use]
    pub fn read(token: &Secret) -> Option<Self> {
        let claims = payload(token.expose())?;
        let audience = match claims.get("aud") {
            Some(Value::String(audience)) => vec![audience.clone()],
            Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            _ => Vec::new(),
        };
        let scopes = text(&claims, "scp")
            .map(|scopes| scopes.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default();
        Some(Self {
            audience,
            expires_at: time(&claims, "exp"),
            scopes,
            tenant: text(&claims, "tid"),
        })
    }
}
