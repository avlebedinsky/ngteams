//! One pipeline per device, owned by its pump thread. Only this thread starts/stops pipelines. A choice or hotplug
//! replaces the pipeline before the next read/write; losing a named device falls back to the default without
//! forgetting the choice. A fatal pipeline error closes the queue, so consumers never wait forever.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use gstreamer_app::{AppSink, AppSrc};

use super::{Entry, Key, POLL, WindowsDevices, error};
use crate::devices::catalog::DeviceKind;
use crate::devices::{
    AudioFrame, AudioSink, AudioSource, DeviceError, FRAME_SAMPLES, PixelFormat, VideoFrame, VideoSource,
};
use crate::queue::Queue;
use gst::prelude::*;
use gstreamer as gst;

struct Running(gst::Pipeline);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

struct Pump {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn start(
    devices: WindowsDevices,
    kind: DeviceKind,
    mut make: impl FnMut(gst::Element) -> Result<Tick, DeviceError> + Send + 'static,
    close: impl FnOnce() + Send + 'static,
) -> Result<Pump, DeviceError> {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let (ready, response) = mpsc::sync_channel(1);
    let thread = thread::Builder::new()
        .name(format!("windows-{}", kind.key()))
        .spawn(move || {
            let result = pump(&devices, kind, &stopped, &ready, &mut make);
            if let Err(failure) = result {
                let _ = ready.try_send(Err(failure.to_string()));
                log::warn!("a Windows {} stream stopped: {failure}", kind.key());
            }
            close();
        })?;
    let guard = Pump {
        stop,
        thread: Some(thread),
    };
    match response.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => Ok(guard),
        Ok(Err(failure)) => Err(error(failure)),
        Err(failure) => Err(error(failure)),
    }
}

type Tick = Box<dyn FnMut() -> Result<(), DeviceError>>;
type Make = dyn FnMut(gst::Element) -> Result<Tick, DeviceError>;
fn pump(
    devices: &WindowsDevices,
    kind: DeviceKind,
    stop: &AtomicBool,
    ready: &mpsc::SyncSender<Result<(), String>>,
    make: &mut Make,
) -> Result<(), DeviceError> {
    let mut current = None;
    let mut tick: Option<Tick> = None;
    let mut announced = false;
    while !stop.load(Ordering::Acquire) {
        let entry = devices.shared.selected(kind);
        let name = entry.as_ref().map(Entry::key);
        if tick.is_none() || name != current {
            // Drop the old stream first: exclusive cameras cannot be opened twice. All queues survive the switch.
            drop(tick.take());
            let element = match entry {
                Some(entry) => entry.device.create_element(None).map_err(error)?,
                None => default_element(kind)?,
            };
            tick = Some(make(element)?);
            current = name;
        }
        if !announced {
            let _ = ready.send(Ok(()));
            announced = true;
        }
        if let Some(tick) = tick.as_mut()
            && let Err(failure) = tick()
        {
            // Device removal can error the old pipeline just before the monitor publishes its new default.
            // Give that event a bounded chance to arrive, then rebuild only if the effective device changed.
            if !changed_after_removal(devices, kind, current.as_ref(), stop) {
                return Err(failure);
            }
        }
    }
    Ok(())
}

fn changed_after_removal(devices: &WindowsDevices, kind: DeviceKind, current: Option<&Key>, stop: &AtomicBool) -> bool {
    for _ in 0..20 {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        let next = devices.shared.selected(kind);
        if next.as_ref().map(Entry::key).as_ref() != current {
            return true;
        }
        thread::sleep(POLL);
    }
    false
}

fn default_element(kind: DeviceKind) -> Result<gst::Element, DeviceError> {
    gst::ElementFactory::make(super::factory(kind)).build().map_err(error)
}

fn audio_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("rate", 48_000i32)
        .field("channels", 1i32)
        .build()
}

fn playing(elements: &[&gst::Element]) -> Result<Running, DeviceError> {
    let running = Running(gst::Pipeline::new());
    running.0.add_many(elements.iter().copied()).map_err(error)?;
    gst::Element::link_many(elements.iter().copied()).map_err(error)?;
    running.0.set_state(gst::State::Playing).map_err(error)?;
    let (state, _, _) = running.0.state(gst::ClockTime::from_seconds(5));
    state.map_err(error)?;
    Ok(running)
}

