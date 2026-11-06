//! The device streams the loop thread makes: the microphone, the speaker and the camera, each linked by the session
//! manager to the device chosen for it ([`Target`]: `target.object`) or, with none chosen, to the system's default,
//! which it then follows; and a shared screen's, connected to the node the screen-cast portal named ([`screen`]).
//!
//! A chosen device is pinned with `node.dont-move`: WirePlumber takes a stream whose `target.object` names the
//! current default for one following the default (the rule `PulseAudio` has: it writes `target.node = -1` into the
//! `default` metadata for it), so a device chosen while it is the default would be left for every later default.
//! With `node.dont-move` WirePlumber reads the stream's target only from its own properties, never from that
//! metadata, and still falls back to the default when the chosen node goes, so the call is never left without a
//! device. `node.dont-reconnect` would pin it too, but a stream with it is destroyed when its node goes.
//!
//! A stream hands its media to its device through a [`Feed`], gated by its [`Gate`]: a **live** stream feeds the
//! device and closes its queue when it ends; a stream made to replace it (the device was moved to another node) is
//! **pending** until the session manager has linked it, and feeds nothing (a pending speaker plays silence) while the
//! live one still carries the media; once the loop swaps them, the old one is **retired**: it feeds nothing and
//! closes nothing on its way out. So a device moved to another node never says it closed, and the media doesn't stop
//! while the new link is made.

use std::any::Any;
use std::cell::Cell;
use std::io::Cursor;
use std::rc::Rc;

use pipewire::core::CoreRc;
use pipewire::properties::{PropertiesBox, properties};
use pipewire::spa;
use pipewire::spa::param::ParamType;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pipewire::spa::param::format_utils;
use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{self, ChoiceValue, Pod, Value};
use pipewire::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Rectangle, SpaTypes};
use pipewire::stream::{StreamFlags, StreamListener, StreamRc, StreamState};

use crate::devices::{AudioFrame, FRAME_SAMPLES, PixelFormat, SAMPLE_RATE, VideoFrame};
use crate::queue::Queue;

/// The node a stream asks to be linked to; `None` follows the system's default.
pub(super) type Target<'a> = Option<&'a str>;

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

/// A stream's part in its device (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Pending,
    Live,
    Retired,
}

/// A stream's role, and what its state has been, shared by the loop and the stream's callbacks.
#[derive(Debug)]
pub(super) struct Gate {
    role: Cell<Role>,
    streaming: Cell<bool>,
    ended: Cell<bool>,
}

impl Gate {
    fn new(role: Role) -> Rc<Self> {
        Rc::new(Self {
            role: Cell::new(role),
            streaming: Cell::new(false),
            ended: Cell::new(false),
        })
    }

    /// A stream that feeds its device from the start (a device opened).
    pub(super) fn live() -> Rc<Self> {
        Self::new(Role::Live)
    }

    /// A stream made to replace a live one, feeding nothing until [`Gate::go_live`].
    pub(super) fn pending() -> Rc<Self> {
        Self::new(Role::Pending)
    }

    pub(super) fn go_live(&self) {
        self.role.set(Role::Live);
    }

    /// The stream is being replaced: it feeds and closes nothing from now on.
    pub(super) fn retire(&self) {
        self.role.set(Role::Retired);
    }

    fn is_live(&self) -> bool {
        self.role.get() == Role::Live
    }

    /// The session manager linked the stream and media flows (it may have stopped since).
    pub(super) fn has_streamed(&self) -> bool {
        self.streaming.get()
    }

    /// The stream erred, or was unlinked or disconnected for good.
    pub(super) fn has_ended(&self) -> bool {
        self.ended.get()
    }
}

/// A stream's end of its device's queue: fed only while the stream is live, closed when a live stream ends or goes.
pub(super) struct Feed<T> {
    queue: Queue<T>,
    gate: Rc<Gate>,
}

impl<T> Feed<T> {
    pub(super) fn new(queue: Queue<T>, gate: Rc<Gate>) -> Self {
        Self { queue, gate }
    }

    /// The queue, while the stream is live.
    fn live(&self) -> Option<&Queue<T>> {
        self.gate.is_live().then_some(&self.queue)
    }

    /// Takes the stream's new state: it streamed, or it ended (closing a live stream's queue).
    fn state(&self, new: &StreamState) {
        match new {
            StreamState::Streaming => self.gate.streaming.set(true),
            StreamState::Error(_) | StreamState::Unconnected => {
                self.gate.ended.set(true);
                if self.gate.is_live() {
                    self.queue.close();
                }
            }
            StreamState::Connecting | StreamState::Paused => {}
        }
    }
}

