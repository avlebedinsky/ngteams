//! A movie file as the file backend's microphone and camera: `ffmpeg` decodes it from an offset, its first audio
//! stream to 48 kHz mono 16-bit PCM and its first video stream to I420 frames at the camera's size and frame rate, each
//! on a pipe of its own read by a thread into a short queue; the source hands the frames out at the device's pace
//! ([`Pacing`]). Nothing is decoded ahead of what the pipe and the queue hold, so a long movie takes no disk and little
//! memory.
//!
//! Both devices play the same movie time: the first one opened sets the movie's clock at the offset asked, and one
//! opened later (the camera, turned on after the call connected) starts as far into the movie as the time since
//! then; a looped movie wraps at its end, one played once goes silent and keeps its last picture.
//!
//! Each decoder is an `ffmpeg` child of this process, killed and reaped when its device is dropped; its own messages
//! (at `error` level) are logged, a few at most.

use std::io::{BufRead as _, BufReader, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::file::{Clock, Pacing, PatternCamera};
use super::{
    AudioFrame, AudioSource, DeviceError, FRAME_DURATION, FRAME_SAMPLES, PixelFormat, VideoFrame, VideoSource,
};

/// The decoder, found on `PATH`.
const FFMPEG: &str = "ffmpeg";
/// The prober, found on `PATH`.
const FFPROBE: &str = "ffprobe";
/// Audio frames queued ahead of the device (the pipe holds more).
const AUDIO_QUEUE: usize = 20;
/// Video frames queued ahead of the device.
const VIDEO_QUEUE: usize = 3;
/// Lines of a decoder's own messages logged at most.
const MESSAGES_LOGGED: usize = 5;
/// The audio's gain, as `ffmpeg`'s filter: Big Buck Bunny's 5.1 track mixed down to mono peaks at about a quarter of
/// full scale.
const AUDIO_GAIN: &str = "volume=2";
/// Bytes of one audio frame: [`FRAME_SAMPLES`] 16-bit samples.
const AUDIO_FRAME_BYTES: usize = FRAME_SAMPLES * 2;

/// What `ffprobe` says of a movie file.
#[derive(Clone, Debug, PartialEq)]
pub struct MovieInfo {
    /// How long it plays, when the file says.
    pub duration: Option<Duration>,
    /// Its first video stream's size and frame rate, when it has one.
    pub video: Option<(u32, u32, f64)>,
    /// Whether it has an audio stream.
    pub audio: bool,
}

/// Probes `path` with `ffprobe`.
///
/// # Errors
/// [`DeviceError::Movie`] when `ffprobe` can't be run, fails on the file, or says something unreadable.
pub fn probe(path: &Path) -> Result<MovieInfo, DeviceError> {
    let output = Command::new(FFPROBE)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| DeviceError::Movie(format!("{FFPROBE} can't be run: {error}")))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        return Err(DeviceError::Movie(format!(
            "{FFPROBE} failed ({}): {}",
            output.status,
            message.trim()
        )));
    }
    let json: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| DeviceError::Movie(format!("{FFPROBE}'s answer isn't JSON: {error}")))?;
    Ok(read_probe(&json))
}

/// Reads `ffprobe -print_format json -show_format -show_streams`'s answer.
#[must_use]
pub fn read_probe(json: &Value) -> MovieInfo {
    let streams = json
        .get("streams")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let of_type = |kind: &str| {
        streams
            .iter()
            .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some(kind))
    };
    let video = of_type("video").and_then(|stream| {
        let size = |key: &str| {
            stream
                .get(key)
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
        };
        let rate = ["avg_frame_rate", "r_frame_rate"]
            .iter()
            .find_map(|key| stream.get(*key).and_then(Value::as_str).and_then(frame_rate))?;
        Some((size("width")?, size("height")?, rate))
    });
    let duration = json
        .pointer("/format/duration")
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(Duration::from_secs_f64);
    MovieInfo {
        duration,
        video,
        audio: of_type("audio").is_some(),
    }
}

/// A frame rate as `ffprobe` writes it (`24/1`, `30000/1001`); `None` for `0/0` and the like.
fn frame_rate(text: &str) -> Option<f64> {
    let (numerator, denominator) = text.split_once('/')?;
    let rate = numerator.parse::<f64>().ok()? / denominator.parse::<f64>().ok()?;
    (rate.is_finite() && rate > 0.0).then_some(rate)
}

