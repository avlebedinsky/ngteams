//! Two media sessions through the loopback TURN server, on the file backend (or devices of a test's own).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::{AudioFrame, AudioSink, AudioSource, DeviceError, Devices, SAMPLE_RATE, VideoSource, wav};
use teams_media::peer::RelayedPeer;
use teams_media::relay::{RelayLink, RelayOptions, RelayServer};
use teams_media::session::{MediaSession, SessionConfig, SessionEvent};
use teams_media::test_turn::{PASSWORD, TurnServer, USER};
use teams_media::tone::{Tone, amplitude_at};
use teams_media::turn::{Credentials, Transport};

/// How long ICE and DTLS may take through the relay on a loaded machine.
pub const CONNECT_WAIT: Duration = Duration::from_secs(20);

pub const A_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
pub const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};

/// A peer behind its own allocation on `turn`.
pub fn relayed(turn: &TurnServer) -> RelayedPeer {
    let server = RelayServer {
        addr: turn.udp,
        transport: Transport::Udp,
    };
    let credentials = Credentials {
        username: USER.into(),
        password: PASSWORD.into(),
    };
    let link = RelayLink::allocate(server, credentials, &RelayOptions::default()).unwrap();
    RelayedPeer::new(Instant::now(), link).unwrap()
}

/// The file backend at the device's pace: `tone` in, a recording at `recording` out.
pub fn file_devices(tone: Tone, recording: &Path) -> FileDevices {
    FileDevices {
        input: AudioInput::Tone(tone),
        recording: Some(recording.to_path_buf()),
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    }
}

/// A's offer and B's answer, each carried as text as signaling would carry it.
pub fn negotiate(a: &MediaSession, b: &MediaSession) {
    let offer = a.offer_audio().unwrap();
    let offer = SdpOffer::from_sdp_string(&offer.to_sdp_string()).unwrap();
    assert!(
        offer.to_sdp_string().contains("typ relay"),
        "only the relayed candidate"
    );
    let answer = b.accept_offer(offer).unwrap();
    let answer = SdpAnswer::from_sdp_string(&answer.to_sdp_string()).unwrap();
    a.accept_answer(answer).unwrap();
}

/// Waits for `session` to report `wanted`, failing on any final event or after `wait`.
pub fn wait_for(session: &MediaSession, wanted: &SessionEvent, wait: Duration) {
    let limit = Instant::now() + wait;
    loop {
        let left = limit.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "no {wanted:?} within {wait:?}");
        match session.next_event(left) {
            Some(event) if &event == wanted => return,
            Some(event @ (SessionEvent::Failed(_) | SessionEvent::Closed)) => panic!("{event:?} before {wanted:?}"),
            _ => {}
        }
    }
}

/// A recording and the stretch of it to judge: from `from` to `to` after the speaker started.
pub struct Heard {
    pub samples: Vec<f32>,
    pub from: Duration,
    pub to: Duration,
}

impl Heard {
    pub fn read(path: &Path, from: Duration, to: Duration) -> Self {
        Self {
            samples: wav::read(path).unwrap(),
            from,
            to,
        }
    }

    fn stretch(&self) -> &[f32] {
        let index = |at: Duration| {
            let samples = at.as_micros() * u128::from(SAMPLE_RATE) / 1_000_000;
            usize::try_from(samples).unwrap().min(self.samples.len())
        };
        &self.samples[index(self.from)..index(self.to)]
    }

    /// The level of `hz` over the whole stretch.
    pub fn level(&self, hz: f32) -> f32 {
        amplitude_at(self.stretch(), hz)
    }

    /// The share of the stretch's 20 ms windows in which `hz` is heard at `level` or more.
    pub fn continuity(&self, hz: f32, level: f32) -> f64 {
        let (windows, _) = self.stretch().as_chunks::<960>();
        assert!(
            windows.len() >= 25,
            "a stretch of {} windows is too short",
            windows.len()
        );
        let heard = windows
            .iter()
            .filter(|window| amplitude_at(window.as_slice(), hz) >= level)
            .count();
        let count = |n: usize| f64::from(u32::try_from(n).unwrap());
        count(heard) / count(windows.len())
    }
}

/// A gate a stuck device waits on until the test opens it.
#[derive(Clone, Default)]
pub struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    pub fn wait(&self) {
        let (open, changed) = &*self.0;
        let guard = open.lock().unwrap_or_else(PoisonError::into_inner);
        let _open = changed
            .wait_while(guard, |open| !*open)
            .unwrap_or_else(PoisonError::into_inner);
    }

    pub fn open(&self) {
        let (open, changed) = &*self.0;
        *open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        changed.notify_all();
    }
}

/// How a test's own device misbehaves.
#[derive(Clone)]
pub enum Fault {
    /// Its first use blocks until the gate opens.
    Stuck(Gate),
    /// It says it was closed.
    Gone,
}

/// The file backend with the microphone or the speaker replaced by a faulty one.
pub struct FaultyDevices {
    pub files: FileDevices,
    pub microphone: Option<Fault>,
    pub speaker: Option<Fault>,
}

struct FaultySource(Fault, bool);

impl AudioSource for FaultySource {
    fn next_frame(&mut self, _timeout: Duration) -> Result<Option<AudioFrame>, DeviceError> {
        match &self.0 {
            Fault::Stuck(gate) if !self.1 => {
                self.1 = true;
                gate.wait();
                Ok(None)
            }
            Fault::Stuck(_) => Ok(None),
            Fault::Gone => Err(DeviceError::Closed),
        }
    }
}

struct FaultySink(Fault, bool);

impl AudioSink for FaultySink {
    fn play(&mut self, _frame: &AudioFrame) -> Result<(), DeviceError> {
        match &self.0 {
            Fault::Stuck(gate) if !self.1 => {
                self.1 = true;
                gate.wait();
                Ok(())
            }
            Fault::Stuck(_) => Ok(()),
            Fault::Gone => Err(DeviceError::Closed),
        }
    }
}

impl Devices for FaultyDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        match &self.microphone {
            Some(fault) => Ok(Box::new(FaultySource(fault.clone(), false))),
            None => self.files.microphone(),
        }
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        match &self.speaker {
            Some(fault) => Ok(Box::new(FaultySink(fault.clone(), false))),
            None => self.files.speaker(),
        }
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.files.camera()
    }
}

/// Where a test's recordings go.
pub fn recording(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.wav"))
}

/// Starts a session on `devices` behind its own allocation on `turn`.
pub fn start(turn: &TurnServer, devices: &dyn Devices) -> MediaSession {
    MediaSession::start(relayed(turn), devices, SessionConfig::default()).unwrap()
}
