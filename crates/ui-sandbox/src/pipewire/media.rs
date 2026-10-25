//! The sandbox's camera and the speaker's recording: two streams on a thread of the process that owns the sandbox.
//!
//! - **The camera** (`sandbox-camera`, a video source): `teams-media`'s test pattern ([`TestPattern`]), I420 at the
//!   size and rate asked, a frame on each tick of its own timer (it drives its graph, as a camera does), each frame
//!   carrying its index.
//! - **The recording**: a capture of the speaker's monitor, written to a WAV file whose header is brought up to date
//!   every second.
//!
//! The thread connects to the server's socket itself and hands libpipewire the descriptor, with the sandbox's own
//! client config named outright, so nothing of this process's session (its runtime dir, its config dirs) is read.
//! The thread runs in this process, with its environment: libpipewire reads `PIPEWIRE_CONFIG_NAME` and
//! `PIPEWIRE_CONFIG_PREFIX` ahead of the config a context names, so a process that sets either is refused
//! ([`check_environment`]) before anything starts.

use std::cell::Cell;
use std::ffi::OsString;
use std::io::Cursor;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use pipewire::context::ContextRc;
use pipewire::core::CoreRc;
use pipewire::main_loop::MainLoopRc;
use pipewire::properties::properties;
use pipewire::spa;
use pipewire::spa::param::ParamType;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pipewire::spa::param::video::VideoFormat;
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{self, Pod, Property, Value};
use pipewire::spa::utils::{Direction, Fraction, Rectangle, SpaTypes};
use pipewire::stream::{StreamFlags, StreamListener, StreamRc, StreamState};
use teams_media::devices::file::PatternCamera;
use teams_media::devices::pattern::TestPattern;
use teams_media::devices::pipewire::CONFIG_OVERRIDES;
use teams_media::devices::wav::WavWriter;

use super::{CAMERA_NODE, SPEAKER_NODE};
use crate::{SOCKET_WAIT, SandboxResult};

/// The camera and the recording, stopped (and the recording's header brought up to date) on drop.
pub struct MediaHelper {
    quit: pipewire::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
    frames: Arc<AtomicU64>,
}

impl Drop for MediaHelper {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl MediaHelper {
    /// Connects to `socket` with the client config `client_conf`, shows the camera and records the speaker to
    /// `recording`; returns once the session manager has linked the recording.
    ///
    /// # Errors
    /// This process sets a config override ([`check_environment`]), the socket can't be reached, a stream can't be
    /// made, or the recording isn't linked within [`SOCKET_WAIT`].
    pub fn start(socket: &Path, client_conf: &Path, recording: &Path, camera: PatternCamera) -> SandboxResult<Self> {
        check_environment()?;
        let stream = UnixStream::connect(socket)?;
        let (quit, quit_rx) = pipewire::channel::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let frames = Arc::new(AtomicU64::new(0));
        let setup = Setup {
            stream,
            client_conf: client_conf.to_owned(),
            recording: recording.to_owned(),
            camera,
            frames: Arc::clone(&frames),
        };
        let thread = thread::Builder::new()
            .name("ui-sandbox-media".to_owned())
            .spawn(move || {
                if let Err(error) = setup.run(quit_rx, &ready_tx) {
                    let _ = ready_tx.send(Err(error));
                }
            })?;
        let helper = Self {
            quit,
            thread: Some(thread),
            frames,
        };
        match ready_rx.recv_timeout(SOCKET_WAIT) {
            Ok(Ok(())) => Ok(helper),
            Ok(Err(error)) => Err(format!("the sandbox's camera and recording: {error}").into()),
            Err(_) => Err("the session manager didn't link the speaker's recording in time".into()),
        }
    }

