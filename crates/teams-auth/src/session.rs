//! One signed-in account's tokens while a client runs (M1 Task 4).
//!
//! - **An access token per audience**, from the account's one stored refresh token, kept in memory only and refreshed
//!   once it is within the session's margin ([`REFRESH_MARGIN`]) of its expiry. However many callers ask
//!   for an audience at once, one refresh grant serves them all; grants for different audiences are serialized too,
//!   so a refresh token Entra rotates is read and replaced by one grant at a time (the rotated one is stored by
//!   [`TokenClient::refresh`]).
//! - **A forced refresh for a caller's 401** ([`Session::refresh_rejected`], [`Session::skypetoken_rejected`]): the
//!   caller names the token that was refused, so callers refused with the same token share one refresh, and one that
//!   comes after a newer token is already there just gets that token. A refused skypetoken stays unusable until a
//!   renewal succeeds, even if that renewal fails and the refused one is still in memory or in the store.
//! - **The skypetoken** ([`Session::skypetoken`]) is the one stored by `proto-lab auth` while it is good for longer
//!   than the margin. A renewal always refreshes `api.spaces.skype.com` first, and posts to authz
//!   ([`AuthzKind::TokenRefresh`]) only with that fresh token: one that runs out within the margin is refused before
//!   anything is sent ([`TokenError::StaleEntraToken`]), since authz copies the Entra token's expiry into the
//!   skypetoken. A renewed skypetoken never outlives its Entra token (its expiry is
//!   capped at it) and replaces the stored one, its capped expiry stored next to it so that a later session reads it
//!   back as it was kept; its answer's region replaces the session's.
//! - **Retries** (the session's part of the plan's retry table): none hidden here. Entra's or authz's 429 and 503 are
//!   [`TokenError::Throttled`] with the `Retry-After` given, for the caller to wait out. The session keeps to that
//!   wait itself too, per service: callers that were already waiting when the throttling answer came get it as well,
//!   and until its `Retry-After` has passed every caller that would need that service gets
//!   [`TokenError::Throttled`] with the wait left, so nothing is sent to it (a renewal needing authz sends no Entra
//!   grant either). A throttling answer without `Retry-After` is shared only with the callers already waiting. Claims
//!   challenges aren't handled: the native client doesn't advertise CAE (the chat service took the non-CAE `ic3`
//!   token, M1 Task 3).
//! - **Renewal notifications** ([`Session::subscribe`]): every refreshed access token and every renewed skypetoken is
//!   announced by audience and expiry, never by value; a subscriber (Trouter re-authenticating) asks the session for
//!   the token itself.
//! - **Signed out** ([`Session::signed_out`], [`Session::watch_signed_out`]): once Entra refuses the stored refresh
//!   token ([`TokenError::InvalidGrant`]: expired, revoked, already used) or none is stored
//!   ([`TokenError::NotSignedIn`]), the session is signed out for good: every later call answers that error again at
//!   once, from memory, with nothing sent and no token handed out (a cached one included), and a watcher hears of it
//!   once. Only a new session (after signing in again) has tokens again.
//! - **The region** comes from the account's stored [`Region`] (`proto-lab auth` persists it), and service bases are
//!   read through [`Region::base`]: trimmed, with a fallback for names the region lacks.
//!
//! Nothing here logs. No `Debug` output or error carries a token: [`Secret`] redacts itself, and the session's own
//! `Debug` names only the account and which audiences it holds.
//!
//! Sign-in and renewal POSTs (Entra's token endpoint, authz) go through [`TokenClient`] and [`AuthzClient`], the
//! reviewed exception to the native request policy: they touch only the account's own credentials.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use tokio::sync::{Mutex as AsyncMutex, broadcast, watch};

use crate::authz::{AuthzClient, AuthzKind, Region, Skypetoken, skypetoken_expiry_key, skypetoken_key};
use crate::secrets::{Secret, SecretStore};
use crate::token::{EntraError, SPACES_RESOURCE, Scope, TokenClient, TokenError, Tokens};

