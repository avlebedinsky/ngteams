//! A lab account's native client, as the binary builds it: the fence from [`Fence::open_lab`] (the roster from
//! `lab.toml`, the lab's `fence.log`), the tokens from the Secret Service (`teams-lab`, [`LabPaths::KEYRING_APPLICATION`],
//! as `proto-lab auth` stored them) through the account's [`Session`], the chat base from the region record
//! `proto-lab auth` saved ([`LabPaths::region_file`], read by [`Region::load`]), the 1:1 chats' members read live
//! ([`LiveMembershipSource`]), and the store at [`LabPaths::store_file`]. Nothing here reads or prints a token: the
//! session asks the Secret Service for one only when a request needs it.

use std::sync::Arc;

use teams_auth::authz::{AuthzClient, AuthzError, CHAT_BASE_GTM, Region, RegionFileError};
use teams_auth::secrets::{KeyringSecretStore, SecretError};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_core::Outbox;
use teams_core::engine::{self, EngineError, EngineHandle, EngineOptions, Parts, Push};
use teams_fence::lab::{Alias, ChatsError, ConfigError, LabChats, LabConfig, LabLocations, LabPaths};
use teams_fence::{Fence, LiveMembershipSource, LiveSourceError, Mri, OpenError};
use teams_proto::{Author, BuildError, Client, HttpTransport};
use teams_store::{Store, StoreError};
use teams_trouter::Trouter;

use crate::chat::LabNames;

/// Why the lab's client couldn't be set up.
#[derive(Debug, thiserror::Error)]
pub enum LabError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Chats(#[from] ChatsError),
    #[error("the fence can't be opened: {0}")]
    Fence(#[from] OpenError),
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
    Secrets(#[from] SecretError),
    #[error(transparent)]
    Token(#[from] TokenError),
    #[error(transparent)]
    Authz(#[from] AuthzError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// The lab as one account's commands see it: its config, the names of its accounts, its recorded group chat.
#[derive(Debug)]
pub struct Lab {
    pub config: LabConfig,
    pub alias: Alias,
    pub names: LabNames,
    /// The a–b–c group chat from `chats.toml`, if recorded.
    pub abc: Option<String>,
}

impl Lab {
    /// Reads `lab.toml` (from the XDG locations) and `chats.toml`; reads no token.
    ///
    /// # Errors
    /// When either can't be read.
    pub fn load(alias: Alias) -> Result<Self, LabError> {
        let config = LabConfig::load(&LabLocations::from_env()?)?;
        let abc = LabChats::load(config.paths())?.abc;
        Ok(Self {
            names: LabNames::from_config(&config),
            config,
            alias,
            abc,
        })
    }

    #[must_use]
    pub fn paths(&self) -> &LabPaths {
        self.config.paths()
    }

    /// The account's store, `<data_dir>/store/<alias>.sqlite`.
    ///
    /// # Errors
    /// When it can't be opened (see [`Store::open`]).
    pub fn open_store(&self) -> Result<Store, LabError> {
        Ok(Store::open(&self.paths().store_file(self.alias))?)
    }

    /// The account's engine: with its push channel (`push`) for `watch`, without for a one-shot command. Connects to
    /// the Secret Service; no request goes out before the engine makes one.
    ///
    /// # Errors
    /// See [`LabError`].
    pub async fn start_engine(&self, push: bool) -> Result<EngineHandle, LabError> {
        let account = self.config.account(self.alias);
        let mri = account.mri.clone().ok_or(LabError::NoMri(self.alias))?;
        let region = Region::load(&self.paths().region_file(self.alias)).map_err(|source| LabError::Region {
            alias: self.alias,
            source,
        })?;
        let fence = Arc::new(Fence::open_lab()?);
        let store = self.open_store()?;
        let secrets = KeyringSecretStore::new(LabPaths::KEYRING_APPLICATION).await?;
        let session = Arc::new(Session::new(
            account.upn.clone(),
            secrets,
            TokenClient::new(EntraConfig::teams_native())?,
            AuthzClient::new()?,
            region,
        ));
        let base = session.service_base(CHAT_BASE_GTM).ok_or(LabError::NoChatBase)?;
        let transport = Arc::new(HttpTransport::new(Arc::clone(&session), Arc::clone(&fence), &base)?);
        let membership = LiveMembershipSource::new()?;
        membership.set_credential_source(transport.clone());
        let client = Arc::new(Client::new(HttpTransport::new(
            Arc::clone(&session),
            Arc::clone(&fence),
            &base,
        )?));
        let push = if push {
            Some(Push::new(Trouter::new(
                Arc::clone(&transport),
                session,
                Arc::clone(&fence),
            )?))
        } else {
            None
        };
        let author = Author::new(Mri::new(&mri), display_name(&account.upn));
        let outbox = Outbox::new(transport, fence, Arc::new(membership), author);
        Ok(engine::start(Parts {
            store,
            client,
            outbox,
            push,
            options: EngineOptions::default(),
        })?)
    }
}

/// The name a post carries (`imdisplayname`): the UPN's local part. lab.toml keeps no display name.
#[must_use]
pub fn display_name(upn: &str) -> &str {
    upn.split_once('@').map_or(upn, |(local, _)| local)
}
