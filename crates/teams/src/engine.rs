//! The personal account's engine, assembled as every binary builds one (`teams_core::account::start`) from the personal
//! build's parts: the fence the binary opened (`Fence::open_personal`, its account from `account.toml`), its own
//! membership source (`PersonalMembershipSource`: a chat's thread read fresh at every decision, its credential the chat
//! transport's), the account's own store at `~/.local/share/teams/store/<oid>.sqlite` (never another account's: a store
//! kept after `teams --forget` stays its account's), the region the sign-in saved, the push channel.
//! Nothing is sent while it is built: the session asks its store for a token only when a request needs one.

use std::sync::Arc;

use teams_auth::authz::{Region, RegionFileError};
use teams_auth::secrets::SecretStore;
use teams_core::account::{self, Account, AccountEngine, AccountError, Membership};
use teams_fence::Fence;
use teams_fence::personal::{PersonalAccount, PersonalMembershipSource, PersonalPaths};

/// Why the engine couldn't be built or started.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("no account signed in yet: sign in first")]
    NotSignedIn,
    #[error("the region the sign-in saved can't be read ({0}): sign in again")]
    Region(#[from] RegionFileError),
    #[error(transparent)]
    Account(#[from] AccountError),
}

/// The account's running engine and its session (the app watches it for a sign-out).
pub type PersonalEngine<S> = AccountEngine<S>;

/// Builds and starts `account`'s engine over `fence` (the personal fence the binary opened) and `secrets`, with its
/// push channel when `push` (see the module docs).
///
/// # Errors
/// [`StartError`]; [`AccountError::Engine`] with `EngineError::AlreadyRunning` when another `teams` runs.
pub fn start<S: SecretStore + 'static>(
    paths: &PersonalPaths,
    account: &PersonalAccount,
    fence: Arc<Fence>,
    secrets: S,
    push: bool,
) -> Result<PersonalEngine<S>, StartError> {
    let mri = account.mri().ok_or(StartError::NotSignedIn)?;
    let store_file = paths.store_file(account).ok_or(StartError::NotSignedIn)?;
    let region = Region::load(&paths.region_file())?;
    let parts = Account {
        upn: account.upn.clone(),
        mri: mri.as_str().to_owned(),
        display_name: display_name(account).to_owned(),
        store_file,
        region,
        secrets,
        push,
        known_names: Vec::new(),
    };
    let membership: Membership<'_> = Box::new(|fence, credential| {
        let membership = PersonalMembershipSource::for_fence(fence)?;
        membership.set_credential_source(credential);
        Ok(Arc::new(membership))
    });
    Ok(account::start(parts, fence, membership)?)
}

/// The name the account's posts carry (`imdisplayname`): the display name the sign-in's ID token gave (`account.toml`'s
/// `name`), else, from an `account.toml` without one, the UPN's local part.
#[must_use]
pub fn display_name(account: &PersonalAccount) -> &str {
    account.name.as_deref().unwrap_or_else(|| {
        let upn = account.upn.as_str();
        upn.split_once('@').map_or(upn, |(local, _)| local)
    })
}