/// How long before its expiry a token is refreshed.
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

/// Why a session is signed out: the account has to sign in again before it has a token (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignedOut {
    /// Entra refused the stored refresh token: it expired, was revoked or was already used.
    InvalidGrant(Box<EntraError>),
    /// No refresh token is stored for the account.
    NotSignedIn,
}

impl SignedOut {
    /// What signed the session out, if `error` is such an answer.
    fn from_error(error: &TokenError) -> Option<Self> {
        match error {
            TokenError::InvalidGrant(refusal) => Some(Self::InvalidGrant(refusal.clone())),
            TokenError::NotSignedIn { .. } => Some(Self::NotSignedIn),
            _ => None,
        }
    }

    /// The error every call answers once the session is signed out.
    fn error(&self, account: &str) -> TokenError {
        match self {
            Self::InvalidGrant(refusal) => TokenError::InvalidGrant(refusal.clone()),
            Self::NotSignedIn => TokenError::NotSignedIn {
                account: account.to_owned(),
            },
        }
    }
}

/// The tokens of one account (see the module docs). Shared between tasks behind an `Arc`; every method takes `&self`.
pub struct Session<S> {
    account: String,
    store: S,
    tokens: TokenClient,
    authz: AuthzClient,
    clock: Arc<dyn Clock>,
    /// One slot per audience, each behind its own lock: the refresh of one audience doesn't hold up another's cached
    /// token.
    audiences: Mutex<BTreeMap<String, Arc<AsyncMutex<Option<AccessToken>>>>>,
    /// Held around every refresh grant, so the stored refresh token is read and replaced by one grant at a time.
    grants: AsyncMutex<()>,
    skypetoken: AsyncMutex<SkypetokenSlot>,
    /// Entra's throttling answers (any audience's grant).
    entra_backoff: Backoff,
    /// Authz's throttling answers.
    authz_backoff: Backoff,
    region: Mutex<Region>,
    renewals: broadcast::Sender<Renewal>,
    /// Set once, for good, when a grant finds the account signed out.
    signed_out: watch::Sender<Option<SignedOut>>,
}

impl<S> fmt::Debug for Session<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let audiences: Vec<String> = lock(&self.audiences).keys().cloned().collect();
        f.debug_struct("Session")
            .field("account", &self.account)
            .field("audiences", &audiences)
            .field("signed_out", &self.signed_out.borrow().is_some())
            .finish_non_exhaustive()
    }
}

impl<S: SecretStore> Session<S> {
    /// A session for `account` (its UPN, the name its refresh token and skypetoken are stored under in `store`), with
    /// the account's stored `region` and the system clock.
    #[must_use]
    pub fn new(account: impl Into<String>, store: S, tokens: TokenClient, authz: AuthzClient, region: Region) -> Self {
        let (renewals, _) = broadcast::channel(RENEWAL_BACKLOG);
        Self {
            account: account.into(),
            store,
            tokens,
            authz,
            clock: Arc::new(SystemClock),
            audiences: Mutex::default(),
            grants: AsyncMutex::new(()),
            skypetoken: AsyncMutex::default(),
            entra_backoff: Backoff::default(),
            authz_backoff: Backoff::default(),
            region: Mutex::new(region),
            renewals,
            signed_out: watch::Sender::new(None),
        }
    }

    /// Another clock (a fake one in tests).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
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

    /// Why the session is signed out, once it is (see the module docs).
    #[must_use]
    pub fn signed_out(&self) -> Option<SignedOut> {
        self.signed_out.borrow().clone()
    }

    /// The session's signed-out state from now on: `None` until it is signed out, then why, for good.
    #[must_use]
    pub fn watch_signed_out(&self) -> watch::Receiver<Option<SignedOut>> {
        self.signed_out.subscribe()
    }

