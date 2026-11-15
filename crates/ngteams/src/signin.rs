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

use crate::accounts_file::{OwnServer, PersonalAccount, PersonalError};
use gpui::{App, Global};
use teams_auth::authz::{AuthzClient, AuthzError, AuthzKind, Region};
use teams_auth::pkce::PkceError;
use teams_auth::redirect::RedirectUri;
use teams_auth::refresh::{PasswordSignIn, refresh_token_key};
use teams_auth::secrets::{Secret, SecretError, SecretStore};
use teams_auth::signin::{SignInAnswer, SignInRequest, WindowError, WindowSignInError, sign_in_with};
use teams_auth::token::{Authorization, EntraConfig, Scope, TokenClient, TokenError};
use teams_signin::hosts::{HostRefused, SignInHosts};
use teams_signin::window::{self, Bound, WindowConfig, Windows};

use crate::setup::Setup;

/// A sign-in window for one request: shows the request's page and gives its answer (blocking: it waits for the person
/// signing in). The app's own is [`window_for`]'s; a test's answers by itself.
pub type Window = Box<dyn FnOnce(SignInRequest) -> SignInAnswer + Send>;

/// The app's sign-in windows, opened on GPUI's foreground for the sign-ins running off it.
struct SignInWindows(Windows);

impl Global for SignInWindows {}

/// The current account setup's shell identity also groups its sign-in worker. In particular a lab sign-in must
/// never share the installed client's taskbar identity, even before its first signed-in main window exists.
pub fn windows_for(setup: &Setup, cx: &mut App) -> Windows {
    let app_id = crate::app::shell_id(setup, false, cx);
    windows_under(app_id, cx)
}

fn windows_under(app_id: &str, cx: &mut App) -> Windows {
    if let Some(windows) = cx.try_global::<SignInWindows>() {
        return windows.0.clone();
    }
    let windows = Windows::serve(app_id, cx);
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
    #[error("the account signs in another way: Microsoft 365 in Microsoft's window, an own server's with its password")]
    WrongCloud,
    #[error("{OWN_SERVERS_OFF}")]
    OwnServersOff,
}

/// What an own server's sign-in gives when the server took the password only to have it replaced: the sign-in screen
/// asks for a new one instead of saying it ([`crate::own_prompt`]).
pub const PASSWORD_MUST_CHANGE: &str = "Your password must be changed before you sign in.";

/// The server's word for a password that must change, in its refusal of the password grant.
const PASSWORD_EXPIRED: &str = "user_password_expired";

/// What the user is told when an administrator turned the account's sign-in off.
pub const ACCOUNT_TURNED_OFF: &str = "An administrator turned off sign-in for this account.";

/// `AADSTS50057`, the refusal of an account whose sign-in is turned off (an own server answers it too).
pub const ACCOUNT_DISABLED_CODE: u32 = 50_057;

/// Whether `error` (or what caused it) is a server certificate that isn't trusted, as against a server that can't be
/// reached at all.
#[must_use]
pub fn untrusted_certificate(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut at = Some(error);
    while let Some(error) = at {
        // rustls's words for a certificate its verifier refused
        if error.to_string().contains("invalid peer certificate") {
            return true;
        }
        at = error.source();
    }
    false
}

/// The transport error under a failed sign-in, if that is why it failed.
fn transport(error: &SignInError) -> Option<&(dyn std::error::Error + 'static)> {
    match error {
        SignInError::Token(TokenError::Http(error)) | SignInError::Authz(AuthzError::Http(error)) => Some(error),
        _ => None,
    }
}

/// What the user is told about an own server's account while own servers are off.
pub const OWN_SERVERS_OFF: &str = "Own servers are turned off. Set NGTEAMS_OWN_SERVER=1 and start ngteams again.";

