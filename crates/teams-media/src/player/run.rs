//! The player's thread: `GStreamer`'s `playbin` on an `appsrc://` whose bytes come from the [`Cache`], its frames and
//! its sound to appsinks, driven by the [`Command`]s and watched through its bus (see `crate::player`'s docs).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::source::{Cache, SourceError};
use super::{Command, Failure, PlayerOutput, PlayerSetup, RATES, State, Status};
use crate::devices::{AudioFrame, AudioSink, FRAME_SAMPLES, PixelFormat, SAMPLE_RATE, VideoFrame};

/// How long the loop waits on the bus before it looks at the commands again.
const POLL: Duration = Duration::from_millis(40);
/// How long a read waits for the network before playing pauses for it (buffering): a short wait is covered by what
/// the pipeline's queues hold.
const STALL: Duration = Duration::from_millis(250);
/// How often the position is told while playing.
const TELL_EVERY: Duration = Duration::from_millis(250);

/// What the appsrc's callbacks share with the loop: where the next read starts, and whether a read waits for the
/// network or failed.
struct Feed {
    cache: Arc<Cache>,
    at: AtomicU64,
    waiting: AtomicBool,
    failed: Mutex<Option<SourceError>>,
    /// An element that would read the network was made, and kept from starting.
    refused: Mutex<bool>,
}

/// Where a playing pipeline's bytes come from, as the loop sees it: a file's cache ([`Feed`]) or a presentation's
/// parts (`super::stream`).
pub(super) trait Input: Send + Sync {
    /// Stops every read: one waiting for the network gives up, so the pipeline's threads can stop.
    fn close(&self);
    /// A seek is coming: the read the pipeline waits on is let go.
    fn interrupt(&self);
    /// A read the pipeline waits on waits for the network.
    fn waiting(&self) -> bool;
    /// Why a read failed, when one did.
    fn failed(&self) -> Option<SourceError>;
    /// An element that would read the network was made, and kept from starting.
    fn refused(&self) -> bool;
}

impl Input for Feed {
    fn close(&self) {
        self.cache.close();
    }

    fn interrupt(&self) {
        self.cache.interrupt();
    }

    fn waiting(&self) -> bool {
        self.waiting.load(Ordering::Relaxed)
    }