impl<T> Drop for Feed<T> {
    fn drop(&mut self) {
        if self.gate.is_live() {
            self.queue.close();
        }
    }
}

/// A stream and its listener; the listener goes first (it unhooks itself from the stream).
struct Open<D: 'static> {
    _listener: StreamListener<D>,
    _stream: StreamRc,
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
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
    connect_to(stream, direction, None, format)
}

/// [`connect`], to the node `node` when one is named (a shared screen's).
fn connect_to(stream: &StreamRc, direction: Direction, node: Option<u32>, format: &[u8]) -> Result<(), String> {
    let pod = Pod::from_bytes(format).ok_or("a format PipeWire can't read")?;
    stream
        .connect(
            direction,
            node,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut [pod],
        )
        .map_err(text)
}

/// Keeps a stream on the target its own properties name (see the module docs); the `pipewire` crate has no constant
/// for it.
const NODE_DONT_MOVE: &str = "node.dont-move";

/// A stream named `name` with `props`, asking for `target` when one is chosen, and pinned to it.
fn new_stream(core: &CoreRc, name: &str, mut props: PropertiesBox, target: Target<'_>) -> Result<StreamRc, String> {
    if let Some(target) = target {
        props.insert(*pipewire::keys::TARGET_OBJECT, target);
        props.insert(NODE_DONT_MOVE, "true");
    }
    StreamRc::new(core.clone(), name, props).map_err(text)
}

struct MicrophoneState {
    pending: Vec<f32>,
    frames: Feed<AudioFrame>,
}

pub(super) fn microphone(
    core: &CoreRc,
    frames: Queue<AudioFrame>,
    target: Target<'_>,
    gate: Rc<Gate>,
) -> Result<Box<dyn Any>, String> {
    let stream = new_stream(
        core,
        "teams-microphone",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Audio",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Communication",
        },
        target,
    )?;
    let state = MicrophoneState {
        pending: Vec::with_capacity(4 * FRAME_SAMPLES),
        frames: Feed::new(frames, gate),
    };
    let listener = stream
        .add_local_listener_with_user_data(state)
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(queue) = state.frames.live() else {
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
                    queue.push(frame);
                }
                state.pending.drain(..FRAME_SAMPLES);
            }
        })
        .state_changed(|_, state, _, new| state.frames.state(&new))
        .register()
        .map_err(text)?;
    connect(&stream, Direction::Input, &audio_format()?)?;
    Ok(Box::new(Open {
        _listener: listener,
        _stream: stream,
    }))
}

pub(super) fn speaker(
    core: &CoreRc,
    queue: Queue<f32>,
    target: Target<'_>,
    gate: Rc<Gate>,
) -> Result<Box<dyn Any>, String> {
    let stream = new_stream(
        core,
        "teams-speaker",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Audio",
            *pipewire::keys::MEDIA_CATEGORY => "Playback",
            *pipewire::keys::MEDIA_ROLE => "Communication",
        },
        target,
    )?;
    let listener = stream
        .add_local_listener_with_user_data(Feed::new(queue, gate))
        .process(|stream, feed| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested = to_usize_u64(buffer.requested());
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let written = data.data().map_or(0, |bytes| fill(bytes, requested, feed.live()));
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = 4;
            *chunk.size_mut() = u32::try_from(written * 4).unwrap_or(0);
        })
        .state_changed(|_, feed, _, new| feed.state(&new))
        .register()
        .map_err(text)?;
    connect(&stream, Direction::Output, &audio_format()?)?;
    Ok(Box::new(Open {
        _listener: listener,
        _stream: stream,
    }))
}

/// Fills a playback buffer with up to `requested` queued samples (all it holds when `requested` is 0), silence
/// where the queue runs out or when there is none to play from (a stream not live); returns how many samples it wrote.
fn fill(bytes: &mut [u8], requested: usize, queue: Option<&Queue<f32>>) -> usize {
    let capacity = bytes.len() / 4;
    let count = if requested == 0 {
        capacity
    } else {
        requested.min(capacity)
    };
    let slots = bytes.as_chunks_mut::<4>().0.iter_mut().take(count);
    match queue {
        Some(queue) => queue.with_items(|queue| {
            for slot in slots {
                slot.copy_from_slice(&queue.pop_front().unwrap_or(0.0).to_le_bytes());
            }
        }),
        None => {
            for slot in slots {
                slot.copy_from_slice(&0.0_f32.to_le_bytes());
            }
        }
    }
    count
}

