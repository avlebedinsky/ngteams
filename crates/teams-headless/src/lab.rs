//! A lab account's native client, as the binary builds it: [`teams_core::lab::start`] over the fence from
//! [`Fence::open_lab_account`] (the roster from `lab.toml`, the lab's `fence.log`, the account's own MRI) and the tokens
//! from the Secret Service
//! (`teams-lab`, [`LabPaths::KEYRING_APPLICATION`], as `proto-lab auth` stored them); the store at
//! [`LabPaths::store_file`]. Nothing here reads or prints a token: the session asks the Secret Service for one only
//! when a request needs it.
//!
//! A call's engine ([`Lab::start_calling`]) has its calling too: its media on the file backend ([`call_devices`]: no
//! audio server), relayed through Teams' relay, the microphone unprocessed (a file has no acoustic echo, and a keyed
//! tone must reach the far end as it is), the account describing its client as the web client does. With `--video`
//! a call takes video ([`teams_core::lab::call_video`]): the far end's frames counted ([`VideoCounts`]), the camera
//! the file backend's test pattern. With `--media` a movie is the microphone and the camera ([`call_movie`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use teams_auth::secrets::{KeyringSecretStore, SecretError};
use teams_core::account::AccountCalls;
use teams_core::engine::EngineHandle;
use teams_core::lab::{self as core_lab, LAB_LANGUAGE, LabEngine, web_client_info};
use teams_core::{CallMedia, CallVideo, Relay, VideoOutput};
use teams_fence::lab::{Alias, ChatsError, ConfigError, LabChats, LabConfig, LabLocations, LabPaths};
use teams_fence::{Fence, Mri, OpenError};
use teams_media::devices::file::{AudioInput, FileDevices, FrameCounter, FrameCounts, Pacing, PatternCamera};
use teams_media::devices::movie::Movie;
use teams_media::devices::{AudioFrame, AudioSink, AudioSource, DeviceError, Devices, VideoSink, VideoSource};
use teams_media::processing::ProcessingConfig;
use teams_media::session::SessionConfig;
use teams_store::{Store, StoreError};

pub use teams_core::lab::display_name;

use crate::chat::LabNames;

