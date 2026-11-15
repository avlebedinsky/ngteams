//! The TURN relay's credentials (`docs/protocol/calling-echo.md`, "The TURN credentials"): `GET
//! https://teams.microsoft.com/trap/tokens` ([`RelayTokens`], a read on [`Service::Relay`]) answers one credential for
//! the `rtcmedia` realm, valid 7 days. The media session's TURN client authenticates to the relay with it. An own
//! server's answer (`docs/server.md`, "The TURN relay") also names its relay's URLs (`urls`), which Teams' doesn't:
//! a Teams client knows its relay's host.

use std::fmt;
use std::time::Duration;

use serde_json::Value;
use teams_auth::secrets::Secret;

use crate::endpoint::{Endpoint, Service};
use crate::read::Read;

/// `GET https://teams.microsoft.com/trap/tokens`: the TURN relay's credentials. Its answer is read with
/// [`RelayTokens::read_answer`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RelayTokens;

impl Read for RelayTokens {
    fn service(&self) -> Service {
        Service::Relay
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["tokens"])
    }
}

/// One TURN credential: the realm (without the quotes the service puts inside the string: `"rtcmedia"` →
/// `rtcmedia`), the user name and the password. `Debug` shows no password.
#[derive(Clone)]
pub struct RelayCredential {
    pub realm: String,
    pub username: String,
    pub password: Secret,
}

impl fmt::Debug for RelayCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayCredential")
            .field("realm", &self.realm)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// The answer of [`RelayTokens`]: its credentials, how long they hold, and the relay's URLs when the answer names
/// them (an own server's `urls`; none in Teams' answer).
#[derive(Clone, Debug)]
pub struct RelayAnswer {
    pub credentials: Vec<RelayCredential>,
    pub expires: Duration,
    pub urls: Vec<String>,
}

/// Why [`RelayTokens::read_answer`] read nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RelayAnswerError {
    /// Not `{"tokens": [{realm, username, password}, …], "expires": <seconds>}` with at least one credential, every
    /// field a non-empty string.
    #[error("the answer isn't the relay's credentials in their captured shape")]
    Malformed,
}

impl RelayTokens {
    /// Reads `{"tokens":[{"realm","username","password"}],"expires":<seconds>}`, and an own server's `"urls":
    /// [<url>, …]` beside them (each string kept; anything else there reads as no URL).
    ///
    /// # Errors
    /// [`RelayAnswerError::Malformed`] for any other answer.
    pub fn read_answer(body: &[u8]) -> Result<RelayAnswer, RelayAnswerError> {
        let answer: Value = serde_json::from_slice(body).map_err(|_| RelayAnswerError::Malformed)?;
        let credentials = answer
            .get("tokens")
            .and_then(Value::as_array)
            .ok_or(RelayAnswerError::Malformed)?
            .iter()
            .map(credential)
            .collect::<Option<Vec<_>>>()
            .filter(|credentials| !credentials.is_empty())
            .ok_or(RelayAnswerError::Malformed)?;
        let expires = answer
            .get("expires")
            .and_then(Value::as_u64)
            .ok_or(RelayAnswerError::Malformed)?;
        let urls = answer
            .get("urls")
            .and_then(Value::as_array)
            .map(|urls| urls.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        Ok(RelayAnswer {
            credentials,
            expires: Duration::from_secs(expires),
            urls,
        })
    }
}

/// One credential of the answer, each field a non-empty string.
fn credential(token: &Value) -> Option<RelayCredential> {
    let text = |key: &str| token.get(key).and_then(Value::as_str).filter(|text| !text.is_empty());
    let realm = text("realm")?;
    let realm = realm
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(realm);
    Some(RelayCredential {
        realm: realm.to_owned(),
        username: text("username")?.to_owned(),
        password: Secret::new(text("password")?),
    })
}
