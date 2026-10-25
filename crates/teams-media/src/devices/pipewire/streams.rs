//! The loop thread: PipeWire's main loop over the one connection, and the streams the devices asked for.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Cursor;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;

use pipewire::context::ContextRc;
use pipewire::core::{CoreRc, PW_ID_CORE};
use pipewire::main_loop::MainLoopRc;
use pipewire::properties::{PropertiesBox, properties};
use pipewire::spa;
use pipewire::spa::param::ParamType;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pipewire::spa::param::format_utils;
use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{self, Pod, Value};
use pipewire::spa::utils::{Direction, Fraction, Rectangle, SpaTypes};
use pipewire::stream::{StreamFlags, StreamListener, StreamRc, StreamState};

use super::queue::{Closing, Queue};
use crate::devices::{AudioFrame, FRAME_SAMPLES, PixelFormat, SAMPLE_RATE, VideoFrame};

/// The device side's way into the loop thread.
pub(super) enum Command {
    Microphone {
        id: u64,
        frames: Queue<AudioFrame>,
        ready: mpsc::Sender<Result<(), String>>,
    },
    Speaker {
        id: u64,
        queue: Queue<f32>,
        ready: mpsc::Sender<Result<(), String>>,
    },
    Camera {
        id: u64,
        frames: Queue<VideoFrame>,
        ready: mpsc::Sender<Result<(), String>>,
    },
    Close(u64),
    Quit,
}

/// The raw formats a camera may hand over, the first preferred.
const CAMERA_FORMATS: [(VideoFormat, PixelFormat); 9] = [
    (VideoFormat::I420, PixelFormat::I420),
    (VideoFormat::NV12, PixelFormat::Nv12),
    (VideoFormat::YUY2, PixelFormat::Yuy2),
    (VideoFormat::RGBx, PixelFormat::Rgbx),
    (VideoFormat::BGRx, PixelFormat::Bgrx),
    (VideoFormat::RGBA, PixelFormat::Rgba),
    (VideoFormat::BGRA, PixelFormat::Bgra),
    (VideoFormat::RGB, PixelFormat::Rgb),
    (VideoFormat::BGR, PixelFormat::Bgr),
];

/// A stream and its listener; the listener goes first (it unhooks itself from the stream).
struct Open<D: 'static> {
    _listener: StreamListener<D>,
    _stream: StreamRc,
}

type Streams = Rc<RefCell<HashMap<u64, Box<dyn Any>>>>;

/// Runs the loop until [`Command::Quit`] or the server's fatal error; reports the first round trip (or why there was
/// none) on `started`.
pub(super) fn run(
    fd: OwnedFd,
    client_conf: Option<&Path>,
    commands: pipewire::channel::Receiver<Command>,
    started: &mpsc::Sender<Result<(), String>>,
) {
    if let Err(error) = serve(fd, client_conf, commands, started) {
        let _ = started.send(Err(error));
    }
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn serve(
    fd: OwnedFd,
    client_conf: Option<&Path>,
    commands: pipewire::channel::Receiver<Command>,
    started: &mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    pipewire::init();
    let main_loop = MainLoopRc::new(None).map_err(text)?;
    let mut context_props = PropertiesBox::new();
    if let Some(conf) = client_conf {
        let conf = conf.to_str().ok_or("the client config's path isn't UTF-8")?;
        context_props.insert(*pipewire::keys::CONFIG_NAME, conf);
    }
    let context = ContextRc::new(&main_loop, Some(context_props)).map_err(text)?;
    let core = context.connect_fd_rc(fd, None).map_err(text)?;
    let pending = core.sync(0).map_err(text)?;
    let started = started.clone();
    let quit_loop = main_loop.downgrade();
    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == PW_ID_CORE && seq == pending {
                let _ = started.send(Ok(()));
            }
        })
        .error(move |id, _seq, _res, _message| {
            if id == PW_ID_CORE
                && let Some(main_loop) = quit_loop.upgrade()
            {
                main_loop.quit();
            }
        })
        .register();
    let streams: Streams = Rc::default();
    let handler = {
        let core = core.clone();
        let streams = Rc::clone(&streams);
        let main_loop = main_loop.downgrade();
        move |command| handle(command, &core, &streams, &main_loop)
    };
    let _receiver = commands.attach(main_loop.loop_(), handler);
    main_loop.run();
    streams.borrow_mut().clear();
    Ok(())
}

