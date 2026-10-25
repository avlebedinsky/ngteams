//! The lab app's calls (M3 Task 17): the account's engine gets its calling ([`account_calls`]: the calls' media on
//! the devices the app opened, relayed through Teams' relay, the microphone processed as a call's is, the account
//! describing its client as the web client does), and the window shows calls ([`AppCalls`]: the ringtone on the same
//! devices, the echo service's MRI for the test call).
//!
//! The devices are the PipeWire the build allows ([`PipeWireDevices`]): a lab or test build's is the UI sandbox's
//! private one only, so a lab app's microphone, speaker and ringtone never reach the desktop's audio. Without it (no
//! sandbox PipeWire) the app runs without calls: no call is shown, and the engine has no calling socket.

use std::sync::Arc;

use teams_core::account::AccountCalls;
use teams_core::lab::{LAB_LANGUAGE, web_client_info};
use teams_core::{CallMedia, Relay};
use teams_fence::Mri;
use teams_fence::lab::LabConfig;
use teams_media::devices::pipewire::PipeWireDevices;
use teams_media::devices::{DeviceError, Devices};
use teams_media::relay::RelayOptions;
use teams_media::session::SessionConfig;
use teams_ui::{DeviceRinger, Ringer};

/// What the window's calls are made of.
#[derive(Clone)]
pub struct AppCalls {
    /// What plays an incoming call's ringtone; `None` plays none.
    pub ringer: Option<Arc<dyn Ringer>>,
    /// The echo service's MRI, for the test call; `None` until lab.toml records it.
    pub echo: Option<Mri>,
}

impl std::fmt::Debug for AppCalls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppCalls")
            .field("ringer", &self.ringer.is_some())
            .field("echo", &self.echo)
            .finish()
    }
}

/// The account's calling on `devices` (see the module docs).
#[must_use]
pub fn account_calls(devices: Arc<dyn Devices + Send + Sync>) -> AccountCalls {
    AccountCalls {
        media: CallMedia {
            devices,
            relay: Relay::Teams,
            relay_options: RelayOptions::default(),
            session: SessionConfig::default(),
        },
        client: web_client_info(),
        language_id: Some(LAB_LANGUAGE.to_owned()),
    }
}

/// The echo service's MRI, as lab.toml records it.
#[must_use]
pub fn echo_of(config: &LabConfig) -> Option<Mri> {
    config
        .service_participants()
        .iter()
        .find(|service| service.name == "echo")
        .and_then(|service| service.mri.as_deref())
        .map(Mri::new)
}

/// The engine's calling and the window's calls on `devices`.
#[must_use]
pub fn on_devices(devices: Arc<dyn Devices + Send + Sync>, echo: Option<Mri>) -> (AccountCalls, AppCalls) {
    let ringer: Arc<dyn Ringer> = Arc::new(DeviceRinger::new(Arc::clone(&devices)));
    (
        account_calls(devices),
        AppCalls {
            ringer: Some(ringer),
            echo,
        },
    )
}

/// The engine's calling and the window's calls on the PipeWire this build allows (see the module docs).
///
/// # Errors
/// The PipeWire is refused or can't be reached ([`PipeWireDevices::connect`]).
pub fn open(config: &LabConfig) -> Result<(AccountCalls, AppCalls), DeviceError> {
    let devices = PipeWireDevices::connect()?;
    Ok(on_devices(Arc::new(devices), echo_of(config)))
}
