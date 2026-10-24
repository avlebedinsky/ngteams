//! A lab account's engine, assembled the one way every lab binary builds it (`teams-headless`, `teams-app`): the
//! fence the binary opened ([`teams_fence::Fence::open_lab_account`]: the roster from `lab.toml`, the lab's
//! `fence.log`, the account's own MRI, whose own availability alone it permits), the
//! account's session over the secret store the binary hands in (the Secret Service for `teams-headless`, the
//! credential broker's socket for the sandboxed app), the chat base from the region record `proto-lab auth` saved
//! ([`LabPaths::region_file`], read by [`Region::load`]), 1:1, group and meeting chats' members read live
//! ([`LiveMembershipSource::for_fence`], its credential the chat transport's), the store at [`LabPaths::store_file`],
//! the reads' transport with the middle tier's base from the GTM `middleTier` (the profile lookup's), CSA's from the
//! GTM `chatSvcAggAfd` (the teams read's) and UPS's from the GTM `presenceUPS` (the presence read's; the writes'
//! transport gets UPS's base too, for the account's own availability, M2 Task 19), the lab
//! accounts' names as lab.toml has them ([`known_names`]), and the push channel when asked for.
//!
//! Nothing here reads or prints a token, and nothing is sent while the engine is built: the session asks its store for
//! a token only when a request needs one.
//!
//! The parts every account's engine has are built by [`crate::account::start`], which the personal build uses too;
//! this module gives it the lab's account, region, store and member read.

use std::sync::Arc;

use teams_auth::authz::{Region, RegionFileError};
use teams_auth::secrets::SecretStore;
use teams_fence::lab::{Alias, LabConfig};
use teams_fence::{Fence, LiveMembershipSource};

use crate::account::{self, Account, AccountEngine, AccountError, Membership};
pub use crate::account::{CSA_GTM, MIDDLE_TIER_GTM, PRESENCE_GTM};

/// Why a lab account's engine couldn't be built or started.
#[derive(Debug, thiserror::Error)]
pub enum LabError {
    #[error("lab.toml has no MRI for {0} yet (run `proto-lab login {0}`)")]
    NoMri(Alias),
    #[error("{source} (run `proto-lab auth {alias}`)")]
    Region { alias: Alias, source: RegionFileError },
    #[error(transparent)]
    Account(#[from] AccountError),
}

/// A lab account's running engine and the session its requests take their tokens from (the app watches it for a
/// sign-out, [`Session::watch_signed_out`](teams_auth::session::Session::watch_signed_out)).
pub type LabEngine<S> = AccountEngine<S>;

/// Builds `alias`'s engine as the module docs say, over `fence` and `secrets`, with its push channel when `push`, and
/// starts it ([`account::start`]: refused while another engine runs on the account).
///
/// # Errors
/// See [`LabError`]; [`LabError::Account`] with [`AccountError::Engine`] and
/// [`EngineError::AlreadyRunning`](crate::engine::EngineError::AlreadyRunning) when
/// another engine holds the account.
pub fn start<S: SecretStore + 'static>(
    config: &LabConfig,
    alias: Alias,
    fence: Arc<Fence>,
    secrets: S,
    push: bool,
) -> Result<LabEngine<S>, LabError> {
    let account = config.account(alias);
    let mri = account.mri.clone().ok_or(LabError::NoMri(alias))?;
    let paths = config.paths();
    let region = Region::load(&paths.region_file(alias)).map_err(|source| LabError::Region { alias, source })?;
    let lab_account = Account {
        upn: account.upn.clone(),
        mri,
        display_name: display_name(&account.upn).to_owned(),
        store_file: paths.store_file(alias),
        region,
        secrets,
        push,
        known_names: known_names(config),
    };
    let membership: Membership<'_> = Box::new(|fence, credential| {
        let membership = LiveMembershipSource::for_fence(fence)?;
        membership.set_credential_source(credential);
        Ok(Arc::new(membership))
    });
    Ok(account::start(lab_account, fence, membership)?)
}

/// The lab accounts' names as lab.toml has them ([`display_name`]), by MRI: the last a person's name comes from.
#[must_use]
pub fn known_names(config: &LabConfig) -> Vec<(String, String)> {
    config
        .accounts()
        .iter()
        .filter_map(|account| Some((account.mri.clone()?, display_name(&account.upn).to_owned())))
        .collect()
}

/// The name a post carries (`imdisplayname`): the UPN's local part. lab.toml keeps no display name.
#[must_use]
pub fn display_name(upn: &str) -> &str {
    upn.split_once('@').map_or(upn, |(local, _)| local)
}