    fn failed(&self) -> Option<SourceError> {
        *self.failed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn refused(&self) -> bool {
        *self.refused.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The speaker the sound appsink plays on, opened at the first play; samples waiting for a whole 10 ms frame.
#[derive(Default)]
pub(super) struct Sound {
    speaker: Option<Box<dyn AudioSink>>,
    pending: Vec<f32>,
    /// The file's sound can't be decoded: no speaker is opened for it.
    silenced: bool,
}

/// The player's thread (see the module docs).
pub(super) fn run(
    cache: Arc<Cache>,
    setup: &PlayerSetup,
    output: &Arc<dyn PlayerOutput>,
    commands: &Receiver<Command>,
    play: bool,
) {
    let mut status = Status {
        state: State::Loading,
        position: Duration::ZERO,
        duration: Duration::ZERO,
        sound: false,
        rate: 1.0,
    };
    output.status(status);
    let feed = Arc::new(Feed {
        cache,
        at: AtomicU64::new(0),
        waiting: AtomicBool::new(false),
        failed: Mutex::new(None),
        refused: Mutex::new(false),
    });
    let sound = Arc::new(Mutex::new(Sound::default()));
    let pipeline = match pipeline(&feed, &sound, output) {
        Ok(pipeline) => pipeline,
        Err(failure) => {
            status.state = State::Failed(failure);
            output.status(status);
            outlast(commands);
            return;
        }
    };
    let mute = pipeline.clone();
    Playing::new((pipeline, Some(mute)), feed, sound, setup, output, play).lead_to_end(commands);
}

/// `playbin` on `appsrc://`, its video to a BGRA appsink and its sound to a 48 kHz mono appsink, paused (prerolling).
fn pipeline(
    feed: &Arc<Feed>,
    sound: &Arc<Mutex<Sound>>,
    output: &Arc<dyn PlayerOutput>,
) -> Result<gst::Element, Failure> {
    start_gstreamer()?;
    let total = feed.cache.total().map_err(failure_of)?;
    if let Some(index) = index_after_media(&feed.cache, total) {
        // the demuxer reads it before any frame: it arrives while `GStreamer` sets up and guesses the type
        feed.cache.prefetch(index);
    }
    let video = video_sink(output);
    let audio = audio_sink(sound);
    let playbin = gst::ElementFactory::make("playbin")
        .property("uri", "appsrc://")
        .property("video-sink", &video)
        .property("audio-sink", &audio)
        .build()
        .map_err(|_| Failure::NoCodec)?;
    if let Some(tempo) = tempo() {
        playbin.set_property("audio-filter", &tempo);
    }
    let refused = Arc::clone(feed);
    playbin.connect("element-setup", false, move |values| {
        if let Some(element) = values.get(1).and_then(|value| value.get::<gst::Element>().ok())
            && fetches(&element)
        {
            // never let it start: it would read from the network outside the engine
            element.set_locked_state(true);
            *refused.refused.lock().unwrap_or_else(PoisonError::into_inner) = true;
            gst::element_error!(element, gst::CoreError::Failed, ["an element that reads the network"]);
        }
        None
    });
    let feed = Arc::clone(feed);
    playbin.connect("source-setup", false, move |values| {
        if let Some(source) = values.get(1).and_then(|value| value.get::<gst::Element>().ok())
            && let Ok(source) = source.downcast::<gst_app::AppSrc>()
        {
            set_up_source(&source, Arc::clone(&feed), total);
        }
        None
    });
    playbin
        .set_state(gst::State::Paused)
        .map_err(|_| Failure::Unsupported)?;
    Ok(playbin)
}

/// The element that keeps the sound's pitch at another speed (`gst-plugins-good`'s `scaletempo`), when installed:
/// without it a speed other than 1× would play the sound's samples faster than the speaker takes them.
pub(super) fn tempo() -> Option<gst::Element> {
    gst::ElementFactory::make("scaletempo").build().ok()
}

/// `GStreamer` started, with nothing plugged that it mustn't ([`nothing_that_fetches_or_probes`]).
pub(crate) fn start_gstreamer() -> Result<(), Failure> {
    gst::init().map_err(|_| Failure::NoCodec)?;
    nothing_that_fetches_or_probes();
    Ok(())
}

/// Keeps playbin from plugging what it mustn't, done once a process on the registry's ranks (nothing is loaded for
/// it): an element that fetches ([`fetches`]: an adaptive demuxer, which a file whose bytes are an HLS or DASH
/// playlist would plug and which reads the playlist's links itself, or a network source), and a hardware video decoder
/// (VA-API, NVDEC, V4L2), whose making probes the GPU's drivers: ten seconds on a machine whose VA-API driver it then
/// refused, while a chat's video decodes in software at once.
fn nothing_that_fetches_or_probes() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for factory in gst::ElementFactory::factories_with_type(gst::ElementFactoryType::ANY, gst::Rank::NONE) {
            let klass = factory.metadata(gst::ELEMENT_METADATA_KLASS).unwrap_or_default();
            let hardware_decoder = klass.contains("Decoder") && klass.contains("Hardware");
            if klass_fetches(klass) || hardware_decoder {
                factory.set_rank(gst::Rank::NONE);
            }
        }
    });
}

/// Whether an element of this klass reads the network: an adaptive demuxer (HLS, DASH, Smooth Streaming) or a network
/// source.
fn klass_fetches(klass: &str) -> bool {
    klass.contains("Adaptive") || klass.contains("Network")
}

/// Whether `element` reads the network ([`klass_fetches`]), by its factory's klass.
fn fetches(element: &gst::Element) -> bool {
    element
        .factory()
        .and_then(|factory| factory.metadata(gst::ELEMENT_METADATA_KLASS).map(klass_fetches))
        .unwrap_or(false)
}

/// The top-level boxes of an ISO media file (MP4, MOV, M4V) looked at for where its media ends.
const BOXES: usize = 8;

/// Where an ISO media file's index (its `moov` box) starts when it comes after its media (`mdat`), as a recording
/// that wasn't made for streaming has it: the end of its `mdat` box, by its top-level boxes' headers; `None` for a
/// file that isn't one, or whose index comes first.
fn index_after_media(cache: &Cache, total: u64) -> Option<u64> {
    let mut at = 0;
    for _ in 0..BOXES {
        let head = cache.read_at(at, 16, true).ok()??;
        let size = u64::from(u32::from_be_bytes(head.get(0..4)?.try_into().ok()?));
        let kind = head.get(4..8)?;
        if at == 0 && kind != b"ftyp" {
            return None;
        }
        let size = match size {
            // to the file's end
            0 => return None,
            1 => u64::from_be_bytes(head.get(8..16)?.try_into().ok()?),
            size => size,
        };
        let end = at.checked_add(size).filter(|end| *end > at)?;
        match kind {
            b"moov" => return None,
            b"mdat" => return (end < total).then_some(end),
            _ => at = end,
        }
    }
    None
}