/// How `ca_file` is written in `accounts.toml`, in a form the platform's paths survive: a TOML literal string, which
/// keeps a Windows path's backslashes.
pub const CA_FILE_EXAMPLE: &str = if cfg!(windows) {
    r"ca_file = 'C:\path\to\ca.pem'"
} else {
    "ca_file = '/path/to/ca.pem'"
};

/// The clients a sign-in talks to: Entra's token endpoint (and the authorize request's config) and authz.
#[derive(Clone, Debug)]
pub struct SignInClients {
    pub entra: EntraConfig,
    pub token: TokenClient,
    pub authz: AuthzClient,
}

impl SignInClients {
    /// The ones of `cloud`: its token endpoint and authz, trusting its certificate authority.
    ///
    /// # Errors
    /// When an HTTP client can't be built.
    pub fn for_cloud(cloud: &teams_proto::Cloud) -> Result<Self, SignInError> {
        let token = TokenClient::for_cloud(cloud)?;
        Ok(Self {
            entra: token.config().clone(),
            token,
            authz: AuthzClient::for_cloud(cloud)?,
        })
    }

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
    Ok(store.get(&refresh_token_key(&account.id())).await?.is_some())
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
    if account.server.is_some() {
        // an own server's account never sees Microsoft's page, and Microsoft never its user name
        return Err(SignInError::WrongCloud);
    }
    let mut authorization = Authorization::new(&clients.entra, &account.upn, Scope::spaces())?;
    if let (Some(oid), Some(tid)) = (&account.oid, &account.tid) {
        authorization = authorization.expect_object_id(oid).expect_tenant(tid);
    }
    let signed = sign_in_with(&clients.token, store, &authorization, window).await?;
    finish(setup, account, clients, store, &signed.account).await
}

/// Signs the own server's account `account` in with `password` (the password grant against its server, never
/// Microsoft's window), then as [`sign_in`]: its entry named, its skypetoken and region taken from its server's authz.
/// Its tokens are stored under the account qualified by its server ([`PersonalAccount::id`]).
///
/// # Errors
/// [`SignInError`]: own servers are off or the account isn't an own server's, its CA file can't be read, the server
/// refused the user name or password or named another account (nothing stored then), authz refused, or a file
/// couldn't be written.
pub async fn sign_in_own<S: SecretStore>(
    setup: &Setup,
    account: &PersonalAccount,
    password: &Secret,
    store: &S,
) -> Result<PersonalAccount, SignInError> {
    if account.server.is_none() {
        return Err(SignInError::WrongCloud);
    }
    if !setup.own_servers() {
        return Err(SignInError::OwnServersOff);
    }
    let cloud = account.cloud()?;
    let clients = SignInClients::for_cloud(&cloud)?;
    let id = account.id();
    let signed = clients
        .token
        .password_sign_in(
            store,
            PasswordSignIn {
                account: &id,
                username: &account.upn,
                expected: account.tid.as_deref().zip(account.oid.as_deref()),
            },
            password,
            &Scope::spaces(),
        )
        .await?;
    finish(setup, account, &clients, store, &signed.account).await
}

/// What follows a sign-in that stored `account`'s refresh token: its entry saved as `signed` names it, its skypetoken
/// stored and its region saved.
async fn finish<S: SecretStore>(
    setup: &Setup,
    account: &PersonalAccount,
    clients: &SignInClients,
    store: &S,
    signed: &teams_auth::id_token::Account,
) -> Result<PersonalAccount, SignInError> {
    let id = account.id();
    let named = account.signed_in_as(&signed.object_id, &signed.tenant, signed.display_name.as_deref())?;
    // If another entry already has this object id, remove the credential just stored for this sign-in.
    let saved = (|| {
        setup.paths.make_dirs().map_err(|source| PersonalError::Write {
            path: setup.paths.data_dir().to_path_buf(),
            source,
        })?;
        named.save(&setup.paths)
    })();
    if let Err(error) = saved {
        store.delete(&refresh_token_key(&id)).await?;
        return Err(error.into());
    }
    save_authz(setup, &named, clients, store, AuthzKind::ExplicitLogin).await?;
    Ok(named)
}