/// A movie file as a microphone and a camera (see the module docs).
#[derive(Clone, Debug)]
pub struct Movie {
    path: PathBuf,
    from: Duration,
    looped: bool,
    info: MovieInfo,
    camera: PatternCamera,
    pacing: Pacing,
    /// When the movie was at `from`: set by the first device opened.
    epoch: Arc<Mutex<Option<Instant>>>,
}

impl Movie {
    /// The movie at `path`, played from `from` (once, or `looped`), its video at `camera`'s size and frame rate (an
    /// odd size rounded down to even); probed now.
    ///
    /// # Errors
    /// [`DeviceError::Movie`] when it can't be probed, has neither audio nor video, or `from` is past its end.
    pub fn open(
        path: PathBuf,
        from: Duration,
        looped: bool,
        camera: PatternCamera,
        pacing: Pacing,
    ) -> Result<Self, DeviceError> {
        let info = probe(&path)?;
        if !info.audio && info.video.is_none() {
            return Err(DeviceError::Movie("the file has neither audio nor video".to_owned()));
        }
        if let Some(duration) = info.duration
            && from >= duration
        {
            return Err(DeviceError::Movie(format!(
                "the offset {:.1} s is past the movie's end ({:.1} s)",
                from.as_secs_f64(),
                duration.as_secs_f64()
            )));
        }
        let camera = PatternCamera {
            width: (camera.width & !1).max(2),
            height: (camera.height & !1).max(2),
            fps: camera.fps.max(1),
        };
        Ok(Self {
            path,
            from,
            looped,
            info,
            camera,
            pacing,
            epoch: Arc::default(),
        })
    }

    #[must_use]
    pub fn info(&self) -> &MovieInfo {
        &self.info
    }

    /// The camera's size and frame rate.
    #[must_use]
    pub fn camera_format(&self) -> PatternCamera {
        self.camera
    }

    /// The movie's audio from where its clock is now, mixed down to mono and raised by [`AUDIO_GAIN`] (a film's
    /// surround track mixed down is quiet, and the call's audio processing is off); silence for a movie without audio.
    ///
    /// # Errors
    /// [`DeviceError::Movie`] when `ffmpeg` can't be started.
    pub fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        let decoder = if self.info.audio {
            let output = [
                "-map",
                "0:a:0",
                "-af",
                AUDIO_GAIN,
                "-ac",
                "1",
                "-ar",
                "48000",
                "-c:a",
                "pcm_s16le",
                "-f",
                "s16le",
            ];
            Some(self.decode(&output, AUDIO_FRAME_BYTES, AUDIO_QUEUE)?)
        } else {
            None
        };
        Ok(Box::new(MovieAudio {
            decoder,
            clock: Clock::new(self.pacing, FRAME_DURATION),
        }))
    }

    /// The movie's video from where its clock is now, at the camera's size and frame rate; black frames for a movie
    /// without video.
    ///
    /// # Errors
    /// [`DeviceError::Movie`] when `ffmpeg` can't be started.
    pub fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        let PatternCamera { width, height, fps } = self.camera;
        let decoder = if self.info.video.is_some() {
            let filter = format!(
                "fps={fps},scale={width}:{height}:force_original_aspect_ratio=decrease,\
                 pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,format=yuv420p"
            );
            let output = ["-map", "0:v:0", "-vf", &filter, "-f", "rawvideo"];
            Some(self.decode(&output, i420_len(width, height), VIDEO_QUEUE)?)
        } else {
            None
        };
        Ok(Box::new(MovieVideo {
            decoder,
            last: None,
            width,
            height,
            period: Duration::from_secs(1) / fps,
            clock: Clock::new(self.pacing, Duration::from_secs(1) / fps),
        }))
    }

    /// Where in the movie a device opened now starts: `from` for the first one, later ones as far on as the time
    /// since (a looped movie wrapped at its end).
    fn position(&self) -> Duration {
        let now = Instant::now();
        let epoch = *self
            .epoch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(now);
        let position = self.from + now.saturating_duration_since(epoch);
        match self.info.duration {
            Some(duration) if self.looped && !duration.is_zero() => {
                Duration::from_nanos(u64::try_from(position.as_nanos() % duration.as_nanos()).unwrap_or(0))
            }
            _ => position,
        }
    }

    /// An `ffmpeg` decoding from [`Movie::position`] with `output`'s options to its standard output, read in chunks
    /// of `chunk` bytes into a queue of `queue`.
    fn decode(&self, output: &[&str], chunk: usize, queue: usize) -> Result<Decoder, DeviceError> {
        let at = format!("{:.3}", self.position().as_secs_f64());
        let mut command = Command::new(FFMPEG);
        command.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-ss", &at]);
        if self.looped {
            command.args(["-stream_loop", "-1"]);
        }
        command
            .arg("-i")
            .arg(&self.path)
            .args(output)
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Decoder::spawn(command, chunk, queue)
    }
}

