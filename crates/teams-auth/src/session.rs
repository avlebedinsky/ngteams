//! One signed-in account's tokens while a client runs (M1 Task 4).
//!
//! - **An access token per audience**, from the account's one stored refresh token, kept in memory only and refreshed
//!   once it is within the session's margin ([`REFRESH_MARGIN`] by default) of its expiry. However many callers ask
//!   for an audience at once, one refresh grant serves them all; grants for different audiences are serialized too,
//!   so a refresh token Entra rotates is read and replaced by one grant at a time (the rotated one is stored by
//!   [`TokenClient::refresh`]).
//! - **A forced refresh for a caller's 401** ([`Session::refresh_rejected`], [`Session::skypetoken_rejected`]): the
//!   caller names the token that was refused, so callers refused with the same token share one refresh, and one that
//!   comes after a newer token is already there just gets that token.
//! - **The skypetoken** ([`Session::skypetoken`]) is the one stored by `proto-lab auth` while it is good for longer
//!   than the margin. A renewal always refreshes `api.spaces.skype.com` first, and posts to authz
//!   ([`AuthzKind::TokenRefresh`]) only with that fresh token: one that runs out within the margin is refused before
//!   anything is sent ([`TokenError::StaleEntraToken`]), since authz copies the Entra token's expiry into the
//!   skypetoken. A renewed skypetoken never outlives its Entra token (its expiry is
//!   capped at it) and replaces the stored one; its answer's region replaces the session's.
//! - **Retries** (the session's part of the plan's retry table): none hidden here. Entra's or authz's 429 and 503 are
//!   [`TokenError::Throttled`] with the `Retry-After` given, for the caller to wait out. Claims challenges aren't
//!   handled: the native client doesn't advertise CAE (the chat service took the non-CAE `ic3` token, M1 Task 3).
//! - **Renewal notifications** ([`Session::subscribe`]): every refreshed access token and every renewed skypetoken is
//!   announced by audience and expiry, never by value; a subscriber (Trouter re-authenticating) asks the session for
//!   the token itself.
//! - **The region** comes from the account's stored [`Region`] (`proto-lab auth` persists it), and service bases are
//!   read through [`Region::base`]: trimmed, with a fallback for names the region lacks.
//!
//! Nothing here logs. No `Debug` output or error carries a token: [`Secret`] redacts itself, and the session's own
//! `Debug` names only the account, the margin and which audiences it holds.
//!
//! Sign-in and renewal POSTs (Entra's token endpoint, authz) go through [`TokenClient`] and [`AuthzClient`], the
//! reviewed exception to the native request policy: they touch only the account's own credentials.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use tokio::sync::{Mutex as AsyncMutex, broadcast};

use crate::authz::{AuthzClient, AuthzKind, Region, Skypetoken, skypetoken_key};
use crate::secrets::{Secret, SecretStore};
use crate::token::{SPACES_RESOURCE, Scope, TokenClient, TokenError, Tokens};

/// How long before its expiry a token is refreshed by default.
pub const REFRESH_MARGIN: Duration = Duration::from_mins(5);

/// How many renewal notifications a slow subscriber may fall behind before it misses some
/// ([`broadcast::error::RecvError::Lagged`]); a missed one only means asking the session again.
const RENEWAL_BACKLOG: usize = 16;

/// The time the session reckons expiries by: the system's, or a fake one in tests.
pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
}

/// The system's clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// An access token for one audience. `Debug` shows no token.
#[derive(Clone, Debug)]
pub struct AccessToken {
    pub token: Secret,
    /// The time of the request plus `expires_in`: on the early side.
    pub expires_at: SystemTime,
    /// The granted scopes, as Entra lists them.
    pub scopes: Vec<String>,
}

impl From<Tokens> for AccessToken {
    fn from(tokens: Tokens) -> Self {
        Self {
            token: tokens.access_token,
            expires_at: tokens.expires_at,
            scopes: tokens.scopes,
        }
    }
}

/// A renewal a subscriber hears of: which token, and until when it is good. Never the token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Renewal {
    /// A new access token for `resource`.
    AccessToken { resource: String, expires_at: SystemTime },
    /// A new skypetoken.
    Skypetoken { expires_at: SystemTime },
}

