//! The PipeWire backend: the microphone, the speaker and the camera as streams on one PipeWire server.
//!
//! **Which server is fixed when the crate is built** ([`THIS_BUILD`]), never by an argument:
//!
//! - [`Reach::Sandbox`] for the lab (no feature) and for every build with `test-support` on, even with `personal` on
//!   too (as `cargo test --workspace` and `clippy --all-features` build it): only the UI sandbox's private server, at
//!   [`SANDBOX_SOCKET`] inside the dir [`SANDBOX_ENV`] names, with the client config the sandbox wrote there
//!   ([`SANDBOX_CLIENT_CONF`]). Anything else is refused: no sandbox, a sandbox dir that is (or holds) the desktop's
//!   runtime dir, a socket that leads out of the sandbox dir, the desktop's PipeWire or Pulse sockets, and a process
//!   whose environment sets one of [`CONFIG_OVERRIDES`] (libpipewire would load that config instead of the sandbox's).
//! - [`Reach::Desktop`] only for `personal` without `test-support` (only `crates/teams` enables `personal`): the
//!   desktop session's server, `pipewire-0` in `PIPEWIRE_RUNTIME_DIR` or else `XDG_RUNTIME_DIR`.
//!
//! The socket is connected here and handed to libpipewire as a descriptor (`pw_context_connect_fd`), so libpipewire
//! never looks for a server by itself (it would fall back from one runtime dir to another, and to `/run/pipewire`).
//!
//! **Threads**: libpipewire's objects live on one thread of this backend's own, running PipeWire's main loop
//! ([`PipeWireDevices`] starts it and stops it when the last device and handle are gone); libpipewire adds its own
//! data thread. Frames cross to and from the caller over bounded queues that drop their oldest entries when full: the
//! microphone keeps the newest [`QUEUED_FRAMES`] and the camera the newest [`QUEUED_VIDEO`] when nobody reads, the
//! speaker keeps [`SPEAKER_QUEUE`] of audio. A device whose stream ends (it errs or is unlinked, or the server goes
//! away) reports [`DeviceError::Closed`]: a source once its queue is drained, the speaker at once. Once the server
//! went away and the loop thread ended, opening a device reports [`DeviceError::Closed`] too. The crate has no
//! logger: libpipewire's own warnings go to stderr.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{AudioFrame, AudioSink, AudioSource, DeviceError, Devices, VideoFrame, VideoSource};

mod streams;

use crate::queue::Queue;

/// Which PipeWire server a build may open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Only the UI sandbox's private server.
    Sandbox,
    /// Only the desktop session's server.
    Desktop,
}

/// The reach of a build with these features: `test-support` always wins.
#[must_use]
pub const fn reach_for(test_support: bool, personal: bool) -> Reach {
    if personal && !test_support {
        Reach::Desktop
    } else {
        Reach::Sandbox
    }
}

/// This build's reach, fixed by its features.
pub const THIS_BUILD: Reach = reach_for(cfg!(feature = "test-support"), cfg!(feature = "personal"));

/// The variable naming the UI sandbox's dir (as `ui_sandbox::SANDBOX_ENV`).
pub const SANDBOX_ENV: &str = "UI_SANDBOX_DIR";
/// The sandbox's PipeWire socket, relative to its dir.
pub const SANDBOX_SOCKET: &str = "pipewire/pipewire-0";
/// The client config the sandbox writes for its clients, relative to its dir: libpipewire reads it and nothing of
/// the session's (no `client.conf.d` from the user's config dirs).
pub const SANDBOX_CLIENT_CONF: &str = "pipewire-config/client.conf";

/// How long the server gets to answer the first round trip.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the loop thread gets to create a stream.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// How often an open that waits for its stream checks that the loop thread still runs.
const OPEN_CHECK: Duration = Duration::from_millis(50);
/// Audio frames the microphone keeps when nobody reads them (0.5 s), the newest; older ones are dropped.
pub const QUEUED_FRAMES: usize = 50;
/// Video frames the camera keeps when nobody reads them, the newest; older ones are dropped.
pub const QUEUED_VIDEO: usize = 3;
/// Audio the speaker keeps queued, in samples (200 ms); more drops the oldest.
pub const SPEAKER_QUEUE: usize = 9_600;

/// The session's variables this backend reads, and the user's id (for the fixed runtime dir, `/run/user/<uid>`).
#[derive(Clone, Debug, Default)]
pub struct SessionEnv {
    vars: HashMap<String, OsString>,
    uid: u32,
}