    /// An access token for `resource` (such as `https://ic3.teams.office.com`): the one in memory while it is good for
    /// longer than the margin, else a fresh one from the stored refresh token.
    ///
    /// # Errors
    /// As [`TokenClient::refresh`]: [`TokenError::NotSignedIn`], Entra's refusal, [`TokenError::Throttled`] (not
    /// retried), or a store failure. Once signed out, that error again, at once.
    pub async fn access_token(&self, resource: &str) -> Result<AccessToken, TokenError> {
        self.check_signed_in()?;
        let ticket = self.entra_backoff.ticket();
        let slot = self.slot(resource);
        let mut cached = slot.lock().await;
        // a caller that waited for the slot while another call signed the session out gets no token, a cached one too
        self.check_signed_in()?;
        if let Some(token) = cached.as_ref().filter(|token| self.fresh(token.expires_at)) {
            return Ok(token.clone());
        }
        self.refresh_into(resource, &mut cached, ticket).await
    }

    /// A new access token for `resource` after a service refused `rejected` (a 401). Only while the session still
    /// holds `rejected` is a refresh sent; if another caller has already replaced it, the replacement comes back.
    ///
    /// # Errors
    /// As [`Self::access_token`].
    pub async fn refresh_rejected(&self, resource: &str, rejected: &Secret) -> Result<AccessToken, TokenError> {
        self.check_signed_in()?;
        let ticket = self.entra_backoff.ticket();
        let slot = self.slot(resource);
        let mut cached = slot.lock().await;
        // a caller that waited for the slot while another call signed the session out gets no token, a cached one too
        self.check_signed_in()?;
        match cached.as_ref() {
            Some(token) if token.token.expose() == rejected.expose() => *cached = None,
            Some(token) if self.fresh(token.expires_at) => return Ok(token.clone()),
            _ => {}
        }
        self.refresh_into(resource, &mut cached, ticket).await
    }

    /// The account's skypetoken: the one in memory, else the stored one, while it is good for longer than the margin
    /// and isn't one a service refused since the last renewal; else a renewal (see the module docs).
    ///
    /// # Errors
    /// The refresh's errors (as [`Self::access_token`]), [`TokenError::StaleEntraToken`] when the refreshed Entra token
    /// isn't new enough to renew with, authz's throttling as [`TokenError::Throttled`], any other refusal as
    /// [`TokenError::Authz`], or a store failure.
    pub async fn skypetoken(&self) -> Result<Skypetoken, TokenError> {
        self.check_signed_in()?;
        let tickets = self.tickets();
        let mut slot = self.skypetoken.lock().await;
        self.check_signed_in()?;
        self.load_stored_skypetoken(&mut slot).await?;
        if let Some(skypetoken) = self.usable(&slot) {
            return Ok(skypetoken.clone());
        }
        self.renew_into(&mut slot, tickets).await
    }

    /// A renewed skypetoken after a service refused `rejected` (a 401), as [`Self::refresh_rejected`] does for an
    /// access token: only while the session still holds `rejected` (or holds nothing good) is it renewed; a newer one
    /// already there comes back as it is. A held `rejected` is never handed out again, by this or
    /// [`Self::skypetoken`], until a renewal succeeds.
    ///
    /// # Errors
    /// As [`Self::skypetoken`].
    pub async fn skypetoken_rejected(&self, rejected: &Secret) -> Result<Skypetoken, TokenError> {
        self.check_signed_in()?;
        let tickets = self.tickets();
        let mut slot = self.skypetoken.lock().await;
        self.check_signed_in()?;
        self.load_stored_skypetoken(&mut slot).await?;
        if let Some(skypetoken) = self
            .usable(&slot)
            .filter(|skypetoken| skypetoken.token.expose() != rejected.expose())
        {
            return Ok(skypetoken.clone());
        }
        // the held one, when refused, is unusable from now on whatever the renewal brings: a failed renewal leaves it in
        // memory and in the store (one refused earlier stays so too)
        if slot
            .held
            .as_ref()
            .is_some_and(|held| held.token.expose() == rejected.expose())
        {
            slot.refused = Some(rejected.clone());
        }
        self.renew_into(&mut slot, tickets).await
    }

