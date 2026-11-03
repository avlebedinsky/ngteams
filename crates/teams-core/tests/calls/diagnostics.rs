//! Call lifecycle logs and actual device failures through the loopback engine.

use std::sync::{Arc, Mutex, Once};

use teams_core::{CallAction, CallStatus, End};
use teams_fence::Mri;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, Devices, VideoSource};
use teams_mock::{AB_CHAT, ECHO, LAB_A};

use super::support::{A_TONE, Lab};

fn test_call() -> CallAction {
    CallAction::TestCall { echo: Mri::new(ECHO) }
}

static LOG: Capture = Capture(Mutex::new(Vec::new()));
static INSTALL: Once = Once::new();

struct Capture(Mutex<Vec<String>>);

impl log::Log for Capture {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info && metadata.target().starts_with("teams_core")
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            self.0
                .lock()
                .unwrap()
                .push(format!("{}: {}", record.target(), record.args()));
        }
    }

    fn flush(&self) {}
}

pub fn install() {
    INSTALL.call_once(|| {
        log::set_logger(&LOG).unwrap();
        log::set_max_level(log::LevelFilter::Info);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_diagnostics_keep_lifecycle_but_omit_credentials_names_and_sdp() {
    let lab = Lab::start().await;
    let mut account = lab.account(LAB_A, A_TONE).await;
    let requests = account.handle.requests();
    let call = requests.call(test_call()).await.unwrap();
    account.status(call, &CallStatus::Connected).await;
    assert!(matches!(
        requests.call(CallAction::Call { chat: AB_CHAT.into() }).await,
        Err(teams_core::EngineError::Call(teams_core::CallActionError::Busy))
    ));
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(account.ended(call).await, End::HungUp);
    let text = LOG.0.lock().unwrap().join("\n");
    for kept in [
        "action: a call from",
        "the test call of",
        "a hang-up of call",
        "Connected",
        "HungUp",
    ] {
        assert!(text.contains(kept), "missing lifecycle diagnostic {kept:?}");
    }
    for omitted in [
        "v=0",
        "application/sdp",
        "a=ice-pwd",
        "Lab",
        &lab.mock.token(LAB_A).unwrap(),
        &lab.mock.skypetoken(LAB_A).unwrap(),
    ] {
        assert!(!text.contains(omitted), "private call data entered diagnostics");
    }
}

struct FailingDevices {
    normal: FileDevices,
    microphone_fails: bool,
}

impl Devices for FailingDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        if self.microphone_fails {
            Err(DeviceError::Refused("synthetic microphone unavailable".into()))
        } else {
            self.normal.microphone()
        }
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        Err(DeviceError::Refused("synthetic speaker unavailable".into()))
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.normal.camera()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_open_failure_after_signaling_ends_the_call_and_sends_leave() {
    for microphone_fails in [true, false] {
        let lab = Lab::start().await;
        let mut account = lab
            .account_with_devices(Arc::new(FailingDevices {
                normal: FileDevices {
                    input: AudioInput::Tone(A_TONE),
                    recording: None,
                    camera: PatternCamera::default(),
                    pacing: Pacing::RealTime,
                },
                microphone_fails,
            }))
            .await;
        let call = account.handle.requests().call(test_call()).await.unwrap();
        assert_eq!(account.ended(call).await, End::Failed("media"));
        super::support::wait_until(|| lab.sent_by(LAB_A).last().is_some_and(|op| op == "leave")).await;
        assert_eq!(lab.sent_by(LAB_A).iter().filter(|op| op.as_str() == "start").count(), 1);
        assert!(
            LOG.0
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.contains("its media failed:"))
        );
    }
}

struct ClosedSource;
impl AudioSource for ClosedSource {
    fn next_frame(&mut self, _: std::time::Duration) -> Result<Option<teams_media::devices::AudioFrame>, DeviceError> {
        Err(DeviceError::Closed)
    }
}

struct ClosedSink;
impl AudioSink for ClosedSink {
    fn play(&mut self, _: &teams_media::devices::AudioFrame) -> Result<(), DeviceError> {
        Err(DeviceError::Closed)
    }
}

struct DisappearingDevice {
    normal: FileDevices,
    microphone: bool,
}

impl Devices for DisappearingDevice {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        if self.microphone {
            Ok(Box::new(ClosedSource))
        } else {
            self.normal.microphone()
        }
    }
    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        if self.microphone {
            self.normal.speaker()
        } else {
            Ok(Box::new(ClosedSink))
        }
    }
    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.normal.camera()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_audio_stream_disappearing_after_it_opened_is_reported_once_and_keeps_the_call_alive() {
    for microphone in [true, false] {
        let lab = Lab::start().await;
        let mut account = lab
            .account_with_devices(Arc::new(DisappearingDevice {
                normal: FileDevices {
                    input: AudioInput::Tone(A_TONE),
                    recording: None,
                    camera: PatternCamera::default(),
                    pacing: Pacing::RealTime,
                },
                microphone,
            }))
            .await;
        let call = account.handle.requests().call(test_call()).await.unwrap();
        account
            .next(|event| match event {
                teams_core::Event::AudioClosed {
                    call: ended,
                    microphone: gone,
                } if *ended == call => {
                    assert_eq!(*gone, microphone);
                    Some(())
                }
                teams_core::Event::Call {
                    call: ended,
                    status: CallStatus::Ended(end),
                } if *ended == call => panic!("the call ended instead of reporting its device: {end:?}"),
                _ => None,
            })
            .await;
        let after = account.events_within(std::time::Duration::from_millis(100)).await;
        assert!(!after.iter().any(|event| matches!(
            event,
            teams_core::Event::AudioClosed { .. }
                | teams_core::Event::Call {
                    status: CallStatus::Ended(_),
                    ..
                }
        )));
        account
            .handle
            .requests()
            .call(CallAction::Mute { call, muted: true })
            .await
            .unwrap();
        account
            .handle
            .requests()
            .call(CallAction::HangUp { call })
            .await
            .unwrap();
        assert_eq!(account.ended(call).await, End::HungUp);
    }
}
