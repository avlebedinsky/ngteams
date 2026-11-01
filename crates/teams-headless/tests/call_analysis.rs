//! Causal timing of the client's own keyed tone: a short recording must not claim unplayed later blocks, and a
//! speaker that opens after the microphone still preserves the positive physical round trip.

#![cfg(test)]

use std::time::Duration;

use teams_headless::call::{CallFiles, Keying};
use teams_headless::echo;
use teams_media::devices::{AudioFrame, Devices, wav};
use teams_media::tone::Tone;

const RATE: usize = 48_000;
const TONE: Tone = Tone {
    hz: 1_000.0,
    amplitude: 0.5,
};

fn samples(duration: Duration) -> usize {
    usize::try_from(duration.as_nanos() * 48_000 / 1_000_000_000).unwrap()
}

fn recording(files: &CallFiles, received: &[f32]) {
    let mut writer = wav::WavWriter::create(&files.recording).unwrap();
    writer.write(received).unwrap();
    writer.finish().unwrap();
}

#[test]
fn the_own_tone_cannot_claim_later_blocks_after_a_slot_is_lost_from_a_short_recording() {
    let dir = tempfile::tempdir().unwrap();
    let sent = Duration::from_secs(54);
    let files = CallFiles::write_for(dir.path().join("short.wav"), TONE.hz, Keying::Keyed, sent).unwrap();
    let mut source = files.devices().microphone().unwrap();
    source.next_frame(Duration::ZERO).unwrap().unwrap();
    drop(source);
    let mut sink = files.devices().speaker().unwrap();
    sink.play(&AudioFrame::silence()).unwrap();
    drop(sink);
    let signal = echo::keyed(TONE, Duration::from_secs(4));
    let mut received = vec![0.0; RATE * 9 / 2];
    for (at, sample) in signal.iter().enumerate() {
        received[RATE / 2 + at] = sample * 0.7;
    }
    // Losing slot 18 creates the ten-slot bit sequence of the unplayed block at 46 s.
    received[RATE / 2 + 18 * 9_600..RATE / 2 + 19 * 9_600].fill(0.0);
    let opened = files.sample_starts();
    let late = opened
        .speaker
        .unwrap()
        .wall
        .duration_since(opened.microphone.unwrap().wall)
        .unwrap();
    recording(&files, &received[samples(late)..]);
    let heard = files.heard_own_echo(TONE.hz).unwrap().unwrap();
    assert!(
        heard.blocks.iter().all(|block| block.sent_at < Duration::from_secs(4)),
        "{heard:?}"
    );
    assert!(
        heard.delay_percentile(50).unwrap().abs_diff(Duration::from_millis(500)) < Duration::from_millis(30),
        "{heard:?}"
    );
}

#[test]
fn the_own_tone_is_rebased_to_the_microphone_when_the_speaker_starts_later() {
    let dir = tempfile::tempdir().unwrap();
    let sent = Duration::from_secs(8);
    let files = CallFiles::write_for(dir.path().join("late-speaker.wav"), TONE.hz, Keying::Keyed, sent).unwrap();
    let mut source = files.devices().microphone().unwrap();
    source.next_frame(Duration::ZERO).unwrap().unwrap();
    drop(source);
    std::thread::sleep(Duration::from_millis(500));
    let mut sink = files.devices().speaker().unwrap();
    sink.play(&AudioFrame::silence()).unwrap();
    drop(sink);
    let opened = files.sample_starts();
    let late = opened
        .speaker
        .unwrap()
        .wall
        .duration_since(opened.microphone.unwrap().wall)
        .unwrap();
    let physical = Duration::from_millis(200);
    assert!(late > physical, "the recording's raw offset is negative");
    let signal = echo::keyed(TONE, sent);
    let mut at_speaker = vec![0.0; samples(physical)];
    at_speaker.extend(signal);
    recording(&files, &at_speaker[samples(late)..]);
    let heard = files
        .heard_own_echo(TONE.hz)
        .unwrap()
        .expect("the later capture retains the keyed tone");
    assert!(
        heard.delay_percentile(50).unwrap().abs_diff(physical) < Duration::from_millis(30),
        "{heard:?}"
    );
}