/// The variables libpipewire reads ahead of the client config a context names (`config.name`, `config.prefix`): set,
/// they load another config in its place.
pub const CONFIG_OVERRIDES: [&str; 2] = ["PIPEWIRE_CONFIG_NAME", "PIPEWIRE_CONFIG_PREFIX"];

/// The variables [`SessionEnv::current`] takes from the process.
const READ_VARS: [&str; 5] = [
    SANDBOX_ENV,
    "PIPEWIRE_RUNTIME_DIR",
    "XDG_RUNTIME_DIR",
    CONFIG_OVERRIDES[0],
    CONFIG_OVERRIDES[1],
];

impl SessionEnv {
    /// This process's.
    #[must_use]
    pub fn current() -> Self {
        let vars = READ_VARS
            .iter()
            .filter_map(|name| std::env::var_os(name).map(|value| ((*name).to_owned(), value)))
            .collect();
        Self { vars, uid: own_uid() }
    }

    /// The given variables and user id (tests).
    #[must_use]
    pub fn from_vars<K: Into<String>, V: Into<OsString>>(vars: impl IntoIterator<Item = (K, V)>, uid: u32) -> Self {
        Self {
            vars: vars
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
            uid,
        }
    }

    fn var(&self, name: &str) -> Option<&Path> {
        self.vars.get(name).filter(|value| !value.is_empty()).map(Path::new)
    }

    /// The first of [`CONFIG_OVERRIDES`] set at all (even empty: libpipewire reads it as it is).
    fn config_override(&self) -> Option<&str> {
        CONFIG_OVERRIDES.into_iter().find(|name| self.vars.contains_key(*name))
    }

    /// The desktop's fixed runtime dirs: the user's session dir and the system-wide PipeWire dir.
    fn desktop_dirs(&self) -> [PathBuf; 2] {
        [
            PathBuf::from(format!("/run/user/{}", self.uid)),
            PathBuf::from("/run/pipewire"),
        ]
    }
}

/// The user id of this process.
fn own_uid() -> u32 {
    // SAFETY: getuid has no preconditions, can't fail and touches no memory of ours.
    unsafe { libc::getuid() }
}

/// Where this build's devices connect, and with which client config (`None`: libpipewire's default lookup).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub socket: PathBuf,
    pub client_conf: Option<PathBuf>,
}

/// The server `reach` allows in the session `env` describes.
///
/// # Errors
/// [`DeviceError::Refused`], with the reason: see the module's notes.
pub fn endpoint(reach: Reach, env: &SessionEnv) -> Result<Endpoint, DeviceError> {
    match reach {
        Reach::Sandbox => sandbox_endpoint(env),
        Reach::Desktop => desktop_endpoint(env),
    }
}

fn refused(reason: impl Into<String>) -> DeviceError {
    DeviceError::Refused(reason.into())
}

fn sandbox_endpoint(env: &SessionEnv) -> Result<Endpoint, DeviceError> {
    let dir = env.var(SANDBOX_ENV).ok_or_else(|| {
        refused(format!(
            "this build opens only the UI sandbox's PipeWire, and {SANDBOX_ENV} is unset (the sandbox sets it)"
        ))
    })?;
    if let Some(name) = env.config_override() {
        return Err(refused(format!(
            "{name} is set: libpipewire would load that config instead of the sandbox's client config"
        )));
    }
    if !dir.is_absolute() || dir.components().any(|part| part == Component::ParentDir) {
        return Err(refused(format!("{SANDBOX_ENV} must be an absolute path without `..`")));
    }
    let dir = dir.canonicalize()?;
    for desktop in env.desktop_dirs() {
        let desktop = desktop.canonicalize().unwrap_or(desktop);
        if desktop.starts_with(&dir) {
            return Err(refused(format!(
                "the sandbox dir {} is or holds the desktop's runtime dir {}",
                dir.display(),
                desktop.display()
            )));
        }
    }
    let socket = dir.join(SANDBOX_SOCKET).canonicalize().map_err(|error| {
        refused(format!(
            "the sandbox has no PipeWire at {}: {error}",
            dir.join(SANDBOX_SOCKET).display()
        ))
    })?;
    if !socket.starts_with(&dir) {
        return Err(refused(format!(
            "the sandbox's PipeWire socket leads out of the sandbox dir, to {}",
            socket.display()
        )));
    }
    if desktop_sockets(env).contains(&socket) {
        return Err(refused(format!("{} is the desktop's", socket.display())));
    }
    check_socket(&socket)?;
    let client_conf = dir.join(SANDBOX_CLIENT_CONF).canonicalize().map_err(|error| {
        refused(format!(
            "the sandbox wrote no PipeWire client config ({SANDBOX_CLIENT_CONF}): {error}"
        ))
    })?;
    if !client_conf.starts_with(&dir) {
        return Err(refused(
            "the sandbox's PipeWire client config leads out of the sandbox dir",
        ));
    }
    Ok(Endpoint {
        socket,
        client_conf: Some(client_conf),
    })
}

