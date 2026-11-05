//! The player on a presentation streamed in parts, as a meeting recording's is (`tests/fixtures/media/dash/`: 3 s of
//! `FFmpeg`'s test pattern, 320×180 H.264 High with B-frames in 1 s segments, and a 1 kHz tone in mono AAC-LC at
//! 16 kHz, made up with `FFmpeg`'s DASH muxer): its parts read from a source that answers late, played by the system's
//! `GStreamer` (its H.264 and AAC decoders installed, as the app needs them), the sound played on a speaker that keeps
//! what it is given.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use teams_media::devices::{AudioFrame, AudioSink, AudioSource, DeviceError, Devices, VideoFrame, VideoSource};
use teams_media::player::{
    Abandon, Command, Failure, Part, Player, PlayerOutput, PlayerSetup, Presentation, SegmentSource, SourceError, Span,
    State, Status, Track, TrackKind,
};
use teams_media::tone::amplitude_at;

const VIDEO_INIT: &[u8] = include_bytes!("fixtures/media/dash/init-0.m4s");
const VIDEO: [&[u8]; 3] = [
    include_bytes!("fixtures/media/dash/chunk-0-00001.m4s"),
    include_bytes!("fixtures/media/dash/chunk-0-00002.m4s"),
    include_bytes!("fixtures/media/dash/chunk-0-00003.m4s"),
];
const AUDIO_INIT: &[u8] = include_bytes!("fixtures/media/dash/init-1.m4s");
const AUDIO: [&[u8]; 4] = [
    include_bytes!("fixtures/media/dash/chunk-1-00001.m4s"),
    include_bytes!("fixtures/media/dash/chunk-1-00002.m4s"),
    include_bytes!("fixtures/media/dash/chunk-1-00003.m4s"),
    include_bytes!("fixtures/media/dash/chunk-1-00004.m4s"),
];

/// The presentation as the MPD `FFmpeg` wrote says it: its audio first, as the transform service's lists it.
fn presentation() -> Presentation {
    let audio = [(0, 14_336), (14_336, 16_384), (30_720, 16_384), (47_104, 896)];
    Presentation {
        duration: Duration::from_secs(3),
        tracks: vec![
            Track {
                kind: TrackKind::Audio,
                timescale: 16_000,
                segments: audio
                    .iter()
                    .map(|&(start, duration)| Span { start, duration })
                    .collect(),
            },
            Track {
                kind: TrackKind::Video,
                timescale: 16_384,
                segments: (0..3)
                    .map(|index| Span {
                        start: index * 16_384,
                        duration: 16_384,
                    })
                    .collect(),
            },
        ],
    }
}

/// The parts, each answered after a pause, as a network would; counts the reads of each part, and fails the first read
/// of the parts in `flaky` on the way.
#[derive(Default)]
struct SlowParts {
    reads: Mutex<Vec<(usize, Part)>>,
    flaky: Vec<(usize, Part)>,
    presentation_reads: AtomicUsize,
}

impl SegmentSource for SlowParts {
    fn presentation(&self, _: &Abandon) -> Result<Presentation, SourceError> {
        self.presentation_reads.fetch_add(1, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(20));
        Ok(presentation())
    }

    fn part(&self, track: usize, part: Part, _: &Abandon) -> Result<Vec<u8>, SourceError> {
        thread::sleep(Duration::from_millis(20));
        let mut reads = self.reads.lock().unwrap();
        let first = !reads.contains(&(track, part));
        reads.push((track, part));
        drop(reads);
        if first && self.flaky.contains(&(track, part)) {
            return Err(SourceError::Failed);
        }
        let bytes = match (track, part) {
            (0, Part::Init) => AUDIO_INIT,
            (0, Part::Media(index)) => AUDIO[index],
            (1, Part::Init) => VIDEO_INIT,
            (1, Part::Media(index)) => VIDEO[index],
            _ => return Err(SourceError::Unavailable),
        };
        Ok(bytes.to_vec())
    }
}

/// What the player showed and told.
#[derive(Default)]
struct Seen {
    frames: Mutex<Vec<(u32, u32)>>,
    statuses: Mutex<Vec<Status>>,
}

impl PlayerOutput for Seen {
    fn frame(&self, frame: VideoFrame) {
        self.frames.lock().unwrap().push((frame.width, frame.height));
    }