/// The appsrc reads the file at random: each read from where the last seek put it, as many bytes as asked.
fn set_up_source(source: &gst_app::AppSrc, feed: Arc<Feed>, total: u64) {
    source.set_stream_type(gst_app::AppStreamType::RandomAccess);
    source.set_size(i64::try_from(total).unwrap_or(i64::MAX));
    let seeking = Arc::clone(&feed);
    source.set_callbacks(
        gst_app::AppSrcCallbacks::builder()
            .need_data(move |source, length| {
                let at = feed.at.load(Ordering::Relaxed);
                let wanted = usize::try_from(length).unwrap_or(0).max(1);
                // waiting for the network shows as buffering while it plays
                let read = match feed.cache.read_at(at, wanted, false) {
                    Ok(None) => {
                        feed.waiting.store(true, Ordering::Relaxed);
                        let read = feed.cache.read_at(at, wanted, true);
                        feed.waiting.store(false, Ordering::Relaxed);
                        read
                    }
                    other => other,
                };
                match read {
                    Ok(Some(bytes)) if !bytes.is_empty() => {
                        feed.at.store(at + bytes.len() as u64, Ordering::Relaxed);
                        let _ = source.push_buffer(gst::Buffer::from_mut_slice(bytes));
                    }
                    Ok(Some(_)) => {
                        let _ = source.end_of_stream();
                    }
                    // let go for a seek: nothing is pushed, and the seek's flush ends the wait for it
                    Ok(None) => {}
                    Err(error) => {
                        *feed.failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                        gst::element_error!(source, gst::ResourceError::Read, ["the file couldn't be read"]);
                    }
                }
            })
            .seek_data(move |_, offset| {
                seeking.at.store(offset, Ordering::Relaxed);
                true
            })
            .build(),
    );
}

/// The video's appsink: BGRA frames (playbin converts to it), shown at their time, the paused first frame too.
pub(super) fn video_sink(output: &Arc<dyn PlayerOutput>) -> gst_app::AppSink {
    let caps = gst::Caps::builder("video/x-raw").field("format", "BGRA").build();
    let sink = gst_app::AppSink::builder()
        .caps(&caps)
        .sync(true)
        .max_buffers(2)
        .drop(true)
        .build();
    let (shown, prerolled) = (Arc::clone(output), Arc::clone(output));
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                show(&sample, shown.as_ref());
                Ok(gst::FlowSuccess::Ok)
            })
            .new_preroll(move |sink| {
                let sample = sink.pull_preroll().map_err(|_| gst::FlowError::Eos)?;
                show(&sample, prerolled.as_ref());
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    sink
}

/// Hands a BGRA sample to the output as a frame.
fn show(sample: &gst::Sample, output: &dyn PlayerOutput) {
    let shape = sample.caps().and_then(|caps| {
        let structure = caps.structure(0)?;
        let width = u32::try_from(structure.get::<i32>("width").ok()?).ok()?;
        let height = u32::try_from(structure.get::<i32>("height").ok()?).ok()?;
        Some((width, height))
    });
    let (Some((width, height)), Some(buffer)) = (shape, sample.buffer()) else {
        return;
    };
    let Ok(map) = buffer.map_readable() else { return };
    let stride = width * 4;
    let wanted = usize::try_from(u64::from(stride) * u64::from(height)).unwrap_or(usize::MAX);
    let Some(data) = map.as_slice().get(..wanted) else {
        return;
    };
    output.frame(VideoFrame {
        width,
        height,
        format: PixelFormat::Bgra,
        stride,
        data: data.to_vec(),
    });
}

/// The sound's appsink: 48 kHz mono samples (playbin converts and resamples to them), in time, played on the speaker
/// in 10 ms frames.
pub(super) fn audio_sink(sound: &Arc<Mutex<Sound>>) -> gst_app::AppSink {
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("rate", i32::try_from(SAMPLE_RATE).unwrap_or(48_000))
        .field("channels", 1)
        .field("layout", "interleaved")
        .build();
    let sink = gst_app::AppSink::builder().caps(&caps).sync(true).build();
    let sound = Arc::clone(sound);
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                if let Some(buffer) = sample.buffer()
                    && let Ok(map) = buffer.map_readable()
                {
                    play(&sound, map.as_slice());
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    sink
}

/// Plays F32LE samples on the speaker, a 10 ms frame at a time; without a speaker they go nowhere.
fn play(sound: &Mutex<Sound>, bytes: &[u8]) {
    let mut sound = sound.lock().unwrap_or_else(PoisonError::into_inner);
    let Sound { speaker, pending, .. } = &mut *sound;
    let Some(speaker) = speaker.as_mut() else {
        pending.clear();
        return;
    };
    pending.extend(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|sample| f32::from_le_bytes(*sample)),
    );
    let mut failed = false;
    while pending.len() >= FRAME_SAMPLES {
        if let Some(frame) = AudioFrame::from_samples(&pending[..FRAME_SAMPLES]) {
            failed |= speaker.play(&frame).is_err();
        }
        pending.drain(..FRAME_SAMPLES);
    }
    if failed {
        sound.speaker = None;
    }
}

pub(super) fn failure_of(error: SourceError) -> Failure {
    match error {
        SourceError::Unavailable => Failure::Unavailable,
        SourceError::Failed | SourceError::Stopped => Failure::Network,
    }
}

/// How far a player has loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Load {
    /// Its first preroll isn't done.
    Prerolling,
    /// Its first preroll is done (its length is known and it can seek), and the seek that sets a speed asked before
    /// then is prerolling: it is played, or shown paused, once that is done.
    SettingRate,
    /// Loaded: it plays and seeks as asked.
    Ready,
}

