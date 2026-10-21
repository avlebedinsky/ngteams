//! The account's `ic3` token as the push channel needs it: with its expiry (an open socket is never authenticated
//! again, so the client reconnects before the token runs out), a new one on demand, and the session's renewal
//! notifications.

use std::future::Future;

use teams_auth::secrets::{Secret, SecretStore};
use teams_auth::session::{AccessToken, Renewal, Session};
use teams_auth::token::TokenError;
use teams_proto::IC3_AUDIENCE;
use tokio::sync::broadcast;

/// The account's `ic3` tokens for Trouter. The account's [`Session`] is the one a client uses; any type may implement
/// it (a fake in tests), since a token decides nothing about where a frame goes or what it says.
pub trait TrouterCredentials: Send + Sync {
    /// The `ic3` token with its expiry.
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send;

    /// A new `ic3` token in place of `old`: the service refused it (an `unauthorized` disconnect), or it is about to
    /// run out. The session refreshes only while it still holds `old`.
    fn ic3_after(&self, old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send;

    /// The session's renewals from now on: a new `ic3` token means a reconnect with it.
    fn renewals(&self) -> broadcast::Receiver<Renewal>;
}

impl<S: SecretStore> TrouterCredentials for Session<S> {
    async fn ic3(&self) -> Result<AccessToken, TokenError> {
        self.access_token(IC3_AUDIENCE).await
    }

    async fn ic3_after(&self, old: &Secret) -> Result<AccessToken, TokenError> {
        self.refresh_rejected(IC3_AUDIENCE, old).await
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.subscribe()
    }
}