fn healthy(pipeline: &gst::Pipeline) -> Result<(), DeviceError> {
    if let Some(bus) = pipeline.bus() {
        for message in bus.iter() {
            match message.view() {
                gst::MessageView::Error(failure) => return Err(error(failure.error())),
                gst::MessageView::Eos(_) => return Err(DeviceError::Closed),
                // A sink that opens (or reopens) its device while playing reports its latency late. Without this,
                // the pipeline plays each buffer with no latency: every buffer comes late, its start cut.
                gst::MessageView::Latency(_) => pipeline.recalculate_latency().map_err(error)?,
                _ => (),
            }
        }
    }
    Ok(())
}

fn element(name: &str) -> Result<gst::Element, DeviceError> {
    gst::ElementFactory::make(name).build().map_err(error)
}

fn capture(source: &gst::Element, video: bool) -> Result<(Running, AppSink), DeviceError> {
    let sink = AppSink::builder().max_buffers(3u32).drop(true).sync(false).build();
    let convert = element(if video { "videoconvert" } else { "audioconvert" })?;
    let adjust = element(if video { "videoscale" } else { "audioresample" })?;
    let caps = if video {
        // BGRA has an exact width*4 stride. Arbitrary camera dimensions remain intact.
        gst::Caps::builder("video/x-raw").field("format", "BGRA").build()
    } else {
        audio_caps()
    };
    sink.set_caps(Some(&caps));
    let running = playing(&[source, &convert, &adjust, sink.upcast_ref()])?;
    Ok((running, sink))
}

fn sample(sink: &AppSink) -> Option<gst::Sample> {
    sink.try_pull_sample(gst::ClockTime::from_mseconds(20))
}

struct Microphone {
    frames: Queue<AudioFrame>,
    _pump: Pump,
}
impl AudioSource for Microphone {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<AudioFrame>, DeviceError> {
        self.frames.recv(timeout)
    }
}

pub(super) fn microphone(devices: WindowsDevices) -> Result<Box<dyn AudioSource>, DeviceError> {
    let frames = Queue::new(50);
    let (write, close) = (frames.clone(), frames.clone());
    let pump = start(
        devices,
        DeviceKind::Microphone,
        move |source| {
            let (pipeline, sink) = capture(&source, false)?;
            let mut pending = Vec::new();
            let write = write.clone();
            Ok(Box::new(move || {
                healthy(&pipeline.0)?;
                if let Some(sample) = sample(&sink) {
                    audio_frames(&sample, &mut pending, &write)?;
                }
                Ok(())
            }))
        },
        move || close.close(),
    )?;
    Ok(Box::new(Microphone { frames, _pump: pump }))
}

fn audio_frames(sample: &gst::Sample, pending: &mut Vec<f32>, write: &Queue<AudioFrame>) -> Result<(), DeviceError> {
    let map = sample
        .buffer()
        .ok_or(DeviceError::Closed)?
        .map_readable()
        .map_err(error)?;
    pending.extend(
        map.as_slice()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes)),
    );
    for samples in pending.as_chunks::<FRAME_SAMPLES>().0 {
        if let Some(frame) = AudioFrame::from_samples(samples) {
            write.push(frame);
        }
    }
    let whole = pending.len() / FRAME_SAMPLES * FRAME_SAMPLES;
    pending.drain(..whole);
    Ok(())
}

struct Camera {
    frames: Queue<VideoFrame>,
    _pump: Pump,
}
impl VideoSource for Camera {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, DeviceError> {
        self.frames.recv(timeout)
    }
}

pub(super) fn camera(devices: WindowsDevices) -> Result<Box<dyn VideoSource>, DeviceError> {
    let frames = Queue::new(3);
    let (write, close) = (frames.clone(), frames.clone());
    let pump = start(
        devices,
        DeviceKind::Camera,
        move |source| {
            let (pipeline, sink) = capture(&source, true)?;
            let write = write.clone();
            Ok(Box::new(move || {
                healthy(&pipeline.0)?;
                if let Some(sample) = sample(&sink) {
                    write.push(video_frame(&sample)?);
                }
                Ok(())
            }))
        },
        move || close.close(),
    )?;
    Ok(Box::new(Camera { frames, _pump: pump }))
}

