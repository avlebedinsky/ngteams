//! The speaker/microphone clock contract, independent of wall-clock device timing.

use super::*;
use crate::lab::{SampleStart, SampleStarts};
use std::time::{Instant, SystemTime};

fn starts(microphone: Option<Instant>, speaker: Option<Instant>) -> SampleStarts {
    let start = |instant| SampleStart {
        instant,
        wall: SystemTime::UNIX_EPOCH,
    };
    SampleStarts {
        microphone: microphone.map(start),
        speaker: speaker.map(start),
    }
}

#[test]
fn absent_device_clocks_supply_no_evidence_and_equal_clocks_borrow_the_original_samples() {
    let samples = [0.25, -0.5, 0.75];
    let at = Instant::now();
    for opened in [SampleStarts::default(), starts(Some(at), None), starts(None, Some(at))] {
        assert!(microphone_timeline(&samples, opened, Duration::from_secs(1)).is_none());
    }
    let same = microphone_timeline(&samples, starts(Some(at), Some(at)), Duration::from_secs(1)).unwrap();
    assert!(matches!(same, Cow::Borrowed(_)));
    assert_eq!(same.as_ref(), samples);
    assert!(std::ptr::eq(same.as_ptr(), samples.as_ptr()));
}

#[test]
fn an_earlier_speaker_drops_only_the_samples_before_the_microphone_opened() {
    let samples = [0.1, 0.2, 0.3, 0.4];
    let speaker = Instant::now();
    let microphone = speaker + Duration::from_nanos(41_667);
    let opened = starts(Some(microphone), Some(speaker));
    assert_eq!(
        microphone_timeline(&samples, opened, Duration::from_secs(1))
            .unwrap()
            .as_ref(),
        &samples[2..]
    );
    let too_early = starts(Some(speaker + Duration::from_secs(1)), Some(speaker));
    assert!(microphone_timeline(&samples, too_early, Duration::from_secs(1)).is_none());
}

#[test]
fn a_later_speaker_is_padded_only_within_the_causal_recording_window() {
    let samples = [0.1, 0.2];
    let microphone = Instant::now();
    let opened = starts(Some(microphone), Some(microphone + Duration::from_nanos(41_667)));
    assert_eq!(
        microphone_timeline(&samples, opened, Duration::from_secs(1))
            .unwrap()
            .as_ref(),
        [0.0, 0.0, 0.1, 0.2]
    );
    assert!(microphone_timeline(&samples, opened, Duration::ZERO).is_none());
    let overflow = Duration::from_secs(u64::MAX / 48_000 + 1);
    let speaker = microphone.checked_add(overflow).unwrap();
    assert!(microphone_timeline(&samples, starts(Some(microphone), Some(speaker)), Duration::MAX).is_none());
}