/// The desktop's PipeWire and Pulse sockets at their fixed places, as they resolve.
fn desktop_sockets(env: &SessionEnv) -> Vec<PathBuf> {
    let mut sockets = Vec::new();
    for dir in env.desktop_dirs() {
        for name in ["pipewire-0", "pipewire-0-manager", "pulse/native", "native"] {
            let path = dir.join(name);
            sockets.push(path.canonicalize().unwrap_or(path));
        }
    }
    sockets
}

fn check_socket(socket: &Path) -> Result<(), DeviceError> {
    let metadata = socket.metadata()?;
    if !metadata.file_type().is_socket() {
        return Err(refused(format!("{} isn't a socket", socket.display())));
    }
    if metadata.uid() != own_uid() {
        return Err(refused(format!("{} belongs to another user", socket.display())));
    }
    Ok(())
}

fn desktop_endpoint(env: &SessionEnv) -> Result<Endpoint, DeviceError> {
    let dir = env
        .var("PIPEWIRE_RUNTIME_DIR")
        .or_else(|| env.var("XDG_RUNTIME_DIR"))
        .ok_or_else(|| refused("neither PIPEWIRE_RUNTIME_DIR nor XDG_RUNTIME_DIR is set: no desktop session"))?;
    if !dir.is_absolute() {
        return Err(refused("the desktop's runtime dir must be an absolute path"));
    }
    let socket = dir.join("pipewire-0");
    check_socket(&socket)?;
    Ok(Endpoint {
        socket,
        client_conf: None,
    })
}

/// The PipeWire backend's devices: one connection, on a thread of its own, to the server this build allows.
#[derive(Clone)]
pub struct PipeWireDevices {
    connection: Arc<Connection>,
}

impl std::fmt::Debug for PipeWireDevices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeWireDevices").finish_non_exhaustive()
    }
}

impl PipeWireDevices {
    /// Connects to the server this build allows ([`THIS_BUILD`]) in this process's session.
    ///
    /// # Errors
    /// The server is refused ([`DeviceError::Refused`]), can't be reached, or doesn't answer in time.
    pub fn connect() -> Result<Self, DeviceError> {
        let endpoint = endpoint(THIS_BUILD, &SessionEnv::current())?;
        let stream = UnixStream::connect(&endpoint.socket)?;
        Connection::start(stream.into(), endpoint.client_conf).map(|connection| Self {
            connection: Arc::new(connection),
        })
    }