/// macOS's screen share: the main display through `avfvideosrc`'s screen capture, the cursor drawn in, on a thread of
/// its own as a camera's. macOS asks the user for the Screen Recording permission the first time; without it no frame
/// comes, or the pipeline fails and the share ends.
#[cfg(target_os = "macos")]
pub(super) fn screen() -> Result<Box<dyn VideoSource>, DeviceError> {
    let frames = Queue::new(3);
    let (write, close) = (frames.clone(), frames.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let (ready, response) = mpsc::sync_channel(1);
    let thread = thread::Builder::new().name("macos-screen".to_owned()).spawn(move || {
        let shared = || -> Result<(), DeviceError> {
            let source = element("avfvideosrc")?;
            source.set_property("capture-screen", true);
            source.set_property("capture-screen-cursor", true);
            let (pipeline, sink) = capture(&source, true)?;
            let _ = ready.try_send(Ok(()));
            while !stopped.load(Ordering::Acquire) {
                healthy(&pipeline.0)?;
                if let Some(sample) = sample(&sink) {
                    write.push(video_frame(&sample)?);
                }
            }
            Ok(())
        };
        if let Err(failure) = shared() {
            let _ = ready.try_send(Err(failure.to_string()));
            log::warn!("the macOS screen share stopped: {failure}");
        }
        close.close();
    })?;
    let pump = Pump {
        stop,
        thread: Some(thread),
    };
    match response.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => Ok(Box::new(Camera { frames, _pump: pump })),
        Ok(Err(failure)) => Err(error(failure)),
        Err(failure) => Err(error(failure)),
    }
}

fn video_frame(sample: &gst::Sample) -> Result<VideoFrame, DeviceError> {
    let caps = sample
        .caps()
        .and_then(|caps| caps.structure(0))
        .ok_or(DeviceError::Closed)?;
    let width = u32::try_from(caps.get::<i32>("width").map_err(error)?).map_err(error)?;
    let height = u32::try_from(caps.get::<i32>("height").map_err(error)?).map_err(error)?;
    let stride = width.checked_mul(4).ok_or_else(|| error("video width overflow"))?;
    let data = sample
        .buffer()
        .ok_or(DeviceError::Closed)?
        .map_readable()
        .map_err(error)?
        .as_slice()
        .to_vec();
    let length = u64::from(stride) * u64::from(height);
    if u64::try_from(data.len()).ok() != Some(length) {
        return Err(error("unexpected BGRA frame length"));
    }
    Ok(VideoFrame {
        width,
        height,
        stride,
        format: PixelFormat::Bgra,
        data,
    })
}

struct Speaker {
    frames: Queue<AudioFrame>,
    _pump: Pump,
}
impl AudioSink for Speaker {
    fn play(&mut self, frame: &AudioFrame) -> Result<(), DeviceError> {
        self.frames.extend([frame.clone()]).map(drop)
    }
}

/// How late the call's playout may hand over a frame and still play in time. libwebrtc's 10 ms playout timer and the
/// threads between it and the device make frames come in bursts. Each frame plays this long after it comes.
const PLAYOUT_MARGIN: gst::ClockTime = gst::ClockTime::from_mseconds(50);
/// The audio the speaker's input holds before it drops new frames: 200 ms of 48 kHz mono `f32`.
const SPEAKER_BACKLOG: u64 = 38_400;
/// The audio the device's own buffer holds, in microseconds: a sink reports it as its latency (`osxaudiosink`'s
/// default is 200 ms), and each frame plays that much later.
const DEVICE_BUFFER_US: i64 = 40_000;
/// How long an audio sink may take to open its device.
const OPEN_WAIT: Duration = Duration::from_secs(1);

pub(super) fn speaker(devices: WindowsDevices) -> Result<Box<dyn AudioSink>, DeviceError> {
    let frames: Queue<AudioFrame> = Queue::new(20);
    let (read, close) = (frames.clone(), frames.clone());
    let pump = start(
        devices,
        DeviceKind::Speaker,
        move |sink| {
            let (pipeline, source) = playback(&sink)?;
            let read = read.clone();
            Ok(Box::new(move || {
                healthy(&pipeline.0)?;
                if let Some(frame) = read.recv(POLL)? {
                    push(&source, &frame)?;
                }
                // WASAPI's sink clock paces playback; the input queue waits for arriving call frames. Adding a
                // sleep here would serialize that clock with Windows' timer resolution and cause underruns.
                Ok(())
            }))
        },
        move || close.close(),
    )?;
    Ok(Box::new(Speaker { frames, _pump: pump }))
}

