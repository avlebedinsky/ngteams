//! Two native audio sessions on loopback through the TURN test server, on the file backend: each one's tone reaches
//! the other's recording (Goertzel), on a clean relay and with loss and reordering injected (libwebrtc's jitter buffer
//! and FEC fill in); neither waits on a stuck device of its own, and a stuck speaker plays again; a device that goes
//! away and the far end's hang-up are reported. With A's speaker looped back into
//! its microphone, the session's audio processing takes B's echo out of what A sends: every frame A plays is the far
//! end its echo canceller hears.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod support;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use support::{
    A_TONE, B_TONE, CONNECT_WAIT, Fault, FaultyDevices, Gate, Heard, LoopbackDevices, file_devices, negotiate,
    recording, start, start_with, wait_for,
};
use teams_media::Error;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::wav::WavWriter;
use teams_media::devices::{AudioFrame, Devices, FRAME_DURATION, FRAME_SAMPLES};
use teams_media::processing::ProcessingConfig;
use teams_media::session::{SdpAnswer, SdpOffer, SessionDevice, SessionEvent, SessionStats};
use teams_media::test_signal::{rms, samples, speech_like};
use teams_media::test_turn::{Impairment, TurnServer};
use teams_media::tone::decibels;

/// How long the media runs once connected.
const TALK: Duration = Duration::from_secs(3);
/// What is judged of a recording: from this long after the media connected (the jitter buffer filling, the codec
/// warming up) ...
const SETTLE: Duration = Duration::from_millis(500);
/// ... to this long before the talk ended (what was still on its way).
const TAIL: Duration = Duration::from_millis(200);
/// How long a stuck speaker plays again before its delay is judged.
const RESUME: Duration = Duration::from_secs(1);
/// How long the echo canceller talks: [`CONVERGE`] to converge, then what is judged.
const ECHO_TALK: Duration = Duration::from_secs(8);
const CONVERGE: Duration = Duration::from_secs(4);
/// How far below what A's microphone hears the echo B gets back must be, in dB: less than the processing's own
/// margin on frames handed over in step (`tests/processing.rs`), as the speaker's and the microphone's threads hand
/// them over at their own times.
const SESSION_ECHO_MARGIN_DB: f32 = 20.0;

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
        // over short windows: libwebrtc's jitter buffer stretches and conceals around a loss, moving the tone's phase,
        // which one long window would read as a weaker tone
        let level = heard.windowed_level(theirs.hz);
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
    assert_both_ways(&run, 0.95);
}

