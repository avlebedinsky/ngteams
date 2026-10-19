//! The account an ID token names, and the check that binds a code exchange to the account it was asked for.
//!
//! `login_hint` only pre-fills the sign-in page: whoever holds the browser profile's SSO session is the one who signs
//! in. So before [`TokenClient::exchange_code`](crate::token::TokenClient::exchange_code) stores a refresh token under
//! an account, the ID token of the same answer has to name that account ([`IdTokenClaims::bind`]).
//!
//! The claims are read without checking the token's signature. The ID token comes straight from the token endpoint,
//! over TLS, in the answer to our own PKCE-bound code redemption, so the channel already authenticates it (OIDC
//! Core 3.1.3.7 lets a code-flow client rely on TLS server validation instead of the signature). The check is
//! there to catch the wrong account signed in to a profile, not a forged answer. Nothing here fetches signing keys.
//!
//! No error carries the token or a piece of it: a malformed token is reported by kind, a wrong claim by its name and
//! value (an object id, a tenant id, a UPN or the client ID, none of them secret).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value};

use crate::secrets::Secret;
use crate::token::{Authorization, TokenError};

/// Why an ID token can't be read. Never carries the token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdTokenError {
    /// The code exchange answered without an ID token, so the account can't be checked.
    #[error("the token answer has no ID token, so the signed-in account can't be checked")]
    Missing,
    /// Not a signed JWT (three base64url parts) with a JSON object as its payload.
    #[error("the ID token is not a JWT with a JSON claims object")]
    Malformed,
    /// A claim the account check needs is missing, empty or not a string.
    #[error("the ID token has no valid {0} claim")]
    MissingClaim(&'static str),
}

/// The claims of an ID token that say who signed in and for which client. Read with no signature check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdTokenClaims {
    /// `aud`: the client ID(s) the token was issued to.
    pub audiences: Vec<String>,
    /// `oid`: the account's Entra object id.
    pub object_id: String,
    /// `tid`: the account's tenant id.
    pub tenant: String,
    /// `preferred_username`: the UPN, for a work account.
    pub preferred_username: Option<String>,
    /// `upn`, an optional claim.
    pub upn: Option<String>,
}

/// The account a code exchange signed in, as its ID token named it once it matched the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    /// The UPN as the ID token spells it (`preferred_username`, else `upn`).
    pub username: String,
    pub object_id: String,
    pub tenant: String,
}

impl IdTokenClaims {
    /// The claims of `id_token`, decoded locally without checking the signature (see the module doc).
    ///
    /// # Errors
    /// [`IdTokenError::Malformed`] when it isn't a three-part JWT with a JSON object payload,
    /// [`IdTokenError::MissingClaim`] when `aud`, `oid` or `tid` is missing or empty.
    pub fn parse(id_token: &Secret) -> Result<Self, IdTokenError> {
        let claims = payload(id_token.expose()).ok_or(IdTokenError::Malformed)?;
        Ok(Self {
            audiences: audiences(&claims).ok_or(IdTokenError::MissingClaim("aud"))?,
            object_id: text(&claims, "oid").ok_or(IdTokenError::MissingClaim("oid"))?,
            tenant: text(&claims, "tid").ok_or(IdTokenError::MissingClaim("tid"))?,
            preferred_username: text(&claims, "preferred_username"),
            upn: text(&claims, "upn"),
        })
    }

    /// The account these claims name, if it is the one `authorization` asked for on behalf of `client_id`:
    /// - `aud` includes `client_id`;
    /// - at least one of `preferred_username` and `upn` is present, and every one present equals the authorization's
    ///   account (ASCII case aside);
    /// - `tid` and `oid` equal the authorization's expected tenant and object id, where it has them
    ///   ([`Authorization::expect_tenant`], [`Authorization::expect_object_id`]; GUIDs, case aside).
    ///
    /// # Errors
    /// [`TokenError::IdToken`] when no name claim is present, [`TokenError::WrongAccount`] naming the first claim
    /// that doesn't match.
    pub fn bind(self, authorization: &Authorization, client_id: &str) -> Result<Account, TokenError> {
        if !self.audiences.iter().any(|audience| audience == client_id) {
            return Err(wrong("aud", client_id, &self.audiences.join(" ")));
        }
        let account = authorization.account();
        let names = [
            ("preferred_username", self.preferred_username.as_deref()),
            ("upn", self.upn.as_deref()),
        ];
        let mut username = None;
        for (claim, name) in names {
            let Some(name) = name else { continue };
            if !name.eq_ignore_ascii_case(account) {
                return Err(wrong(claim, account, name));
            }
            username.get_or_insert(name);
        }
        let username = username
            .ok_or(IdTokenError::MissingClaim("preferred_username"))?
            .to_owned();
        expect(authorization.expected_tenant(), "tid", &self.tenant)?;
        expect(authorization.expected_object_id(), "oid", &self.object_id)?;
        Ok(Account {
            username,
            object_id: self.object_id,
            tenant: self.tenant,
        })
    }
}

/// Checks `found` against an expected GUID claim, case aside; no expectation passes.
fn expect(expected: Option<&str>, claim: &'static str, found: &str) -> Result<(), TokenError> {
    match expected {
        Some(expected) if !expected.eq_ignore_ascii_case(found) => Err(wrong(claim, expected, found)),
        _ => Ok(()),
    }
}

fn wrong(claim: &'static str, expected: &str, found: &str) -> TokenError {
    TokenError::WrongAccount {
        claim,
        expected: expected.to_owned(),
        found: found.to_owned(),
    }
}

/// The JSON object in a JWS compact token's payload part.
fn payload(token: &str) -> Option<Map<String, Value>> {
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
fn text(claims: &Map<String, Value>, name: &str) -> Option<String> {
    claims
        .get(name)?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// `aud`: one string, or a non-empty array of strings.
fn audiences(claims: &Map<String, Value>) -> Option<Vec<String>> {
    match claims.get("aud")? {
        Value::String(audience) if !audience.is_empty() => Some(vec![audience.clone()]),
        Value::Array(list) => {
            let audiences: Vec<String> = list
                .iter()
                .filter_map(Value::as_str)
                .filter(|audience| !audience.is_empty())
                .map(str::to_owned)
                .collect();
            (!audiences.is_empty()).then_some(audiences)
        }
        _ => None,
    }
}
