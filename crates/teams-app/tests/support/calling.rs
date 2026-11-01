//! The call-device controller over silent file devices and an in-memory catalog; never desktop PipeWire.

use std::cell::Cell;
use std::sync::Arc;

use teams_fence::lab::LabConfig;
use teams_media::devices::catalog::{DeviceKind, DeviceList, DeviceWatcher, FixedDirectory};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, Devices, VideoSource};

thread_local! {
    static REFUSE: Cell<bool> = const { Cell::new(false) };
}

pub(super) struct PipeWireDevices {
    files: FileDevices,
    directory: FixedDirectory,
}

impl PipeWireDevices {
    pub(super) fn connect() -> Result<Self, DeviceError> {
        if REFUSE.get() {
            return Err(DeviceError::Refused("synthetic unavailable devices".to_owned()));
        }
        Ok(Self {
            files: FileDevices {
                input: AudioInput::Silence,
                recording: None,
                camera: PatternCamera::default(),
                pacing: Pacing::Unpaced,
            },
            directory: FixedDirectory::default(),
        })
    }
}

impl Devices for PipeWireDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        self.files.microphone()
    }
    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        self.files.speaker()
    }
    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.files.camera()
    }
}

impl teams_media::devices::catalog::DeviceDirectory for PipeWireDevices {
    fn devices(&self) -> DeviceList {
        self.directory.devices()
    }
    fn watch(&self, watcher: DeviceWatcher) {
        self.directory.watch(watcher);
    }
    fn choose(&self, kind: DeviceKind, name: Option<&str>) {
        self.directory.choose(kind, name);
    }
    fn chosen(&self, kind: DeviceKind) -> Option<String> {
        self.directory.chosen(kind)
    }
}

fn config(directory: &tempfile::TempDir) -> LabConfig {
    LabConfig::parse(
        &format!(
            "{}\n[[service_participants]]\nname = \"echo\"\nmri = \"28:00000000-0000-4000-8000-0000000000ec\"\n",
            teams_mock::lab_toml(directory.path())
        ),
        directory.path(),
    )
    .unwrap()
}

#[test]
fn account_devices_share_the_catalog_and_playback_but_keep_their_video_feeds_separate() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(&directory);
    let devices = super::CallDevices::connect(&config).unwrap();
    let (first_engine, first) = devices.account();
    let (_, second) = devices.account();
    assert_eq!(first.echo, super::echo_of(&config));
    assert!(first.ringer.is_some() && first.camera.is_some());
    first
        .devices
        .as_ref()
        .unwrap()
        .choose(DeviceKind::Speaker, Some("synthetic-speaker"));
    assert_eq!(
        second.devices.as_ref().unwrap().chosen(DeviceKind::Speaker).as_deref(),
        Some("synthetic-speaker")
    );
    assert!(Arc::ptr_eq(
        first.devices.as_ref().unwrap(),
        second.devices.as_ref().unwrap()
    ));
    assert!(format!("{second:?}").contains("camera: true"));
    let first_feed = first.video.unwrap();
    let second_feed = second.video.unwrap();
    first_feed.set_enabled(false);
    assert!(!teams_core::VideoOutput::enabled(
        &*first_engine.media.video.unwrap().output
    ));
    assert!(teams_core::VideoOutput::enabled(&second_feed));
    let mut speaker = devices.playback().speaker.unwrap().speaker().unwrap();
    let audio = teams_media::devices::AudioFrame::silence();
    speaker.play(&audio).unwrap();
    assert!(format!("{devices:?}").contains("CallDevices"));
    let (opened, _) = super::open(&config).unwrap();
    assert!(opened.media.video.is_some());
}

#[test]
fn a_device_backend_failure_is_returned_before_any_account_calling_is_built() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(&directory);
    REFUSE.set(true);
    let refused = super::CallDevices::connect(&config).unwrap_err();
    REFUSE.set(false);
    assert!(matches!(refused, DeviceError::Refused(_)));
}
