//! An account's engine assembled from its store, region, credentials, push channel and optional calling devices.
//! Accounts share `CallFloor` so only one call or ringing invitation is active across the app.

use crate::accounts_file::{PersonalAccount, PersonalPaths};
use teams_auth::authz::{Region, RegionFileError};
use teams_auth::secrets::SecretStore;
use teams_core::CallFloor;
use teams_core::account::{self, Account, AccountCalls, AccountEngine, AccountError};

/// Why the engine couldn't be built or started.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("the account isn't signed in yet: sign it in first")]
    NotSignedIn,
    #[error("the region the sign-in saved can't be read ({0}): sign in again")]
    Region(#[from] RegionFileError),
    #[error(transparent)]
    Account(#[from] AccountError),
}

/// The account's running engine and its session (the app watches it for a sign-out).
pub type PersonalEngine<S> = AccountEngine<S>;

/// Builds and starts `account`'s engine with `secrets` and the app's shared call floor, with its
/// push channel when `push` and its calling when `calls` is given (`crate::calling`; see the module docs).
///
/// # Errors
/// [`StartError`]; [`AccountError::Engine`] with `EngineError::AlreadyRunning` when another engine runs for the account.
pub fn start<S: SecretStore + 'static>(
    paths: &PersonalPaths,
    account: &PersonalAccount,
    floor: CallFloor,
    secrets: S,
    push: bool,
    calls: Option<AccountCalls>,
) -> Result<PersonalEngine<S>, StartError> {
    start_with(paths, account, floor, secrets, (push, calls), false)
}

/// `account`'s engine in the read-only mode (`ngteams --read-only`, `crate::read_only`): its transports send reads only
/// (every write refused below the views, logged to the diagnostic log as `refused_read_only`), no push channel and no calling
/// (no endpoint registered, no second Trouter connection on the account), a catch-up by reads every
/// `teams_core::account::READ_ONLY_POLL`. `paths` are the snapshot's, never the user's own.
///
/// # Errors
/// As [`start`].
pub fn start_read_only<S: SecretStore + 'static>(
    paths: &PersonalPaths,
    account: &PersonalAccount,
    floor: CallFloor,
    secrets: S,
) -> Result<PersonalEngine<S>, StartError> {
    start_with(paths, account, floor, secrets, (false, None), true)
}

fn start_with<S: SecretStore + 'static>(
    paths: &PersonalPaths,
    account: &PersonalAccount,
    floor: CallFloor,
    secrets: S,
    (push, calls): (bool, Option<AccountCalls>),
    read_only: bool,
) -> Result<PersonalEngine<S>, StartError> {
    let mri = account.mri().ok_or(StartError::NotSignedIn)?;
    let store_file = paths.store_file(account).ok_or(StartError::NotSignedIn)?;
    let region = Region::load(&paths.region_file(account).ok_or(StartError::NotSignedIn)?)?;
    let parts = Account {
        upn: account.upn.clone(),
        mri: mri.as_str().to_owned(),
        display_name: display_name(account).to_owned(),
        store_file,
        region,
        secrets,
        push,
        known_names: Vec::new(),
        calls,
        read_only,
    };
    Ok(account::start(parts, floor)?)
}

/// The name the account's posts carry (`imdisplayname`): the display name the sign-in's ID token gave (its
/// `accounts.toml` entry's `name`), else, from an entry without one, the UPN's local part.
#[must_use]
pub fn display_name(account: &PersonalAccount) -> &str {
    account.name.as_deref().unwrap_or_else(|| {
        let upn = account.upn.as_str();
        upn.split_once('@').map_or(upn, |(local, _)| local)
    })
}
