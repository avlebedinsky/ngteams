//! Call lifecycle logs and actual device failures through the loopback engine.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};

use teams_core::{CallAction, CallStatus, End};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, Devices, VideoSource};
use teams_mock::{AB_CHAT, ECHO, LAB_A};
use teams_proto::Mri;

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
    microphone_fails: AtomicBool,
}

impl Devices for FailingDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        if self.microphone_fails.load(Ordering::Relaxed) {
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
async fn missing_audio_devices_keep_the_call_connected_and_unmute_can_retry_the_microphone() {
    for microphone_fails in [true, false] {
        let lab = Lab::start().await;
        let devices = Arc::new(FailingDevices {
            normal: FileDevices {
                input: AudioInput::Tone(A_TONE),
                recording: None,
                camera: PatternCamera::default(),
                pacing: Pacing::RealTime,
            },
            microphone_fails: AtomicBool::new(microphone_fails),
        });
        let mut account = lab.account_with_devices(devices.clone()).await;
        let call = account.handle.requests().call(test_call()).await.unwrap();
        account.status(call, &CallStatus::Connected).await;
        assert!(!lab.sent_by(LAB_A).iter().any(|op| op == "leave"));
        if microphone_fails {
            assert!(matches!(
                account
                    .handle
                    .requests()
                    .call(CallAction::Mute { call, muted: false })
                    .await,
                Err(teams_core::EngineError::Call(teams_core::CallActionError::Media(_)))
            ));
            devices.microphone_fails.store(false, Ordering::Relaxed);
        }
        account
            .handle
            .requests()
            .call(CallAction::Mute { call, muted: false })
            .await
            .unwrap();
        account
            .handle
            .requests()
            .call(CallAction::HangUp { call })
            .await
            .unwrap();
        assert_eq!(account.ended(call).await, End::HungUp);
        super::support::wait_until(|| lab.sent_by(LAB_A).last().is_some_and(|op| op == "leave")).await;
        assert_eq!(lab.sent_by(LAB_A).iter().filter(|op| op.as_str() == "start").count(), 1);
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

struct RecoveringDevices {
    normal: FileDevices,
    microphone_opens: std::sync::atomic::AtomicUsize,
    speaker_opens: std::sync::atomic::AtomicUsize,
    speaker_ready: AtomicBool,
    speaker_lost: Arc<AtomicBool>,
    played: Arc<std::sync::atomic::AtomicUsize>,
    microphone_ready: AtomicBool,
    retry_entered: AtomicBool,
    retry_release: (Mutex<bool>, std::sync::Condvar),
}
impl RecoveringDevices {
    fn new(microphone_ready: bool) -> Self {
        Self {
            normal: FileDevices {
                input: AudioInput::Tone(A_TONE),
                recording: None,
                camera: PatternCamera::default(),
                pacing: Pacing::RealTime,
            },
            microphone_opens: std::sync::atomic::AtomicUsize::default(),
            speaker_opens: std::sync::atomic::AtomicUsize::default(),
            speaker_ready: AtomicBool::new(false),
            speaker_lost: Arc::new(AtomicBool::new(false)),
            played: Arc::default(),
            microphone_ready: AtomicBool::new(microphone_ready),
            retry_entered: AtomicBool::new(false),
            retry_release: (Mutex::new(false), std::sync::Condvar::new()),
        }
    }
    fn release(&self) {
        *self.retry_release.0.lock().unwrap() = true;
        self.retry_release.1.notify_all();
    }
}
struct RecoveringSink {
    lost: Arc<AtomicBool>,
    played: Arc<std::sync::atomic::AtomicUsize>,
}
impl AudioSink for RecoveringSink {
    fn play(&mut self, _: &teams_media::devices::AudioFrame) -> Result<(), DeviceError> {
        if self.lost.load(Ordering::SeqCst) {
            return Err(DeviceError::Closed);
        }
        self.played.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
impl Devices for RecoveringDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        self.microphone_opens.fetch_add(1, Ordering::SeqCst);
        if !self.microphone_ready.load(Ordering::SeqCst) {
            return Err(DeviceError::Closed);
        }
        // Deterministically hold a retry while the engine processes a newer mute. The guard is bounded even if
        // the assertion fails, so a failed test cannot leave the blocking device worker behind.
        if self.microphone_opens.load(Ordering::SeqCst) > 1 {
            self.retry_entered.store(true, Ordering::SeqCst);
            let (_guard, timeout) = self
                .retry_release
                .1
                .wait_timeout_while(self.retry_release.0.lock().unwrap(), super::support::WAIT, |released| {
                    !*released
                })
                .unwrap();
            assert!(!timeout.timed_out(), "device retry was not released");
        }
        self.normal.microphone()
    }
    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        self.speaker_opens.fetch_add(1, Ordering::SeqCst);
        if !self.speaker_ready.load(Ordering::SeqCst) {
            return Err(DeviceError::Closed);
        }
        Ok(Box::new(RecoveringSink {
            lost: self.speaker_lost.clone(),
            played: self.played.clone(),
        }))
    }
    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.normal.camera()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_and_lost_speaker_reopens_while_the_microphone_keeps_running() {
    let lab = Lab::start().await;
    let devices = Arc::new(RecoveringDevices::new(true));
    let mut account = lab.account_with_devices(devices.clone()).await;
    let requests = account.handle.requests();
    let call = requests.call(test_call()).await.unwrap();
    account.status(call, &CallStatus::Connected).await;
    super::support::wait_until(|| devices.speaker_opens.load(Ordering::SeqCst) > 0).await;
    devices.speaker_ready.store(true, Ordering::SeqCst);
    super::support::wait_until(|| devices.played.load(Ordering::SeqCst) > 10).await;
    assert_eq!(devices.microphone_opens.load(Ordering::SeqCst), 1);
    devices.speaker_ready.store(false, Ordering::SeqCst);
    devices.speaker_lost.store(true, Ordering::SeqCst);
    let before = devices.speaker_opens.load(Ordering::SeqCst);
    super::support::wait_until(|| devices.speaker_opens.load(Ordering::SeqCst) > before).await;
    devices.speaker_lost.store(false, Ordering::SeqCst);
    devices.speaker_ready.store(true, Ordering::SeqCst);
    let played = devices.played.load(Ordering::SeqCst);
    super::support::wait_until(|| devices.played.load(Ordering::SeqCst) > played + 10).await;
    assert_eq!(devices.microphone_opens.load(Ordering::SeqCst), 1);
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(account.ended(call).await, End::HungUp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_recovery_never_replaces_a_newer_user_or_organizer_mute() {
    use super::support::{B_TONE, TALK, heard, wait_until};
    use teams_mock::{ABC_CHAT, LAB_B};
    for organizer in [false, true] {
        let lab = Lab::start().await;
        let devices = Arc::new(RecoveringDevices::new(false));
        devices.speaker_ready.store(true, Ordering::SeqCst);
        let mut account = lab.account_with_devices(devices.clone()).await;
        let mut other = lab.account_without_video(LAB_B, B_TONE).await;
        let their_call = other
            .handle
            .requests()
            .call(CallAction::Call { chat: ABC_CHAT.into() })
            .await
            .unwrap();
        let ring = account.incoming().await;
        let requests = account.handle.requests();
        let call = requests.call(CallAction::Answer { call: ring.call }).await.unwrap();
        account.status(call, &CallStatus::Connected).await;
        other.status(their_call, &CallStatus::Connected).await;
        wait_until(|| devices.microphone_opens.load(Ordering::SeqCst) == 1).await;
        devices.microphone_ready.store(true, Ordering::SeqCst);
        let unmute = requests.call(CallAction::Mute { call, muted: false });
        wait_until(|| devices.retry_entered.load(Ordering::SeqCst)).await;
        if organizer {
            other
                .handle
                .requests()
                .call(CallAction::MuteAll { call: their_call })
                .await
                .unwrap();
            account
                .next(|event| {
                    matches!(event, teams_core::Event::CallMeeting { state, .. } if state.muted_by_organizer.is_some())
                        .then_some(())
                })
                .await;
        } else {
            requests.call(CallAction::Mute { call, muted: true }).await.unwrap();
        }
        devices.release();
        unmute.await.unwrap();
        tokio::time::sleep(TALK).await;
        requests.call(CallAction::HangUp { call }).await.unwrap();
        account.ended(call).await;
        let id = lab.last_call();
        lab.mock.close_peer(&id, LAB_A).unwrap();
        let recorded = lab.mock.peer_recording(&id, LAB_A).unwrap();
        assert!(
            heard(&recorded, A_TONE, std::time::Duration::from_secs(1)) < 0.005,
            "a stale device completion unmuted capture (organizer={organizer})"
        );
        other
            .handle
            .requests()
            .call(CallAction::HangUp { call: their_call })
            .await
            .unwrap();
    }
}