fn handle(command: Command, core: &CoreRc, streams: &Streams, main_loop: &pipewire::main_loop::MainLoopWeak) {
    let (id, opened, ready) = match command {
        Command::Microphone { id, frames, ready } => (id, microphone(core, frames), ready),
        Command::Speaker { id, queue, ready } => (id, speaker(core, queue), ready),
        Command::Camera { id, frames, ready } => (id, camera(core, frames), ready),
        Command::Close(id) => {
            streams.borrow_mut().remove(&id);
            return;
        }
        Command::Quit => {
            if let Some(main_loop) = main_loop.upgrade() {
                main_loop.quit();
            }
            return;
        }
    };
    let reply = opened.map(|open| {
        streams.borrow_mut().insert(id, open);
    });
    let _ = ready.send(reply);
}

fn serialize(object: pod::Object) -> Result<Vec<u8>, String> {
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|error| format!("{error:?}"))
}

/// The one audio format every device stream takes: F32 mono at 48 kHz (PipeWire converts).
fn audio_format() -> Result<Vec<u8>, String> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(1);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    info.set_position(position);
    serialize(pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    })
}

fn connect(stream: &StreamRc, direction: Direction, format: &[u8]) -> Result<(), String> {
    let pod = Pod::from_bytes(format).ok_or("a format PipeWire can't read")?;
    stream
        .connect(
            direction,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut [pod],
        )
        .map_err(text)
}

fn new_stream(core: &CoreRc, name: &str, props: PropertiesBox) -> Result<StreamRc, String> {
    StreamRc::new(core.clone(), name, props).map_err(text)
}

/// Whether a stream in state `new` is gone for good (it erred, or was unlinked or disconnected): its device's queue
/// is closed then.
fn ended(new: &StreamState) -> bool {
    matches!(new, StreamState::Error(_) | StreamState::Unconnected)
}

struct MicrophoneState {
    pending: Vec<f32>,
    frames: Closing<AudioFrame>,
}

fn microphone(core: &CoreRc, frames: Queue<AudioFrame>) -> Result<Box<dyn Any>, String> {
    let stream = new_stream(
        core,
        "teams-microphone",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Audio",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Communication",
        },
    )?;
    let state = MicrophoneState {
        pending: Vec::with_capacity(4 * FRAME_SAMPLES),
        frames: Closing::new(frames),
    };
    let listener = stream
        .add_local_listener_with_user_data(state)
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            if let Some(data) = buffer.datas_mut().first_mut() {
                let (offset, size) = (to_usize(data.chunk().offset()), to_usize(data.chunk().size()));
                let bytes = data.data().unwrap_or_default();
                let end = offset.saturating_add(size).min(bytes.len());
                let (samples, _) = bytes.get(offset..end).unwrap_or_default().as_chunks::<4>();
                state
                    .pending
                    .extend(samples.iter().map(|sample| f32::from_le_bytes(*sample)));
            }
            while state.pending.len() >= FRAME_SAMPLES {
                if let Some(frame) = AudioFrame::from_samples(&state.pending[..FRAME_SAMPLES]) {
                    state.frames.queue().push(frame);
                }
                state.pending.drain(..FRAME_SAMPLES);
            }
        })
        .state_changed(|_, state, _, new| {
            if ended(&new) {
                state.frames.queue().close();
            }
        })
        .register()
        .map_err(text)?;
    connect(&stream, Direction::Input, &audio_format()?)?;
    Ok(Box::new(Open {
        _listener: listener,
        _stream: stream,
    }))
}

fn speaker(core: &CoreRc, queue: Queue<f32>) -> Result<Box<dyn Any>, String> {
    let stream = new_stream(
        core,
        "teams-speaker",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Audio",
            *pipewire::keys::MEDIA_CATEGORY => "Playback",
            *pipewire::keys::MEDIA_ROLE => "Communication",
        },
    )?;
    let listener = stream
        .add_local_listener_with_user_data(Closing::new(queue))
        .process(|stream, queue| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested = to_usize_u64(buffer.requested());
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let written = data.data().map_or(0, |bytes| fill(bytes, requested, queue.queue()));
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = 4;
            *chunk.size_mut() = u32::try_from(written * 4).unwrap_or(0);
        })
        .state_changed(|_, queue, _, new| {
            if ended(&new) {
                queue.queue().close();
            }
        })
        .register()
        .map_err(text)?;
    connect(&stream, Direction::Output, &audio_format()?)?;
    Ok(Box::new(Open {
        _listener: listener,
        _stream: stream,
    }))
}

