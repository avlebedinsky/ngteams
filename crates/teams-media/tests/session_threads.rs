//! The media session runs on threads of its own, which closing it joins; its devices open (and their threads start)
//! only when asked. A test binary of its own, so no other session's threads share the process.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, Devices, VideoSource};
use teams_media::peer::RelayedPeer;
use teams_media::relay::{RelayLink, RelayOptions, RelayServer};
use teams_media::session::{MediaSession, SessionConfig};
use teams_media::test_turn::{PASSWORD, TurnServer, USER};
use teams_media::tone::Tone;
use teams_media::turn::{Credentials, Transport};

const THREADS: [&str; 3] = ["teams-media-net", "teams-media-mic", "teams-media-spk"];

/// The names of this process's threads.
fn thread_names() -> Vec<String> {
    fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|task| fs::read_to_string(task.ok()?.path().join("comm")).ok())
        .map(|name| name.trim().to_owned())
        .collect()
}

/// Waits until each of `threads` runs (a thread names itself once it runs), up to 10 s; this process's threads then.
fn running(threads: &[&str]) -> Vec<String> {
    let limit = Instant::now() + Duration::from_secs(10);
    let mut names = thread_names();
    while !threads.iter().all(|thread| names.iter().any(|name| name == thread)) && Instant::now() < limit {
        std::thread::sleep(Duration::from_millis(10));
        names = thread_names();
    }
    names
}

/// File devices that count how often the microphone was opened.
struct Counted {
    devices: FileDevices,
    microphones: AtomicUsize,
}

impl Devices for Counted {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        self.microphones.fetch_add(1, Ordering::SeqCst);
        self.devices.microphone()
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        self.devices.speaker()
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.devices.camera()
    }
}

#[test]
fn the_session_runs_on_threads_of_its_own_its_devices_open_when_asked_and_closing_it_joins_them() {
    assert!(
        thread_names().iter().all(|name| !THREADS.contains(&name.as_str())),
        "no session yet"
    );
    let turn = TurnServer::start().unwrap();
    let server = RelayServer {
        addr: turn.udp,
        transport: Transport::Udp,
    };
    let credentials = Credentials {
        username: USER.into(),
        password: PASSWORD.into(),
    };
    let link = RelayLink::allocate(server, credentials, &RelayOptions::default()).unwrap();
    let peer = RelayedPeer::new(Instant::now(), link).unwrap();
    let devices = Counted {
        devices: FileDevices {
            input: AudioInput::Tone(Tone {
                hz: 440.0,
                amplitude: 0.5,
            }),
            recording: None,
            camera: PatternCamera::default(),
            pacing: Pacing::RealTime,
        },
        microphones: AtomicUsize::new(0),
    };
    let session = MediaSession::start_without_devices(peer, SessionConfig::default()).unwrap();
    let names = running(&THREADS[..1]);
    session.offer_audio().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let names_then = thread_names();
    assert_eq!(devices.microphones.load(Ordering::SeqCst), 0, "no device opened");
    for thread in &THREADS[1..] {
        assert!(
            !names.iter().chain(&names_then).any(|name| name == thread),
            "{thread} waits for the devices: {names_then:?}"
        );
    }
    session.open_devices(&devices).unwrap();
    session.open_devices(&devices).unwrap();
    assert_eq!(devices.microphones.load(Ordering::SeqCst), 1, "opened once");
    let names = running(&THREADS);
    for thread in THREADS {
        assert_eq!(
            names.iter().filter(|name| *name == thread).count(),
            1,
            "{thread} runs once: {names:?}"
        );
    }
    let _ = session.close();
    let names = thread_names();
    assert!(
        names.iter().all(|name| !THREADS.contains(&name.as_str())),
        "closing joined every thread: {names:?}"
    );
}
