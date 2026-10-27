//! The lab app's calls (M3 Task 17): the account's engine gets its calling ([`account_calls`], shared with the
//! personal build: the calls' media on the devices the app opened, relayed through Teams' relay, the microphone
//! processed as a call's is, the account describing its client as the web client does), and the window shows calls
//! ([`AppCalls`]: the ringtone on the same devices, the echo service's MRI for the test call).
//!
//! The devices are the PipeWire the build allows ([`PipeWireDevices`]): a lab or test build's is the UI sandbox's
//! private one only, so a lab app's microphone, speaker, camera and ringtone never reach the desktop's. Without it (no
//! sandbox PipeWire) the app runs without calls: no call is shown, and the engine has no calling socket. The settings
//! list its devices and choose among them ([`AppCalls::devices`], M3 Task 20): a choice moves the calls' devices at
//! once, a running call's included.
//!
//! An app of several accounts opens the devices once ([`CallDevices::connect`]) and makes each account's calls on them
//! ([`CallDevices::account`]): each its own engine calling and its own video feed, one ringtone and one device choice
//! for the app.
//!
//! **Video** (M3 Task 18): a 1:1 call takes video when the lab's copy of Cisco's OpenH264 is there and is Cisco's
//! ([`teams_core::lab::openh264`], fetched by `scripts/fetch-openh264.sh`); its frames are shown on the window's
//! [`VideoFeed`], and the settings' switch ("OpenH264 Video Codec provided by Cisco Systems, Inc.") turns the codec off
//! and on again. Without the library the calls carry audio only.

use std::sync::Arc;

use teams_core::account::{AccountCalls, account_calls};
use teams_fence::Mri;
use teams_fence::lab::LabConfig;
use teams_media::devices::catalog::DeviceDirectory;
use teams_media::devices::pipewire::PipeWireDevices;
use teams_media::devices::{DeviceError, Devices};
use teams_media::video::codec::H264Library;
use teams_ui::{DeviceRinger, Ringer, VideoFeed};

/// What the window's calls are made of.
#[derive(Clone)]
pub struct AppCalls {
    /// What plays an incoming call's ringtone; `None` plays none.
    pub ringer: Option<Arc<dyn Ringer>>,
    /// The echo service's MRI, for the test call; `None` until lab.toml records it.
    pub echo: Option<Mri>,
    /// Where a 1:1 call's video is shown; `None`: the calls carry no video (no codec).
    pub video: Option<VideoFeed>,
    /// The devices the settings list and choose among; `None` lists none (devices that can't be chosen).
    pub devices: Option<Arc<dyn DeviceDirectory>>,
}

impl std::fmt::Debug for AppCalls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppCalls")
            .field("ringer", &self.ringer.is_some())
            .field("echo", &self.echo)
            .field("video", &self.video.is_some())
            .field("devices", &self.devices.is_some())
            .finish()
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

/// The engine's calling and the window's calls on `devices`, a 1:1 call's video on `video`'s library (none without).
#[must_use]
pub fn on_devices(
    devices: Arc<dyn Devices + Send + Sync>,
    echo: Option<Mri>,
    video: Option<H264Library>,
) -> (AccountCalls, AppCalls) {
    let ringer: Arc<dyn Ringer> = Arc::new(DeviceRinger::new(Arc::clone(&devices)));
    let feed = video.as_ref().map(|_| VideoFeed::new());
    let video = video
        .zip(feed.clone())
        .map(|(library, feed)| teams_core::lab::call_video(library, Arc::new(feed)));
    (
        account_calls(devices, video),
        AppCalls {
            ringer: Some(ringer),
            echo,
            video: feed,
            devices: None,
        },
    )
}

/// The calls' devices of the whole app, opened once: the PipeWire this build allows, the codec when it is there, the
/// test call's echo. Every account's calls are made on them ([`CallDevices::account`]): one microphone, speaker and
/// camera for the app, however many accounts it runs (one call at a time across them: `teams_fence::CallFloor`).
#[derive(Clone)]
pub struct CallDevices {
    devices: Arc<PipeWireDevices>,
    video: Option<H264Library>,
    echo: Option<Mri>,
}

impl std::fmt::Debug for CallDevices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallDevices")
            .field("video", &self.video.is_some())
            .field("echo", &self.echo)
            .finish_non_exhaustive()
    }
}

impl CallDevices {
    /// The PipeWire this build allows (see the module docs), the lab's codec when it is Cisco's, lab.toml's echo.
    ///
    /// # Errors
    /// The PipeWire is refused or can't be reached ([`PipeWireDevices::connect`]).
    pub fn connect(config: &LabConfig) -> Result<Self, DeviceError> {
        let devices = Arc::new(PipeWireDevices::connect()?);
        let library = teams_core::lab::openh264(config.paths());
        let video = match library.check() {
            Ok(()) => Some(library),
            Err(error) => {
                log::info!("the calls carry no video: {error}");
                None
            }
        };
        Ok(Self {
            devices,
            video,
            echo: echo_of(config),
        })
    }

    /// One account's engine calling and window calls on these devices ([`on_devices`]), its video on a feed of its
    /// own; the settings list these devices.
    #[must_use]
    pub fn account(&self) -> (AccountCalls, AppCalls) {
        let (engine, mut window) = on_devices(self.devices.clone(), self.echo.clone(), self.video.clone());
        window.devices = Some(self.devices.clone());
        (engine, window)
    }
}

/// The engine's calling and the window's calls of one account on the PipeWire this build allows (see the module docs).
///
/// # Errors
/// The PipeWire is refused or can't be reached ([`PipeWireDevices::connect`]).
pub fn open(config: &LabConfig) -> Result<(AccountCalls, AppCalls), DeviceError> {
    CallDevices::connect(config).map(|devices| devices.account())
}
