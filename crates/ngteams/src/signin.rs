//! The personal build's sign-in of one of its accounts (`docs/personal-build.md`, "Its sign-in" and "The binary and its
//! data"):
//!
//! 1. the authorize request for the account `accounts.toml` holds (its UPN as `login_hint`; once known, its object id
//!    and tenant required of the ID token), the native client `1fec8e78-…` with the `nativeclient` redirect, never
//!    another client id or a borrowed device claim;
//! 2. the sign-in window, a window of this app ([`window_for`]: `teams_signin::window`, its page's engine the sibling
//!    `ngteams-web` worker), bound to Entra's sign-in hosts and that account's identity provider hosts, which
//!    answers the redirect (`teams_auth::signin`);
//! 3. the code redeemed only for that account: the refresh token stored under the account's UPN in the store handed in
//!    (the desktop's Secret Service under [`crate::KEYRING_APPLICATION`]); a sign-in that lands in another account or
//!    tenant stores nothing; an account another entry of `accounts.toml` already is (by its object id) has its token
//!    deleted again and is refused;
//! 4. the account's entry in `accounts.toml` given the object id, tenant and display name the ID token named (the other
//!    accounts kept as they are), and the region from authz saved (`regions/<oid>.json`, `0600` in a `0700`
//!    directory, never a token).
//!
//! An `AADSTS` refusal (Conditional Access, a device requirement, a multi-factor registration) comes back as the
//! window's or Entra's error and stores nothing.
//!
//! **Adding an account** ([`add`], the first run's screen and the rail's "+" alike): its entry is written first
//! ([`Setup::ask_for`]: the one held of that UPN, with whatever the user wrote into it), then it signs in. One whose
//! sign-in stores nothing is taken out again only while it holds nothing but its UPN ([`Setup::abandon`]), and is kept
//! when the window refused an identity provider's host ([`refused_host`]): what the user says ([`failure`]) is to add
//! that host to the entry's `idp_hosts` by hand and sign in again, which takes the entry up. Nothing adds a host by
//! itself, and an entry no sign-in named never starts an engine.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::accounts_file::{PersonalAccount, PersonalError};
use gpui::{App, Global};
use teams_auth::authz::{AuthzClient, AuthzError, AuthzKind, Region};
use teams_auth::pkce::PkceError;
use teams_auth::redirect::RedirectUri;
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{SecretError, SecretStore};
use teams_auth::signin::{SignInAnswer, SignInRequest, WindowError, WindowSignInError, sign_in_with};
use teams_auth::token::{Authorization, EntraConfig, Scope, TokenClient, TokenError};
use teams_signin::hosts::{HostRefused, SignInHosts};
use teams_signin::window::{self, Bound, WindowConfig, Windows};
use teams_ui::window::PERSONAL_APP_ID;

use crate::setup::Setup;

/// A sign-in window for one request: shows the request's page and gives its answer (blocking: it waits for the person
/// signing in). The app's own is [`window_for`]'s; a test's answers by itself.
pub type Window = Box<dyn FnOnce(SignInRequest) -> SignInAnswer + Send>;

/// The app's sign-in windows, opened on GPUI's foreground for the sign-ins running off it.
struct SignInWindows(Windows);

impl Global for SignInWindows {}

/// The app's sign-in windows ([`Windows`], under the app's own app id), served from the first ask on.
pub fn windows(cx: &mut App) -> Windows {
    if let Some(windows) = cx.try_global::<SignInWindows>() {
        return windows.0.clone();
    }
    let windows = Windows::serve(PERSONAL_APP_ID, cx);
    cx.set_global(SignInWindows(windows.clone()));
    windows
}

