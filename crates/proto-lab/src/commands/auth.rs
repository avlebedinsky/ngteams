//! `proto-lab auth <a|b|c>`: the native sign-in of one lab account, with no human (M0 spike 1).
//!
//! 1. **Authorize** in the account's persistent profile, headless, under the armed fence (fail-closed, the seeded
//!    own-state list: no sign-in form gets through). The authorize URL ([`Authorization`]: the native Teams client, the
//!    `nativeclient` redirect, PKCE, `login_hint`) is opened in the profile, whose Microsoft session signs it in
//!    silently. The fence catches Entra's redirect to the registered redirect URI and fails it
//!    ([`RedirectCatch`]); the code comes from that URL once its `state` is the request's. If Entra shows a page
//!    instead (the session has expired, or it wants an interaction), the run stops: nothing is ever typed here.
//! 2. **Code → tokens** in Rust ([`TokenClient::exchange_code`]): the refresh token is stored only once the ID token
//!    names the account (and its object id from `lab.toml`).
//! 3. **Refresh** for `api.spaces.skype.com`, proving the stored refresh token works; with `--audiences`, one refresh
//!    grant per other audience of the auth notes' table, to see what Conditional Access allows.
//! 4. **Skypetoken** through authz ([`AuthzClient`]), stored next to the refresh token; the account's region is
//!    persisted to `<data dir>/auth/<alias>.json` ([`save_region`]).
//!
//! The CLI keeps the secrets in the Secret Service under the lab's own application attribute ([`KEYRING_APPLICATION`]);
//! tests pass a memory store. Nothing prints or logs a token, a code or the redirect URL: the report
//! ([`AuthReport::lines`]) shows only each token's audience, expiry, scopes and tenant, and the skypetoken's expiry and
//! region, or the refusal.

use std::fmt::Write as _;
use std::future::Future;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde_json::json;
use teams_auth::authz::{AuthzClient, AuthzError, AuthzKind, Region, Skypetoken, unix_seconds};
use teams_auth::claims::TokenSummary;
use teams_auth::pkce::PkceError;
use teams_auth::redirect::RedirectError;
use teams_auth::secrets::{Secret, SecretStore};
use teams_auth::token::{Authorization, EntraConfig, Scope, TokenClient, TokenError};
use tokio::sync::mpsc;

use crate::classify::Classifier;
use crate::config::{Alias, LabConfig, LabPaths};
use crate::fence_http::{CatchError, RedirectCatch};
use crate::session::{FenceReport, LabSession, SessionError, SessionState};

/// The `application` attribute of the lab's items in the Secret Service.
pub const KEYRING_APPLICATION: &str = "teams-lab";

/// The audiences `--audiences` asks a token for, besides `api.spaces.skype.com` (the auth notes' table).
pub const OTHER_AUDIENCES: [&str; 7] = [
    "https://chatsvcagg.teams.microsoft.com",
    "https://presence.teams.microsoft.com",
    "https://ic3.teams.office.com",
    "https://graph.microsoft.com",
    "https://outlook.office.com",
    "https://outlook.office.com/search",
    "https://substrate.office.com",
];

/// How long a sign-in page has to stand before the run gives up on a redirect: Entra may pass through a page of its
/// own that moves on by itself.
const SIGN_IN_GRACE: Duration = Duration::from_secs(5);
/// How often the session's events are looked at while waiting for the redirect.
const TICK: Duration = Duration::from_millis(250);

/// What to do beyond the sign-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthOptions {
    /// How long the browser step may take, from opening the authorize URL to the redirect.
    pub timeout: Duration,
    /// Exchange the access token for a skypetoken.
    pub skypetoken: bool,
    /// Ask a token for each of [`OTHER_AUDIENCES`] too.
    pub other_audiences: bool,
}

/// Why the sign-in didn't get as far as a token.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("can't make the authorization request: {0}")]
    Pkce(#[from] PkceError),
    #[error("can't catch the redirect: {0}")]
    Catch(#[from] CatchError),
    #[error("the redirect: {0}")]
    Redirect(#[from] RedirectError),
    #[error(transparent)]
    Token(#[from] TokenError),
    #[error(
        "Entra showed a page instead of redirecting: the profile's session has expired or needs an interaction (run \
         `proto-lab login`); nothing was typed"
    )]
    InteractionNeeded,
    #[error("the authorize page went to {0} instead of redirecting")]
    Elsewhere(String),
    #[error("no redirect within {0:?}")]
    Timeout(Duration),
    #[error("the fence lost its connection to the browser")]
    ConnectionLost,
    #[error("the fence was breached during the sign-in: {0}")]
    Breached(String),
    #[error("the account's MRI in lab.toml isn't 8:orgid:<object id>")]
    BadMri,
}

/// One audience's token, or why there is none.
#[derive(Debug)]
pub struct AudienceOutcome {
    pub resource: String,
    pub token: Result<Option<TokenSummary>, TokenError>,
}

/// What a sign-in got. Holds no token.
#[derive(Debug)]
pub struct AuthReport {
    /// What the fence did while the browser ran.
    pub fence: FenceReport,
    /// The code exchange's access token for `api.spaces.skype.com`.
    pub signed_in: Option<TokenSummary>,
    /// The access token from refreshing it.
    pub refreshed: Option<TokenSummary>,
    /// One per [`OTHER_AUDIENCES`] when asked.
    pub audiences: Vec<AudienceOutcome>,
    /// The skypetoken's expiry and the region, or the refusal; `None` when not asked.
    pub skypetoken: Option<Result<SkypetokenSummary, AuthzError>>,
}

/// What may be shown of a skypetoken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkypetokenSummary {
    pub expires_at: SystemTime,
    pub region: Region,
}