/// Why the lab's client couldn't be set up.
#[derive(Debug, thiserror::Error)]
pub enum LabError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Chats(#[from] ChatsError),
    #[error("the fence can't be opened: {0}")]
    Fence(#[from] OpenError),
    #[error(transparent)]
    Secrets(#[from] SecretError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Start(#[from] core_lab::LabError),
    #[error("lab.toml has no MRI for the echo service yet (run `proto-lab service-mri echo <mri>`)")]
    NoEcho,
    #[error("the recordings directory can't be made: {0}")]
    Recordings(#[source] io::Error),
}

/// The lab as one account's commands see it: its config, the names of its accounts, its recorded group chat.
#[derive(Debug)]
pub struct Lab {
    pub config: LabConfig,
    pub alias: Alias,
    pub names: LabNames,
    /// The a–b–c group chat from `chats.toml`, if recorded.
    pub abc: Option<String>,
}

impl Lab {
    /// Reads `lab.toml` (from the XDG locations) and `chats.toml`; reads no token.
    ///
    /// # Errors
    /// When either can't be read.
    pub fn load(alias: Alias) -> Result<Self, LabError> {
        let config = LabConfig::load(&LabLocations::from_env()?)?;
        let abc = LabChats::load(config.paths())?.abc;
        Ok(Self {
            names: LabNames::from_config(&config),
            config,
            alias,
            abc,
        })
    }

    #[must_use]
    pub fn paths(&self) -> &LabPaths {
        self.config.paths()
    }

    /// The account's store, `<data_dir>/store/<alias>.sqlite`.
    ///
    /// # Errors
    /// When it can't be opened (see [`Store::open`]).
    pub fn open_store(&self) -> Result<Store, LabError> {
        Ok(Store::open(&self.paths().store_file(self.alias))?)
    }

    /// The account's engine: with its push channel (`push`) for `watch`, without for a one-shot command. Connects to
    /// the Secret Service; no request goes out before the engine makes one.
    ///
    /// # Errors
    /// See [`LabError`].
    pub async fn start_engine(&self, push: bool) -> Result<EngineHandle, LabError> {
        let fence = Arc::new(Fence::open_lab_account(self.alias)?);
        let secrets = KeyringSecretStore::new(LabPaths::KEYRING_APPLICATION).await?;
        let LabEngine { handle, .. } = core_lab::start(&self.config, self.alias, fence, secrets, push, None)?;
        Ok(handle)
    }

    /// The account's engine with its calling, its calls' media on `devices` (see the module docs): with its messaging
    /// push channel (`push`) for `watch`, without for `call` and `join` (a call needs only the calling socket).
    ///
    /// # Errors
    /// See [`LabError`].
    pub async fn start_calling(
        &self,
        devices: impl Devices + Send + Sync + 'static,
        push: bool,
        video: Option<CallVideo>,
    ) -> Result<EngineHandle, LabError> {
        let fence = Arc::new(Fence::open_lab_account(self.alias)?);
        let secrets = KeyringSecretStore::new(LabPaths::KEYRING_APPLICATION).await?;
        let calls = AccountCalls {
            media: CallMedia {
                video,
                ..call_media(devices, Relay::Teams)
            },
            client: web_client_info(),
            language_id: Some(LAB_LANGUAGE.to_owned()),
        };
        let LabEngine { handle, .. } = core_lab::start(&self.config, self.alias, fence, secrets, push, Some(calls))?;
        Ok(handle)
    }

    /// The echo service's MRI, as lab.toml records it.
    ///
    /// # Errors
    /// [`LabError::NoEcho`] until it is recorded.
    pub fn echo(&self) -> Result<Mri, LabError> {
        self.config
            .service_participants()
            .iter()
            .find(|service| service.name == "echo")
            .and_then(|service| service.mri.as_deref())
            .map(Mri::new)
            .ok_or(LabError::NoEcho)
    }

    /// Where a call's audio is kept, `<data_dir>/recordings/` (`0700`, made when missing): lab data, never in the repo.
    ///
    /// # Errors
    /// [`LabError::Recordings`] when it can't be made.
    pub fn recordings_dir(&self) -> Result<PathBuf, LabError> {
        let dir = self.paths().data_dir().join("recordings");
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(LabError::Recordings)?;
        Ok(dir)
    }
}

/// A call's devices on the file backend: the microphone plays the WAV at `sent` (then silence), the speaker records
/// to `recording`, both at a device's pace; the camera is the test pattern.
#[must_use]
pub fn call_devices(sent: PathBuf, recording: PathBuf) -> FileDevices {
    FileDevices {
        input: AudioInput::Wav(sent),
        recording: Some(recording),
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    }
}

/// The camera's size and frame rate when a movie is the camera (`--media`): 720p at the movie's own 24 fps.
pub const MEDIA_CAMERA: PatternCamera = PatternCamera {
    width: 1280,
    height: 720,
    fps: 24,
};

/// The movie at `path` from `from` seconds (once, or `looped`) as a call's microphone and camera
/// ([`MEDIA_CAMERA`]), probed now.
///
/// # Errors
/// When it can't be probed or `from` isn't in it.
pub fn call_movie(path: PathBuf, from: f64, looped: bool) -> Result<Movie, DeviceError> {
    let from = Duration::try_from_secs_f64(from)
        .map_err(|_| DeviceError::Movie(format!("{from} isn't an offset in seconds")))?;
    Movie::open(path, from, looped, MEDIA_CAMERA, Pacing::RealTime)
}

/// The `media` line printed when a movie is a call's microphone and camera: `media file=<name> from_s=<n>
/// loop=<bool> duration_s=<n|-> source=<w>x<h>@<fps>|- audio=<bool> camera=<w>x<h>@<fps>` (the file's name, not its
/// path).
#[must_use]
pub fn media_line(path: &Path, from: f64, looped: bool, movie: &Movie) -> String {
    let info = movie.info();
    let name = path
        .file_name()
        .map_or_else(|| "-".to_owned(), |name| name.to_string_lossy().into_owned());
    let duration = info
        .duration
        .map_or_else(|| "-".to_owned(), |duration| format!("{:.1}", duration.as_secs_f64()));
    let source = info.video.map_or_else(
        || "-".to_owned(),
        |(width, height, fps)| format!("{width}x{height}@{fps:.3}"),
    );
    let camera = movie.camera_format();
    format!(
        "media file={} from_s={from:.1} loop={looped} duration_s={duration} source={source} audio={} camera={}x{}@{}",
        serde_json::Value::String(name),
        info.audio,
        camera.width,
        camera.height,
        camera.fps
    )
}

/// When a call's devices last began opening, by the wall clock. These construction diagnostics are separate from
/// the audio sample origins ([`SampleStarts`]), because loading a WAV can delay the first frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Opened {
    pub microphone: Option<SystemTime>,
    pub speaker: Option<SystemTime>,
}

/// The first audio frame's time: a monotonic clock for one process's measurements and the wall clock for comparing
/// recordings made by separate processes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleStart {
    pub wall: SystemTime,
    pub(crate) instant: Instant,
}

impl SampleStart {
    fn now() -> Self {
        Self {
            instant: Instant::now(),
            wall: SystemTime::now(),
        }
    }
}

/// When the latest microphone first returned a frame and the latest speaker first accepted one. Merely constructing
/// a device supplies no sample origin; an unsuccessful read or write supplies none either.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SampleStarts {
    pub microphone: Option<SampleStart>,
    pub speaker: Option<SampleStart>,
}