#[test]
fn a_stuck_speaker_stops_only_what_is_played() {
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
    std::thread::sleep(RESUME);
    let resumed = a.stats();
    let (a_stats, _) = (a.close(), b.close());
    // A still sends: B hears A's tone all along
    let heard = Heard::read(&b_path, connected + SETTLE, ended.saturating_sub(TAIL));
    let share = heard.continuity(A_TONE.hz, 0.1);
    assert!(share >= 0.98, "B hears A in {share:.3} of the windows");
    eprintln!("stuck: {stuck:?}\nresumed: {resumed:?}");
    assert_eq!(stuck.frames_played, 0, "{stuck:?}");
    assert!(resumed.frames_played >= 20, "the speaker plays again: {resumed:?}");
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
    let (_, b_stats) = (a.close(), b.close());
    // A still plays what B sends
    let heard = Heard::read(&a_path, connected + SETTLE, ended.saturating_sub(TAIL));
    let share = heard.continuity(B_TONE.hz, 0.1);
    assert!(share >= 0.98, "A hears B in {share:.3} of the windows");
    assert_eq!((stuck.encoded, stuck.sent), (0, 0), "{stuck:?}");
    // B's playout goes on at libwebrtc's pace, with nothing of A's in it
    let silent = Heard::read(&b_path, connected + SETTLE, ended.saturating_sub(TAIL));
    let level = silent.level(A_TONE.hz);
    assert!(
        level < 0.03,
        "B heard A's tone at {level} while A's microphone was stuck: {b_stats:?}"
    );
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
        if gone == SessionDevice::Microphone {
            // a microphone that went away hears nothing: its last level isn't kept
            assert_eq!(session.levels().captured.to_bits(), 0);
        }
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
    // libwebrtc tells the far end gone as the media lost (ICE stops hearing it), then failed
    let limit = Instant::now() + Duration::from_secs(45);
    while Instant::now() < limit {
        if let Some(SessionEvent::Closed | SessionEvent::Disconnected | SessionEvent::Failed(_)) =
            a.next_event(Duration::from_millis(100))
        {
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

/// How long after A's speaker played a frame A's microphone hears its echo.
const ECHO_DELAY: Duration = Duration::from_millis(100);
/// How many runs the echo test may make for one whose echo path stayed as it was set.
const ECHO_RUNS: usize = 3;

/// What B gets back of its own speech-like signal when A's speaker is looped into A's microphone ([`ECHO_DELAY`]
/// later, 6 dB down), and the level of the echo A's microphone heard over the same stretch; `None` when the loop dropped frames
/// once the media connected (A's microphone thread fell a second behind: the echo path changed under the canceller).
fn echo_back(processing: ProcessingConfig) -> Option<(f32, f32)> {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let speech = dir.path().join("speech.wav");
    let mut writer = WavWriter::create(&speech).unwrap();
    writer.write(&samples(&speech_like(3000, 5, 0.5))).unwrap();
    writer.finish().unwrap();
    let b_path = recording(dir.path(), "b");
    let a_devices = LoopbackDevices::new(ECHO_DELAY, 6.0);
    let b_devices = FileDevices {
        input: AudioInput::Wav(speech),
        recording: Some(b_path.clone()),
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    };
    let started = Instant::now();
    let a = start_with(&turn, &a_devices, processing);
    let b = start(&turn, &b_devices);
    negotiate(&a, &b);
    wait_for(&a, &SessionEvent::Connected, CONNECT_WAIT);
    wait_for(&b, &SessionEvent::Connected, CONNECT_WAIT);
    let connected = started.elapsed();
    std::thread::sleep(ECHO_TALK);
    let ended = started.elapsed();
    let (a_stats, b_stats) = (a.close(), b.close());
    eprintln!("{processing:?}\nA {a_stats:?}\nB {b_stats:?}");
    let dropped = a_devices.dropped_since(started + connected);
    if dropped > 0 {
        let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
        eprintln!(
            "the loop dropped {dropped} frames after the media connected (load {}): run again",
            load.trim()
        );
        return None;
    }
    let (from, to) = (connected + CONVERGE, ended.saturating_sub(TAIL));
    let back = Heard::read(&b_path, from, to).rms();
    // A's speaker started with A's session, a frame every 10 ms
    let frame_at = |at: Duration| usize::try_from(at.as_micros() / FRAME_DURATION.as_micros()).unwrap();
    let played = a_devices.played.lock().unwrap();
    let stretch = &played[frame_at(from).min(played.len())..frame_at(to).min(played.len())];
    assert!(
        stretch.len() * FRAME_SAMPLES >= 48_000,
        "A played {} frames",
        played.len()
    );
    Some((back, rms(stretch) * a_devices.gain()))
}

/// [`echo_back`] from the first of [`ECHO_RUNS`] runs whose echo path stayed as it was set.
fn echo_back_undisturbed(processing: ProcessingConfig) -> (f32, f32) {
    (0..ECHO_RUNS)
        .find_map(|_| echo_back(processing))
        .unwrap_or_else(|| panic!("the loop dropped frames in all {ECHO_RUNS} runs"))
}

#[test]
fn the_session_takes_the_far_end_s_echo_out_of_what_it_sends() {
    // without the processing B gets its own echo back, about as loud as A's microphone heard it
    let (back, echo) = echo_back_undisturbed(ProcessingConfig::OFF);
    eprintln!("processing off: A's microphone heard the echo at {echo:.4}, B got back {back:.4}");
    assert!(echo > 0.02, "A's microphone heard B's speech: {echo}");
    assert!(decibels(back, echo) > -6.0, "B got its echo back at {back}");
    // with it, the echo is taken out
    let (back, echo) = echo_back_undisturbed(ProcessingConfig::CALL);
    let suppressed = -decibels(back, echo);
    eprintln!(
        "processing on: A's microphone heard the echo at {echo:.4}, B got back {back:.6} ({suppressed:.1} dB down)"
    );
    assert!(echo > 0.02, "A's microphone heard B's speech: {echo}");
    assert!(
        suppressed >= SESSION_ECHO_MARGIN_DB,
        "B's echo is only {suppressed:.1} dB down"
    );
}

#[test]
fn the_loopback_hands_a_played_frame_to_the_microphone_only_once_its_delay_passed() {
    let delay = Duration::from_millis(100);
    let devices = LoopbackDevices::new(delay, 6.0);
    let mut microphone = devices.microphone().unwrap();
    let mut speaker = devices.speaker().unwrap();
    let frame = AudioFrame::from_samples(&[0.5; FRAME_SAMPLES]).unwrap();
    let played = Instant::now();
    speaker.play(&frame).unwrap();
    assert!(
        microphone.next_frame(Duration::ZERO).unwrap().is_none(),
        "the microphone heard something before the delay"
    );
    let heard = microphone
        .next_frame(Duration::from_secs(2))
        .unwrap()
        .expect("the echo");
    let after = played.elapsed();
    assert!(after >= delay, "the echo came back after {after:?}");
    let expected = 0.5 * devices.gain();
    assert!(
        heard.samples().iter().all(|sample| (sample - expected).abs() < 1e-6),
        "the microphone heard something else than the played frame"
    );
}

#[test]
fn a_reoffer_renegotiates_the_running_line_and_the_audio_goes_on() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let b_path = recording(dir.path(), "b");
    let started = Instant::now();
    let a = start(&turn, &file_devices(A_TONE, &recording(dir.path(), "a")));
    let b = start(&turn, &file_devices(B_TONE, &b_path));
    negotiate(&a, &b);
    wait_for(&a, &SessionEvent::Connected, CONNECT_WAIT);
    wait_for(&b, &SessionEvent::Connected, CONNECT_WAIT);
    std::thread::sleep(SETTLE);

    // A offers the running line again (the same one audio line), B answers it
    let offer = a.reoffer().unwrap().to_sdp_string();
    assert_eq!(offer.matches("m=audio").count(), 1, "{offer}");
    let answer = b.accept_offer(SdpOffer::from_sdp_string(&offer).unwrap()).unwrap();
    a.accept_answer(SdpAnswer::from_sdp_string(&answer.to_sdp_string()).unwrap())
        .unwrap();
    let renegotiated = started.elapsed();
    std::thread::sleep(TALK);
    let ended = started.elapsed();
    let (a, b) = (a.close(), b.close());
    let heard = Heard::read(&b_path, renegotiated + SETTLE, ended.saturating_sub(TAIL));
    let level = heard.level(A_TONE.hz);
    let share = heard.continuity(A_TONE.hz, 0.1);
    eprintln!("after the reoffer B hears A at {level:.3} in {share:.3} of the windows\nA {a:?}\nB {b:?}");
    assert!(level > 0.3, "B hears A's tone at {level} after the renegotiation");
    assert!(share >= 0.9, "B hears A's tone in {share:.3} of the windows");
}

#[test]
fn a_muted_microphone_sends_silence_and_the_line_goes_on() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let b_path = recording(dir.path(), "b");
    let started = Instant::now();
    let a = start(&turn, &file_devices(A_TONE, &recording(dir.path(), "a")));
    let b = start(&turn, &file_devices(B_TONE, &b_path));
    negotiate(&a, &b);
    wait_for(&a, &SessionEvent::Connected, CONNECT_WAIT);
    wait_for(&b, &SessionEvent::Connected, CONNECT_WAIT);
    std::thread::sleep(SETTLE);
    assert!(!a.is_muted());
    a.set_muted(true);
    assert!(a.is_muted());
    let muted = started.elapsed();
    std::thread::sleep(Duration::from_millis(1_500));
    a.set_muted(false);
    let unmuted = started.elapsed();
    std::thread::sleep(Duration::from_millis(1_500));
    let ended = started.elapsed();
    let _ = (a.close(), b.close());
    let silent = Heard::read(&b_path, muted + SETTLE, unmuted.saturating_sub(TAIL));
    let heard = Heard::read(&b_path, unmuted + SETTLE, ended.saturating_sub(TAIL));
    let (while_muted, after) = (silent.level(A_TONE.hz), heard.level(A_TONE.hz));
    eprintln!("B hears A at {while_muted:.4} while muted, {after:.3} after");
    assert!(while_muted < 0.03, "B heard A's tone while A was muted: {while_muted}");
    assert!(after > 0.3, "B hears A's tone again once unmuted: {after}");
}