    /// Fills an empty slot with the stored skypetoken, if there is one with a readable expiry, stored next to it.
    async fn load_stored_skypetoken(&self, slot: &mut SkypetokenSlot) -> Result<(), TokenError> {
        if slot.held.is_none() {
            let stored = self.store.get(&skypetoken_key(&self.account)).await?;
            let expiry = self.store.get(&skypetoken_expiry_key(&self.account)).await?;
            slot.held = stored.and_then(|token| Skypetoken::from_stored(token, expiry.as_ref(), self.region()));
        }
        Ok(())
    }

    /// The held skypetoken, while it is good past the margin and isn't the one last refused.
    fn usable<'slot>(&self, slot: &'slot SkypetokenSlot) -> Option<&'slot Skypetoken> {
        slot.held.as_ref().filter(|skypetoken| {
            self.fresh(skypetoken.expires_at)
                && slot
                    .refused
                    .as_ref()
                    .is_none_or(|refused| refused.expose() != skypetoken.token.expose())
        })
    }

    /// A renewal kept in the slot, which then has no refused token any more.
    async fn renew_into(&self, slot: &mut SkypetokenSlot, tickets: Tickets) -> Result<Skypetoken, TokenError> {
        let renewed = self.renew_skypetoken(tickets).await?;
        slot.held = Some(renewed.clone());
        slot.refused = None;
        Ok(renewed)
    }

    /// The caller's place in both services' throttling answers, taken before it waits for any lock.
    fn tickets(&self) -> Tickets {
        Tickets {
            entra: self.entra_backoff.ticket(),
            authz: self.authz_backoff.ticket(),
        }
    }

    /// Refreshes `api.spaces.skype.com`, checks the new token is worth renewing with, exchanges it, caps the
    /// skypetoken's expiry at the Entra token's, stores it and takes its region.
    ///
    /// Only a token good for longer than the margin is exchanged: authz copies its expiry, so an older one would give
    /// a skypetoken due for renewal at once. While authz throttles, nothing is sent, the Entra grant included.
    async fn renew_skypetoken(&self, tickets: Tickets) -> Result<Skypetoken, TokenError> {
        self.authz_backoff.check(tickets.authz, self.clock.now())?;
        let spaces = {
            let slot = self.slot(SPACES_RESOURCE);
            let mut cached = slot.lock().await;
            self.refresh_into(SPACES_RESOURCE, &mut cached, tickets.entra).await?
        };
        if !self.fresh(spaces.expires_at) {
            return Err(TokenError::StaleEntraToken);
        }
        let exchanged = self
            .authz
            .exchange_at(&spaces.token, AuthzKind::TokenRefresh, self.clock.as_ref())
            .await
            .map_err(TokenError::from);
        let mut skypetoken = self.authz_backoff.record(exchanged, self.clock.now())?;
        skypetoken.expires_at = skypetoken.expires_at.min(spaces.expires_at);
        skypetoken.store(&self.store, &self.account).await?;
        *lock(&self.region) = skypetoken.region.clone();
        self.announce(Renewal::Skypetoken {
            expires_at: skypetoken.expires_at,
        });
        Ok(skypetoken)
    }

    /// One refresh grant for `resource` (under the grant lock), kept in `cached` and announced; none while Entra
    /// throttles (`ticket`: see [`Backoff`]).
    async fn refresh_into(
        &self,
        resource: &str,
        cached: &mut Option<AccessToken>,
        ticket: u64,
    ) -> Result<AccessToken, TokenError> {
        let tokens = {
            let _grant = self.grants.lock().await;
            // a caller that waited for the grant lock while another grant signed the session out sends nothing
            self.check_signed_in()?;
            self.entra_backoff.check(ticket, self.clock.now())?;
            let refreshed = self
                .tokens
                .refresh_at(
                    &self.store,
                    &self.account,
                    &Scope::for_resource(resource),
                    self.clock.as_ref(),
                )
                .await;
            if let Err(error) = &refreshed {
                self.sign_out_on(error);
            }
            self.entra_backoff.record(refreshed, self.clock.now())?
        };
        let token = AccessToken::from(tokens);
        *cached = Some(token.clone());
        self.announce(Renewal::AccessToken {
            resource: resource.to_owned(),
            expires_at: token.expires_at,
        });
        Ok(token)
    }

    /// [`SignedOut::error`] once the session is signed out.
    fn check_signed_in(&self) -> Result<(), TokenError> {
        match &*self.signed_out.borrow() {
            Some(signed_out) => Err(signed_out.error(&self.account)),
            None => Ok(()),
        }
    }

    /// Signs the session out for good when `error` says the account has to sign in again; told to watchers once.
    fn sign_out_on(&self, error: &TokenError) {
        if let Some(signed_out) = SignedOut::from_error(error) {
            self.signed_out.send_if_modified(|state| {
                let first = state.is_none();
                if first {
                    *state = Some(signed_out);
                }
                first
            });
        }
    }

    fn slot(&self, resource: &str) -> Arc<AsyncMutex<Option<AccessToken>>> {
        Arc::clone(lock(&self.audiences).entry(resource.to_owned()).or_default())
    }

    /// Whether a token expiring at `expires_at` is good for longer than the margin.
    fn fresh(&self, expires_at: SystemTime) -> bool {
        expires_at
            .duration_since(self.clock.now())
            .is_ok_and(|left| left > REFRESH_MARGIN)
    }

    fn announce(&self, renewal: Renewal) {
        // no subscriber is no error: nobody needs to hear of it
        let _ = self.renewals.send(renewal);
    }
}