/// The tokens of one account (see the module docs). Shared between tasks behind an `Arc`; every method takes `&self`.
pub struct Session<S> {
    account: String,
    store: S,
    tokens: TokenClient,
    authz: AuthzClient,
    clock: Arc<dyn Clock>,
    margin: Duration,
    /// One slot per audience, each behind its own lock: the refresh of one audience doesn't hold up another's cached
    /// token.
    audiences: Mutex<BTreeMap<String, Arc<AsyncMutex<Option<AccessToken>>>>>,
    /// Held around every refresh grant, so the stored refresh token is read and replaced by one grant at a time.
    grants: AsyncMutex<()>,
    skypetoken: AsyncMutex<Option<Skypetoken>>,
    region: Mutex<Region>,
    renewals: broadcast::Sender<Renewal>,
}

impl<S> fmt::Debug for Session<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let audiences: Vec<String> = lock(&self.audiences).keys().cloned().collect();
        f.debug_struct("Session")
            .field("account", &self.account)
            .field("margin", &self.margin)
            .field("audiences", &audiences)
            .finish_non_exhaustive()
    }
}

impl<S: SecretStore> Session<S> {
    /// A session for `account` (its UPN, the name its refresh token and skypetoken are stored under in `store`), with
    /// the account's stored `region`, the system clock and [`REFRESH_MARGIN`].
    #[must_use]
    pub fn new(account: impl Into<String>, store: S, tokens: TokenClient, authz: AuthzClient, region: Region) -> Self {
        let (renewals, _) = broadcast::channel(RENEWAL_BACKLOG);
        Self {
            account: account.into(),
            store,
            tokens,
            authz,
            clock: Arc::new(SystemClock),
            margin: REFRESH_MARGIN,
            audiences: Mutex::default(),
            grants: AsyncMutex::new(()),
            skypetoken: AsyncMutex::new(None),
            region: Mutex::new(region),
            renewals,
        }
    }

    /// Another clock (a fake one in tests).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Another refresh margin.
    #[must_use]
    pub fn with_margin(mut self, margin: Duration) -> Self {
        self.margin = margin;
        self
    }

    #[must_use]
    pub fn account(&self) -> &str {
        &self.account
    }

    #[must_use]
    pub fn store(&self) -> &S {
        &self.store
    }

    /// The account's region: the stored one, or the latest skypetoken renewal's.
    #[must_use]
    pub fn region(&self) -> Region {
        lock(&self.region).clone()
    }

    /// The base URL of a regional service ([`Region::base`]: trimmed, with a fallback for absent names).
    #[must_use]
    pub fn service_base(&self, service: &str) -> Option<String> {
        lock(&self.region).base(service).map(str::to_owned)
    }

    /// Renewals from now on (see [`Renewal`]).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }

    /// An access token for `resource` (such as `https://ic3.teams.office.com`): the one in memory while it is good for
    /// longer than the margin, else a fresh one from the stored refresh token.
    ///
    /// # Errors
    /// As [`TokenClient::refresh`]: [`TokenError::NotSignedIn`], Entra's refusal, [`TokenError::Throttled`] (not
    /// retried), or a store failure.
    pub async fn access_token(&self, resource: &str) -> Result<AccessToken, TokenError> {
        let slot = self.slot(resource);
        let mut cached = slot.lock().await;
        if let Some(token) = cached.as_ref().filter(|token| self.fresh(token.expires_at)) {
            return Ok(token.clone());
        }
        self.refresh_into(resource, &mut cached).await
    }

    /// A new access token for `resource` after a service refused `rejected` (a 401). Only while the session still
    /// holds `rejected` is a refresh sent; if another caller has already replaced it, the replacement comes back.
    ///
    /// # Errors
    /// As [`Self::access_token`].
    pub async fn refresh_rejected(&self, resource: &str, rejected: &Secret) -> Result<AccessToken, TokenError> {
        let slot = self.slot(resource);
        let mut cached = slot.lock().await;
        match cached.as_ref() {
            Some(token) if token.token.expose() == rejected.expose() => *cached = None,
            Some(token) if self.fresh(token.expires_at) => return Ok(token.clone()),
            _ => {}
        }
        self.refresh_into(resource, &mut cached).await
    }

    /// The account's skypetoken: the one in memory, else the stored one, while it is good for longer than the margin;
    /// else a renewal (see the module docs).
    ///
    /// # Errors
    /// The refresh's errors (as [`Self::access_token`]), [`TokenError::StaleEntraToken`] when the refreshed Entra token
    /// isn't new enough to renew with, authz's throttling as [`TokenError::Throttled`], any other refusal as
    /// [`TokenError::Authz`], or a store failure.
    pub async fn skypetoken(&self) -> Result<Skypetoken, TokenError> {
        let mut cached = self.skypetoken.lock().await;
        self.load_stored_skypetoken(&mut cached).await?;
        if let Some(skypetoken) = cached.as_ref().filter(|skypetoken| self.fresh(skypetoken.expires_at)) {
            return Ok(skypetoken.clone());
        }
        let renewed = self.renew_skypetoken().await?;
        *cached = Some(renewed.clone());
        Ok(renewed)
    }