    /// Frames the camera has shown so far.
    #[must_use]
    pub fn frames_shown(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
}

/// Refuses this process when its environment sets one of libpipewire's config overrides ([`CONFIG_OVERRIDES`]): the
/// helper's thread would load that config (with its modules and D-Bus support) instead of the sandbox's.
///
/// # Errors
/// The override set, by name.
pub fn check_environment() -> SandboxResult {
    config_override(|name| std::env::var_os(name)).map_or(Ok(()), |name| {
        Err(format!(
            "{name} is set in this process: the sandbox's PipeWire clients in it would load that config instead of the \
             sandbox's; unset it"
        )
        .into())
    })
}

/// The first of [`CONFIG_OVERRIDES`] `var` has at all (even empty: libpipewire reads it as it is).
fn config_override(var: impl Fn(&str) -> Option<OsString>) -> Option<&'static str> {
    CONFIG_OVERRIDES.into_iter().find(|name| var(name).is_some())
}

struct Setup {
    stream: UnixStream,
    client_conf: PathBuf,
    recording: PathBuf,
    camera: PatternCamera,
    frames: Arc<AtomicU64>,
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

impl Setup {
    fn run(
        self,
        quit: pipewire::channel::Receiver<()>,
        ready: &mpsc::Sender<Result<(), String>>,
    ) -> Result<(), String> {
        pipewire::init();
        let main_loop = MainLoopRc::new(None).map_err(text)?;
        let conf = self
            .client_conf
            .to_str()
            .ok_or("the client config's path isn't UTF-8")?;
        let context =
            ContextRc::new(&main_loop, Some(properties! { *pipewire::keys::CONFIG_NAME => conf })).map_err(text)?;
        let core = context.connect_fd_rc(self.stream.into(), None).map_err(text)?;
        let _recorder = recorder(&core, &self.recording, ready.clone())?;
        let camera = Camera::show(&core, self.camera, self.frames)?;
        let ticks = camera.stream.clone();
        let timer = main_loop.loop_().add_timer(move |_| {
            let _ = ticks.trigger_process();
        });
        let period = Duration::from_secs(1) / self.camera.fps.max(1);
        timer
            .update_timer(Some(period), Some(period))
            .into_sync_result()
            .map_err(text)?;
        let stop = main_loop.downgrade();
        let _quit = quit.attach(main_loop.loop_(), move |()| {
            if let Some(main_loop) = stop.upgrade() {
                main_loop.quit();
            }
        });
        main_loop.run();
        Ok(())
    }
}

fn serialize(object: pod::Object) -> Result<Vec<u8>, String> {
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|error| format!("{error:?}"))
}

fn as_pod(bytes: &[u8]) -> Result<&Pod, String> {
    Pod::from_bytes(bytes).ok_or_else(|| "a parameter PipeWire can't read".to_owned())
}

/// A stream and its listener; the listener goes first (it unhooks itself from the stream).
struct Open<D: 'static> {
    _listener: StreamListener<D>,
    _stream: StreamRc,
}

struct Recording {
    writer: Option<WavWriter>,
    ready: Option<mpsc::Sender<Result<(), String>>>,
}

fn recorder(core: &CoreRc, path: &Path, ready: mpsc::Sender<Result<(), String>>) -> Result<Open<Recording>, String> {
    let props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CATEGORY => "Capture",
        *pipewire::keys::MEDIA_ROLE => "Production",
        *pipewire::keys::NODE_NAME => "sandbox-recorder",
        *pipewire::keys::TARGET_OBJECT => SPEAKER_NODE,
        *pipewire::keys::STREAM_CAPTURE_SINK => "true",
    };
    let stream = StreamRc::new(core.clone(), "sandbox-recorder", props).map_err(text)?;
    let state = Recording {
        writer: Some(WavWriter::create(path).map_err(text)?),
        ready: Some(ready),
    };
    let listener = stream
        .add_local_listener_with_user_data(state)
        .state_changed(|_, state, _, new| {
            if matches!(new, StreamState::Streaming)
                && let Some(ready) = state.ready.take()
            {
                let _ = ready.send(Ok(()));
            }
        })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let (offset, size) = (to_usize(data.chunk().offset()), to_usize(data.chunk().size()));
            let bytes = data.data().unwrap_or_default();
            let end = offset.saturating_add(size).min(bytes.len());
            let (samples, _) = bytes.get(offset..end).unwrap_or_default().as_chunks::<4>();
            let samples: Vec<f32> = samples.iter().map(|sample| f32::from_le_bytes(*sample)).collect();
            if let Some(writer) = &mut state.writer
                && writer.write(&samples).is_err()
            {
                state.writer = None;
            }
        })
        .register()
        .map_err(text)?;
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(teams_media::devices::SAMPLE_RATE);
    info.set_channels(1);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    info.set_position(position);
    let format = serialize(pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    })?;
    stream
        .connect(
            Direction::Input,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut [as_pod(&format)?],
        )
        .map_err(text)?;
    Ok(Open {
        _listener: listener,
        _stream: stream,
    })
}

struct Camera {
    stream: StreamRc,
    _open: Open<Showing>,
}

