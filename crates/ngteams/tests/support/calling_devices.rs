//! Personal call-device controllers over silent file devices; the real backend's test-build guard is separate.

use std::cell::Cell;
use std::process::Command;
use std::sync::Arc;

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

#[test]
fn account_devices_share_the_catalog_and_playback_but_keep_their_video_feeds_separate() {
    let devices = super::CallDevices::open().unwrap();
    let (first_engine, first) = devices.for_account();
    let (_, second) = devices.for_account();
    assert_eq!(first.echo, super::echo());
    assert!(first.ringer.is_some() && first.camera.is_some());
    first
        .devices
        .as_ref()
        .unwrap()
        .choose(DeviceKind::Camera, Some("synthetic-camera"));
    assert_eq!(
        second.devices.as_ref().unwrap().chosen(DeviceKind::Camera).as_deref(),
        Some("synthetic-camera")
    );
    assert!(Arc::ptr_eq(
        first.devices.as_ref().unwrap(),
        second.devices.as_ref().unwrap()
    ));
    assert!(format!("{second:?}").contains("camera: true"));
    first.video.unwrap().set_enabled(false);
    assert!(!teams_core::VideoOutput::enabled(
        &*first_engine.media.video.unwrap().output
    ));
    assert!(teams_core::VideoOutput::enabled(&second.video.unwrap()));
    let mut speaker = devices.playback().speaker.unwrap().speaker().unwrap();
    speaker.play(&teams_media::devices::AudioFrame::silence()).unwrap();
    assert!(format!("{devices:?}").contains("CallDevices"));
    let (opened, _) = super::open().unwrap();
    assert!(opened.media.video.is_some());
}

#[test]
fn a_device_backend_failure_is_returned_before_any_account_calling_is_built() {
    REFUSE.set(true);
    let refused = super::CallDevices::open().unwrap_err();
    REFUSE.set(false);
    assert!(matches!(refused, DeviceError::Refused(_)));
}

#[test]
fn the_real_backend_in_a_test_build_refuses_the_desktop() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", "calling::tests::real_guard_child", "--nocapture"])
        .env_clear()
        .env("NGTEAMS_CALL_DEVICE_GUARD", "1")
        .env("HOME", directory.path());
    for key in ["PATH", "LANG", "LD_LIBRARY_PATH", "LLVM_PROFILE_FILE"] {
        if let Some(value) = std::env::var_os(key) {
            child.env(key, value);
        }
    }
    let output = child.output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn real_guard_child() {
    if std::env::var_os("NGTEAMS_CALL_DEVICE_GUARD").is_none() {
        return;
    }
    let actual = teams_media::devices::pipewire::PipeWireDevices::connect();
    assert!(
        matches!(actual, Err(DeviceError::Refused(_))),
        "a test build never opens the personal device backend"
    );
}
