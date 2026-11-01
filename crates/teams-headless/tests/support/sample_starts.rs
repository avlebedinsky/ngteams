//! Stream origins are the first successful frame, with constructor diagnostics preserved separately.

use super::*;

fn devices(recording: &Path) -> TimedDevices {
    TimedDevices::new(FileDevices {
        input: AudioInput::Silence,
        recording: Some(recording.into()),
        camera: PatternCamera::default(),
        pacing: Pacing::Unpaced,
    })
}

#[test]
fn constructing_streams_sets_only_opened_times_and_the_first_frames_set_origins_once() {
    let dir = tempfile::tempdir().unwrap();
    let devices = devices(&dir.path().join("synthetic.wav"));
    let mut source = devices.microphone().unwrap();
    let mut sink = devices.speaker().unwrap();
    assert!(devices.opened().microphone.is_some() && devices.opened().speaker.is_some());
    assert_eq!(devices.sample_starts(), SampleStarts::default());
    source.next_frame(Duration::ZERO).unwrap().unwrap();
    sink.play(&AudioFrame::silence()).unwrap();
    let started = devices.sample_starts();
    assert!(started.microphone.is_some() && started.speaker.is_some());
    source.next_frame(Duration::ZERO).unwrap().unwrap();
    sink.play(&AudioFrame::silence()).unwrap();
    assert_eq!(devices.sample_starts(), started);
    drop(source);
    drop(sink);
    let mut source = devices.microphone().unwrap();
    assert!(devices.sample_starts().microphone.is_none());
    assert_eq!(devices.sample_starts().speaker, started.speaker);
    source.next_frame(Duration::ZERO).unwrap().unwrap();
    assert!(devices.sample_starts().microphone.unwrap().instant >= started.microphone.unwrap().instant);
    let mut sink = devices.speaker().unwrap();
    assert!(devices.sample_starts().speaker.is_none());
    sink.play(&AudioFrame::silence()).unwrap();
    assert!(devices.sample_starts().speaker.unwrap().instant >= started.speaker.unwrap().instant);
}

struct UnreadySource(usize);
impl AudioSource for UnreadySource {
    fn next_frame(&mut self, _: Duration) -> Result<Option<AudioFrame>, DeviceError> {
        self.0 += 1;
        match self.0 {
            1 => Err(DeviceError::Closed),
            2 => Ok(None),
            _ => Ok(Some(AudioFrame::silence())),
        }
    }
}
struct UnreadySink(bool);
impl AudioSink for UnreadySink {
    fn play(&mut self, _: &AudioFrame) -> Result<(), DeviceError> {
        if std::mem::take(&mut self.0) {
            Err(DeviceError::Closed)
        } else {
            Ok(())
        }
    }
}

#[test]
fn failed_or_empty_reads_and_failed_writes_do_not_supply_sample_origins() {
    let samples = Arc::new(Mutex::new(SampleStarts::default()));
    let mut source = TimedSource {
        source: Box::new(UnreadySource(0)),
        samples: Arc::clone(&samples),
        started: false,
    };
    assert!(source.next_frame(Duration::ZERO).is_err());
    assert!(source.next_frame(Duration::ZERO).unwrap().is_none());
    assert!(samples.lock().unwrap().microphone.is_none());
    source.next_frame(Duration::ZERO).unwrap().unwrap();
    let mut sink = TimedSink {
        sink: Box::new(UnreadySink(true)),
        samples: Arc::clone(&samples),
        started: false,
    };
    assert!(sink.play(&AudioFrame::silence()).is_err());
    assert!(samples.lock().unwrap().speaker.is_none());
    sink.play(&AudioFrame::silence()).unwrap();
    let started = *samples.lock().unwrap();
    source.next_frame(Duration::ZERO).unwrap().unwrap();
    sink.play(&AudioFrame::silence()).unwrap();
    assert_eq!(*samples.lock().unwrap(), started);
}

#[test]
fn a_failed_reopening_clears_the_previous_streams_origin() {
    let dir = tempfile::tempdir().unwrap();
    let mut devices = devices(&dir.path().join("synthetic.wav"));
    devices
        .microphone()
        .unwrap()
        .next_frame(Duration::ZERO)
        .unwrap()
        .unwrap();
    devices.speaker().unwrap().play(&AudioFrame::silence()).unwrap();
    devices.files.input = AudioInput::Wav(dir.path().join("missing.wav"));
    assert!(devices.microphone().is_err());
    assert!(devices.sample_starts().microphone.is_none());
    assert!(devices.sample_starts().speaker.is_some());
    devices.files.recording = Some(dir.path().join("missing/speaker.wav"));
    assert!(devices.speaker().is_err());
    assert!(devices.sample_starts().speaker.is_none());
}

#[test]
fn compatible_wall_clock_diagnostics_keep_construction_and_sample_starts_separate() {
    use crate::call::{devices_line, samples_line};
    assert_eq!(
        samples_line(SampleStarts::default()),
        "samples microphone_started_ms=- speaker_started_ms=-"
    );
    let instant = Instant::now();
    let source = SampleStart {
        instant,
        wall: SystemTime::UNIX_EPOCH + Duration::from_millis(200),
    };
    let speaker = SampleStart {
        instant: instant + Duration::from_millis(50),
        wall: SystemTime::UNIX_EPOCH + Duration::from_millis(250),
    };
    assert_eq!(
        samples_line(SampleStarts {
            microphone: Some(source),
            speaker: Some(speaker)
        }),
        "samples microphone_started_ms=200 speaker_started_ms=250"
    );
    assert_eq!(
        devices_line(Opened {
            microphone: Some(SystemTime::UNIX_EPOCH),
            speaker: None
        }),
        "devices microphone_opened_ms=0 speaker_opened_ms=-"
    );
}