/// Why a sign-in stored nothing.
#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    #[error("the system random source failed: {0}")]
    Random(#[from] PkceError),
    #[error(transparent)]
    Window(#[from] WindowSignInError),
    #[error(transparent)]
    Token(#[from] TokenError),
    #[error(transparent)]
    Authz(#[from] AuthzError),
    #[error(transparent)]
    Secrets(#[from] SecretError),
    #[error(transparent)]
    Personal(#[from] PersonalError),
    #[error("the region can't be saved to {}: {source}", path.display())]
    Region { path: PathBuf, source: io::Error },
    #[error("the sign-in window's page: {0}")]
    NoWindow(io::Error),
    #[error(transparent)]
    Hosts(#[from] HostRefused),
}

/// The clients a sign-in talks to: Entra's token endpoint (and the authorize request's config) and authz.
#[derive(Clone, Debug)]
pub struct SignInClients {
    pub entra: EntraConfig,
    pub token: TokenClient,
    pub authz: AuthzClient,
}

impl SignInClients {
    /// The live ones: the native client against Entra, and Teams' authz.
    ///
    /// # Errors
    /// When an HTTP client can't be built.
    pub fn live() -> Result<Self, SignInError> {
        let entra = EntraConfig::teams_native();
        Ok(Self {
            token: TokenClient::new(entra.clone())?,
            authz: AuthzClient::new()?,
            entra,
        })
    }
}

/// The sign-in window for `account`, opened through `windows`: bound to Entra's sign-in hosts and the identity
/// provider's hosts its `accounts.toml` entry names ([`SignInHosts::entra_with`]), its decisions in the app's
/// diagnostic log, its web process extension the one beside this binary. A request whose authorize URL isn't on that
/// bound, or whose redirect URI isn't usable, opens no window and starts no engine.
///
/// # Errors
/// [`SignInError::Hosts`] for an `idp_hosts` entry the bound refuses, [`SignInError::NoWindow`] when the running
/// executable or the extension beside it can't be found.
pub fn window_for(setup: &Setup, account: &PersonalAccount, windows: &Windows) -> Result<Window, SignInError> {
    let hosts = Arc::new(SignInHosts::entra_with(&account.idp_hosts)?);
    let exe = std::env::current_exe().map_err(SignInError::NoWindow)?;
    let extension = window::extension_beside(&exe).ok_or_else(|| {
        SignInError::NoWindow(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no {} beside {}", window::EXTENSION_FILE, exe.display()),
        ))
    })?;
    Ok(bound_window(setup, hosts, windows, extension))
}

fn bound_window(setup: &Setup, hosts: Arc<SignInHosts>, windows: &Windows, extension: PathBuf) -> Window {
    let paths = setup.paths.clone();
    let windows = windows.clone();
    Box::new(move |request: SignInRequest| {
        if !hosts.admits_url(&request.authorize) {
            return SignInAnswer::Failed("the authorize URL isn't on the sign-in window's hosts".into());
        }
        if let Err(error) = paths.make_dirs() {
            return SignInAnswer::Failed(format!("the personal data dir: {error}"));
        }
        // before the engine: a request no page could answer never starts, or downloads, one
        if RedirectUri::parse(&request.redirect_uri).is_err() {
            return SignInAnswer::Failed("the redirect URI isn't usable".into());
        }
        let engine = match windows.engine_source(paths.data_dir()) {
            Ok(engine) => engine,
            Err(answer) => return answer,
        };
        windows.ask(WindowConfig {
            request,
            bound: Bound::Hosts(hosts),
            extension,
            engine,
            setup: Box::new(|guard| guard),
        })
    })
}

/// Whether `account` can start without a sign-in: a sign-in named its object id, its region is saved, and a refresh
/// token is stored for it.
///
/// # Errors
/// When the store can't be read.
pub async fn ready<S: SecretStore>(setup: &Setup, account: &PersonalAccount, store: &S) -> Result<bool, SignInError> {
    if !account.is_signed_in() || !setup.paths.region_file(account).is_some_and(|region| region.is_file()) {
        return Ok(false);
    }
    Ok(store.get(&refresh_token_key(&account.upn)).await?.is_some())
}

/// Signs `account` in through the window `window` starts (see the module docs) and gives the account as the sign-in
/// named it, saved.
///
/// # Errors
/// [`SignInError`]: the window gave no code for this request, Entra refused or the ID token named another account or
/// tenant (nothing stored then), an account another entry already is (its token deleted again), authz refused,
/// or a file couldn't be written.
pub async fn sign_in<S: SecretStore>(
    setup: &Setup,
    account: &PersonalAccount,
    clients: &SignInClients,
    store: &S,
    window: Window,
) -> Result<PersonalAccount, SignInError> {
    let mut authorization = Authorization::new(&clients.entra, &account.upn, Scope::spaces())?;
    if let (Some(oid), Some(tid)) = (&account.oid, &account.tid) {
        authorization = authorization.expect_object_id(oid).expect_tenant(tid);
    }
    let signed = sign_in_with(&clients.token, store, &authorization, window).await?;
    let named = account.signed_in_as(
        &signed.account.object_id,
        &signed.account.tenant,
        signed.account.display_name.as_deref(),
    )?;
    // If another entry already has this object id, remove the credential just stored for this sign-in.
    let saved = (|| {
        setup.paths.make_dirs().map_err(|source| PersonalError::Write {
            path: setup.paths.data_dir().to_path_buf(),
            source,
        })?;
        named.save(&setup.paths)
    })();
    if let Err(error) = saved {
        store.delete(&refresh_token_key(&account.upn)).await?;
        return Err(error.into());
    }
    let spaces = clients.token.refresh(store, &account.upn, &Scope::spaces()).await?;
    let skypetoken = clients
        .authz
        .exchange_and_store(
            store,
            &account.upn,
            &spaces.access_token,
            spaces.expires_at,
            AuthzKind::ExplicitLogin,
        )
        .await?;
    let region = setup.paths.region_file(&named).ok_or(PersonalError::NoAccount)?;
    save_region(&region, &skypetoken.region)?;
    Ok(named)
}

/// Adds the account `upn` (or takes up the entry held of it, as the user left it: [`Setup::ask_for`]) and signs it in
/// through the window `window` makes for it ([`sign_in`]); see the module docs for what a sign-in that stores nothing
/// leaves behind.
///
/// # Errors
/// What the user is told ([`failure`]) when the account can't be added or its sign-in stored nothing.
pub async fn add<S: SecretStore>(
    setup: &Setup,
    upn: &str,
    clients: &SignInClients,
    store: &S,
    window: impl FnOnce(&PersonalAccount) -> Result<Window, SignInError>,
) -> Result<PersonalAccount, String> {
    let account = setup.ask_for(upn).map_err(|error| error.to_string())?;
    let signed = match window(&account) {
        Ok(window) => sign_in(setup, &account, clients, store, window).await,
        Err(error) => Err(error),
    };
    signed.map_err(|error| {
        // kept when the window refused a host: the user adds it to this entry's `idp_hosts`
        if refused_host(&error).is_none() {
            let _ = setup.abandon(upn);
        }
        failure(setup, &account, &error)
    })
}

/// The host the sign-in window refused to go to, when that is why it stored nothing: a federated identity provider's
/// page the account's `idp_hosts` doesn't name (`teams_signin::hosts::SignInHosts::idp_candidate`; the host alone).
#[must_use]
pub fn refused_host(error: &SignInError) -> Option<&str> {
    match error {
        SignInError::Window(WindowSignInError::Window(WindowError::Refused(host))) => Some(host),
        _ => None,
    }
}

/// What the user is told when `account`'s sign-in stored nothing for `error`: for a host the window refused, that host
/// and where to add it (`idp_hosts` in the account's entry of `accounts.toml`), never a URL; else the error.
#[must_use]
pub fn failure(setup: &Setup, account: &PersonalAccount, error: &SignInError) -> String {
    match refused_host(error) {
        Some(host) => format!(
            "the sign-in window refused {host}, your organization's sign-in page: add it to idp_hosts in the \
             [[account]] entry of {} in {} (idp_hosts = [\"{host}\"]), then sign in again",
            account.upn,
            setup.paths.accounts_file().display(),
        ),
        None => format!("the sign-in stored nothing: {error}"),
    }
}

/// Writes `region` (its GTMs, never a token) to `path`, `0600` in a `0700` directory, through a new file renamed over it.
fn save_region(path: &Path, region: &Region) -> Result<(), SignInError> {
    let failed = |source| SignInError::Region {
        path: path.to_path_buf(),
        source,
    };
    let text = serde_json_text(region);
    let mut staging = path.as_os_str().to_owned();
    staging.push(format!(".{}.new", std::process::id()));
    let staging = PathBuf::from(staging);
    if let Some(dir) = path.parent() {
        teams_platform::private::create_dir_all(dir).map_err(failed)?;
    }
    // Cleanup belongs only to a file this call created. A colliding staging file is somebody else's data.
    let mut file =
        teams_platform::private::open(&staging, teams_platform::private::Open::NewReadWrite).map_err(failed)?;
    let written = (|| {
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&staging, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&staging);
    }
    written.map_err(failed)
}

/// The region record as text.
fn serde_json_text(region: &Region) -> String {
    format!("{:#}\n", region.to_json())
}

#[cfg(test)]
#[path = "../tests/support/signin_window.rs"]
mod tests;