struct CameraState {
    format: Option<(PixelFormat, u32, u32)>,
    frames: Feed<VideoFrame>,
    /// The buffers the stream takes ([`camera_buffers`]), answered to every format.
    buffers: Vec<u8>,
}

pub(super) fn camera(
    core: &CoreRc,
    frames: Queue<VideoFrame>,
    target: Target<'_>,
    gate: Rc<Gate>,
) -> Result<Box<dyn Any>, String> {
    let stream = new_stream(
        core,
        "teams-camera",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Video",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Camera",
        },
        target,
    )?;
    let formats = video_formats(Rectangle {
        width: 640,
        height: 360,
    })?;
    video_capture(stream, frames, gate, (None, &formats))
}

/// A shared screen's stream: the node `node` the screen-cast portal named, its picture taken at its own size (1080p
/// preferred) in the formats a camera's may come in.
pub(super) fn screen(
    core: &CoreRc,
    frames: Queue<VideoFrame>,
    node: u32,
    gate: Rc<Gate>,
) -> Result<Box<dyn Any>, String> {
    let stream = new_stream(
        core,
        "teams-screen",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Video",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Screen",
        },
        None,
    )?;
    let formats = video_formats(Rectangle {
        width: 1920,
        height: 1080,
    })?;
    video_capture(stream, frames, gate, (Some(node), &formats))
}

/// A video stream's listeners and its connection (to `node` when one is named, offering `formats`): each buffer's
/// frame queued on `frames` while it is live.
fn video_capture(
    stream: StreamRc,
    frames: Queue<VideoFrame>,
    gate: Rc<Gate>,
    (node, formats): (Option<u32>, &[u8]),
) -> Result<Box<dyn Any>, String> {
    let state = CameraState {
        format: None,
        frames: Feed::new(frames, gate),
        buffers: camera_buffers()?,
    };
    let listener = stream
        .add_local_listener_with_user_data(state)
        .param_changed(|stream, state, id, param| {
            if id == ParamType::Format.as_raw() {
                state.format = param.and_then(video_format);
                if state.format.is_some()
                    && let Some(buffers) = Pod::from_bytes(&state.buffers)
                {
                    // a refused update leaves the link without buffers: the stream errs and the camera reports closed
                    let _ = stream.update_params(&mut [buffers]);
                }
            }
        })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            if let (Some(queue), Some(format)) = (state.frames.live(), state.format)
                && let Some(frame) = buffer.datas_mut().first_mut().and_then(|data| frame_of(data, format))
            {
                queue.push(frame);
            }
        })
        .state_changed(|_, state, _, new| state.frames.state(&new))
        .register()
        .map_err(text)?;
    connect_to(&stream, Direction::Input, node, formats)?;
    Ok(Box::new(Open {
        _listener: listener,
        _stream: stream,
    }))
}

/// The frame one camera buffer's data holds, in the negotiated `format`; `None` for an empty one.
fn frame_of(data: &mut spa::buffer::Data, (format, width, height): (PixelFormat, u32, u32)) -> Option<VideoFrame> {
    let chunk = data.chunk();
    let (offset, size) = (to_usize(chunk.offset()), to_usize(chunk.size()));
    let stride = u32::try_from(chunk.stride()).unwrap_or(0);
    let bytes = data.data()?;
    let end = offset.saturating_add(size).min(bytes.len());
    let bytes = bytes.get(offset..end).filter(|bytes| !bytes.is_empty())?;
    Some(VideoFrame {
        width,
        height,
        format,
        stride: if stride == 0 { width } else { stride },
        data: bytes.to_vec(),
    })
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

/// The memory a camera's buffers may come in: a DMA-BUF or a memfd (shared with this process, mapped by the stream's
/// `MAP_BUFFERS`), or plain memory. Without it the link allows every type and a camera that allocates its own buffers
/// (a V4L2 node, `uvcvideo`) maps them as plain memory in the PipeWire daemon, which a client can't be handed: the
/// server refuses the buffers (`mix use buffers failed: -22`) and the stream errs before its first frame. With it,
/// such a camera exports its buffers as DMA-BUFs, as it does for the `pipewiresrc` element and libwebrtc's capturer.
fn camera_buffers() -> Result<Vec<u8>, String> {
    let types: u32 =
        (1 << spa::sys::SPA_DATA_DmaBuf) | (1 << spa::sys::SPA_DATA_MemFd) | (1 << spa::sys::SPA_DATA_MemPtr);
    let types = i32::try_from(types).map_err(text)?;
    serialize(pod::Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![pod::Property::new(
            spa::sys::SPA_PARAM_BUFFERS_dataType,
            Value::Choice(ChoiceValue::Int(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Flags {
                    default: types,
                    flags: Vec::new(),
                },
            ))),
        )],
    })
}

