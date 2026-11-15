//! `GET /trap/tokens`: the TURN relay's credentials, in Teams' shape (`docs/protocol/calling-echo.md`, "The TURN
//! credentials"). With coturn's `use-auth-secret` a credential is the TURN REST API's: the user name is its expiry
//! and the account (`<unix seconds>:<oid>`), the password the base64 HMAC-SHA1 of that under the shared secret, so
//! coturn checks it without asking the server and it runs out by itself.

use std::sync::Arc;

use aws_lc_rs::hmac;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use serde_json::json;

use super::config::TurnConfig;
use crate::app::App;
use crate::auth::extract::{Bearer, Ic3};
use crate::db::users;

/// A TURN REST API credential of `account` under `secret`, good until `expires` (Unix seconds): its user name and
/// its password.
#[must_use]
pub fn rest_credential(secret: &str, account: &str, expires: i64) -> (String, String) {
    let username = format!("{expires}:{account}");
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret.as_bytes());
    let tag = hmac::sign(&key, username.as_bytes());
    let password = base64::engine::general_purpose::STANDARD.encode(tag.as_ref());
    (username, password)
}

/// The credential `turn` gives `account` now, and for how many seconds it holds.
fn credential(turn: &TurnConfig, account: &str, now: i64) -> Option<(String, String, u64)> {
    if let Some(secret) = turn.secret.as_deref().filter(|secret| !secret.is_empty()) {
        let expires = now + i64::try_from(turn.ttl).unwrap_or(i64::MAX / 2);
        let (username, password) = rest_credential(secret, account, expires);
        return Some((username, password, turn.ttl));
    }
    let (username, password) = (turn.username.as_ref()?, turn.password.as_ref()?);
    Some((username.clone(), password.clone(), turn.ttl))
}

pub(super) async fn tokens(State(app): State<Arc<App>>, bearer: Bearer<Ic3>) -> Response {
    match users::by_id(&app.db, &bearer.claims.oid).await {
        Ok(Some(user)) if !user.disabled => {}
        Ok(_) => return StatusCode::FORBIDDEN.into_response(),
        Err(error) => {
            tracing::error!(%error, "relay credentials: database failure");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    let turn = &app.config.calling.turn;
    let Some((username, password, expires)) = credential(turn, &bearer.claims.oid, crate::db::now()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    Json(json!({
        "tokens": [{"realm": format!("\"{}\"", turn.realm), "username": username, "password": password}],
        "expires": expires,
        "urls": turn.urls,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rest_credential_is_the_hmac_sha1_of_its_user_name_under_the_secret() {
        // RFC 2202's second HMAC-SHA1 case, in the REST API's form
        let (username, password) = rest_credential("Jefe", "what do ya want for nothing?", 0);
        assert_eq!(username, "0:what do ya want for nothing?");
        let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, b"Jefe");
        let expected = hmac::sign(&key, username.as_bytes());
        assert_eq!(
            base64::engine::general_purpose::STANDARD.decode(password).unwrap(),
            expected.as_ref()
        );
        let known = hmac::sign(&key, b"what do ya want for nothing?");
        let rfc_2202: [u8; 20] = [
            0xef, 0xfc, 0xdf, 0x6a, 0xe5, 0xeb, 0x2f, 0xa2, 0xd2, 0x74, 0x16, 0xd5, 0xf1, 0x84, 0xdf, 0x9c, 0x25, 0x9a,
            0x7c, 0x79,
        ];
        assert_eq!(known.as_ref(), rfc_2202);
    }

    #[test]
    fn a_relay_gives_a_secrets_credential_before_a_fixed_one_and_none_without_either() {
        let mut turn = TurnConfig::default();
        assert!(credential(&turn, "a", 100).is_none());
        turn.username = Some("user".into());
        assert!(credential(&turn, "a", 100).is_none());
        turn.password = Some("pass".into());
        assert_eq!(
            credential(&turn, "a", 100),
            Some(("user".into(), "pass".into(), 86_400))
        );
        turn.secret = Some("shared".into());
        let (username, password, expires) = credential(&turn, "a", 100).unwrap();
        assert_eq!((username.as_str(), expires), ("86500:a", 86_400));
        assert_eq!(password, rest_credential("shared", "a", 86_500).1);
        assert!(!format!("{turn:?}").contains("shared"));
    }
}