/// The bytes of an I420 frame of `width` by `height`.
#[must_use]
pub fn i420_len(width: u32, height: u32) -> usize {
    let (width, height) = (to_usize(width), to_usize(height));
    width * height + 2 * (width.div_ceil(2) * height.div_ceil(2))
}

/// A black I420 frame.
fn black(width: u32, height: u32) -> VideoFrame {
    let luma = to_usize(width) * to_usize(height);
    let mut data = vec![128; i420_len(width, height)];
    data[..luma].fill(16);
    VideoFrame {
        width,
        height,
        format: PixelFormat::I420,
        stride: width,
        data,
    }
}

/// Reads `reader` in chunks of exactly `chunk` bytes into `sink` until it ends (a part chunk at the end is dropped),
/// fails, or the receiving side is gone.
pub fn read_chunks(mut reader: impl Read, chunk: usize, sink: &SyncSender<Vec<u8>>) {
    loop {
        let mut buffer = vec![0; chunk];
        match reader.read_exact(&mut buffer) {
            Ok(()) => {
                if sink.send(buffer).is_err() {
                    return;
                }
            }
            Err(error) => {
                if error.kind() != ErrorKind::UnexpectedEof {
                    log::warn!("the movie's decoder can't be read: {error}");
                }
                return;
            }
        }
    }
}

/// One `ffmpeg` child, its output read into a queue; killed and reaped when dropped.
struct Decoder {
    /// Dropped first, so a reader waiting on a full queue stops.
    chunks: Receiver<Vec<u8>>,
    child: Child,
    readers: Vec<JoinHandle<()>>,
}

