//! The code exchange and refresh grants with the refresh token kept in a [`SecretStore`].
//!
//! One refresh token per account, under the audience name [`REFRESH_TOKEN`]: the native client's refresh token is
//! multi-resource, so the same one is redeemed for every audience (`docs/protocol/auth.md`). Whenever Entra sends a new
//! one it replaces the stored one. Access tokens aren't stored; callers keep them in memory until `expires_at`.
//!
//! An error never deletes the stored refresh token: a Conditional Access block on one audience leaves it good for the
//! others, and an expired one is replaced by the next sign-in anyway.

use crate::secrets::{SecretKey, SecretStore};
use crate::token::{Authorization, Scope, TokenClient, TokenError, Tokens};

/// The audience name the refresh token is stored under.
pub const REFRESH_TOKEN: &str = "refresh_token";

/// Where `account`'s refresh token lives in a [`SecretStore`].
#[must_use]
pub fn refresh_token_key(account: &str) -> SecretKey {
    SecretKey::new(account, REFRESH_TOKEN)
}

impl TokenClient {
    /// Redeems the code from `authorization`'s redirect and stores the refresh token under the authorization's
    /// account. Returns the access token for the authorization's scope.
    ///
    /// The caller has already checked the redirect's `state` against [`Authorization::state`].
    ///
    /// # Errors
    /// Entra's refusal ([`TokenError::InvalidGrant`], [`TokenError::RedirectMismatch`], …), an answer without a refresh
    /// token ([`TokenError::MissingField`]; nothing is stored then), or a store failure.
    pub async fn exchange_code<S: SecretStore>(
        &self,
        store: &S,
        authorization: &Authorization,
        code: &str,
    ) -> Result<Tokens, TokenError> {
        let grant = self.redeem_code(authorization, code).await?;
        let refresh_token = grant.refresh_token.ok_or(TokenError::MissingField("refresh_token"))?;
        store
            .put(&refresh_token_key(authorization.account()), &refresh_token)
            .await?;
        Ok(grant.tokens)
    }

    /// An access token for `scope` from `account`'s stored refresh token. A new refresh token from Entra replaces the
    /// stored one.
    ///
    /// # Errors
    /// [`TokenError::NotSignedIn`] when nothing is stored (no request is sent), Entra's refusal
    /// ([`TokenError::InvalidGrant`], [`TokenError::ConditionalAccess`], [`TokenError::Throttled`], …), or a store
    /// failure.
    pub async fn refresh<S: SecretStore>(&self, store: &S, account: &str, scope: &Scope) -> Result<Tokens, TokenError> {
        let key = refresh_token_key(account);
        let current = store.get(&key).await?.ok_or_else(|| TokenError::NotSignedIn {
            account: account.to_owned(),
        })?;
        let grant = self.redeem_refresh_token(&current, scope).await?;
        if let Some(rotated) = grant.refresh_token
            && rotated.expose() != current.expose()
        {
            store.put(&key, &rotated).await?;
        }
        Ok(grant.tokens)
    }
}
