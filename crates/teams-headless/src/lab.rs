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
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use teams_auth::secrets::{KeyringSecretStore, SecretError};
use teams_core::account::AccountCalls;
use teams_core::engine::EngineHandle;
use teams_core::lab::{self as core_lab, LAB_LANGUAGE, LabEngine, web_client_info};
use teams_core::{CallMedia, Relay};
use teams_fence::lab::{Alias, ChatsError, ConfigError, LabChats, LabConfig, LabLocations, LabPaths};
use teams_fence::{Fence, Mri, OpenError};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, Devices, VideoSource};
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

    /// The account's engine with its calling, its calls' media on `devices` (see the module docs): with its messaging
    /// push channel (`push`) for `watch`, without for `call` and `join` (a call needs only the calling socket).
    ///
    /// # Errors
    /// See [`LabError`].
    pub async fn start_calling(
        &self,
        devices: impl Devices + Send + Sync + 'static,
        push: bool,
    ) -> Result<EngineHandle, LabError> {
        let fence = Arc::new(Fence::open_lab_account(self.alias)?);
        let secrets = KeyringSecretStore::new(LabPaths::KEYRING_APPLICATION).await?;
        let calls = AccountCalls {
            media: call_media(devices, Relay::Teams),
            client: web_client_info(),
            language_id: Some(LAB_LANGUAGE.to_owned()),
        };
        let LabEngine { handle, .. } = core_lab::start(&self.config, self.alias, fence, secrets, push, Some(calls))?;
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

/// When a call's devices last opened, by the wall clock: what lines the far end's recording up with the tone sent, two
/// processes apart ([`TimedDevices`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Opened {
    pub microphone: Option<SystemTime>,
    pub speaker: Option<SystemTime>,
}

/// File devices that note when each one opens ([`Opened`]; a later call's open replaces an earlier one's).
#[derive(Clone, Debug)]
pub struct TimedDevices {
    pub files: FileDevices,
    opened: Arc<Mutex<Opened>>,
}

impl TimedDevices {
    #[must_use]
    pub fn new(files: FileDevices) -> Self {
        Self {
            files,
            opened: Arc::default(),
        }
    }

    /// When the devices last opened.
    #[must_use]
    pub fn opened(&self) -> Opened {
        *self.opened.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn note(&self, what: impl FnOnce(&mut Opened, SystemTime)) {
        what(
            &mut self.opened.lock().unwrap_or_else(PoisonError::into_inner),
            SystemTime::now(),
        );
    }
}

impl Devices for TimedDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        self.note(|opened, now| opened.microphone = Some(now));
        self.files.microphone()
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        self.note(|opened, now| opened.speaker = Some(now));
        self.files.speaker()
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.files.camera()
    }
}

/// A call's media on `devices` through `relay`, the microphone unprocessed (see the module docs).
#[must_use]
pub fn call_media(devices: impl Devices + Send + Sync + 'static, relay: Relay) -> CallMedia {
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
