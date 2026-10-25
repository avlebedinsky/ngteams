//! Two native audio sessions on loopback through the TURN test server, on the file backend: each one's tone reaches
//! the other's recording (Goertzel), on a clean relay and with loss and reordering injected; neither waits on a stuck
//! device of its own, and a stuck speaker that plays again does so at the jitter buffer's depth; a device that goes
//! away and the far end's hang-up are reported; DTX follows the far end's Opus line.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod support;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer};
use support::{
    A_TONE, B_TONE, CONNECT_WAIT, Fault, FaultyDevices, Gate, Heard, file_devices, negotiate, recording, start,
    wait_for,
};
use teams_media::Error;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::session::{SessionDevice, SessionEvent, SessionStats};
use teams_media::test_turn::{Impairment, TurnServer};

/// How long the media runs once connected.
const TALK: Duration = Duration::from_secs(3);
/// What is judged of a recording: from this long after the media connected (the jitter buffer filling, the codec
/// warming up) ...
const SETTLE: Duration = Duration::from_millis(500);
/// ... to this long before the talk ended (what was still on its way).
const TAIL: Duration = Duration::from_millis(200);
/// How long a stuck speaker plays again before its delay is judged.
const RESUME: Duration = Duration::from_secs(1);

/// One run: A and B on `turn`, talking for [`TALK`]; returns what each heard of the other and each one's counts.
struct Run {
    a_heard: Heard,
    b_heard: Heard,
    a: SessionStats,
    b: SessionStats,
}

fn talk(turn: &TurnServer) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let (a_path, b_path) = (recording(dir.path(), "a"), recording(dir.path(), "b"));
    let started = Instant::now();
    let a = start(turn, &file_devices(A_TONE, &a_path));
    let b = start(turn, &file_devices(B_TONE, &b_path));
    negotiate(&a, &b);
    wait_for(&a, &SessionEvent::Connected, CONNECT_WAIT);
    wait_for(&b, &SessionEvent::Connected, CONNECT_WAIT);
    let connected = started.elapsed();
    std::thread::sleep(TALK);
    let ended = started.elapsed();
    let (a, b) = (a.close(), b.close());
    let (from, to) = (connected + SETTLE, ended.saturating_sub(TAIL));
    Run {
        a_heard: Heard::read(&a_path, from, to),
        b_heard: Heard::read(&b_path, from, to),
        a,
        b,
    }
}

/// Each side hears the other's tone, all along, and not its own.
fn assert_both_ways(run: &Run, continuity: f64) {
    for (who, heard, theirs, own) in [("A", &run.a_heard, B_TONE, A_TONE), ("B", &run.b_heard, A_TONE, B_TONE)] {
        let level = heard.level(theirs.hz);
        assert!(level > 0.3, "{who} hears the far tone at {level}");
        let echo = heard.level(own.hz);
        assert!(echo < 0.03, "{who} hears its own tone at {echo}");
        let share = heard.continuity(theirs.hz, 0.1);
        eprintln!("{who} hears the far tone at {level:.3} in {share:.3} of the windows, its own at {echo:.4}");
        assert!(
            share >= continuity,
            "{who} hears the far tone in {share:.3} of the 20 ms windows"
        );
    }
}

#[test]
fn a_tone_crosses_both_ways_through_the_relay() {
    let turn = TurnServer::start().unwrap();
    let run = talk(&turn);
    eprintln!("clean relay: A {:?}\nB {:?}", run.a, run.b);
    assert_both_ways(&run, 0.98);
    for stats in [run.a, run.b] {
        // 50 packets a second; the talk is 3 s and more
        assert!(stats.sent >= 140, "{stats:?}");
        assert!(stats.received >= 140, "{stats:?}");
        assert_eq!(stats.malformed, 0, "{stats:?}");
    }
    assert!(
        turn.counters.channel_data.load(Ordering::Relaxed) > 0,
        "the media went through the relay"
    );
}