/// The playing pipeline and what the loop knows of it.
pub(super) struct Playing {
    pipeline: gst::Element,
    input: Arc<dyn Input>,
    /// The element whose `mute` mutes the sound and whose `volume` sets how loud it plays (none without a sound).
    mute: Option<gst::Element>,
    sound: Arc<Mutex<Sound>>,
    setup: PlayerSetup,
    output: Arc<dyn PlayerOutput>,
    status: Status,
    /// Asked to play (it plays once loaded).
    wants_play: bool,
    /// How far it has loaded.
    load: Load,
    told: Instant,
    /// Since when a read has waited for the network while playing (a stall is buffering after [`STALL`]).
    stalled_since: Option<Instant>,
    /// No element plugged reads the file's own format (neither a picture's nor a sound's: a playlist, whose adaptive
    /// demuxer is never plugged, or a container no installed plugin reads): it fails as [`Failure::Unsupported`].
    format_unread: bool,
}

impl Playing {
    /// The loop's state for `pipeline` (paused, prerolling) and the element that mutes its sound, reading from
    /// `input`; played once loaded when `play`.
    pub(super) fn new(
        (pipeline, mute): (gst::Element, Option<gst::Element>),
        input: Arc<dyn Input>,
        sound: Arc<Mutex<Sound>>,
        setup: &PlayerSetup,
        output: &Arc<dyn PlayerOutput>,
        play: bool,
    ) -> Self {
        Self {
            pipeline,
            input,
            mute,
            sound,
            setup: setup.clone(),
            output: Arc::clone(output),
            status: Status {
                state: State::Loading,
                position: Duration::ZERO,
                duration: Duration::ZERO,
                sound: false,
                rate: 1.0,
            },
            wants_play: play,
            load: Load::Prerolling,
            told: Instant::now(),
            stalled_since: None,
            format_unread: false,
        }
    }

    /// Leads the pipeline until the player is dropped or fails, then stops its reads and the pipeline.
    pub(super) fn lead_to_end(mut self, commands: &Receiver<Command>) {
        self.lead(commands);
        // a read waiting for the network gives up, so the pipeline's threads can stop
        self.input.close();
        let _ = self.pipeline.set_state(gst::State::Null);
    }