/// File devices that note when each one opens ([`Opened`]; a later call's open replaces an earlier one's); with a
/// movie, its audio and video are the microphone and the camera.
#[derive(Clone, Debug)]
pub struct TimedDevices {
    pub files: FileDevices,
    pub movie: Option<Movie>,
    opened: Arc<Mutex<Opened>>,
    samples: Arc<Mutex<SampleStarts>>,
}

impl TimedDevices {
    #[must_use]
    pub fn new(files: FileDevices) -> Self {
        Self {
            files,
            movie: None,
            opened: Arc::default(),
            samples: Arc::default(),
        }
    }

    /// The same devices, `movie` the microphone and the camera.
    #[must_use]
    pub fn with_movie(self, movie: Movie) -> Self {
        Self {
            movie: Some(movie),
            ..self
        }
    }

    /// When the devices last opened.
    #[must_use]
    pub fn opened(&self) -> Opened {
        *self.opened.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The latest streams' actual first-sample times, independent of how long constructing a device took.
    #[must_use]
    pub fn sample_starts(&self) -> SampleStarts {
        *self.samples.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn note(&self, what: impl FnOnce(&mut Opened, SystemTime)) {
        what(
            &mut self.opened.lock().unwrap_or_else(PoisonError::into_inner),
            SystemTime::now(),
        );
    }
}

impl Devices for TimedDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        self.note(|opened, now| opened.microphone = Some(now));
        self.samples.lock().unwrap_or_else(PoisonError::into_inner).microphone = None;
        let source = match &self.movie {
            Some(movie) => movie.microphone(),
            None => self.files.microphone(),
        }?;
        Ok(Box::new(TimedSource {
            source,
            samples: Arc::clone(&self.samples),
            started: false,
        }))
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        self.note(|opened, now| opened.speaker = Some(now));
        self.samples.lock().unwrap_or_else(PoisonError::into_inner).speaker = None;
        let sink = self.files.speaker()?;
        Ok(Box::new(TimedSink {
            sink,
            samples: Arc::clone(&self.samples),
            started: false,
        }))
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        match &self.movie {
            Some(movie) => movie.camera(),
            None => self.files.camera(),
        }
    }
}

struct TimedSource {
    source: Box<dyn AudioSource>,
    samples: Arc<Mutex<SampleStarts>>,
    started: bool,
}

impl AudioSource for TimedSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<AudioFrame>, DeviceError> {
        let frame = self.source.next_frame(timeout)?;
        if frame.is_some() && !self.started {
            self.samples.lock().unwrap_or_else(PoisonError::into_inner).microphone = Some(SampleStart::now());
            self.started = true;
        }
        Ok(frame)
    }
}