#[test]
fn a_tone_crosses_both_ways_with_loss_and_reordering() {
    let turn = TurnServer::start_impaired(Impairment {
        loss_percent: 5,
        reorder_percent: 10,
        seed: 0x5EED,
    })
    .unwrap();
    let run = talk(&turn);
    let (dropped, reordered) = (
        turn.counters.rtp_dropped.load(Ordering::Relaxed),
        turn.counters.rtp_reordered.load(Ordering::Relaxed),
    );
    eprintln!(
        "impaired relay: dropped {dropped}, reordered {reordered}\nA {:?}\nB {:?}",
        run.a, run.b
    );
    assert!(dropped >= 5 && reordered >= 10, "the relay impaired the media");
    // what was lost is filled in (from FEC or by concealment), so the tone is heard all along
    assert_both_ways(&run, 0.97);
    let jitter = |stats: &SessionStats| stats.jitter;
    let (a, b) = (jitter(&run.a), jitter(&run.b));
    assert!(
        a.recovered + b.recovered > 0,
        "a lost packet came back from FEC: {a:?} {b:?}"
    );
    // a packet one place out of order still comes in time for its turn
    let late = a.late + b.late;
    assert!(
        late * 4 <= u64::try_from(reordered).unwrap(),
        "{late} late of {reordered} reordered"
    );
    assert!(a.played + b.played >= 250, "{a:?} {b:?}");
}

#[test]
fn a_stuck_speaker_fills_only_the_jitter_buffer() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let gate = Gate::default();
    let a_devices = FaultyDevices {
        files: file_devices(A_TONE, &recording(dir.path(), "a")),
        microphone: None,
        speaker: Some(Fault::Stuck(gate.clone())),
    };
    let b_path = recording(dir.path(), "b");
    let started = Instant::now();
    let a = start(&turn, &a_devices);
    let b = start(&turn, &file_devices(B_TONE, &b_path));
    negotiate(&a, &b);
    wait_for(&a, &SessionEvent::Connected, CONNECT_WAIT);
    wait_for(&b, &SessionEvent::Connected, CONNECT_WAIT);
    let connected = started.elapsed();
    std::thread::sleep(TALK);
    let ended = started.elapsed();
    let stuck = a.stats();
    gate.open();
    // once the speaker plays again, A's delay goes back to the jitter buffer's depth: the backlog is shed, not played
    std::thread::sleep(RESUME);
    let resumed = a.stats();
    let (a_stats, _) = (a.close(), b.close());
    // A still sends: B hears A's tone all along
    let heard = Heard::read(&b_path, connected + SETTLE, ended.saturating_sub(TAIL));
    let share = heard.continuity(A_TONE.hz, 0.1);
    assert!(share >= 0.98, "B hears A in {share:.3} of the windows");
    // A keeps receiving into its jitter buffer, which drops its oldest beyond its capacity
    assert!(stuck.received >= 140, "{stuck:?}");
    assert_eq!(stuck.frames_played, 0, "{stuck:?}");
    assert!(stuck.jitter.overflowed > 0 && stuck.buffered == 50, "{stuck:?}");
    eprintln!("stuck: {stuck:?}\nresumed: {resumed:?}");
    assert!(resumed.frames_played >= 20, "the speaker plays again: {resumed:?}");
    // the buffer is shed to its depth (3) whenever it holds more than twice that; a few more may arrive between turns
    assert!(
        resumed.buffered <= 10,
        "{} packets still queued ahead of the speaker: {resumed:?}",
        resumed.buffered
    );
    assert!(resumed.jitter.shed >= 40, "{resumed:?}");
    assert!(a_stats.sent >= 140, "{a_stats:?}");
}

#[test]
fn a_stuck_microphone_stops_only_what_is_sent() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let gate = Gate::default();
    let a_path = recording(dir.path(), "a");
    let a_devices = FaultyDevices {
        files: file_devices(A_TONE, &a_path),
        microphone: Some(Fault::Stuck(gate.clone())),
        speaker: None,
    };
    let started = Instant::now();
    let a = start(&turn, &a_devices);
    let b = start(&turn, &file_devices(B_TONE, &recording(dir.path(), "b")));
    negotiate(&a, &b);
    wait_for(&a, &SessionEvent::Connected, CONNECT_WAIT);
    wait_for(&b, &SessionEvent::Connected, CONNECT_WAIT);
    let connected = started.elapsed();
    std::thread::sleep(TALK);
    let ended = started.elapsed();
    let stuck = a.stats();
    gate.open();
    let (_, b_stats) = (a.close(), b.close());
    // A still plays what B sends
    let heard = Heard::read(&a_path, connected + SETTLE, ended.saturating_sub(TAIL));
    let share = heard.continuity(B_TONE.hz, 0.1);
    assert!(share >= 0.98, "A hears B in {share:.3} of the windows");
    assert_eq!((stuck.encoded, stuck.sent), (0, 0), "{stuck:?}");
    assert_eq!(b_stats.received, 0, "{b_stats:?}");
}

