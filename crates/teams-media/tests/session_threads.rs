//! The media session runs on threads of its own, which closing it joins. A test binary of its own, so no other
//! session's threads share the process.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::time::{Duration, Instant};

use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
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

#[test]
fn the_session_runs_on_threads_of_its_own_and_closing_it_joins_them() {
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
    let devices = FileDevices {
        input: AudioInput::Tone(Tone {
            hz: 440.0,
            amplitude: 0.5,
        }),
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    };
    let session = MediaSession::start(peer, &devices, SessionConfig::default()).unwrap();
    // a thread names itself once it runs
    let limit = Instant::now() + Duration::from_secs(10);
    let mut names = thread_names();
    while !THREADS.iter().all(|thread| names.iter().any(|name| name == thread)) && Instant::now() < limit {
        std::thread::sleep(Duration::from_millis(10));
        names = thread_names();
    }
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
