//! The code exchange and refresh grants with the refresh token kept in a [`SecretStore`].
//!
//! One refresh token per account, under the audience name [`REFRESH_TOKEN`]: the native client's refresh token is
//! multi-resource, so the same one is redeemed for every audience (`docs/protocol/auth.md`). Whenever Entra sends a new
//! one it replaces the stored one. Access tokens aren't stored; callers keep them in memory until `expires_at`.
//!
//! A refresh token is stored under an account only once the ID token of the same answer names that account
//! ([`IdTokenClaims::bind`]): `login_hint` doesn't decide who signs in, the profile's SSO session does, and another
//! account's refresh token under this name would silently act as that account from then on. An answer without an ID
//! token, or with one that can't be read, stores nothing either.
//!
//! An error never deletes the stored refresh token: a Conditional Access block on one audience leaves it good for the
//! others, and an expired one is replaced by the next sign-in anyway.

use crate::id_token::{Account, IdTokenClaims, IdTokenError};
use crate::secrets::{SecretKey, SecretStore};
use crate::session::{Clock, SystemClock};
use crate::token::{Authorization, Scope, TokenClient, TokenError, Tokens};

/// The audience name the refresh token is stored under.
pub const REFRESH_TOKEN: &str = "refresh_token";

/// Where `account`'s refresh token lives in a [`SecretStore`].
#[must_use]
pub fn refresh_token_key(account: &str) -> SecretKey {
    SecretKey::new(account, REFRESH_TOKEN)
}

/// A finished code exchange: the access token for the authorization's scope, and the account the ID token named.
#[derive(Clone, Debug)]
pub struct SignIn {
    pub tokens: Tokens,
    pub account: Account,
}

/// An own server's sign-in by user name and password ([`TokenClient::password_sign_in`]).
#[derive(Clone, Copy, Debug)]
pub struct PasswordSignIn<'a> {
    /// What the refresh token is stored under ([`refresh_token_key`]): the account qualified by its server.
    pub account: &'a str,
    /// The user name the server signs in, which its ID token must name.
    pub username: &'a str,
    /// The tenant and object id an earlier sign-in named, which this one must match.
    pub expected: Option<(&'a str, &'a str)>,
}

impl TokenClient {
    /// Signs in with a user name and password (the password grant; an own server's sign-in, never Microsoft's),
    /// checks that the answer's ID token names that user for this client (and the tenant and object id expected), and
    /// only then stores the refresh token under the sign-in's account. The password is sent once and kept nowhere.
    ///
    /// # Errors
    /// The server's refusal ([`TokenError::InvalidGrant`] for a wrong user name or password,
    /// [`TokenError::Throttled`] while the account is held off), an ID token that is missing or names another account,
    /// an answer without a refresh token, or a store failure. On every error but the last nothing is stored.
    pub async fn password_sign_in<S: SecretStore>(
        &self,
        store: &S,
        sign_in: PasswordSignIn<'_>,
        password: &crate::secrets::Secret,
        scope: &Scope,
    ) -> Result<SignIn, TokenError> {
        let grant = self.redeem_password(sign_in.username, password, scope).await?;
        let id_token = grant.tokens.id_token.as_ref().ok_or(IdTokenError::Missing)?;
        let (tenant, object_id) = sign_in
            .expected
            .map_or((None, None), |(tid, oid)| (Some(tid), Some(oid)));
        let account =
            IdTokenClaims::parse(id_token)?.bind_to(sign_in.username, tenant, object_id, self.config().client_id())?;
        let refresh_token = grant.refresh_token.ok_or(TokenError::MissingField("refresh_token"))?;
        store.put(&refresh_token_key(sign_in.account), &refresh_token).await?;
        Ok(SignIn {
            tokens: grant.tokens,
            account,
        })
    }

    /// Redeems the code from `authorization`'s redirect, checks that the answer's ID token names the authorization's
    /// account (and its expected tenant and object id, where given) and was issued to this client, and only then
    /// stores the refresh token under that account.
    ///
    /// The caller has already checked the redirect's `state` against [`Authorization::state`].
    ///
    /// # Errors
    /// Entra's refusal ([`TokenError::InvalidGrant`], [`TokenError::RedirectMismatch`], …), an ID token that is
    /// missing or unreadable ([`TokenError::IdToken`]) or names another account ([`TokenError::WrongAccount`]), an
    /// answer without a refresh token ([`TokenError::MissingField`]), or a store failure. On every error but the last
    /// nothing is stored, and a refresh token already stored for the account is left as it was.
    pub async fn exchange_code<S: SecretStore>(
        &self,
        store: &S,
        authorization: &Authorization,
        code: &str,
    ) -> Result<SignIn, TokenError> {
        let grant = self.redeem_code(authorization, code).await?;
        let id_token = grant.tokens.id_token.as_ref().ok_or(IdTokenError::Missing)?;
        let account = IdTokenClaims::parse(id_token)?.bind(authorization, self.config().client_id())?;
        let refresh_token = grant.refresh_token.ok_or(TokenError::MissingField("refresh_token"))?;
        store
            .put(&refresh_token_key(authorization.account()), &refresh_token)
            .await?;
        Ok(SignIn {
            tokens: grant.tokens,
            account,
        })
    }

    /// An access token for `scope` from `account`'s stored refresh token. A new refresh token from Entra replaces the
    /// stored one.
    ///
    /// # Errors
    /// [`TokenError::NotSignedIn`] when nothing is stored (no request is sent), Entra's refusal
    /// ([`TokenError::InvalidGrant`], [`TokenError::ConditionalAccess`], [`TokenError::Throttled`], …), or a store
    /// failure.
    pub async fn refresh<S: SecretStore>(&self, store: &S, account: &str, scope: &Scope) -> Result<Tokens, TokenError> {
        self.refresh_at(store, account, scope, &SystemClock).await
    }

    /// [`Self::refresh`], with the token's expiry counted from `clock`'s time of the request.
    pub(crate) async fn refresh_at<S: SecretStore>(
        &self,
        store: &S,
        account: &str,
        scope: &Scope,
        clock: &dyn Clock,
    ) -> Result<Tokens, TokenError> {
        let key = refresh_token_key(account);
        let current = store.get(&key).await?.ok_or_else(|| TokenError::NotSignedIn {
            account: account.to_owned(),
        })?;
        let grant = self.redeem_refresh_token(&current, scope, clock).await?;
        if let Some(rotated) = grant.refresh_token
            && rotated.expose() != current.expose()
        {
            store.put(&key, &rotated).await?;
        }
        Ok(grant.tokens)
    }
}