    /// The loop: the commands, then the bus; ends when the player is dropped or fails.
    fn lead(&mut self, commands: &Receiver<Command>) {
        let Some(bus) = self.pipeline.bus() else { return };
        loop {
            loop {
                match commands.try_recv() {
                    Ok(command) => self.take(command),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            if let Some(message) = bus.timed_pop(gst::ClockTime::from_nseconds(nanos(POLL))) {
                self.read(&message);
                if matches!(self.status.state, State::Failed(_)) {
                    self.input.close();
                    let _ = self.pipeline.set_state(gst::State::Null);
                    outlast(commands);
                    return;
                }
            }
            self.tick();
        }
    }

    fn take(&mut self, command: Command) {
        match command {
            Command::Play => {
                self.wants_play = true;
                // played once the speed's seek prerolled, when one does
                if self.load == Load::Ready {
                    self.play();
                }
            }
            Command::Pause => {
                self.wants_play = false;
                if self.loaded() && matches!(self.status.state, State::Playing | State::Buffering) {
                    let _ = self.pipeline.set_state(gst::State::Paused);
                    self.status.state = State::Paused;
                    self.tell();
                }
            }
            Command::Seek(at) | Command::SeekNear(at) => {
                if self.loaded() {
                    // kept within the movie, once its length is known
                    let at = if self.status.duration.is_zero() {
                        at
                    } else {
                        at.min(self.status.duration)
                    };
                    self.seek_to(at, matches!(command, Command::Seek(_)));
                    if self.status.state == State::Ended {
                        self.status.state = State::Paused;
                    }
                    self.status.position = at;
                    self.tell();
                }
            }
            Command::Muted(muted) => {
                if let Some(mute) = &self.mute {
                    mute.set_property("mute", muted);
                }
            }
            Command::Volume(level) => {
                if let Some(mute) = &self.mute {
                    mute.set_property("volume", if level.is_finite() { level.clamp(0.0, 1.0) } else { 1.0 });
                }
            }
            Command::Rate(rate) => self.set_rate(rate),
        }
    }

    /// Plays at `rate` (kept within [`RATES`]): a seek where it plays now, with the new rate, once it has loaded (until
    /// then the rate is kept for its first play).
    fn set_rate(&mut self, rate: f64) {
        let (slowest, fastest) = (RATES[0], RATES[RATES.len() - 1]);
        let rate = if rate.is_finite() {
            rate.clamp(slowest, fastest)
        } else {
            1.0
        };
        if (rate - self.status.rate).abs() < f64::EPSILON {
            return;
        }
        // every later seek keeps it
        self.status.rate = rate;
        self.tell();
        if self.loaded() && self.status.state != State::Ended {
            let at = self
                .pipeline
                .query_position::<gst::ClockTime>()
                .map_or(self.status.position, |at| Duration::from_nanos(at.nseconds()));
            self.seek_to(at, true);
        }
    }

    fn play(&mut self) {
        if self.status.state == State::Ended {
            self.seek_to(Duration::ZERO, true);
        }
        let mut sound = self.sound.lock().unwrap_or_else(PoisonError::into_inner);
        if sound.speaker.is_none() && !sound.silenced {
            // a speaker that can't be opened: the video plays silently, its status saying so
            sound.speaker = self.setup.speaker.as_ref().and_then(|devices| devices.speaker().ok());
        }
        self.status.sound = sound.speaker.is_some();
        drop(sound);
        let _ = self.pipeline.set_state(gst::State::Playing);
        self.status.state = State::Playing;
        self.tell();
    }

    /// Seeks to `at`, `exactly` or to the key frame nearest it. The read the pipeline waits on is let go first, and the
    /// old place's streams give way to the new place's (a flushing seek waits for the source's read to return, which
    /// could take the old place's bytes' time).
    fn seek_to(&self, at: Duration, exactly: bool) {
        let flags = if exactly {
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE
        } else {
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_NEAREST
        };
        self.input.interrupt();
        let _ = self.pipeline.seek(
            self.status.rate,
            flags,
            gst::SeekType::Set,
            gst::ClockTime::from_nseconds(nanos(at)),
            gst::SeekType::None,
            gst::ClockTime::NONE,
        );
    }

    /// Takes one message of the bus.
    fn read(&mut self, message: &gst::Message) {
        use gst::MessageView;
        match message.view() {
            MessageView::AsyncDone(_) if self.load == Load::Prerolling => {
                self.load = Load::Ready;
                self.read_duration();
                self.status.sound = self.setup.speaker.is_some();
                if (self.status.rate - 1.0).abs() >= f64::EPSILON {
                    // a speed asked before it loaded: its seek prerolls before it plays
                    self.load = Load::SettingRate;
                    self.seek_to(Duration::ZERO, true);
                } else {
                    self.start();
                }
            }
            MessageView::AsyncDone(_) if self.load == Load::SettingRate => {
                self.load = Load::Ready;
                self.start();
            }
            MessageView::DurationChanged(_) => self.read_duration(),
            MessageView::Element(element) => self.missing_plugin(element.structure()),
            MessageView::Eos(_) => {
                self.status.state = State::Ended;
                self.status.position = self.status.duration;
                let _ = self.pipeline.set_state(gst::State::Paused);
                self.tell();
            }
            MessageView::Error(error) => {
                let source = self.input.failed();
                let refused = self.input.refused();
                let failure = match source {
                    Some(source) => failure_of(source),
                    None if refused || self.format_unread => Failure::Unsupported,
                    None => failure_of_error(&error.error()),
                };
                self.status.state = State::Failed(failure);
                self.tell();
            }
            _ => {}
        }
    }

    /// Its first preroll is done: its length is known and it can seek.
    fn loaded(&self) -> bool {
        self.load != Load::Prerolling
    }

    /// Loaded: played when asked to, else shown paused.
    fn start(&mut self) {
        if self.wants_play {
            self.play();
        } else {
            self.status.state = State::Paused;
            self.tell();
        }
    }

    /// A stream no installed plugin decodes: playbin goes on with the others (a warning, not an error), so a video whose
    /// picture can't be decoded fails here ([`Failure::NoCodec`]) rather than play its sound over the poster; one whose
    /// sound can't be, plays silently and says so; one for the file's own format (`format_unread`) makes the
    /// error that follows [`Failure::Unsupported`].
    fn missing_plugin(&mut self, structure: Option<&gst::StructureRef>) {
        let Some(structure) = structure.filter(|structure| structure.name() == "missing-plugin") else {
            return;
        };
        let missing = structure
            .get::<gst::Caps>("detail")
            .ok()
            .and_then(|caps| caps.structure(0).map(|caps| caps.name().to_string()))
            .unwrap_or_default();
        if missing.starts_with("video/") {
            self.status.state = State::Failed(Failure::NoCodec);
        } else if missing.starts_with("audio/") {
            self.status.sound = false;
            let mut sound = self.sound.lock().unwrap_or_else(PoisonError::into_inner);
            sound.speaker = None;
            sound.silenced = true;
        } else {
            self.format_unread = true;
        }
        self.tell();
    }

    /// The movie's length as the pipeline knows it now; one it doesn't know yet (a stream whose container gives none
    /// at its start) is asked again as it plays.
    fn read_duration(&mut self) {
        if let Some(duration) = self.pipeline.query_duration::<gst::ClockTime>() {
            self.status.duration = Duration::from_nanos(duration.nseconds());
        }
    }

    /// The position told while playing; buffering while a read waits for the network: the pipeline paused (its clock
    /// stops, so nothing plays ahead and no sound is lost) once the wait passed [`STALL`], and playing again once the
    /// read came.
    fn tick(&mut self) {
        if !matches!(self.status.state, State::Playing | State::Buffering) {
            self.stalled_since = None;
            return;
        }
        let waiting = self.input.waiting();
        let since = *self.stalled_since.get_or_insert_with(Instant::now);
        if !waiting {
            self.stalled_since = None;
        }
        let state = match (self.status.state, waiting) {
            (State::Playing, true) if since.elapsed() >= STALL => {
                let _ = self.pipeline.set_state(gst::State::Paused);
                State::Buffering
            }
            (State::Buffering, false) => {
                let _ = self.pipeline.set_state(gst::State::Playing);
                State::Playing
            }
            (state, _) => state,
        };
        if state != self.status.state || self.told.elapsed() >= TELL_EVERY {
            self.status.state = state;
            if self.status.duration.is_zero() {
                self.read_duration();
            }
            if let Some(position) = self.pipeline.query_position::<gst::ClockTime>() {
                let position = Duration::from_nanos(position.nseconds());
                // a length not known yet holds no position back
                self.status.position = if self.status.duration.is_zero() {
                    position
                } else {
                    position.min(self.status.duration)
                };
            }
            self.tell();
        }
    }

    fn tell(&mut self) {
        self.told = Instant::now();
        self.output.status(self.status);
    }
}

/// What a `GStreamer` error says of the file: a format or codec no plugin plays, or a file that doesn't add up.
fn failure_of_error(error: &gst::glib::Error) -> Failure {
    if error.matches(gst::CoreError::MissingPlugin) || error.matches(gst::StreamError::CodecNotFound) {
        Failure::NoCodec
    } else {
        Failure::Unsupported
    }
}

/// Waits for the player to be dropped (its commands end), taking nothing more: a failed player stays until then, so
/// its owner keeps telling the failure.
pub(super) fn outlast(commands: &Receiver<Command>) {
    while commands.recv().is_ok() {}
}

pub(super) fn nanos(at: Duration) -> u64 {
    u64::try_from(at.as_nanos()).unwrap_or(u64::MAX)
}
