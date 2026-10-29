//! The personal build's sign-in of one of its accounts (`docs/personal-build.md`, "Its sign-in" and "The binary and its
//! data"):
//!
//! 1. the authorize request for the account `accounts.toml` holds (its UPN as `login_hint`; once known, its object id
//!    and tenant required of the ID token), the native client `1fec8e78-…` with the `nativeclient` redirect, never
//!    another client id or a borrowed device claim;
//! 2. the sign-in window, `ngteams-signin --account <UPN>` beside this binary ([`window_command`]; its arguments
//!    read by [`window_account`]), bound to that account's identity provider hosts, which answers the redirect
//!    (`teams_auth::signin`);
//! 3. the code redeemed only for that account: the refresh token stored under the account's UPN in the store handed in
//!    (the desktop's Secret Service under [`crate::KEYRING_APPLICATION`]); a sign-in that lands in another account or
//!    tenant stores nothing; a lab account (by its MRI, now known), or an account another entry of `accounts.toml`
//!    already is (by its object id), has its token deleted again and is refused;
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

use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use teams_auth::authz::{AuthzClient, AuthzError, AuthzKind, Region};
use teams_auth::pkce::PkceError;
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{SecretError, SecretStore};
use teams_auth::signin::{WindowError, WindowSignInError, sign_in_with_window};
use teams_auth::token::{Authorization, EntraConfig, Scope, TokenClient, TokenError};
use teams_fence::personal::{PersonalAccount, PersonalError};

use crate::setup::Setup;

/// The sign-in window's binary, beside `ngteams`.
pub const WINDOW_BINARY: &str = "ngteams-signin";

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
    #[error("where the sign-in window is: {0}")]
    NoWindow(io::Error),
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

/// The sign-in window's command for `account`: [`WINDOW_BINARY`] beside the running executable, `--account <UPN>`.
///
/// # Errors
/// [`SignInError::NoWindow`] when the running executable can't be found.
pub fn window_command(account: &PersonalAccount) -> Result<Command, SignInError> {
    let exe = std::env::current_exe().map_err(SignInError::NoWindow)?;
    let mut command = Command::new(exe.with_file_name(WINDOW_BINARY));
    command.arg("--account").arg(&account.upn);
    Ok(command)
}

/// The UPN the sign-in window's arguments (`args`, its own name left out) give: exactly `--account <UPN>`, as
/// [`window_command`] passes it; `None` for anything else (the window's usage error).
#[must_use]
pub fn window_account(args: impl IntoIterator<Item = OsString>) -> Option<String> {
    let mut args = args.into_iter();
    let (Some(flag), Some(upn), None) = (args.next(), args.next(), args.next()) else {
        return None;
    };
    (flag == "--account").then(|| upn.into_string().ok()).flatten()
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
/// tenant (nothing stored then), a lab account or one another entry already is (its token deleted again), authz refused,
/// or a file couldn't be written.
pub async fn sign_in<S: SecretStore>(
    setup: &Setup,
    account: &PersonalAccount,
    clients: &SignInClients,
    store: &S,
    window: Command,
) -> Result<PersonalAccount, SignInError> {
    account.check_not_lab(&setup.lab)?;
    let mut authorization = Authorization::new(&clients.entra, &account.upn, Scope::spaces())?;
    if let (Some(oid), Some(tid)) = (&account.oid, &account.tid) {
        authorization = authorization.expect_object_id(oid).expect_tenant(tid);
    }
    let signed = sign_in_with_window(&clients.token, store, &authorization, window).await?;
    let named = account.signed_in_as(
        &signed.account.object_id,
        &signed.account.tenant,
        signed.account.display_name.as_deref(),
    )?;
    // a lab account is never driven by the personal fence, and an account another entry already is (by its object id)
    // isn't added twice: what was just stored goes again
    let saved = named.check_not_lab(&setup.lab).and_then(|()| {
        setup.paths.make_dirs().map_err(|source| PersonalError::Write {
            path: setup.paths.data_dir().to_path_buf(),
            source,
        })?;
        named.save(&setup.paths)
    });
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
    window: impl FnOnce(&PersonalAccount) -> Result<Command, SignInError>,
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
    let written = (|| {
        if let Some(dir) = path.parent() {
            DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&staging)?;
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