/// Fills a playback buffer with up to `requested` queued samples (all it holds when `requested` is 0), silence
/// where the queue runs out; returns how many samples it wrote.
fn fill(bytes: &mut [u8], requested: usize, queue: &Queue<f32>) -> usize {
    let capacity = bytes.len() / 4;
    let count = if requested == 0 {
        capacity
    } else {
        requested.min(capacity)
    };
    queue.with_items(|queue| {
        for slot in bytes.as_chunks_mut::<4>().0.iter_mut().take(count) {
            slot.copy_from_slice(&queue.pop_front().unwrap_or(0.0).to_le_bytes());
        }
    });
    count
}

struct CameraState {
    format: Option<(PixelFormat, u32, u32)>,
    frames: Closing<VideoFrame>,
}

fn camera(core: &CoreRc, frames: Queue<VideoFrame>) -> Result<Box<dyn Any>, String> {
    let stream = new_stream(
        core,
        "teams-camera",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Video",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Camera",
        },
    )?;
    let state = CameraState {
        format: None,
        frames: Closing::new(frames),
    };
    let listener = stream
        .add_local_listener_with_user_data(state)
        .param_changed(|_, state, id, param| {
            if id == ParamType::Format.as_raw() {
                state.format = param.and_then(video_format);
            }
        })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some((format, width, height)) = state.format else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let chunk = data.chunk();
            let (offset, size) = (to_usize(chunk.offset()), to_usize(chunk.size()));
            let stride = u32::try_from(chunk.stride()).unwrap_or(0);
            let Some(bytes) = data.data() else {
                return;
            };
            let end = offset.saturating_add(size).min(bytes.len());
            let Some(bytes) = bytes.get(offset..end).filter(|bytes| !bytes.is_empty()) else {
                return;
            };
            let frame = VideoFrame {
                width,
                height,
                format,
                stride: if stride == 0 { width } else { stride },
                data: bytes.to_vec(),
            };
            state.frames.queue().push(frame);
        })
        .state_changed(|_, state, _, new| {
            if ended(&new) {
                state.frames.queue().close();
            }
        })
        .register()
        .map_err(text)?;
    connect(&stream, Direction::Input, &camera_formats()?)?;
    Ok(Box::new(Open {
        _listener: listener,
        _stream: stream,
    }))
}

/// A negotiated video format we take, as its pixel format and size.
fn video_format(param: &Pod) -> Option<(PixelFormat, u32, u32)> {
    let (media_type, media_subtype) = format_utils::parse_format(param).ok()?;
    if media_type != MediaType::Video || media_subtype != MediaSubtype::Raw {
        return None;
    }
    let mut info = VideoInfoRaw::new();
    info.parse(param).ok()?;
    let format = CAMERA_FORMATS
        .iter()
        .find(|(video, _)| *video == info.format())
        .map(|(_, pixel)| *pixel)?;
    Some((format, info.size().width, info.size().height))
}

/// The camera's offer: any of [`CAMERA_FORMATS`], 640x360 at 15 frames a second preferred, anything up to 4K.
fn camera_formats() -> Result<Vec<u8>, String> {
    let object = pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::I420,
            VideoFormat::I420,
            VideoFormat::NV12,
            VideoFormat::YUY2,
            VideoFormat::RGBx,
            VideoFormat::BGRx,
            VideoFormat::RGBA,
            VideoFormat::BGRA,
            VideoFormat::RGB,
            VideoFormat::BGR
        ),
        pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            Rectangle {
                width: 640,
                height: 360
            },
            Rectangle { width: 1, height: 1 },
            Rectangle {
                width: 4096,
                height: 4096
            }
        ),
        pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: 15, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction { num: 120, denom: 1 }
        ),
    );
    serialize(object)
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn to_usize_u64(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