struct Showing {
    pattern: TestPattern,
    index: Rc<Cell<u32>>,
    frames: Arc<AtomicU64>,
}

impl Camera {
    fn show(core: &CoreRc, camera: PatternCamera, frames: Arc<AtomicU64>) -> Result<Self, String> {
        let pattern = TestPattern::new(camera.width, camera.height);
        let props = properties! {
            *pipewire::keys::MEDIA_TYPE => "Video",
            *pipewire::keys::MEDIA_CATEGORY => "Source",
            *pipewire::keys::MEDIA_ROLE => "Camera",
            *pipewire::keys::MEDIA_CLASS => "Video/Source",
            *pipewire::keys::NODE_NAME => CAMERA_NODE,
            *pipewire::keys::NODE_DESCRIPTION => "Sandbox camera",
            *pipewire::keys::PRIORITY_SESSION => "2000",
        };
        let stream = StreamRc::new(core.clone(), CAMERA_NODE, props).map_err(text)?;
        let showing = Showing {
            pattern,
            index: Rc::default(),
            frames,
        };
        let listener = stream
            .add_local_listener_with_user_data(showing)
            .param_changed(|stream, showing, id, param| {
                if id == ParamType::Format.as_raw() && param.is_some() {
                    let _ = buffers(showing.pattern)
                        .and_then(|params| stream.update_params(&mut [as_pod(&params)?]).map_err(text));
                }
            })
            .process(show_frame)
            .register()
            .map_err(text)?;
        let format = camera_format(pattern, camera.fps)?;
        stream
            .connect(
                Direction::Output,
                None,
                StreamFlags::DRIVER | StreamFlags::MAP_BUFFERS,
                &mut [as_pod(&format)?],
            )
            .map_err(text)?;
        Ok(Self {
            stream: stream.clone(),
            _open: Open {
                _listener: listener,
                _stream: stream,
            },
        })
    }
}

/// Writes the next frame of the pattern into the stream's next buffer.
fn show_frame(stream: &pipewire::stream::Stream, showing: &mut Showing) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let frame = showing.pattern.frame(showing.index.get());
    let Some(bytes) = data.data() else {
        return;
    };
    let size = frame.data.len().min(bytes.len());
    bytes[..size].copy_from_slice(&frame.data[..size]);
    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = i32::try_from(frame.stride).unwrap_or(0);
    *chunk.size_mut() = u32::try_from(size).unwrap_or(0);
    showing.index.set(showing.index.get().wrapping_add(1));
    showing.frames.fetch_add(1, Ordering::Relaxed);
}

/// The camera's one format: I420 at the pattern's size and `fps`.
fn camera_format(pattern: TestPattern, fps: u32) -> Result<Vec<u8>, String> {
    serialize(pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pod::property!(FormatProperties::VideoFormat, Id, VideoFormat::I420),
        pod::property!(
            FormatProperties::VideoSize,
            Rectangle,
            Rectangle {
                width: pattern.width,
                height: pattern.height
            }
        ),
        pod::property!(
            FormatProperties::VideoFramerate,
            Fraction,
            Fraction {
                num: fps.max(1),
                denom: 1
            }
        ),
    ))
}

/// The buffers the camera's frames need: one block of a whole I420 frame each.
fn buffers(pattern: TestPattern) -> Result<Vec<u8>, String> {
    let size = pattern.width * pattern.height * 3 / 2;
    let int = |value: u32| Value::Int(i32::try_from(value).unwrap_or(i32::MAX));
    serialize(pod::Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![
            Property::new(spa::sys::SPA_PARAM_BUFFERS_buffers, int(4)),
            Property::new(spa::sys::SPA_PARAM_BUFFERS_blocks, int(1)),
            Property::new(spa::sys::SPA_PARAM_BUFFERS_size, int(size)),
            Property::new(spa::sys::SPA_PARAM_BUFFERS_stride, int(pattern.width)),
        ],
    })
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_config_override_is_named_even_empty() {
        assert_eq!(config_override(|_| None), None);
        for name in CONFIG_OVERRIDES {
            for value in ["client.conf", ""] {
                let found = config_override(|asked| (asked == name).then(|| value.into()));
                assert_eq!(found, Some(name), "{name}={value:?}");
            }
        }
        assert!(
            CONFIG_OVERRIDES.contains(&"PIPEWIRE_CONFIG_NAME") && CONFIG_OVERRIDES.contains(&"PIPEWIRE_CONFIG_PREFIX")
        );
    }
}