/// Asks `account`'s authz with its stored refresh token: its skypetoken stored, its region saved.
async fn save_authz<S: SecretStore>(
    setup: &Setup,
    account: &PersonalAccount,
    clients: &SignInClients,
    store: &S,
    kind: AuthzKind,
) -> Result<(), SignInError> {
    let id = account.id();
    let spaces = clients.token.refresh(store, &id, &Scope::spaces()).await?;
    let skypetoken = clients
        .authz
        .exchange_and_store(store, &id, &spaces.access_token, spaces.expires_at, kind)
        .await?;
    let region = setup.paths.region_file(account).ok_or(PersonalError::NoAccount)?;
    save_region(&region, &skypetoken.region)
}

/// Asks the own server of `account` (signed in, its tokens there) for its authz again and saves the region it names
/// now: a server updated since the sign-in may serve what it didn't then. Nothing is asked of Microsoft, and no
/// password is needed while the account's refresh token is good.
///
/// # Errors
/// [`SignInError`]: the account isn't an own server's or own servers are off, its server can't be reached or refused
/// the stored token, or a file couldn't be written; the region saved before stays then.
pub async fn region_again<S: SecretStore>(
    setup: &Setup,
    account: &PersonalAccount,
    store: &S,
) -> Result<(), SignInError> {
    if account.server.is_none() {
        return Err(SignInError::WrongCloud);
    }
    if !setup.own_servers() {
        return Err(SignInError::OwnServersOff);
    }
    let clients = SignInClients::for_cloud(&account.cloud()?)?;
    save_authz(setup, account, &clients, store, AuthzKind::TokenRefresh).await
}

/// Signs the own server's `account` in again with `password` ([`sign_in_own`]), as its entry in `accounts.toml` reads
/// now ([`Setup::as_held`]): a `ca_file` the user wrote into it while the sign-in screen was open is used at once.
///
/// # Errors
/// What the user is told ([`own_failure`], of the entry as read now).
pub async fn sign_in_own_again<S: SecretStore>(
    setup: &Setup,
    account: &PersonalAccount,
    password: &Secret,
    store: &S,
) -> Result<PersonalAccount, String> {
    let account = setup.as_held(account).map_err(|error| error.to_string())?;
    sign_in_own(setup, &account, password, store)
        .await
        .map_err(|error| own_failure(setup, &account, &error))
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
            let _ = setup.abandon(&account.id());
        }
        failure(setup, &account, &error)
    })
}

/// Adds the own server `server`'s account `upn` (or takes up the entry held of it: [`Setup::ask_for_on`]) and signs
/// it in with `password` ([`sign_in_own`]); no sign-in window opens. One whose sign-in stores nothing is taken out
/// again: an entry held before while it holds nothing the user wrote ([`Setup::abandon`]), one this attempt made
/// whatever it holds.
///
/// # Errors
/// What the user is told ([`form_failure`]).
pub async fn add_own<S: SecretStore>(
    setup: &Setup,
    upn: &str,
    server: &OwnServer,
    password: &Secret,
    store: &S,
) -> Result<PersonalAccount, String> {
    let origin = Some(server.origin.as_str());
    let held = setup
        .accounts()
        .map_err(|error| error.to_string())?
        .iter()
        .any(|held| held.server.as_deref() == origin && held.upn.eq_ignore_ascii_case(upn));
    let account = setup.ask_for_on(upn, server).map_err(|error| error.to_string())?;
    sign_in_own(setup, &account, password, store).await.map_err(|error| {
        if held {
            let _ = setup.abandon(&account.id());
        } else {
            // made by this attempt alone, its CA file typed on the sign-in screen: nothing of it stays
            let _ = PersonalAccount::remove(&setup.paths, &account.id());
        }
        form_failure(setup, &account, server, &error)
    })
}