    /// A renewed skypetoken after a service refused `rejected` (a 401), as [`Self::refresh_rejected`] does for an
    /// access token: only while the session still holds `rejected` (or holds nothing good) is it renewed; a newer one
    /// already there comes back as it is.
    ///
    /// # Errors
    /// As [`Self::skypetoken`].
    pub async fn skypetoken_rejected(&self, rejected: &Secret) -> Result<Skypetoken, TokenError> {
        let mut cached = self.skypetoken.lock().await;
        self.load_stored_skypetoken(&mut cached).await?;
        if let Some(skypetoken) = cached
            .as_ref()
            .filter(|skypetoken| skypetoken.token.expose() != rejected.expose() && self.fresh(skypetoken.expires_at))
        {
            return Ok(skypetoken.clone());
        }
        let renewed = self.renew_skypetoken().await?;
        *cached = Some(renewed.clone());
        Ok(renewed)
    }

    /// Fills an empty `cached` with the stored skypetoken, if there is one with a readable expiry.
    async fn load_stored_skypetoken(&self, cached: &mut Option<Skypetoken>) -> Result<(), TokenError> {
        if cached.is_none() {
            let stored = self.store.get(&skypetoken_key(&self.account)).await?;
            *cached = stored.and_then(|token| Skypetoken::from_stored(token, self.region()));
        }
        Ok(())
    }

    /// Refreshes `api.spaces.skype.com`, checks the new token is worth renewing with, exchanges it, caps the
    /// skypetoken's expiry at the Entra token's, stores it and takes its region.
    ///
    /// Only a token good for longer than the margin is exchanged: authz copies its expiry, so an older one would give
    /// a skypetoken due for renewal at once.
    async fn renew_skypetoken(&self) -> Result<Skypetoken, TokenError> {
        let spaces = {
            let slot = self.slot(SPACES_RESOURCE);
            let mut cached = slot.lock().await;
            self.refresh_into(SPACES_RESOURCE, &mut cached).await?
        };
        if !self.fresh(spaces.expires_at) {
            return Err(TokenError::StaleEntraToken);
        }
        let mut skypetoken = self
            .authz
            .exchange_at(&spaces.token, AuthzKind::TokenRefresh, self.clock.as_ref())
            .await?;
        skypetoken.expires_at = skypetoken.expires_at.min(spaces.expires_at);
        self.store
            .put(&skypetoken_key(&self.account), &skypetoken.token)
            .await?;
        *lock(&self.region) = skypetoken.region.clone();
        self.announce(Renewal::Skypetoken {
            expires_at: skypetoken.expires_at,
        });
        Ok(skypetoken)
    }

    /// One refresh grant for `resource` (under the grant lock), kept in `cached` and announced.
    async fn refresh_into(&self, resource: &str, cached: &mut Option<AccessToken>) -> Result<AccessToken, TokenError> {
        let tokens = {
            let _grant = self.grants.lock().await;
            self.tokens
                .refresh_at(
                    &self.store,
                    &self.account,
                    &Scope::for_resource(resource),
                    self.clock.as_ref(),
                )
                .await?
        };
        let token = AccessToken::from(tokens);
        *cached = Some(token.clone());
        self.announce(Renewal::AccessToken {
            resource: resource.to_owned(),
            expires_at: token.expires_at,
        });
        Ok(token)
    }

    fn slot(&self, resource: &str) -> Arc<AsyncMutex<Option<AccessToken>>> {
        Arc::clone(lock(&self.audiences).entry(resource.to_owned()).or_default())
    }

    /// Whether a token expiring at `expires_at` is good for longer than the margin.
    fn fresh(&self, expires_at: SystemTime) -> bool {
        expires_at
            .duration_since(self.clock.now())
            .is_ok_and(|left| left > self.margin)
    }

    fn announce(&self, renewal: Renewal) {
        // no subscriber is no error: nobody needs to hear of it
        let _ = self.renewals.send(renewal);
    }
}

/// A std lock whose holder can't leave its value half-updated (every use is one assignment or one map call), so a
/// poisoned one is taken as it is.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