impl From<&Skypetoken> for SkypetokenSummary {
    fn from(skypetoken: &Skypetoken) -> Self {
        Self {
            expires_at: skypetoken.expires_at,
            region: skypetoken.region.clone(),
        }
    }
}

/// Signs `alias` in natively through its lab profile (see the module docs), keeping the refresh token and the
/// skypetoken in `store`.
///
/// # Errors
/// [`AuthError`] when no token came of it: the browser couldn't run under the fence or the fence was breached, Entra
/// didn't redirect, the redirect isn't this request's or carries Entra's refusal, or the code exchange or the first
/// refresh failed. A refused audience or skypetoken is in the report instead.
pub async fn authorize<S: SecretStore>(
    config: &LabConfig,
    alias: Alias,
    store: &S,
    options: &AuthOptions,
) -> Result<AuthReport, AuthError> {
    let account = config.account(alias);
    let entra = EntraConfig::teams_native();
    let mut authorization = Authorization::new(&entra, &account.upn, Scope::spaces())?;
    if let Some(mri) = &account.mri {
        let object_id = mri.strip_prefix("8:orgid:").ok_or(AuthError::BadMri)?;
        authorization = authorization.expect_object_id(object_id);
    }
    let (redirect, fence) =
        redirect_through_profile(config.paths(), alias, &entra, &authorization, options.timeout).await?;
    let code = authorization.code_from_redirect(redirect.expose())?;

    let client = TokenClient::new(entra)?;
    let sign_in = client.exchange_code(store, &authorization, code.expose()).await?;
    let refreshed = client.refresh(store, &account.upn, &Scope::spaces()).await?;
    let mut audiences = Vec::new();
    if options.other_audiences {
        for resource in OTHER_AUDIENCES {
            let token = client
                .refresh(store, &account.upn, &Scope::for_resource(resource))
                .await;
            audiences.push(AudienceOutcome {
                resource: resource.to_owned(),
                token: token.map(|tokens| TokenSummary::read(&tokens.access_token)),
            });
        }
    }
    let skypetoken = if options.skypetoken {
        Some(skypetoken(store, &account.upn, &refreshed.access_token).await)
    } else {
        None
    };
    Ok(AuthReport {
        fence,
        signed_in: TokenSummary::read(&sign_in.tokens.access_token),
        refreshed: TokenSummary::read(&refreshed.access_token),
        audiences,
        skypetoken,
    })
}

async fn skypetoken<S: SecretStore>(
    store: &S,
    account: &str,
    spaces_token: &Secret,
) -> Result<SkypetokenSummary, AuthzError> {
    let skypetoken = AuthzClient::new()?
        .exchange_and_store(store, account, spaces_token, AuthzKind::ExplicitLogin)
        .await?;
    Ok(SkypetokenSummary::from(&skypetoken))
}

/// Opens the authorize URL in `alias`'s profile under the armed fence and waits for the redirect the fence catches,
/// the two together bounded by `timeout` ([`within`]). The browser is closed whatever happened, and a breach outranks
/// any other outcome. The fence report comes back with the redirect only; on an error the report adds nothing to it.
async fn redirect_through_profile(
    paths: &LabPaths,
    alias: Alias,
    entra: &EntraConfig,
    authorization: &Authorization,
    timeout: Duration,
) -> Result<(Secret, FenceReport), AuthError> {
    let (catch, mut caught) = RedirectCatch::new(&entra.authorize_endpoint(), entra.redirect_uri())?;
    let mut session = LabSession::open_catching(paths, alias, Classifier::seeded(), catch).await?;
    let redirect = within(timeout, async {
        session.navigate(authorization.url()).await?;
        wait_for_redirect(&mut session, &mut caught).await
    })
    .await;
    let fence = session.close().await?;
    if fence.breached() {
        return Err(AuthError::Breached(fence.summary()));
    }
    Ok((redirect?, fence))
}

/// `step`, given up as [`AuthError::Timeout`] once `timeout` has passed, wherever it is waiting: the browser's
/// navigation (which doesn't answer until the page commits) as much as the wait for the redirect.
///
/// # Errors
/// `step`'s error, or [`AuthError::Timeout`].
pub async fn within<T>(timeout: Duration, step: impl Future<Output = Result<T, AuthError>>) -> Result<T, AuthError> {
    tokio::time::timeout(timeout, step)
        .await
        .unwrap_or(Err(AuthError::Timeout(timeout)))
}

