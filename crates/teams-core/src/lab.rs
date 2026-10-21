//! A lab account's engine, assembled the one way every lab binary builds it (`teams-headless`, `teams-app`): the
//! fence the binary opened ([`teams_fence::Fence::open_lab`]: the roster from `lab.toml`, the lab's `fence.log`), the
//! account's [`Session`] over the secret store the binary hands in (the Secret Service for `teams-headless`, the
//! credential broker's socket for the sandboxed app), the chat base from the region record `proto-lab auth` saved
//! ([`LabPaths::region_file`], read by [`Region::load`]), the 1:1 chats' and the lab group chat's members read live
//! ([`LiveMembershipSource::for_fence`], its credential the chat transport's), the store at [`LabPaths::store_file`],
//! the reads' transport with the middle tier's base from the GTM `middleTier` (the profile lookup's), the lab
//! accounts' names as lab.toml has them ([`known_names`]), and the push channel ([`Trouter`]) when asked for.
//!
//! Nothing here reads or prints a token, and nothing is sent while the engine is built: the session asks its store for
//! a token only when a request needs one.
//!
//! M2 Task 6 (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`) moved it here from `teams-headless`, so the
//! app builds its parts as the headless binary does.

use std::sync::Arc;

use teams_auth::authz::{AuthzClient, AuthzError, CHAT_BASE_GTM, Region, RegionFileError};
use teams_auth::secrets::SecretStore;
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_fence::lab::{Alias, LabConfig};
use teams_fence::{Fence, LiveMembershipSource, LiveSourceError, Mri};
use teams_proto::{Author, BuildError, Client, Credentials, HttpTransport};
use teams_store::{Store, StoreError};
use teams_trouter::Trouter;

use crate::Outbox;
use crate::engine::{self, EngineError, EngineHandle, EngineOptions, Parts, Push};

/// Why a lab account's engine couldn't be built or started.
#[derive(Debug, thiserror::Error)]
pub enum LabError {
    #[error("lab.toml has no MRI for {0} yet (run `proto-lab login {0}`)")]
    NoMri(Alias),
    #[error("{source} (run `proto-lab auth {alias}`)")]
    Region { alias: Alias, source: RegionFileError },
    #[error("the region has neither a chatServiceAfd nor a chatService GTM")]
    NoChatBase,
    #[error("the chat transport can't be built: {0}")]
    Transport(#[from] BuildError),
    #[error("the push channel can't be built: {0}")]
    Push(#[from] teams_trouter::BuildError),
    #[error("the member read can't be built: {0}")]
    Membership(#[from] LiveSourceError),
    #[error(transparent)]
    Token(#[from] TokenError),
    #[error(transparent)]
    Authz(#[from] AuthzError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// A lab account's running engine and the session its requests take their tokens from (the app watches it for a
/// sign-out, [`Session::watch_signed_out`]).
#[derive(Debug)]
pub struct LabEngine<S> {
    pub handle: EngineHandle,
    pub session: Arc<Session<S>>,
}

/// Builds `alias`'s engine as the module docs say, over `fence` and `secrets`, with its push channel when `push`, and
/// starts it ([`engine::start`]: refused while another engine runs on the account).
///
/// # Errors
/// See [`LabError`]; [`LabError::Engine`] with [`EngineError::AlreadyRunning`] when another engine holds the account.
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
    let store = Store::open(&paths.store_file(alias))?;
    let session = Arc::new(Session::new(
        account.upn.clone(),
        secrets,
        TokenClient::new(EntraConfig::teams_native())?,
        AuthzClient::new()?,
        region,
    ));
    let base = session.service_base(CHAT_BASE_GTM).ok_or(LabError::NoChatBase)?;
    let transport = Arc::new(HttpTransport::new(Arc::clone(&session), Arc::clone(&fence), &base)?);
    let membership = LiveMembershipSource::for_fence(&fence)?;
    membership.set_credential_source(transport.clone());
    let reads = with_middle_tier(
        || HttpTransport::new(Arc::clone(&session), Arc::clone(&fence), &base),
        session.service_base(MIDDLE_TIER_GTM).as_deref(),
    )?;
    let client = Arc::new(Client::new(reads));
    let push = if push {
        Some(Push::new(Trouter::new(
            Arc::clone(&transport),
            Arc::clone(&session),
            Arc::clone(&fence),
        )?))
    } else {
        None
    };
    let author = Author::new(Mri::new(&mri), display_name(&account.upn));
    let outbox = Outbox::new(transport, fence, Arc::new(membership), author);
    let handle = engine::start(Parts {
        store,
        client,
        outbox,
        push,
        options: EngineOptions::default(),
        known_names: known_names(config),
    })?;
    Ok(LabEngine { handle, session })
}

/// The reads' transport `build` makes, given the middle tier's base (the profile lookup's) when the region has one
/// the transport accepts. Without one, or with one of another form (logged, without its value), the lookup is
/// refused and names come from Graph and the messages: an optional source of names never stops the engine.
fn with_middle_tier<C: Credentials>(
    build: impl Fn() -> Result<HttpTransport<C>, BuildError>,
    middle_tier: Option<&str>,
) -> Result<HttpTransport<C>, BuildError> {
    let reads = build()?;
    let Some(middle_tier) = middle_tier else {
        return Ok(reads);
    };
    match reads.with_middle_tier(middle_tier) {
        Err(BuildError::MiddleTierBase) => {
            log::warn!("the region's middleTier base isn't of the form the transport accepts: no profile lookup");
            build()
        }
        other => other,
    }
}

/// The GTM the middle tier's base comes from (the profile lookup's).
pub const MIDDLE_TIER_GTM: &str = "middleTier";

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

#[cfg(test)]
mod tests {
    use std::future::{self, Future};

    use teams_auth::secrets::Secret;
    use teams_auth::token::TokenError;
    use teams_fence::{FenceLog, Roster};

    use super::*;

    /// Placeholder tokens; nothing is sent.
    struct NoTokens;

    impl Credentials for NoTokens {
        fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
            future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
        }

        fn bearer_rejected(
            &self,
            audience: &str,
            _: &Secret,
        ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
            self.bearer(audience)
        }
    }

    fn reads(middle_tier: Option<&str>) -> Result<HttpTransport<NoTokens>, BuildError> {
        let fence = Arc::new(Fence::new(Roster::new([], []), FenceLog::from_writer(std::io::sink())));
        let tokens = Arc::new(NoTokens);
        with_middle_tier(
            || {
                HttpTransport::new(
                    Arc::clone(&tokens),
                    Arc::clone(&fence),
                    "https://teams.microsoft.com/api/chatsvc/amer",
                )
            },
            middle_tier,
        )
    }

    #[test]
    fn a_middle_tier_base_of_another_form_leaves_the_reads_without_the_lookup_and_the_engine_starting() {
        let given = reads(Some("https://teams.microsoft.com/api/mt/part/amer-03")).unwrap();
        assert!(given.has_middle_tier());
        assert!(!reads(None).unwrap().has_middle_tier());
        for other in [
            "http://teams.microsoft.com/api/mt/part/amer-03",
            "https://elsewhere.example/api/mt/part/amer-03",
            "https://teams.microsoft.com/api/mt/part/AMER-03",
            "not a url",
        ] {
            assert!(!reads(Some(other)).unwrap().has_middle_tier(), "{other}");
        }
    }
}