struct TimedSink {
    sink: Box<dyn AudioSink>,
    samples: Arc<Mutex<SampleStarts>>,
    started: bool,
}

impl AudioSink for TimedSink {
    fn play(&mut self, frame: &AudioFrame) -> Result<(), DeviceError> {
        let at = (!self.started).then(SampleStart::now);
        self.sink.play(frame)?;
        if let Some(at) = at {
            self.samples.lock().unwrap_or_else(PoisonError::into_inner).speaker = Some(at);
            self.started = true;
        }
        Ok(())
    }
}

/// What a call's video showed: the far end's decoded frames by video line (a 1:1 call's on line 0, a group call's
/// participants' on lines 1, 2, …), and our camera's as sent, each counted.
#[derive(Clone, Debug, Default)]
pub struct VideoCounts {
    pub remote: Arc<Mutex<BTreeMap<usize, FrameCounter>>>,
    pub preview: FrameCounter,
}

impl VideoOutput for VideoCounts {
    fn remote(&self, line: usize) -> Box<dyn VideoSink> {
        let mut lines = self.remote.lock().unwrap_or_else(PoisonError::into_inner);
        Box::new(lines.entry(line).or_default().clone())
    }

    fn preview(&self) -> Option<Box<dyn VideoSink>> {
        Some(Box::new(self.preview.clone()))
    }
}

impl VideoCounts {
    /// Each line's counts so far, by line.
    #[must_use]
    pub fn lines(&self) -> Vec<(usize, FrameCounts)> {
        let lines = self.remote.lock().unwrap_or_else(PoisonError::into_inner);
        lines.iter().map(|(line, counter)| (*line, counter.counts())).collect()
    }

    /// The frames each line brought so far, as `video_lines` prints them while a call runs: `received=<line>:<frames>,…`
    /// in line order; `None` before any line brought one.
    #[must_use]
    pub fn progress(&self) -> Option<String> {
        let lines = self.lines();
        if lines.is_empty() {
            return None;
        }
        let each: Vec<String> = lines
            .iter()
            .map(|(line, counts)| format!("{line}:{}", counts.frames))
            .collect();
        Some(format!("received={}", each.join(",")))
    }

    /// The summary line: `video received=<frames> changed=<n> indexed=<n> gaps=<n> last_index=<n|-> sent=<frames>
    /// lines=<n>`, the far end's frames as decoded over every line (those of a test pattern carry their index; the
    /// last index the highest of any line) and our camera's as sent, then one `video line=<n> received=<frames>
    /// indexed=<n> gaps=<n> last_index=<n|->` per line that brought frames.
    #[must_use]
    pub fn line(&self) -> String {
        let lines = self.lines();
        let sum = |count: fn(&FrameCounts) -> u64| lines.iter().map(|(_, counts)| count(counts)).sum::<u64>();
        let last = |index: Option<u32>| index.map_or_else(|| "-".to_owned(), |index| index.to_string());
        let highest = lines.iter().filter_map(|(_, counts)| counts.last_index).max();
        let mut text = format!(
            "video received={} changed={} indexed={} gaps={} last_index={} sent={} lines={}",
            sum(|counts| counts.frames),
            sum(|counts| counts.changed),
            sum(|counts| counts.indexed),
            sum(|counts| counts.gaps),
            last(highest),
            self.preview.counts().frames,
            lines.len()
        );
        for (line, counts) in &lines {
            let _ = write!(
                text,
                "\nvideo line={line} received={} indexed={} gaps={} last_index={}",
                counts.frames,
                counts.indexed,
                counts.gaps,
                last(counts.last_index)
            );
        }
        text
    }
}

/// A call's media on `devices` through `relay`, the microphone unprocessed (see the module docs).
#[must_use]
pub fn call_media(devices: impl Devices + Send + Sync + 'static, relay: Relay) -> CallMedia {
    CallMedia {
        devices: Arc::new(devices),
        relay,
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
        },
        video: None,
    }
}

#[cfg(test)]
#[path = "../tests/support/sample_starts.rs"]
mod sample_tests;
