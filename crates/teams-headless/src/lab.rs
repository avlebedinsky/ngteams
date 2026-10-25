//! A lab account's native client, as the binary builds it: [`teams_core::lab::start`] over the fence from
//! [`Fence::open_lab_account`] (the roster from `lab.toml`, the lab's `fence.log`, the account's own MRI) and the tokens
//! from the Secret Service
//! (`teams-lab`, [`LabPaths::KEYRING_APPLICATION`], as `proto-lab auth` stored them); the store at
//! [`LabPaths::store_file`]. Nothing here reads or prints a token: the session asks the Secret Service for one only
//! when a request needs it.
//!
//! A call's engine ([`Lab::start_calling`]) has its calling too: its media on the file backend ([`call_devices`]: no
//! audio server), relayed through Teams' relay, the microphone unprocessed (a file has no acoustic echo, and a keyed
//! tone must reach the far end as it is), the account describing its client as the web client does.

use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::Arc;

use teams_auth::secrets::{KeyringSecretStore, SecretError};
use teams_core::account::AccountCalls;
use teams_core::engine::EngineHandle;
use teams_core::lab::{self as core_lab, LAB_LANGUAGE, LabEngine, web_client_info};
use teams_core::{CallMedia, Relay};
use teams_fence::lab::{Alias, ChatsError, ConfigError, LabChats, LabConfig, LabLocations, LabPaths};
use teams_fence::{Fence, Mri, OpenError};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayOptions;
use teams_media::session::SessionConfig;
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
    #[error("lab.toml has no MRI for the echo service yet (run `proto-lab service-mri echo <mri>`)")]
    NoEcho,
    #[error("the recordings directory can't be made: {0}")]
    Recordings(#[source] io::Error),
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
        let fence = Arc::new(Fence::open_lab_account(self.alias)?);
        let secrets = KeyringSecretStore::new(LabPaths::KEYRING_APPLICATION).await?;
        let LabEngine { handle, .. } = core_lab::start(&self.config, self.alias, fence, secrets, push, None)?;
        Ok(handle)
    }

    /// The account's engine with its calling (and without its messaging push channel: a call needs only the calling
    /// socket), its calls' media on `devices` (see the module docs).
    ///
    /// # Errors
    /// See [`LabError`].
    pub async fn start_calling(&self, devices: FileDevices) -> Result<EngineHandle, LabError> {
        let fence = Arc::new(Fence::open_lab_account(self.alias)?);
        let secrets = KeyringSecretStore::new(LabPaths::KEYRING_APPLICATION).await?;
        let calls = AccountCalls {
            media: call_media(devices, Relay::Teams),
            client: web_client_info(),
            language_id: Some(LAB_LANGUAGE.to_owned()),
        };
        let LabEngine { handle, .. } = core_lab::start(&self.config, self.alias, fence, secrets, false, Some(calls))?;
        Ok(handle)
    }

    /// The echo service's MRI, as lab.toml records it.
    ///
    /// # Errors
    /// [`LabError::NoEcho`] until it is recorded.
    pub fn echo(&self) -> Result<Mri, LabError> {
        self.config
            .service_participants()
            .iter()
            .find(|service| service.name == "echo")
            .and_then(|service| service.mri.as_deref())
            .map(Mri::new)
            .ok_or(LabError::NoEcho)
    }

    /// Where a call's audio is kept, `<data_dir>/recordings/` (`0700`, made when missing): lab data, never in the repo.
    ///
    /// # Errors
    /// [`LabError::Recordings`] when it can't be made.
    pub fn recordings_dir(&self) -> Result<PathBuf, LabError> {
        let dir = self.paths().data_dir().join("recordings");
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(LabError::Recordings)?;
        Ok(dir)
    }
}

/// A call's devices on the file backend: the microphone plays the WAV at `sent` (then silence), the speaker records
/// to `recording`, both at a device's pace; the camera is the test pattern.
#[must_use]
pub fn call_devices(sent: PathBuf, recording: PathBuf) -> FileDevices {
    FileDevices {
        input: AudioInput::Wav(sent),
        recording: Some(recording),
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    }
}

/// A call's media on `devices` through `relay`, the microphone unprocessed (see the module docs).
#[must_use]
pub fn call_media(devices: FileDevices, relay: Relay) -> CallMedia {
    CallMedia {
        devices: Arc::new(devices),
        relay,
        relay_options: RelayOptions::default(),
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
            ..SessionConfig::default()
        },
    }
}