/// Pumps the session's events until the fence catches the redirect. A sign-in page that stands for
/// [`SIGN_IN_GRACE`], a page on another host or a lost connection end the wait; the caller bounds it in time.
async fn wait_for_redirect(
    session: &mut LabSession,
    caught: &mut mpsc::UnboundedReceiver<Secret>,
) -> Result<Secret, AuthError> {
    let mut sign_in_since = None;
    loop {
        if let Ok(redirect) = caught.try_recv() {
            return Ok(redirect);
        }
        let now = tokio::time::Instant::now();
        match session.watch().state() {
            SessionState::SignInShown => {
                let since = *sign_in_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= SIGN_IN_GRACE {
                    return Err(AuthError::InteractionNeeded);
                }
            }
            SessionState::Elsewhere(host) => return Err(AuthError::Elsewhere(host)),
            _ => sign_in_since = None,
        }
        if !session.pump(TICK).await {
            return Err(AuthError::ConnectionLost);
        }
    }
}

/// Where [`save_region`] keeps an account's region.
#[must_use]
pub fn region_file(paths: &LabPaths, alias: Alias) -> PathBuf {
    paths.data_dir().join("auth").join(format!("{}.json", alias.as_str()))
}

/// Persists the account's region (`regionGtms` and all) and the skypetoken's expiry to [`region_file`]: the directory
/// `0700`, the file `0600`, replaced by rename from a staging file of its own, so saves that run at once never share
/// one (the last rename wins, whole). Not the token.
///
/// # Errors
/// When the file can't be written.
pub fn save_region(paths: &LabPaths, alias: Alias, skypetoken: &SkypetokenSummary) -> std::io::Result<PathBuf> {
    let file = region_file(paths, alias);
    let dir = file.parent().unwrap_or(paths.data_dir());
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let mut record = skypetoken.region.to_json();
    record["account"] = json!(alias.as_str());
    record["fetchedAt"] = json!(unix_seconds(SystemTime::now()));
    record["skypetokenExpiresAt"] = json!(unix_seconds(skypetoken.expires_at));
    let text = serde_json::to_string_pretty(&record).map_err(std::io::Error::other)?;
    // a uniquely named staging file, removed if anything fails before the rename
    let mut staging = tempfile::Builder::new()
        .prefix(&format!(".{}.json.", alias.as_str()))
        .suffix(".partial")
        .permissions(std::fs::Permissions::from_mode(0o600))
        .tempfile_in(dir)?;
    std::io::Write::write_all(&mut staging, text.as_bytes())?;
    staging.as_file().sync_all()?;
    staging.persist(&file).map_err(|error| error.error)?;
    Ok(file)
}

impl AuthReport {
    /// The report as lines for the operator, as of `now`: claims and expiries only, never a token.
    #[must_use]
    pub fn lines(&self, now: SystemTime) -> Vec<String> {
        let mut lines = vec![format!("{} (fail-closed: expected)", self.fence.summary())];
        lines.push(format!("signed in: {}", summary_line(self.signed_in.as_ref(), now)));
        lines.push(format!("refreshed: {}", summary_line(self.refreshed.as_ref(), now)));
        for audience in &self.audiences {
            let outcome = match &audience.token {
                Ok(summary) => summary_line(summary.as_ref(), now),
                Err(error) => format!("refused: {error}"),
            };
            lines.push(format!("{}: {outcome}", audience.resource));
        }
        match &self.skypetoken {
            None => {}
            Some(Ok(skypetoken)) => lines.push(skypetoken_line(skypetoken, now)),
            Some(Err(error)) => lines.push(format!("skypetoken: {error}")),
        }
        lines
    }
}

fn summary_line(summary: Option<&TokenSummary>, now: SystemTime) -> String {
    let Some(summary) = summary else {
        return "an opaque token (no claims to show)".to_owned();
    };
    let mut line = format!("aud {}", summary.audience.join(" "));
    if let Some(expires_at) = summary.expires_at {
        let _ = write!(line, ", exp {}", when(expires_at, now));
    }
    let _ = write!(line, ", scopes {}", summary.scopes.join(" "));
    if let Some(tenant) = &summary.tenant {
        let _ = write!(line, ", tenant {tenant}");
    }
    line
}

fn skypetoken_line(skypetoken: &SkypetokenSummary, now: SystemTime) -> String {
    let region = &skypetoken.region;
    format!(
        "skypetoken: expires {}, region {}, partition {}, {} regional endpoint(s)",
        when(skypetoken.expires_at, now),
        region.name.as_deref().unwrap_or("?"),
        region.partition.as_deref().unwrap_or("?"),
        region.endpoints.len()
    )
}

/// An HTTP date and how far off it is.
fn when(time: SystemTime, now: SystemTime) -> String {
    let relative = match time.duration_since(now) {
        Ok(ahead) => format!("in {} min", ahead.as_secs() / 60),
        Err(past) => format!("{} min ago", past.duration().as_secs() / 60),
    };
    format!("{} ({relative})", httpdate::fmt_http_date(time))
}
