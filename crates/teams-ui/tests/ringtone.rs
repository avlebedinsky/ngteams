//! The ringtone on a device (M3 Task 17): the [`DeviceRinger`] plays it on the device seam's speaker, here the file
//! backend's recording (no audio server), in real time, until its [`Ringtone`] is dropped; a speaker that can't be
//! opened plays nothing and fails nothing.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::sync::Arc;
use std::time::Duration;

use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::{SAMPLE_RATE, wav};
use teams_media::tone::amplitude_at;
use teams_ui::ringtone::{DeviceRinger, RINGTONE, Ringer};

/// How long the ringtone is let play.
const PLAYED: Duration = Duration::from_millis(1_200);

fn devices(recording: Option<std::path::PathBuf>) -> FileDevices {
    FileDevices {
        input: AudioInput::Silence,
        recording,
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    }
}

#[test]
fn the_ringtone_plays_on_the_speaker_in_real_time_until_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let recording = dir.path().join("speaker.wav");
    let ringer = DeviceRinger::new(Arc::new(devices(Some(recording.clone()))));
    let ringtone = ringer.start();
    std::thread::sleep(PLAYED);
    drop(ringtone);
    // the player stops within a frame or so, its speaker closed
    std::thread::sleep(Duration::from_millis(300));
    let samples = wav::read(&recording).unwrap();
    let seconds = f64::from(u32::try_from(samples.len()).unwrap()) / f64::from(SAMPLE_RATE);
    assert!(
        (0.9..1.8).contains(&seconds),
        "about what was played, ahead by at most its lead: {seconds:.2} s"
    );
    let first = &samples[..19_200];
    let second = &samples[19_200..38_400];
    assert!(amplitude_at(first, RINGTONE[0].hz) > 0.15);
    assert!(amplitude_at(second, RINGTONE[1].hz) > 0.15);
    let length = samples.len();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(wav::read(&recording).unwrap().len(), length, "nothing after it stopped");
}

#[test]
fn a_speaker_that_can_t_be_opened_plays_nothing() {
    let dir = tempfile::tempdir().unwrap();
    // a recording in a directory that doesn't exist can't be made
    let ringer = DeviceRinger::new(Arc::new(devices(Some(dir.path().join("gone/speaker.wav")))));
    let ringtone = ringer.start();
    std::thread::sleep(Duration::from_millis(100));
    drop(ringtone);
    assert!(!dir.path().join("gone").exists());
}
