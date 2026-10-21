//! A lab account's native client, as the binary builds it: [`teams_core::lab::start`] over the fence from
//! [`Fence::open_lab`] (the roster from `lab.toml`, the lab's `fence.log`) and the tokens from the Secret Service
//! (`teams-lab`, [`LabPaths::KEYRING_APPLICATION`], as `proto-lab auth` stored them); the store at
//! [`LabPaths::store_file`]. Nothing here reads or prints a token: the session asks the Secret Service for one only
//! when a request needs it.

use std::sync::Arc;

use teams_auth::secrets::{KeyringSecretStore, SecretError};
use teams_core::engine::EngineHandle;
use teams_core::lab::{self as core_lab, LabEngine};
use teams_fence::lab::{Alias, ChatsError, ConfigError, LabChats, LabConfig, LabLocations, LabPaths};
use teams_fence::{Fence, OpenError};
use teams_store::{Store, StoreError};

pub use teams_core::lab::display_name;

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
    #[error(transparent)]
    Secrets(#[from] SecretError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Start(#[from] core_lab::LabError),
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
        let fence = Arc::new(Fence::open_lab()?);
        let secrets = KeyringSecretStore::new(LabPaths::KEYRING_APPLICATION).await?;
        let LabEngine { handle, .. } = core_lab::start(&self.config, self.alias, fence, secrets, push)?;
        Ok(handle)
    }
}
