//! The inline video player on a made-up MP4 (`tests/fixtures/media/made-up-test-pattern.mp4`: 2 s of `FFmpeg`'s test
//! pattern, 480×270 H.264 High with B-frames, and a 1 kHz tone in stereo AAC-LC at 44.1 kHz, its `moov` after its
//! media): streamed from a source that answers late, played by the system's `GStreamer` (its H.264 and AAC decoders
//! installed, as the app needs them), the sound played on a speaker that keeps what it is given.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use teams_media::devices::{AudioFrame, AudioSink, AudioSource, DeviceError, Devices, VideoFrame, VideoSource};
use teams_media::player::{
    Abandon, ByteSource, ByteStream, Command, Pieces, Player, PlayerOutput, PlayerSetup, SourceError, State, Status,
};
use teams_media::tone::amplitude_at;

const MOVIE: &[u8] = include_bytes!("fixtures/media/made-up-test-pattern.mp4");

/// The file's bytes, each stream answered after a pause, as a network would; counts the streams.
struct SlowFile {
    reads: Mutex<usize>,
}

impl ByteSource for SlowFile {
    fn open(&self, offset: u64, len: u64, _: &Abandon) -> Result<Box<dyn ByteStream>, SourceError> {
        *self.reads.lock().unwrap() += 1;
        thread::sleep(Duration::from_millis(20));
        Ok(Box::new(Pieces::new(
            slice(MOVIE, offset, len),
            MOVIE.len() as u64,
            4096,
        )))
    }
}