#[test]
fn a_device_that_goes_away_is_reported_and_the_rest_goes_on() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    for (microphone, speaker, gone) in [
        (Some(Fault::Gone), None, SessionDevice::Microphone),
        (None, Some(Fault::Gone), SessionDevice::Speaker),
    ] {
        let devices = FaultyDevices {
            files: file_devices(A_TONE, &recording(dir.path(), "a")),
            microphone,
            speaker,
        };
        let session = start(&turn, &devices);
        wait_for(&session, &SessionEvent::DeviceClosed(gone), CONNECT_WAIT);
        // the network thread still answers
        session.offer_audio().unwrap();
        drop(session);
    }
}

#[test]
fn the_far_ends_hang_up_is_reported() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let a = start(&turn, &file_devices(A_TONE, &recording(dir.path(), "a")));
    let b = start(&turn, &file_devices(B_TONE, &recording(dir.path(), "b")));
    negotiate(&a, &b);
    wait_for(&a, &SessionEvent::Connected, CONNECT_WAIT);
    wait_for(&b, &SessionEvent::Connected, CONNECT_WAIT);
    let _ = b.close();
    let limit = Instant::now() + CONNECT_WAIT;
    while Instant::now() < limit {
        if a.next_event(Duration::from_millis(100)) == Some(SessionEvent::Closed) {
            return;
        }
    }
    panic!("A never heard B hang up");
}

#[test]
fn an_answer_with_no_offer_is_refused() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let a = start(&turn, &file_devices(A_TONE, &recording(dir.path(), "a")));
    let b = start(&turn, &file_devices(B_TONE, &recording(dir.path(), "b")));
    let offer = a.offer_audio().unwrap();
    let answer = b.accept_offer(offer).unwrap().to_sdp_string();
    let answer = || SdpAnswer::from_sdp_string(&answer).unwrap();
    // B made no offer of its own: the answer has nothing to answer there
    assert!(matches!(b.accept_answer(answer()), Err(Error::Rtc(_))));
    a.accept_answer(answer()).unwrap();
    assert!(
        matches!(a.accept_answer(answer()), Err(Error::Rtc(_))),
        "the offer was answered already"
    );
}

#[test]
fn dtx_follows_the_far_ends_opus_line() {
    // B's answer asks for DTX (as the media controller's does) and A's offer doesn't: over silence, A sends only DTX's
    // occasional keep-alive while B keeps sending every 20 ms
    let turn = TurnServer::start().unwrap();
    let silent = || FileDevices {
        input: AudioInput::Silence,
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    };
    let a = start(&turn, &silent());
    let b = start(&turn, &silent());
    let offer = a.offer_audio().unwrap();
    let offer = SdpOffer::from_sdp_string(&offer.to_sdp_string()).unwrap();
    let answer = b.accept_offer(offer).unwrap().to_sdp_string();
    assert!(!answer.contains("usedtx"), "{answer}");
    let asked = answer.replace("useinbandfec=1", "useinbandfec=1;usedtx=1");
    assert_ne!(asked, answer, "the answer has an Opus fmtp:\n{answer}");
    a.accept_answer(SdpAnswer::from_sdp_string(&asked).unwrap()).unwrap();
    wait_for(&a, &SessionEvent::Connected, CONNECT_WAIT);
    wait_for(&b, &SessionEvent::Connected, CONNECT_WAIT);
    let (a_from, b_from) = (a.stats().encoded, b.stats().encoded);
    std::thread::sleep(TALK);
    let (a_stats, b_stats) = (a.close(), b.close());
    let (a_sent, b_sent) = (a_stats.encoded - a_from, b_stats.encoded - b_from);
    eprintln!("over {TALK:?} of silence A encoded {a_sent} packets, B {b_sent}");
    assert!(b_sent >= 100, "B has no DTX: {b_sent} packets");
    assert!(a_sent * 5 <= b_sent, "A has DTX: {a_sent} packets against B's {b_sent}");
}