/// [`own_failure`] said on the sign-in form, which has the address and the certificate authority's field: a server
/// not reached or not trusted points there, not at `accounts.toml`.
fn form_failure(setup: &Setup, account: &PersonalAccount, server: &OwnServer, error: &SignInError) -> String {
    let origin = &server.origin;
    let Some(transport) = transport(error) else {
        return own_failure(setup, account, error);
    };
    match (untrusted_certificate(transport), &account.ca_file) {
        (false, _) => {
            format!("{origin} can't be reached. Check the address and that the server is running, then sign in again.")
        }
        (true, None) => format!(
            "{origin}'s certificate isn't trusted. If the server has its own certificate authority, enter its file \
             under Certificate authority file, then sign in again."
        ),
        (true, Some(ca_file)) => format!(
            "{origin}'s certificate isn't trusted by the certificate authority in {}. Check the file under \
             Certificate authority file, then sign in again.",
            ca_file.display()
        ),
    }
}

/// What the user is told when an own server's sign-in stored nothing: a password that must change
/// ([`PASSWORD_MUST_CHANGE`]), an account turned off ([`ACCOUNT_TURNED_OFF`]), a wrong user name or password, a wait
/// the server asked for, a certificate that isn't trusted or a server that can't be reached (with where a self-signed
/// deployment's certificate authority goes, or the one its entry names already), own servers turned off, else the
/// error.
#[must_use]
pub fn own_failure(setup: &Setup, account: &PersonalAccount, error: &SignInError) -> String {
    let server = account.server.as_deref().unwrap_or_default();
    match error {
        SignInError::Token(TokenError::InvalidGrant(refusal))
            if refusal.suberror.as_deref() == Some(PASSWORD_EXPIRED) =>
        {
            PASSWORD_MUST_CHANGE.to_owned()
        }
        SignInError::Token(TokenError::InvalidGrant(refusal)) if refusal.codes.contains(&ACCOUNT_DISABLED_CODE) => {
            ACCOUNT_TURNED_OFF.to_owned()
        }
        SignInError::Token(TokenError::InvalidGrant(_)) => "The user name or password isn't right.".to_owned(),
        SignInError::Token(TokenError::Throttled { .. }) => {
            "Too many attempts: wait a few minutes, then sign in again.".to_owned()
        }
        SignInError::Token(TokenError::Http(error)) | SignInError::Authz(AuthzError::Http(error))
            if untrusted_certificate(error) =>
        {
            let file = setup.paths.accounts_file();
            match &account.ca_file {
                Some(ca_file) => format!(
                    "{server}'s certificate isn't trusted by the certificate authority in {} (ca_file in {}). Check \
                     that file, then sign in again.",
                    ca_file.display(),
                    file.display(),
                ),
                None => format!(
                    "{server}'s certificate isn't trusted. For a server with its own certificate authority, add \
                     {CA_FILE_EXAMPLE} to an [[account]] entry with this user name and server in {}, then sign in \
                     again.",
                    file.display(),
                ),
            }
        }
        SignInError::Token(TokenError::Http(_)) | SignInError::Authz(AuthzError::Http(_)) => {
            let file = setup.paths.accounts_file();
            match &account.ca_file {
                Some(ca_file) => format!(
                    "{server} can't be reached. Check that the server is running and that {} is its certificate \
                     authority (ca_file in {}), then sign in again.",
                    ca_file.display(),
                    file.display(),
                ),
                None => format!(
                    "{server} can't be reached. For a server with its own certificate authority, add \
                     {CA_FILE_EXAMPLE} to an [[account]] entry with this user name and server in {}, then sign in \
                     again.",
                    file.display(),
                ),
            }
        }
        SignInError::OwnServersOff => OWN_SERVERS_OFF.to_owned(),
        error => format!("the sign-in stored nothing: {error}"),
    }
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