/// `len` bytes of `bytes` from `offset`, fewer where they end.
fn slice(bytes: &[u8], offset: u64, len: u64) -> &[u8] {
    let start = usize::try_from(offset).unwrap().min(bytes.len());
    let end = start.saturating_add(usize::try_from(len).unwrap()).min(bytes.len());
    &bytes[start..end]
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
        let until = Instant::now() + within;
        loop {
            if let Some(status) = self.last()
                && status.state == state
            {
                return status;
            }
            assert!(
                Instant::now() < until,
                "no {state:?} within {within:?}: {:?}",
                self.last()
            );
            thread::sleep(Duration::from_millis(10));
        }
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

fn open(play: bool) -> (Player, Arc<Seen>, Kept) {
    let seen = Arc::new(Seen::default());
    let speaker = Kept::default();
    let setup = PlayerSetup {
        speaker: Some(Arc::new(speaker.clone())),
    };
    let source = Arc::new(SlowFile { reads: Mutex::new(0) });
    let output: Arc<dyn PlayerOutput> = seen.clone();
    (Player::open(source, setup, output, play).unwrap(), seen, speaker)
}

#[test]
fn a_movie_plays_to_its_end_with_its_frames_and_its_sound() {
    let started = Instant::now();
    let (_player, seen, speaker) = open(true);
    let ended = seen.wait_for(State::Ended, Duration::from_secs(20));
    assert!(
        started.elapsed() >= Duration::from_millis(1_900),
        "{:?}",
        started.elapsed()
    );
    assert!((ended.duration.as_secs_f32() - 2.0).abs() < 0.1, "{:?}", ended.duration);
    assert!(ended.sound);
    let frames = seen.frames.lock().unwrap().clone();
    // 50 frames and the paused first one: a few may be dropped when their time passed before they were drawn
    assert!(frames.len() >= 40, "{} frames", frames.len());
    assert!(frames.iter().all(|shape| *shape == (480, 270)));
    let sound = speaker.0.lock().unwrap().clone();
    assert!(sound.len() >= 48_000 * 18 / 10, "{} samples", sound.len());
    // the tone, at its frequency, in the middle of the sound
    let middle = &sound[48_000 / 2..48_000];
    // FFmpeg's sine is at an eighth of full scale
    let tone = amplitude_at(middle, 1_000.0);
    assert!((0.06..0.2).contains(&tone), "{tone}");
    assert!(tone > 10.0 * amplitude_at(middle, 3_000.0));
}

#[test]
fn a_paused_seek_shows_the_frame_there() {
    let (player, seen, speaker) = open(false);
    let paused = seen.wait_for(State::Paused, Duration::from_secs(10));
    assert_eq!(paused.position, Duration::ZERO);
    // the first frame stands in before play
    let until = Instant::now() + Duration::from_secs(10);
    while seen.frames.lock().unwrap().is_empty() {
        assert!(Instant::now() < until, "no first frame");
        thread::sleep(Duration::from_millis(10));
    }
    let before = seen.frames.lock().unwrap().len();
    let destination = Duration::from_millis(1_200);
    player.send(Command::Seek(destination));
    // GStreamer's streaming thread may deliver the sought frame before the command loop publishes its status.
    // Both must arrive; neither callback establishes that the other has run.
    let status = seen.wait_for_any(Duration::from_secs(10), |status| {
        status.state == State::Paused && status.position == destination && seen.frames.lock().unwrap().len() > before
    });
    assert_eq!(
        (status.state, status.position),
        (State::Paused, Duration::from_millis(1_200))
    );
    // nothing was played while paused
    assert!(speaker.0.lock().unwrap().is_empty());
    player.send(Command::Volume(0.5));
}

#[test]
fn a_file_that_isnt_an_mp4_is_unsupported() {
    struct NotMp4;
    impl ByteSource for NotMp4 {
        fn open(&self, offset: u64, len: u64, _: &Abandon) -> Result<Box<dyn ByteStream>, SourceError> {
            let junk = b"\x00\x00\x00\x10junkjunkjunkjunk";
            Ok(Box::new(Pieces::new(slice(junk, offset, len), 16, 4096)))
        }
    }
    let seen = Arc::new(Seen::default());
    let output: Arc<dyn PlayerOutput> = seen.clone();
    let _player = Player::open(Arc::new(NotMp4), PlayerSetup::default(), output, true).unwrap();
    seen.wait_for(
        State::Failed(teams_media::player::Failure::Unsupported),
        Duration::from_secs(10),
    );
}

/// A file whose bytes are an HLS playlist (a "video" by its name only) would have `GStreamer` plug its HLS demuxer,
/// which fetches the playlist's links itself: the player plugs none, fails it as unsupported, and nothing is fetched.
#[test]
fn a_playlist_s_links_are_never_fetched() {
    struct Playlist(Vec<u8>);
    impl ByteSource for Playlist {
        fn open(&self, offset: u64, len: u64, _: &Abandon) -> Result<Box<dyn ByteStream>, SourceError> {
            Ok(Box::new(Pieces::new(
                slice(&self.0, offset, len),
                self.0.len() as u64,
                4096,
            )))
        }
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.0,\nhttp://127.0.0.1:{port}/segment-0.ts\n\
         #EXTINF:2.0,\nhttp://127.0.0.1:{port}/segment-1.ts\n#EXT-X-ENDLIST\n"
    );
    let seen = Arc::new(Seen::default());
    let output: Arc<dyn PlayerOutput> = seen.clone();
    let _player = Player::open(
        Arc::new(Playlist(playlist.into_bytes())),
        PlayerSetup::default(),
        output,
        true,
    )
    .unwrap();
    let failed = seen.wait_for_any(Duration::from_secs(10), |status| {
        matches!(status.state, State::Failed(_))
    });
    assert_eq!(failed.state, State::Failed(teams_media::player::Failure::Unsupported));
    thread::sleep(Duration::from_millis(500));
    assert!(listener.accept().is_err(), "a link of the playlist was fetched");
}

/// A player dropped while its first read waits on the network stops: its cache is closed, the read gives up, and its
/// thread and the pipeline let go of the output.
#[test]
fn a_player_dropped_while_its_read_waits_stops() {
    struct Never(Arc<(Mutex<bool>, std::sync::Condvar)>);
    impl ByteSource for Never {
        fn open(&self, _: u64, _: u64, _: &Abandon) -> Result<Box<dyn ByteStream>, SourceError> {
            let (released, changed) = &*self.0;
            let mut released = released.lock().unwrap();
            while !*released {
                released = changed.wait(released).unwrap();
            }
            Err(SourceError::Failed)
        }
    }
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let seen = Arc::new(Seen::default());
    let output: Arc<dyn PlayerOutput> = seen.clone();
    let player = Player::open(Arc::new(Never(Arc::clone(&gate))), PlayerSetup::default(), output, true).unwrap();
    seen.wait_for(State::Loading, Duration::from_secs(5));
    thread::sleep(Duration::from_millis(200));
    drop(player);
    let until = Instant::now() + Duration::from_secs(5);
    while Arc::strong_count(&seen) > 1 {
        assert!(Instant::now() < until, "the player's thread still holds its output");
        thread::sleep(Duration::from_millis(20));
    }
    // the read that never answered is let go too
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}
