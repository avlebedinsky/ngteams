//! The personal build's sign-in (`docs/personal-build.md`, "Its sign-in" and "The binary and its data"):
//!
//! 1. the authorize request for the account `account.toml` names (its UPN as `login_hint`; once known, its object id
//!    and tenant required of the ID token), the native client `1fec8e78-…` with the `nativeclient` redirect, never
//!    another client id or a borrowed device claim;
//! 2. the sign-in window, `teams-signin-personal` beside this binary ([`window_command`]), which answers the redirect
//!    (`teams_auth::signin`);
//! 3. the code redeemed only for that account: the refresh token stored under the account's UPN in the store handed in
//!    (the desktop's Secret Service under [`crate::KEYRING_APPLICATION`]); a sign-in that lands in another account or
//!    tenant stores nothing; a lab account (by its MRI, now known) has its token deleted again and is refused;
//! 4. `account.toml` given the object id, tenant and display name the ID token named, and the region from authz saved
//!    (`region.json`, `0600`, never a token).
//!
//! An `AADSTS` refusal (Conditional Access, a device requirement, a multi-factor registration) comes back as the
//! window's or Entra's error and stores nothing.

use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use teams_auth::authz::{AuthzClient, AuthzError, AuthzKind, Region};
use teams_auth::pkce::PkceError;
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{SecretError, SecretStore};
use teams_auth::signin::{WindowSignInError, sign_in_with_window};
use teams_auth::token::{Authorization, EntraConfig, Scope, TokenClient, TokenError};
use teams_fence::personal::{PersonalAccount, PersonalError};

use crate::setup::Setup;

/// The sign-in window's binary, beside `teams`.
pub const WINDOW_BINARY: &str = "teams-signin-personal";

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

/// The sign-in window's command: [`WINDOW_BINARY`] beside the running executable.
///
/// # Errors
/// [`SignInError::NoWindow`] when the running executable can't be found.
pub fn window_command() -> Result<Command, SignInError> {
    let exe = std::env::current_exe().map_err(SignInError::NoWindow)?;
    Ok(Command::new(exe.with_file_name(WINDOW_BINARY)))
}

/// Whether `account` can start without a sign-in: a sign-in named its object id, its region is saved, and a refresh
/// token is stored for it.
///
/// # Errors
/// When the store can't be read.
pub async fn ready<S: SecretStore>(setup: &Setup, account: &PersonalAccount, store: &S) -> Result<bool, SignInError> {
    if !account.is_signed_in() || !setup.paths.region_file().is_file() {
        return Ok(false);
    }
    Ok(store.get(&refresh_token_key(&account.upn)).await?.is_some())
}

/// Signs `account` in through the window `window` starts (see the module docs) and gives the account as the sign-in
/// named it, saved.
///
/// # Errors
/// [`SignInError`]: the window gave no code for this request, Entra refused or the ID token named another account or
/// tenant (nothing stored then), a lab account (its token deleted again), authz refused, or a file couldn't be written.
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
    if let Err(error) = named.check_not_lab(&setup.lab) {
        // a lab account is never driven by the personal fence: what was just stored goes again
        store.delete(&refresh_token_key(&account.upn)).await?;
        return Err(error.into());
    }
    setup.paths.make_dirs().map_err(|source| PersonalError::Write {
        path: setup.paths.data_dir().to_path_buf(),
        source,
    })?;
    named.save(&setup.paths)?;
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
    save_region(&setup.paths.region_file(), &skypetoken.region)?;
    Ok(named)
}

/// Writes `region` (its GTMs, never a token) to `path`, `0600`, through a new file renamed over it.
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