    /// Asks the loop thread for a stream and waits for it. The guard exists from the moment the command is sent, so
    /// a stream the loop makes after this gave up (a timeout) is closed again right after. A loop thread that has
    /// ended (the server went away), or ends while this waits, makes it [`DeviceError::Closed`]: nobody would ever
    /// take the command.
    fn open(
        &self,
        make: impl FnOnce(u64, mpsc::Sender<Result<(), String>>) -> streams::Command,
    ) -> Result<StreamGuard, DeviceError> {
        let id = self.connection.next_id.fetch_add(1, Ordering::Relaxed);
        let (ready_tx, ready_rx) = mpsc::channel();
        self.connection.send(make(id, ready_tx))?;
        let guard = StreamGuard::new(id, &self.connection);
        let deadline = Instant::now() + OPEN_TIMEOUT;
        loop {
            // read before the wait: the loop thread answers before it ends, so once it has ended its answer, if any,
            // is already in the channel
            let ended = self.connection.ended();
            let slice = OPEN_CHECK.min(deadline.saturating_duration_since(Instant::now()));
            match ready_rx.recv_timeout(slice) {
                Ok(Ok(())) => return Ok(guard),
                Ok(Err(error)) => return Err(DeviceError::PipeWire(error)),
                Err(RecvTimeoutError::Disconnected) => return Err(DeviceError::Closed),
                Err(RecvTimeoutError::Timeout) if ended => return Err(DeviceError::Closed),
                Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                    return Err(DeviceError::PipeWire(format!(
                        "the stream wasn't created within {OPEN_TIMEOUT:?}"
                    )));
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

impl Devices for PipeWireDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        let frames = Queue::new(QUEUED_FRAMES);
        let shared = frames.clone();
        let stream = self.open(|id, ready| streams::Command::Microphone {
            id,
            frames: shared,
            ready,
        })?;
        Ok(Box::new(Microphone {
            frames,
            _stream: stream,
        }))
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        let queue = Queue::new(SPEAKER_QUEUE);
        let shared = queue.clone();
        let stream = self.open(|id, ready| streams::Command::Speaker {
            id,
            queue: shared,
            ready,
        })?;
        Ok(Box::new(Speaker { queue, _stream: stream }))
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        let frames = Queue::new(QUEUED_VIDEO);
        let shared = frames.clone();
        let stream = self.open(|id, ready| streams::Command::Camera {
            id,
            frames: shared,
            ready,
        })?;
        Ok(Box::new(Camera {
            frames,
            _stream: stream,
        }))
    }
}

/// The loop thread and the way to it.
struct Connection {
    commands: Mutex<pipewire::channel::Sender<streams::Command>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    next_id: AtomicU64,
    /// Set once the loop thread has stopped taking commands (it quit, or the server went away): PipeWire's channel
    /// keeps taking them, so a send can't tell by itself.
    ended: Arc<AtomicBool>,
}

/// Marks the loop thread's end when dropped, on its way out (a panic too).
struct EndMark(Arc<AtomicBool>);

impl Drop for EndMark {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Connection {
    fn start(fd: OwnedFd, client_conf: Option<PathBuf>) -> Result<Self, DeviceError> {
        let (commands, receiver) = pipewire::channel::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let ended = Arc::new(AtomicBool::new(false));
        let mark = EndMark(Arc::clone(&ended));
        let thread = thread::Builder::new()
            .name("teams-media-pipewire".to_owned())
            .spawn(move || {
                let _mark = mark;
                streams::run(fd, client_conf.as_deref(), receiver, &started_tx);
            })?;
        let connection = Self {
            commands: Mutex::new(commands),
            thread: Mutex::new(Some(thread)),
            next_id: AtomicU64::new(1),
            ended,
        };
        match started_rx.recv_timeout(CONNECT_TIMEOUT) {
            Ok(Ok(())) => Ok(connection),
            Ok(Err(error)) => Err(DeviceError::PipeWire(error)),
            Err(RecvTimeoutError::Timeout) => Err(DeviceError::PipeWire(format!(
                "the server didn't answer within {CONNECT_TIMEOUT:?}"
            ))),
            Err(RecvTimeoutError::Disconnected) => Err(DeviceError::PipeWire("the loop thread stopped".to_owned())),
        }
    }

    /// Whether the loop thread has stopped taking commands.
    fn ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }

    /// Hands `command` to the loop thread; [`DeviceError::Closed`] once it has ended (the command would stay queued
    /// with nobody to take it).
    fn send(&self, command: streams::Command) -> Result<(), DeviceError> {
        if self.ended() {
            return Err(DeviceError::Closed);
        }
        let sender = self.commands.lock().unwrap_or_else(PoisonError::into_inner);
        sender.send(command).map_err(|_| DeviceError::Closed)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.send(streams::Command::Quit);
        let thread = self.thread.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

/// Closes its stream when the device is dropped, and keeps the connection up while the device lives.
struct StreamGuard {
    id: u64,
    connection: Arc<Connection>,
}

impl StreamGuard {
    fn new(id: u64, connection: &Arc<Connection>) -> Self {
        Self {
            id,
            connection: Arc::clone(connection),
        }
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        let _ = self.connection.send(streams::Command::Close(self.id));
    }
}

struct Microphone {
    frames: Queue<AudioFrame>,
    _stream: StreamGuard,
}

impl AudioSource for Microphone {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<AudioFrame>, DeviceError> {
        self.frames.recv(timeout)
    }
}

struct Camera {
    frames: Queue<VideoFrame>,
    _stream: StreamGuard,
}

impl VideoSource for Camera {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, DeviceError> {
        self.frames.recv(timeout)
    }
}

struct Speaker {
    queue: Queue<f32>,
    _stream: StreamGuard,
}

impl AudioSink for Speaker {
    /// Queues the frame's samples (the oldest dropped beyond [`SPEAKER_QUEUE`]); [`DeviceError::Closed`] once the
    /// playback stream ended or the server went away.
    fn play(&mut self, frame: &AudioFrame) -> Result<(), DeviceError> {
        self.queue.extend(frame.samples().iter().copied()).map(drop)
    }
}