    fn status(&self, status: Status) {
        self.statuses.lock().unwrap().push(status);
    }
}

impl Seen {
    fn last(&self) -> Option<Status> {
        self.statuses.lock().unwrap().last().copied()
    }

    fn wait_for_any(&self, within: Duration, wanted: impl Fn(Status) -> bool) -> Status {
        let until = Instant::now() + within;
        loop {
            if let Some(status) = self.last()
                && wanted(status)
            {
                return status;
            }
            assert!(
                Instant::now() < until,
                "nothing wanted within {within:?}: {:?}",
                self.last()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for(&self, state: State, within: Duration) -> Status {
        self.wait_for_any(within, |status| status.state == state)
    }
}

/// A speaker that keeps every sample it is given; no microphone or camera.
#[derive(Default, Clone)]
struct Kept(Arc<Mutex<Vec<f32>>>);

impl AudioSink for Kept {
    fn play(&mut self, frame: &AudioFrame) -> Result<(), DeviceError> {
        self.0.lock().unwrap().extend_from_slice(frame.samples());
        Ok(())
    }
}

impl Devices for Kept {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        Err(DeviceError::Closed)
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        Ok(Box::new(self.clone()))
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        Err(DeviceError::Closed)
    }
}

fn open_with(source: Arc<dyn SegmentSource>, play: bool) -> (Player, Arc<Seen>, Kept) {
    let seen = Arc::new(Seen::default());
    let speaker = Kept::default();
    let setup = PlayerSetup {
        speaker: Some(Arc::new(speaker.clone())),
    };
    let output: Arc<dyn PlayerOutput> = seen.clone();
    (Player::open_stream(source, setup, output, play).unwrap(), seen, speaker)
}

#[test]
fn a_presentation_plays_to_its_end_with_its_frames_and_its_sound() {
    let started = Instant::now();
    let source = Arc::new(SlowParts::default());
    let (_player, seen, speaker) = open_with(source.clone(), true);
    let ended = seen.wait_for(State::Ended, Duration::from_secs(30));
    assert!(
        started.elapsed() >= Duration::from_millis(2_900),
        "{:?}",
        started.elapsed()
    );
    assert!((ended.duration.as_secs_f32() - 3.0).abs() < 0.1, "{:?}", ended.duration);
    assert!(ended.sound);
    let frames = seen.frames.lock().unwrap().clone();
    // 48 frames and the paused first one: a few may be dropped when their time passed before they were drawn
    assert!(frames.len() >= 38, "{} frames", frames.len());
    assert!(frames.iter().all(|shape| *shape == (320, 180)));
    let sound = speaker.0.lock().unwrap().clone();
    assert!(sound.len() >= 48_000 * 27 / 10, "{} samples", sound.len());
    let middle = &sound[48_000..48_000 * 3 / 2];
    let tone = amplitude_at(middle, 1_000.0);
    assert!((0.06..0.2).contains(&tone), "{tone}");
    assert!(tone > 10.0 * amplitude_at(middle, 3_000.0));
    // each part read once, the presentation once
    let reads = source.reads.lock().unwrap().clone();
    assert_eq!(reads.len(), 2 + VIDEO.len() + AUDIO.len(), "{reads:?}");
    assert_eq!(source.presentation_reads.load(Ordering::SeqCst), 1);
}

/// Twice as fast, asked before it loaded: it plays its 3 s in about half the time, its sound handed to the speaker at
/// the speaker's own pace (about 1.5 s of it) with the tone's pitch kept; a seek keeps the speed, and back at 1× it
/// plays at its own pace again.
#[test]
fn a_presentation_plays_faster_its_sound_s_pitch_kept() {
    let source = Arc::new(SlowParts::default());
    let (player, seen, speaker) = open_with(source, false);
    player.send(Command::Rate(2.0));
    seen.wait_for(State::Paused, Duration::from_secs(10));
    let started = Instant::now();
    player.send(Command::Play);
    seen.wait_for(State::Ended, Duration::from_secs(20));
    let took = started.elapsed();
    assert!(took < Duration::from_millis(2_500), "{took:?}");
    assert!(took >= Duration::from_millis(1_300), "{took:?}");
    let sound = speaker.0.lock().unwrap().clone();
    assert!(
        (48_000 * 12 / 10..48_000 * 2).contains(&sound.len()),
        "{} samples",
        sound.len()
    );
    let middle = &sound[48_000 / 4..48_000 * 3 / 4];
    let tone = amplitude_at(middle, 1_000.0);
    assert!(tone > 0.03, "{tone}");
    assert!(tone > 5.0 * amplitude_at(middle, 2_000.0));
    // a seek keeps the speed: the last 2 s play in about one
    player.send(Command::Seek(Duration::from_secs(1)));
    seen.wait_for(State::Paused, Duration::from_secs(10));
    let started = Instant::now();
    player.send(Command::Play);
    seen.wait_for(State::Playing, Duration::from_secs(10));
    seen.wait_for(State::Ended, Duration::from_secs(20));
    assert!(
        started.elapsed() < Duration::from_millis(1_800),
        "{:?}",
        started.elapsed()
    );
    // back at 1× (a rate past the slowest is kept to it, one that isn't a number is 1×)
    player.send(Command::Rate(0.1));
    seen.wait_for_any(Duration::from_secs(10), |status| {
        (status.rate - 0.8).abs() < f64::EPSILON
    });
    player.send(Command::Rate(f64::NAN));
    seen.wait_for_any(Duration::from_secs(10), |status| {
        (status.rate - 1.0).abs() < f64::EPSILON
    });
    player.send(Command::Seek(Duration::from_secs(1)));
    seen.wait_for(State::Paused, Duration::from_secs(10));
    let started = Instant::now();
    player.send(Command::Play);
    seen.wait_for(State::Playing, Duration::from_secs(10));
    seen.wait_for(State::Ended, Duration::from_secs(20));
    assert!(
        started.elapsed() >= Duration::from_millis(1_800),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_paused_seek_shows_the_frame_there_and_reads_from_its_segment() {
    let source = Arc::new(SlowParts::default());
    let (player, seen, speaker) = open_with(source.clone(), false);
    let paused = seen.wait_for(State::Paused, Duration::from_secs(10));
    assert_eq!(paused.position, Duration::ZERO);
    assert!(
        (paused.duration.as_secs_f32() - 3.0).abs() < 0.1,
        "{:?}",
        paused.duration
    );
    let until = Instant::now() + Duration::from_secs(10);
    while seen.frames.lock().unwrap().is_empty() {
        assert!(Instant::now() < until, "no first frame");
        thread::sleep(Duration::from_millis(10));
    }
    let before = seen.frames.lock().unwrap().len();
    let destination = Duration::from_millis(2_300);
    player.send(Command::Seek(destination));
    let status = seen.wait_for_any(Duration::from_secs(10), |status| {
        status.state == State::Paused && status.position == destination && seen.frames.lock().unwrap().len() > before
    });
    assert_eq!(status.position, destination);
    assert!(speaker.0.lock().unwrap().is_empty());
    // the video's last segment (2 s on) was read for it
    let until = Instant::now() + Duration::from_secs(5);
    while !source.reads.lock().unwrap().contains(&(1, Part::Media(2))) {
        assert!(Instant::now() < until, "{:?}", source.reads.lock().unwrap());
        thread::sleep(Duration::from_millis(10));
    }
    // and played from there to its end
    player.send(Command::Play);
    let ended = seen.wait_for(State::Ended, Duration::from_secs(10));
    assert_eq!(ended.position, ended.duration);
    // played again from its start
    player.send(Command::Play);
    seen.wait_for_any(Duration::from_secs(10), |status| {
        status.state == State::Playing && status.position < Duration::from_millis(1_500)
    });
    player.send(Command::Muted(true));
    player.send(Command::Volume(0.4));
    player.send(Command::Volume(f64::NAN));
    player.send(Command::Pause);
    seen.wait_for(State::Paused, Duration::from_secs(10));
}

/// Its volume and its mute reach the sound the speaker gets: a quarter as loud once turned down, silent once muted.
#[test]
fn a_presentation_s_volume_and_mute_reach_its_sound() {
    let source = Arc::new(SlowParts::default());
    let (player, seen, speaker) = open_with(source, true);
    seen.wait_for_any(Duration::from_secs(10), |status| {
        status.position >= Duration::from_millis(900)
    });
    player.send(Command::Volume(0.25));
    seen.wait_for_any(Duration::from_secs(10), |status| {
        status.position >= Duration::from_millis(1_900)
    });
    player.send(Command::Muted(true));
    seen.wait_for(State::Ended, Duration::from_secs(20));
    let sound = speaker.0.lock().unwrap().clone();
    assert!(sound.len() >= 48_000 * 27 / 10, "{} samples", sound.len());
    // the tone's amplitude between two times, in ms (48 samples each)
    let at = |from: usize, to: usize| amplitude_at(&sound[48 * from..48 * to], 1_000.0);
    let (loud, quieter, muted) = (at(250, 750), at(1_400, 1_800), at(2_400, 2_700));
    assert!(loud > 0.03, "{loud}");
    assert!((0.15..0.4).contains(&(quieter / loud)), "{quieter} of {loud}");
    assert!(muted < loud / 20.0, "{muted} of {loud}");
}

#[test]
fn a_part_that_failed_on_the_way_is_read_again() {
    let source = Arc::new(SlowParts {
        flaky: vec![(1, Part::Init), (0, Part::Media(1)), (1, Part::Media(2))],
        ..SlowParts::default()
    });
    let (_player, seen, _) = open_with(source.clone(), true);
    seen.wait_for(State::Ended, Duration::from_secs(30));
    let reads = source.reads.lock().unwrap().clone();
    assert_eq!(reads.iter().filter(|read| **read == (1, Part::Media(2))).count(), 2);
}

#[test]
fn a_presentation_that_can_t_be_had_is_unavailable() {
    struct Refused;
    impl SegmentSource for Refused {
        fn presentation(&self, _: &Abandon) -> Result<Presentation, SourceError> {
            Err(SourceError::Unavailable)
        }

        fn part(&self, _: usize, _: Part, _: &Abandon) -> Result<Vec<u8>, SourceError> {
            unreachable!("no part is read without a presentation")
        }
    }
    let (_player, seen, _) = open_with(Arc::new(Refused), true);
    seen.wait_for(State::Failed(Failure::Unavailable), Duration::from_secs(10));
}

#[test]
fn a_part_refused_while_it_plays_fails_it_as_unavailable() {
    struct Expired(SlowParts);
    impl SegmentSource for Expired {
        fn presentation(&self, abandon: &Abandon) -> Result<Presentation, SourceError> {
            self.0.presentation(abandon)
        }

        fn part(&self, track: usize, part: Part, abandon: &Abandon) -> Result<Vec<u8>, SourceError> {
            if part == Part::Media(1) {
                return Err(SourceError::Unavailable);
            }
            self.0.part(track, part, abandon)
        }
    }
    let (_player, seen, _) = open_with(Arc::new(Expired(SlowParts::default())), true);
    seen.wait_for(State::Failed(Failure::Unavailable), Duration::from_secs(10));
}

#[test]
fn parts_that_aren_t_a_video_are_unsupported() {
    struct Junk(bool);
    impl SegmentSource for Junk {
        fn presentation(&self, _: &Abandon) -> Result<Presentation, SourceError> {
            Ok(presentation())
        }

        fn part(&self, track: usize, part: Part, abandon: &Abandon) -> Result<Vec<u8>, SourceError> {
            // the initialization segments, or a media segment, not what they say they are
            match (self.0, part) {
                (true, Part::Init) => Ok(b"\x00\x00\x00\x10junkjunkjunkjunk".to_vec()),
                (false, Part::Media(_)) => Ok(b"\x00\x00\x00\x10moofjunkjunkjunk".to_vec()),
                _ => SlowParts::default().part(track, part, abandon),
            }
        }
    }
    for junk_init in [true, false] {
        let (_player, seen, _) = open_with(Arc::new(Junk(junk_init)), true);
        seen.wait_for(State::Failed(Failure::Unsupported), Duration::from_secs(10));
    }
}

#[test]
fn a_track_of_another_kind_than_it_says_or_none_at_all_is_unsupported() {
    // a presentation whose video is said to be audio
    struct Swapped;
    impl SegmentSource for Swapped {
        fn presentation(&self, _: &Abandon) -> Result<Presentation, SourceError> {
            let mut presentation = presentation();
            presentation.tracks[1].kind = TrackKind::Audio;
            presentation.tracks.swap(0, 1);
            Ok(presentation)
        }

        fn part(&self, track: usize, part: Part, abandon: &Abandon) -> Result<Vec<u8>, SourceError> {
            SlowParts::default().part(1 - track, part, abandon)
        }
    }
    // one with no segment at all
    struct Empty;
    impl SegmentSource for Empty {
        fn presentation(&self, _: &Abandon) -> Result<Presentation, SourceError> {
            let mut presentation = presentation();
            presentation.tracks.iter_mut().for_each(|track| track.segments.clear());
            Ok(presentation)
        }

        fn part(&self, _: usize, _: Part, _: &Abandon) -> Result<Vec<u8>, SourceError> {
            unreachable!("no part of an empty presentation is read")
        }
    }
    let (_player, seen, _) = open_with(Arc::new(Swapped), true);
    seen.wait_for(State::Failed(Failure::Unsupported), Duration::from_secs(10));
    let (_player, seen, _) = open_with(Arc::new(Empty), true);
    seen.wait_for(State::Failed(Failure::Unsupported), Duration::from_secs(10));
}

#[test]
fn the_audio_alone_plays() {
    struct AudioOnly;
    impl SegmentSource for AudioOnly {
        fn presentation(&self, _: &Abandon) -> Result<Presentation, SourceError> {
            let mut presentation = presentation();
            presentation.tracks.truncate(1);
            Ok(presentation)
        }

        fn part(&self, track: usize, part: Part, abandon: &Abandon) -> Result<Vec<u8>, SourceError> {
            SlowParts::default().part(track, part, abandon)
        }
    }
    let (_player, seen, speaker) = open_with(Arc::new(AudioOnly), true);
    seen.wait_for(State::Ended, Duration::from_secs(30));
    assert!(seen.frames.lock().unwrap().is_empty());
    assert!(!speaker.0.lock().unwrap().is_empty());
}

/// A player dropped while a read waits on the network stops: the read is let go, and its threads and the pipeline let
/// go of the output.
#[test]
fn a_player_dropped_while_its_read_waits_stops() {
    struct Never {
        gate: Arc<(Mutex<bool>, Condvar)>,
        media: bool,
    }
    impl SegmentSource for Never {
        fn presentation(&self, abandon: &Abandon) -> Result<Presentation, SourceError> {
            if self.media {
                return Ok(presentation());
            }
            self.wait(abandon)
        }

        fn part(&self, track: usize, part: Part, abandon: &Abandon) -> Result<Vec<u8>, SourceError> {
            if part == Part::Init {
                return SlowParts::default().part(track, part, abandon);
            }
            self.wait(abandon)
        }
    }
    impl Never {
        fn wait<T>(&self, abandon: &Abandon) -> Result<T, SourceError> {
            let (released, changed) = &*self.gate;
            let mut released = released.lock().unwrap();
            while !*released && !abandon.is_set() {
                released = changed.wait_timeout(released, Duration::from_millis(20)).unwrap().0;
            }
            Err(SourceError::Stopped)
        }
    }
    for media in [false, true] {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (player, seen, _) = open_with(
            Arc::new(Never {
                gate: Arc::clone(&gate),
                media,
            }),
            true,
        );
        seen.wait_for(State::Loading, Duration::from_secs(5));
        thread::sleep(Duration::from_millis(300));
        drop(player);
        let until = Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&seen) > 1 {
            assert!(Instant::now() < until, "the player's threads still hold its output");
            thread::sleep(Duration::from_millis(20));
        }
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
    }
}

/// A longer presentation (`tests/fixtures/media/dash-long/`: 36 s of a 160×90 test pattern at 4 fps in 6 s segments,
/// and a tone in mono AAC at 8 kHz): longer than what an `appsrc` holds ahead, so its reads wait for room.
struct LongParts {
    reads: Mutex<Vec<(usize, Part)>>,
}

const LONG_VIDEO: [&[u8]; 7] = [
    include_bytes!("fixtures/media/dash-long/init-0.m4s"),
    include_bytes!("fixtures/media/dash-long/chunk-0-00001.m4s"),
    include_bytes!("fixtures/media/dash-long/chunk-0-00002.m4s"),
    include_bytes!("fixtures/media/dash-long/chunk-0-00003.m4s"),
    include_bytes!("fixtures/media/dash-long/chunk-0-00004.m4s"),
    include_bytes!("fixtures/media/dash-long/chunk-0-00005.m4s"),
    include_bytes!("fixtures/media/dash-long/chunk-0-00006.m4s"),
];
const LONG_AUDIO: [&[u8]; 8] = [
    include_bytes!("fixtures/media/dash-long/init-1.m4s"),
    include_bytes!("fixtures/media/dash-long/chunk-1-00001.m4s"),
    include_bytes!("fixtures/media/dash-long/chunk-1-00002.m4s"),
    include_bytes!("fixtures/media/dash-long/chunk-1-00003.m4s"),
    include_bytes!("fixtures/media/dash-long/chunk-1-00004.m4s"),
    include_bytes!("fixtures/media/dash-long/chunk-1-00005.m4s"),
    include_bytes!("fixtures/media/dash-long/chunk-1-00006.m4s"),
    include_bytes!("fixtures/media/dash-long/chunk-1-00007.m4s"),
];

impl SegmentSource for LongParts {
    fn presentation(&self, _: &Abandon) -> Result<Presentation, SourceError> {
        let audio = [
            (0, 47_104),
            (47_104, 48_128),
            (95_232, 48_128),
            (143_360, 48_128),
            (191_488, 48_128),
            (239_616, 48_128),
            (287_744, 256),
        ];
        Ok(Presentation {
            duration: Duration::from_secs(36),
            tracks: vec![
                Track {
                    kind: TrackKind::Audio,
                    timescale: 8000,
                    segments: audio
                        .iter()
                        .map(|&(start, duration)| Span { start, duration })
                        .collect(),
                },
                Track {
                    kind: TrackKind::Video,
                    timescale: 16_384,
                    segments: (0..6)
                        .map(|index| Span {
                            start: index * 98_304,
                            duration: 98_304,
                        })
                        .collect(),
                },
            ],
        })
    }

    fn part(&self, track: usize, part: Part, _: &Abandon) -> Result<Vec<u8>, SourceError> {
        thread::sleep(Duration::from_millis(10));
        self.reads.lock().unwrap().push((track, part));
        let parts: &[&[u8]] = if track == 0 { &LONG_AUDIO } else { &LONG_VIDEO };
        let index = match part {
            Part::Init => 0,
            Part::Media(index) => index + 1,
        };
        parts
            .get(index)
            .map(|bytes| bytes.to_vec())
            .ok_or(SourceError::Unavailable)
    }
}

/// While its reads wait for room (what plays is far behind what was read), a seek still goes where it is asked at
/// once, reads from there and plays on, and one back reads the start again.
#[test]
fn a_seek_while_its_reads_wait_for_room_plays_from_where_it_went() {
    let source = Arc::new(LongParts {
        reads: Mutex::new(Vec::new()),
    });
    let (player, seen, _) = open_with(source.clone(), true);
    seen.wait_for(State::Playing, Duration::from_secs(10));
    // the reads ran ahead to the end and wait for room while the start plays
    let until = Instant::now() + Duration::from_secs(10);
    while !source.reads.lock().unwrap().contains(&(1, Part::Media(5))) {
        assert!(Instant::now() < until, "{:?}", source.reads.lock().unwrap());
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(200));
    assert!(seen.last().unwrap().position < Duration::from_secs(5));
    let destination = Duration::from_secs(31);
    player.send(Command::SeekNear(destination));
    seen.wait_for_any(Duration::from_secs(10), |status| {
        status.state == State::Playing && status.position > destination
    });
    // the segment there was read again for it
    let reads = source.reads.lock().unwrap().clone();
    assert_eq!(
        reads.iter().filter(|read| **read == (1, Part::Media(5))).count(),
        2,
        "{reads:?}"
    );
    seen.wait_for(State::Ended, Duration::from_secs(15));
    let before = source.reads.lock().unwrap().len();
    player.send(Command::Seek(Duration::from_secs(1)));
    player.send(Command::Play);
    seen.wait_for_any(Duration::from_secs(10), |status| {
        status.state == State::Playing
            && status.position > Duration::from_secs(1)
            && status.position < Duration::from_secs(5)
    });
    let reads = source.reads.lock().unwrap().clone();
    assert!(reads[before..].contains(&(1, Part::Media(0))), "{reads:?}");
}