#[test]
fn a_delayed_microphone_constructor_does_not_add_its_delay_to_the_physical_echo() {
    use std::io::Write as _;
    let dir = tempfile::tempdir().unwrap();
    let sent = Duration::from_secs(8);
    let files = CallFiles::write_for(dir.path().join("constructor.wav"), TONE.hz, Keying::Keyed, sent).unwrap();
    let fifo = dir.path().join("synthetic-microphone.wav");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let bytes = std::fs::read(&files.sent).unwrap();
    let producer_path = fifo.clone();
    let producer = std::thread::spawn(move || {
        let mut fifo = std::fs::OpenOptions::new().write(true).open(producer_path).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        fifo.write_all(&bytes).unwrap();
    });
    let mut devices = files.devices();
    devices.files.input = teams_media::devices::file::AudioInput::Wav(fifo);
    let mut microphone = devices.microphone().unwrap();
    producer.join().unwrap();
    let mut speaker = devices.speaker().unwrap();
    let opened = files.opened();
    assert!(
        opened
            .speaker
            .unwrap()
            .duration_since(opened.microphone.unwrap())
            .unwrap()
            >= Duration::from_millis(500)
    );
    assert!(microphone.next_frame(Duration::ZERO).unwrap().is_some());
    speaker.play(&teams_media::devices::AudioFrame::silence()).unwrap();
    drop(microphone);
    drop(speaker);
    let physical = Duration::from_millis(200);
    let mut received = vec![0.0; samples(physical)];
    received.extend(echo::keyed(TONE, sent));
    recording(&files, &received);
    let heard = files.heard_own_echo(TONE.hz).unwrap().unwrap();
    assert!(
        heard.delay_percentile(50).unwrap().abs_diff(physical) < Duration::from_millis(30),
        "constructor time must not become physical echo delay: {heard:?}"
    );
}

#[test]
fn a_same_frequency_foreign_tone_keeps_the_earlier_sources_negative_raw_offset() {
    let dir = tempfile::tempdir().unwrap();
    let sent = Duration::from_secs(12);
    let files = CallFiles::write_for(dir.path().join("foreign-same-hz.wav"), TONE.hz, Keying::Keyed, sent).unwrap();
    // These are the local device origins, unrelated to when the foreign headless peer started its same-Hz tone.
    let mut source = files.devices().microphone().unwrap();
    source.next_frame(Duration::ZERO).unwrap().unwrap();
    drop(source);
    let mut sink = files.devices().speaker().unwrap();
    sink.play(&AudioFrame::silence()).unwrap();
    drop(sink);
    let mut at_far_end = vec![0.0; samples(Duration::from_millis(200))];
    at_far_end.extend(echo::keyed(TONE, sent));
    at_far_end.resize(samples(sent + Duration::from_secs(1)), 0.0);
    recording(&files, &at_far_end[samples(Duration::from_secs(3))..]);
    let generic = teams_headless::call::heard(&files.recording, TONE.hz, sent, sent)
        .unwrap()
        .unwrap();
    assert!((generic.signed_ms(generic.raw_percentile(50).unwrap()) + 2_800).abs() < 20);
    let foreign = files
        .heard(TONE.hz)
        .unwrap()
        .expect("same Hz does not establish own-echo provenance");
    assert_eq!(foreign, generic);
}

fn recording_error<T>(result: Result<T, teams_headless::commands::CommandError>, expected: &str) {
    match result {
        Err(teams_headless::commands::CommandError::Recording(reason)) => {
            assert!(reason.contains(expected), "recording failure lost its cause: {reason}");
        }
        Err(other) => panic!("expected a recording failure, got {other}"),
        Ok(_) => panic!("an unreadable WAV must not become successful evidence"),
    }
}

#[test]
fn missing_and_corrupt_recordings_fail_presence_generic_and_own_echo_analysis() {
    let dir = tempfile::tempdir().unwrap();
    let length = Duration::from_millis(200);
    let files = CallFiles::write_for(dir.path().join("unreadable.wav"), TONE.hz, Keying::Keyed, length).unwrap();
    for corrupt in [false, true] {
        let expected = if corrupt {
            std::fs::write(&files.recording, b"synthetic invalid WAV bytes").unwrap();
            "not a RIFF WAVE file".to_owned()
        } else {
            let missing = std::fs::read(&files.recording).unwrap_err();
            assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
            missing.to_string()
        };
        recording_error(files.presence(TONE.hz), &expected);
        recording_error(files.heard(TONE.hz), &expected);
        recording_error(files.heard_own_echo(TONE.hz), &expected);
        recording_error(
            teams_headless::call::heard(&files.recording, TONE.hz, length, length),
            &expected,
        );
    }
}

#[test]
fn an_obstructed_sent_file_fails_construction_without_touching_the_recording() {
    let dir = tempfile::tempdir().unwrap();
    let recording = dir.path().join("obstructed.wav");
    let sent = dir.path().join("obstructed-sent.wav");
    std::fs::create_dir(&sent).unwrap();
    let cause = std::fs::File::create(&sent).unwrap_err().to_string();
    recording_error(
        CallFiles::write_for(recording.clone(), TONE.hz, Keying::Keyed, Duration::from_millis(200)),
        &cause,
    );
    assert!(sent.is_dir());
    assert!(!recording.exists());
    recording_error(
        CallFiles::write_for(
            std::path::PathBuf::from("/"),
            TONE.hz,
            Keying::Keyed,
            Duration::from_millis(200),
        ),
        "names no file",
    );
}