/// A video capture's offer: any of [`CAMERA_FORMATS`], `preferred`'s size at 15 frames a second preferred (a camera's
/// 640x360, a screen's 1080p), anything up to 4K.
fn video_formats(preferred: Rectangle) -> Result<Vec<u8>, String> {
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
            preferred,
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::devices::DeviceError;

    #[test]
    fn the_camera_takes_dma_bufs_and_memfds_so_a_v4l2_camera_s_buffers_reach_it() {
        let bytes = camera_buffers().expect("the buffers param");
        let (_, value) = pod::deserialize::PodDeserializer::deserialize_any_from(&bytes).expect("a pod");
        let Value::Object(object) = value else {
            panic!("not an object: {value:?}");
        };
        assert_eq!(object.type_, SpaTypes::ObjectParamBuffers.as_raw());
        assert_eq!(object.id, ParamType::Buffers.as_raw());
        let [property] = object.properties.as_slice() else {
            panic!("one property: {:?}", object.properties);
        };
        assert_eq!(property.key, spa::sys::SPA_PARAM_BUFFERS_dataType);
        let Value::Choice(ChoiceValue::Int(Choice(_, ChoiceEnum::Flags { default, .. }))) = property.value else {
            panic!("not a flags choice: {:?}", property.value);
        };
        for wanted in [
            spa::sys::SPA_DATA_DmaBuf,
            spa::sys::SPA_DATA_MemFd,
            spa::sys::SPA_DATA_MemPtr,
        ] {
            assert_ne!(default & (1 << wanted), 0, "data type {wanted}");
        }
    }

    #[test]
    fn a_live_feed_closes_its_queue_when_its_stream_ends_or_goes_and_a_retired_one_never_does() {
        let queue: Queue<u8> = Queue::new(4);
        let gate = Gate::live();
        let feed = Feed::new(queue.clone(), Rc::clone(&gate));
        assert!(feed.live().is_some());
        feed.state(&StreamState::Streaming);
        assert!(gate.has_streamed() && !gate.has_ended());
        drop(feed);
        assert!(matches!(queue.recv(Duration::ZERO), Err(DeviceError::Closed)));

        let queue: Queue<u8> = Queue::new(4);
        let gate = Gate::live();
        let feed = Feed::new(queue.clone(), Rc::clone(&gate));
        gate.retire();
        assert!(feed.live().is_none(), "a retired stream feeds nothing");
        feed.state(&StreamState::Unconnected);
        drop(feed);
        assert!(gate.has_ended());
        assert_eq!(queue.recv(Duration::ZERO).unwrap(), None, "still open");
    }

    #[test]
    fn a_pending_feed_feeds_nothing_until_it_goes_live() {
        let queue: Queue<u8> = Queue::new(4);
        let gate = Gate::pending();
        let feed = Feed::new(queue.clone(), Rc::clone(&gate));
        assert!(feed.live().is_none());
        feed.state(&StreamState::Error("gone".into()));
        assert_eq!(
            queue.recv(Duration::ZERO).unwrap(),
            None,
            "a pending stream closes nothing"
        );
        gate.go_live();
        assert!(feed.live().is_some());
    }

    #[test]
    fn a_speaker_that_isn_t_live_plays_silence_and_leaves_the_queue() {
        let queue = Queue::new(8);
        queue.extend([0.5_f32; 4]).unwrap();
        let mut bytes = [0xff_u8; 16];
        assert_eq!(fill(&mut bytes, 4, None), 4);
        assert!(bytes.iter().all(|byte| *byte == 0));
        assert_eq!(queue.with_items(|items| items.len()), 4);
        assert_eq!(fill(&mut bytes, 0, Some(&queue)), 4);
        assert_eq!(queue.with_items(|items| items.len()), 0);
        assert!((f32::from_le_bytes(bytes[..4].try_into().unwrap()) - 0.5).abs() < f32::EPSILON);
    }
}
