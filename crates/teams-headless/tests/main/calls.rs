//! File-only call orchestration through the same dispatcher and actual core stack as the executable.

#![cfg(test)]

use super::*;
use teams_media::devices::{AudioFrame, Devices};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_watch_and_join_end_cleanly_when_stopped_or_when_there_is_no_call_to_join() {
    let lab = TestLab::new().await;
    for args in [
        vec!["watch", "--calls", "--tone-seconds", "1", "--video"],
        vec![
            "watch",
            "--answer-lab-calls",
            "--tone-seconds",
            "1",
            "--video",
            "--camera-after",
            "0",
        ],
    ] {
        let (outcome, output) = dispatch_args(&lab, &args, Some(Duration::from_millis(30))).await;
        assert_eq!(outcome.unwrap(), ExitCode::SUCCESS, "{output}");
        assert!(output.contains("video received="), "{output}");
    }
    let (outcome, output) =
        dispatch_args(&lab, &["join", "abc", "--ring-wait", "0", "--tone-seconds", "1"], None).await;
    assert_eq!(outcome.unwrap(), ExitCode::FAILURE);
    assert!(output.contains("no_running_call"), "{output}");
    for target in ["echo", "b"] {
        let (outcome, _) = dispatch_args(&lab, &["call", target, "--tone-seconds", "1"], Some(Duration::ZERO)).await;
        assert_eq!(outcome.unwrap(), ExitCode::from(INTERRUPTED));
    }
    let (outcome, output) = dispatch_args(&lab, &["call", "echo", "--wait", "0", "--tone-seconds", "1"], None).await;
    assert_eq!(outcome.unwrap(), ExitCode::FAILURE);
    assert!(output.contains("recording"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dispatched_echo_call_connects_hangs_up_and_reports_its_recording_and_video_counts() {
    let lab = TestLab::new().await;
    let (outcome, output) = dispatch_args(
        &lab,
        &[
            "call",
            "echo",
            "--seconds",
            "1",
            "--wait",
            "15",
            "--tone-seconds",
            "1",
            "--video",
            "--camera-after",
            "0",
        ],
        None,
    )
    .await;
    assert!(outcome.is_ok(), "{outcome:?}: {output}");
    assert!(
        output.contains("call_summary") && output.contains("held=true"),
        "{output}"
    );
    assert!(
        output.contains("recording") && output.contains("video received="),
        "{output}"
    );
    assert!(!lab.mock.calls().is_empty());
}

#[tokio::test]
async fn recording_diagnostics_handle_explicit_tones_and_a_movie_without_assuming_an_echo() {
    let lab = TestLab::new().await;
    let mut audio = audio(&["--hear", "440", "--hear", "660"]);
    let mut output = Vec::new();
    let files = call_files(&lab, &audio, ("diagnostic", Duration::from_millis(200)), &mut output).unwrap();
    let mut speaker = files.devices().speaker().unwrap();
    speaker.play(&AudioFrame::silence()).unwrap();
    drop(speaker);
    assert!(heard_lines(&files, &audio, false, &mut output).unwrap().is_none());
    let printed = String::from_utf8(output).unwrap();
    assert!(printed.contains("440") && printed.contains("660"), "{printed}");
    audio.hear.clear();
    assert!(heard_lines(&files, &audio, false, &mut Vec::new()).unwrap().is_none());

    let movie = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../teams-media/tests/fixtures/media/made-up-test-pattern.mp4"
    );
    let audio = super::audio(&["--media", movie]);
    let mut output = Vec::new();
    let files = call_files(&lab, &audio, ("movie", Duration::from_millis(200)), &mut output).unwrap();
    drop(files.devices().speaker().unwrap());
    assert!(heard_lines(&files, &audio, false, &mut output).unwrap().is_none());
    let printed = String::from_utf8(output).unwrap();
    assert!(printed.contains("media file=\"made-up-test-pattern.mp4\""), "{printed}");
    assert!(printed.contains("echo not_applicable media=true"), "{printed}");
}

#[tokio::test]
async fn recording_analysis_uses_echo_target_provenance_even_when_both_peers_play_the_same_frequency() {
    use teams_media::devices::wav;
    use teams_media::tone::Tone;
    let lab = TestLab::new().await;
    let audio = audio(&[]);
    let length = Duration::from_secs(12);
    let files = call_files(&lab, &audio, ("foreign-peer", length), &mut Vec::new()).unwrap();
    let mut microphone = files.devices().microphone().unwrap();
    microphone.next_frame(Duration::ZERO).unwrap().unwrap();
    drop(microphone);
    let mut speaker = files.devices().speaker().unwrap();
    speaker.play(&AudioFrame::silence()).unwrap();
    drop(speaker);
    let mut received = vec![0.0; 48_000 / 5];
    received.extend(echo::keyed(
        Tone {
            hz: audio.tone,
            amplitude: 0.5,
        },
        length,
    ));
    received.resize(48_000 * 13, 0.0);
    let mut recording = wav::WavWriter::create(&files.recording).unwrap();
    recording.write(&received[3 * 48_000..]).unwrap();
    recording.finish().unwrap();
    let mut output = Vec::new();
    let foreign = heard_lines(&files, &audio, false, &mut output).unwrap().unwrap();
    assert!((foreign.signed_ms(foreign.raw_percentile(50).unwrap()) + 2_800).abs() < 20);
    assert!(String::from_utf8(output).unwrap().contains("echo heard=true"));
    assert!(heard_lines(&files, &audio, true, &mut Vec::new()).unwrap().is_none());
}