impl Decoder {
    fn spawn(mut command: Command, chunk: usize, queue: usize) -> Result<Self, DeviceError> {
        let mut child = command
            .spawn()
            .map_err(|error| DeviceError::Movie(format!("{FFMPEG} can't be started: {error}")))?;
        let stdout: Option<ChildStdout> = child.stdout.take();
        let stderr = child.stderr.take();
        let (sender, chunks) = sync_channel(queue);
        let mut readers = Vec::new();
        let mut started = |name: &str, body: Box<dyn FnOnce() + Send>| {
            thread::Builder::new()
                .name(name.to_owned())
                .spawn(body)
                .map(|reader| readers.push(reader))
                .map_err(|error| DeviceError::Movie(format!("a reader thread can't be started: {error}")))
        };
        if let Some(stdout) = stdout {
            started("movie-decoder", Box::new(move || read_chunks(stdout, chunk, &sender)))?;
        }
        if let Some(stderr) = stderr {
            started("movie-messages", Box::new(move || log_messages(stderr)))?;
        }
        Ok(Self { chunks, child, readers })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // the queue's receiving side goes first: a reader waiting to queue a chunk stops
        let (_, gone) = sync_channel(0);
        drop(std::mem::replace(&mut self.chunks, gone));
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

/// Logs the first few lines `ffmpeg` writes of its own, then reads the rest away.
fn log_messages(stderr: impl Read) {
    for (index, line) in BufReader::new(stderr).lines().enumerate() {
        let Ok(line) = line else { return };
        if index < MESSAGES_LOGGED {
            log::warn!("{FFMPEG}: {line}");
        }
    }
}

/// The movie's audio, at the device's pace: silence while the decoder is behind or done.
struct MovieAudio {
    decoder: Option<Decoder>,
    clock: Clock,
}

impl AudioSource for MovieAudio {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<AudioFrame>, DeviceError> {
        if !self.clock.wait(timeout) {
            return Ok(None);
        }
        self.clock.handed_one();
        let mut frame = AudioFrame::silence();
        if let Some(bytes) = self.decoder.as_ref().and_then(|decoder| decoder.chunks.try_recv().ok()) {
            let (pairs, _) = bytes.as_chunks::<2>();
            for (sample, pair) in frame.samples_mut().iter_mut().zip(pairs) {
                *sample = f32::from(i16::from_le_bytes(*pair)) / 32_768.0;
            }
        }
        Ok(Some(frame))
    }
}

/// The movie's video, at the device's pace: the last picture again while the decoder is behind or done (black before
/// the first one, once the decoder is done).
struct MovieVideo {
    decoder: Option<Decoder>,
    last: Option<VideoFrame>,
    width: u32,
    height: u32,
    period: Duration,
    clock: Clock,
}

impl MovieVideo {
    fn frame(&self, data: Vec<u8>) -> VideoFrame {
        VideoFrame {
            width: self.width,
            height: self.height,
            format: PixelFormat::I420,
            stride: self.width,
            data,
        }
    }
}

impl VideoSource for MovieVideo {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, DeviceError> {
        if !self.clock.wait(timeout) {
            return Ok(None);
        }
        let next = match &self.decoder {
            Some(decoder) => decoder.chunks.recv_timeout(self.period.min(timeout)),
            None => Err(RecvTimeoutError::Disconnected),
        };
        let frame = match next {
            Ok(data) => self.frame(data),
            Err(RecvTimeoutError::Timeout) => match &self.last {
                Some(last) => last.clone(),
                // the decoder hasn't started: nothing to show yet
                None => return Ok(None),
            },
            Err(RecvTimeoutError::Disconnected) => self.last.clone().unwrap_or_else(|| black(self.width, self.height)),
        };
        self.clock.handed_one();
        self.last = Some(frame.clone());
        Ok(Some(frame))
    }
}

impl MovieVideo {
    /// Whether the decoder still runs (for tests).
    #[cfg(test)]
    fn decoding(&self) -> bool {
        self.decoder.as_ref().is_some_and(|decoder| {
            !matches!(
                decoder.chunks.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Disconnected)
            )
        })
    }
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde_json::json;

    use super::*;

    #[test]
    fn a_probe_answer_is_read_for_duration_video_and_audio() {
        let answer = json!({
            "streams": [
                {"codec_type": "video", "width": 1920, "height": 1080, "avg_frame_rate": "24/1"},
                {"codec_type": "audio", "channels": 6},
                {"codec_type": "data"}
            ],
            "format": {"duration": "596.458333"}
        });
        let info = read_probe(&answer);
        assert_eq!(info.video, Some((1920, 1080, 24.0)));
        assert!(info.audio);
        assert_eq!(info.duration.map(|d| d.as_millis()), Some(596_458));

        let silent = read_probe(&json!({
            "streams": [{"codec_type": "video", "width": 640, "height": 360, "avg_frame_rate": "0/0",
                         "r_frame_rate": "30000/1001"}],
            "format": {}
        }));
        assert!(!silent.audio && silent.duration.is_none());
        assert!(silent.video.is_some_and(|(_, _, rate)| (rate - 29.97).abs() < 0.01));
        assert_eq!(read_probe(&json!({})).video, None);
    }

    #[test]
    fn chunks_are_read_whole_and_a_part_one_at_the_end_is_dropped() {
        let bytes: Vec<u8> = (0..=24).collect();
        let (sender, chunks) = sync_channel(10);
        read_chunks(Cursor::new(bytes), 6, &sender);
        drop(sender);
        let read: Vec<Vec<u8>> = chunks.iter().collect();
        assert_eq!(read.len(), 4);
        assert_eq!(read[3], vec![18, 19, 20, 21, 22, 23]);
    }

    #[test]
    fn a_reader_stops_when_the_queue_s_receiver_is_gone() {
        let (sender, chunks) = sync_channel(1);
        drop(chunks);
        // an endless reader: returns only because nobody takes its chunks
        read_chunks(std::io::repeat(7), 4, &sender);
    }

    #[test]
    fn the_i420_size_counts_both_chroma_planes() {
        assert_eq!(i420_len(1280, 720), 1280 * 720 * 3 / 2);
        assert_eq!(i420_len(3, 3), 9 + 2 * 4);
        let frame = black(4, 2);
        assert_eq!(frame.data, vec![16, 16, 16, 16, 16, 16, 16, 16, 128, 128, 128, 128]);
    }

    fn ffmpeg_present() -> bool {
        [FFMPEG, FFPROBE].iter().all(|tool| {
            Command::new(tool)
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        })
    }

    /// A 4 s movie of ffmpeg's test source and a 1 kHz sine, made in `dir`.
    fn test_movie(dir: &Path) -> PathBuf {
        let path = dir.join("test.mkv");
        let status = Command::new(FFMPEG)
            .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=size=320x240:rate=24:duration=4"])
            .args(["-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=44100:duration=4"])
            .args(["-c:v", "ffv1", "-c:a", "pcm_s16le", "-ac", "2"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        path
    }

    const CAMERA: PatternCamera = PatternCamera {
        width: 160,
        height: 90,
        fps: 10,
    };

    #[test]
    fn a_movie_plays_its_audio_and_its_video_at_the_camera_s_size_until_it_ends() {
        if !ffmpeg_present() {
            eprintln!("skipped: no ffmpeg");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let movie = Movie::open(
            test_movie(dir.path()),
            Duration::from_secs(3),
            false,
            CAMERA,
            Pacing::Unpaced,
        )
        .unwrap();
        assert!(movie.info().audio);
        assert_eq!(movie.info().video.map(|(w, h, _)| (w, h)), Some((320, 240)));

        let mut microphone = movie.microphone().unwrap();
        let wait = Duration::from_secs(5);
        let deadline = Instant::now() + wait;
        let mut loud = 0;
        // the decoder starts behind: silence until its frames come
        while loud < 50 && Instant::now() < deadline {
            let frame = microphone.next_frame(wait).unwrap().unwrap();
            // the sine is at 1/8 of full scale, a little less after the stereo file is mixed down
            if frame.samples().iter().any(|sample| sample.abs() > 0.03) {
                loud += 1;
            }
        }
        assert_eq!(loud, 50, "the sine never came");

        // a movie of its own, so its clock starts with the camera
        let movie = Movie::open(
            test_movie(dir.path()),
            Duration::from_secs(3),
            false,
            CAMERA,
            Pacing::Unpaced,
        )
        .unwrap();
        let mut camera = movie.camera().unwrap();
        let deadline = Instant::now() + wait;
        // none while the decoder starts, each wait one frame's time
        let first = loop {
            if let Some(frame) = camera.next_frame(wait).unwrap() {
                break frame;
            }
            assert!(Instant::now() < deadline, "no frame came");
        };
        assert_eq!((first.width, first.height, first.format), (160, 90, PixelFormat::I420));
        assert_eq!(first.data.len(), i420_len(160, 90));
        // from 3 s of a 4 s movie at 10 fps: 10 frames, then the last one again once the decoder is done
        let frames: Vec<VideoFrame> = (0..40)
            .map(|_| camera.next_frame(wait).unwrap().expect("a frame"))
            .collect();
        assert!(frames.windows(2).any(|pair| pair[0] != pair[1]), "the picture moves");
        assert_eq!(frames[38], frames[39], "the last picture stays");
    }

    #[test]
    fn a_dropped_device_kills_its_decoder_and_a_late_one_starts_further_in() {
        if !ffmpeg_present() {
            eprintln!("skipped: no ffmpeg");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let movie = Movie::open(test_movie(dir.path()), Duration::ZERO, true, CAMERA, Pacing::RealTime).unwrap();
        assert_eq!(movie.position(), Duration::ZERO);
        thread::sleep(Duration::from_millis(300));
        assert!(movie.position() >= Duration::from_millis(300));
        let video = MovieVideo {
            decoder: Some(movie.decode(&["-map", "0:v:0", "-f", "rawvideo"], 16, 1).unwrap()),
            last: None,
            width: 2,
            height: 2,
            period: Duration::from_millis(100),
            clock: Clock::new(Pacing::Unpaced, Duration::from_millis(100)),
        };
        thread::sleep(Duration::from_millis(300));
        assert!(video.decoding());
        // the drop kills and reaps the child, and joins its readers: it returns
        drop(video);
        let offset = Movie::open(
            test_movie(dir.path()),
            Duration::from_secs(5),
            false,
            CAMERA,
            Pacing::Unpaced,
        );
        assert!(matches!(offset, Err(DeviceError::Movie(_))));
    }
}
