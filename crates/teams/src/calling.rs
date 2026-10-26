//! The personal build's calls (`docs/personal-build.md`, "Calls"): the account's engine gets its calling
//! ([`account_calls`], shared with the lab app: the calls' media on the devices the app opened, relayed through Teams'
//! relay, the microphone processed as a call's is, the account describing its client as the web client does), and the
//! window shows calls
//! ([`PersonalCalls`]: the ringtone on the same devices, the test call to the echo service, a call's video, the
//! devices the settings choose among).
//!
//! - **The devices** are the PipeWire this build allows ([`PipeWireDevices::connect`]): the personal build's is the
//!   desktop's, its camera through the desktop's camera portal (fixed when `teams-media` is built with `personal`,
//!   which only this crate enables; a test build's is the UI sandbox's only, so `cargo test -p teams` opens neither).
//!   Without them the app runs without calls: no call is shown, and the engine has no calling socket.
//! - **The test call** goes to the echo service by its MRI from the web bundle ([`echo`],
//!   `teams_proto::TEAMS_ECHO_BOT_MRI`), never one taken from the network or the UI.
//! - **Video**: a call takes video when the user's copy of Cisco's OpenH264 is in the personal data dir and is Cisco's
//!   ([`openh264`]: `scripts/fetch-openh264.sh ~/.local/share/teams`); the settings' switch turns the codec off and on.
//!   Without it the calls carry audio only.
//!
//! The fence decides every call write (`teams_fence`'s personal call rule); nothing here sends anything.

use std::sync::Arc;

use teams_core::account::{AccountCalls, OPENH264_DIR, account_calls, call_video};
use teams_fence::Mri;
use teams_fence::personal::PersonalPaths;
use teams_media::devices::catalog::DeviceDirectory;
use teams_media::devices::pipewire::PipeWireDevices;
use teams_media::devices::{DeviceError, Devices};
use teams_media::video::codec::H264Library;
use teams_ui::{DeviceRinger, Ringer, VideoFeed};

/// What the window's calls are made of.
#[derive(Clone)]
pub struct PersonalCalls {
    /// What plays an incoming call's ringtone; `None` plays none.
    pub ringer: Option<Arc<dyn Ringer>>,
    /// The echo service the test call goes to ([`echo`]).
    pub echo: Mri,
    /// Where a call's video is shown; `None`: the calls carry no video (no codec).
    pub video: Option<VideoFeed>,
    /// The devices the settings list and choose among; `None` lists none.
    pub devices: Option<Arc<dyn DeviceDirectory>>,
}

impl std::fmt::Debug for PersonalCalls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonalCalls")
            .field("ringer", &self.ringer.is_some())
            .field("video", &self.video.is_some())
            .field("devices", &self.devices.is_some())
            .finish_non_exhaustive()
    }
}

/// The echo service the personal build's test call goes to: the web bundle's `teamsEchoBotMri`.
#[must_use]
pub fn echo() -> Mri {
    Mri::new(teams_proto::TEAMS_ECHO_BOT_MRI)
}

/// The personal build's H.264 library: Cisco's, in [`OPENH264_DIR`] under the personal data dir (loaded only when it is
/// the one Cisco published).
#[must_use]
pub fn openh264(paths: &PersonalPaths) -> H264Library {
    H264Library::cisco_in(&paths.data_dir().join(OPENH264_DIR))
}

/// The engine's calling and the window's calls on `devices`, the calls' video on `video`'s library (none without).
#[must_use]
pub fn on_devices(
    devices: Arc<dyn Devices + Send + Sync>,
    video: Option<H264Library>,
) -> (AccountCalls, PersonalCalls) {
    let ringer: Arc<dyn Ringer> = Arc::new(DeviceRinger::new(Arc::clone(&devices)));
    let feed = video.as_ref().map(|_| VideoFeed::new());
    let video = video
        .zip(feed.clone())
        .map(|(library, feed)| call_video(library, Arc::new(feed)));
    (
        account_calls(devices, video),
        PersonalCalls {
            ringer: Some(ringer),
            echo: echo(),
            video: feed,
            devices: None,
        },
    )
}

/// The engine's calling and the window's calls on the PipeWire this build allows (see the module docs), the video
/// codec from `paths`' data dir when it is there.
///
/// # Errors
/// The PipeWire is refused or can't be reached ([`PipeWireDevices::connect`]).
pub fn open(paths: &PersonalPaths) -> Result<(AccountCalls, PersonalCalls), DeviceError> {
    let devices = Arc::new(PipeWireDevices::connect()?);
    let library = openh264(paths);
    let video = match library.check() {
        Ok(()) => Some(library),
        Err(error) => {
            log::info!("the calls carry no video: {error}");
            None
        }
    };
    let (engine, mut window) = on_devices(devices.clone(), video);
    window.devices = Some(devices);
    Ok((engine, window))
}
