//! `teams --forget <UPN>`: signs out and forgets one account — its `teams-personal` items (the refresh token, the
//! skypetoken and its expiry, each under its UPN), its region (`regions/<oid>.json`) and its entry in `accounts.toml`,
//! the other accounts kept as they are. Its store stays (`store/<oid>.sqlite`, never another account's), to delete by
//! hand.
//!
//! Refused while the account's engine runs (a `teams` open, in the tray too): that engine's session keeps the tokens it
//! holds and a renewal of its would store them again, so the account's engine lock (`teams_store::EngineLock`) is taken
//! first, without waiting, and held until everything is deleted, so no engine starts on the account meanwhile. Its
//! entry goes last, so a forget that fails part way can be run again.

use std::path::PathBuf;

use teams_auth::authz::{skypetoken_expiry_key, skypetoken_key};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{SecretError, SecretStore};
use teams_fence::personal::{PersonalAccount, PersonalError};
use teams_store::{EngineLock, StoreError};

use crate::setup::Setup;

/// What `teams --forget <UPN>` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Forgot {
    /// No account of that UPN.
    Nothing,
    /// The account `upn` is forgotten; its store, if a sign-in ever named its object id, stays at `store`.
    Account { upn: String, store: Option<PathBuf> },
}

/// Why the account wasn't forgotten (or not all of it).
#[derive(Debug, thiserror::Error)]
pub enum ForgetError {
    #[error(transparent)]
    Personal(#[from] PersonalError),
    /// The account's engine runs: nothing was deleted.
    #[error("Teams is running for {upn} (its engine lock {} is held): quit it, the tray icon too, then forget it", lock.display())]
    Running { upn: String, lock: PathBuf },
    #[error("the account's engine lock can't be taken: {0}")]
    Lock(StoreError),
    #[error(transparent)]
    Secrets(#[from] SecretError),
    #[error("{}: {source}", path.display())]
    Remove { path: PathBuf, source: std::io::Error },
}

/// Forgets the account `upn` (case aside) of `setup`'s `accounts.toml`, its items deleted from `secrets` (see the
/// module docs).
///
/// # Errors
/// [`ForgetError::Running`] while the account's engine runs (nothing deleted); otherwise when `accounts.toml` can't be
/// read or written, or an item or a file can't be deleted.
pub async fn forget<S: SecretStore>(setup: &Setup, upn: &str, secrets: &S) -> Result<Forgot, ForgetError> {
    let Some(account) = PersonalAccount::find(&setup.paths, upn)? else {
        return Ok(Forgot::Nothing);
    };
    let store = setup.paths.store_file(&account);
    // an account no sign-in named has no store, so no engine ran on it
    let _lock = match &store {
        Some(store) => Some(EngineLock::hold(store).map_err(|error| match error {
            StoreError::EngineRunning { path } => ForgetError::Running {
                upn: account.upn.clone(),
                lock: path,
            },
            other => ForgetError::Lock(other),
        })?),
        None => None,
    };
    for key in [
        refresh_token_key(&account.upn),
        skypetoken_key(&account.upn),
        skypetoken_expiry_key(&account.upn),
    ] {
        secrets.delete(&key).await?;
    }
    if let Some(path) = setup.paths.region_file(&account) {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(ForgetError::Remove { path, source }),
        }
    }
    PersonalAccount::remove(&setup.paths, &account.upn)?;
    Ok(Forgot::Account {
        upn: account.upn,
        store,
    })
}