/// The speaker's pipeline into `sink`, playing on the device's own clock, and the source the call's frames go into.
/// Each frame is stamped with that clock when it comes and plays [`PLAYOUT_MARGIN`] later.
fn playback(sink: &gst::Element) -> Result<(Running, AppSrc), DeviceError> {
    // A live call has no preroll before its first received frame. Waiting for it here delays opening the
    // speaker (and therefore ringing or answering) until the state wait expires.
    sink.set_property("async", false);
    if sink.has_property_with_type("buffer-time", i64::static_type()) {
        sink.set_property("buffer-time", DEVICE_BUFFER_US);
    }
    // Not WASAPI2's `low-latency`: it leaves the sink about 5 ms, and frames of a call come later than that.
    let margin = i64::try_from(PLAYOUT_MARGIN.nseconds()).map_err(error)?;
    let source = AppSrc::builder()
        .caps(&audio_caps())
        .is_live(false)
        .format(gst::Format::Time)
        .do_timestamp(true)
        .block(false)
        .max_bytes(SPEAKER_BACKLOG)
        .min_latency(margin)
        .build();
    // Keep old frames: a stalled device's clock stops, and a dropped backlog leaves a jump the sink fills with silence.
    source.set_property_from_str("leaky-type", "upstream");
    let convert = element("audioconvert")?;
    let resample = element("audioresample")?;
    let elements = [source.upcast_ref(), &convert, &resample, sink];
    let running = Running(gst::Pipeline::new());
    running.0.add_many(elements).map_err(error)?;
    gst::Element::link_many(elements).map_err(error)?;
    running.0.set_state(gst::State::Paused).map_err(error)?;
    open(&source, sink)?;
    source.set_is_live(true);
    running.0.set_state(gst::State::Playing).map_err(error)?;
    let (state, _, _) = running.0.state(gst::ClockTime::from_seconds(5));
    state.map_err(error)?;
    Ok((running, source))
}

/// Opens the device with a frame of silence before the pipeline plays. An audio sink opens its device on its first
/// data, and only an open sink gives its clock and latency. Playing on the system clock instead slaves the sink to it:
/// each stall of the device's thread then puts the call's frames further ahead of the device, and the sink plays
/// silence until its skew correction catches up, seconds later. The source is not live until then: a live source
/// holds its data until the pipeline plays.
fn open(source: &AppSrc, sink: &gst::Element) -> Result<(), DeviceError> {
    // stamped: before the pipeline plays there is no clock to stamp it with
    let mut silence = buffer(&AudioFrame::silence());
    if let Some(silence) = silence.get_mut() {
        silence.set_pts(gst::ClockTime::ZERO);
    }
    source.push_buffer(silence).map_err(error)?;
    let audio_sink = gst::glib::Type::from_name("GstAudioBaseSink").is_some_and(|audio| sink.type_().is_a(audio));
    let deadline = std::time::Instant::now() + OPEN_WAIT;
    while audio_sink && sink.provide_clock().is_none() {
        if std::time::Instant::now() >= deadline {
            log::warn!("the speaker's device did not open in {OPEN_WAIT:?}: it plays on the system clock");
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

fn push(source: &AppSrc, frame: &AudioFrame) -> Result<(), DeviceError> {
    source.push_buffer(buffer(frame)).map(drop).map_err(error)
}

fn buffer(frame: &AudioFrame) -> gst::Buffer {
    let bytes: Vec<u8> = frame.samples().iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut buffer = gst::Buffer::from_mut_slice(bytes);
    if let Some(buffer) = buffer.get_mut() {
        buffer.set_duration(gst::ClockTime::from_mseconds(10));
    }
    buffer
}

#[cfg(test)]
mod playback_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_failure_retains_its_cause_while_eos_is_closed() {
        gst::init().unwrap();
        let pipeline = gst::Pipeline::new();
        pipeline
            .post_message(gst::message::Error::builder(gst::ResourceError::Read, "synthetic device vanished").build())
            .unwrap();
        assert!(
            healthy(&pipeline)
                .unwrap_err()
                .to_string()
                .contains("synthetic device vanished")
        );
        pipeline.post_message(gst::message::Eos::new()).unwrap();
        assert!(matches!(healthy(&pipeline), Err(DeviceError::Closed)));
    }

    /// ALSA's `null` device is an audio sink that needs no sound card. The pipeline plays on its clock, with its
    /// latency.
    #[test]
    fn the_speaker_plays_with_the_latency_of_its_open_device() {
        use gstreamer_app::gst_base::prelude::*;
        gst::init().unwrap();
        let Ok(sink) = gst::ElementFactory::make("alsasink").property("device", "null").build() else {
            eprintln!("skipped: GStreamer has no alsasink here");
            return;
        };
        let (pipeline, _source) = playback(&sink).unwrap();
        assert_eq!(pipeline.0.clock(), sink.provide_clock(), "not on the device's clock");
        let mut query = gst::query::Latency::new();
        assert!(sink.query(&mut query), "the device is not open");
        let (_, device, _) = query.result();
        assert!(device >= PLAYOUT_MARGIN, "the device gives only {device}");
        let base = sink.dynamic_cast_ref::<gstreamer_app::gst_base::BaseSink>().unwrap();
        assert_eq!(base.latency(), device);
    }
}