/// The skypetoken held, and the one a service refused last, which stays unusable until a renewal succeeds.
#[derive(Default)]
struct SkypetokenSlot {
    held: Option<Skypetoken>,
    refused: Option<Secret>,
}

/// A caller's [`Backoff::ticket`] for each service a skypetoken renewal may need.
#[derive(Clone, Copy)]
struct Tickets {
    entra: u64,
    authz: u64,
}

/// One service's throttling answers, so callers don't each send while it throttles.
///
/// A caller takes a ticket (how many throttling answers there have been) before it waits for any lock; once it holds
/// the lock, a throttling answer that came while it waited, or one whose `Retry-After` hasn't passed, is its answer
/// too, and it sends nothing.
#[derive(Default)]
struct Backoff(Mutex<Throttling>);

#[derive(Default)]
struct Throttling {
    /// Throttling answers so far.
    answers: u64,
    /// Until when the last one asked to be left alone (`None` without `Retry-After`).
    until: Option<SystemTime>,
}

impl Backoff {
    fn ticket(&self) -> u64 {
        lock(&self.0).answers
    }

    /// [`TokenError::Throttled`] with the wait left, without anything sent, when the caller with `ticket` must not
    /// send.
    fn check(&self, ticket: u64, now: SystemTime) -> Result<(), TokenError> {
        let throttling = lock(&self.0);
        let left = throttling
            .until
            .map(|until| until.duration_since(now).unwrap_or_default());
        let waiting = left.is_some_and(|left| !left.is_zero());
        if throttling.answers > ticket || waiting {
            return Err(TokenError::Throttled {
                retry_after: left,
                error: None,
            });
        }
        Ok(())
    }

    /// Notes a throttling answer of the service (answered at `now`) and hands the result back as it is.
    fn record<T>(&self, result: Result<T, TokenError>, now: SystemTime) -> Result<T, TokenError> {
        if let Err(TokenError::Throttled { retry_after, .. }) = &result {
            let mut throttling = lock(&self.0);
            throttling.answers += 1;
            throttling.until = retry_after.and_then(|wait| now.checked_add(wait));
        }
        result
    }
}

/// A std lock whose holder can't leave its value half-updated (every use is one assignment or one map call), so a
/// poisoned one is taken as it is.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
