//! A presentation streamed in parts, as a DASH player streams one (a meeting recording, as Teams' Recap player plays
//! it): each track's initialization segment and its media segments (fragmented MP4) from a [`SegmentSource`] (the
//! engine's reads, in the app), never from the network by `GStreamer` itself.
//!
//! - **Its tracks**: each one's parts are read here ([`super::fmp4`]) into samples, pushed with their times into an
//!   `appsrc` of their codec's caps (H.264 with its `avcC`, AAC with its `AudioSpecificConfig`), decoded by
//!   `decodebin` (the system's plugins; no hardware decoder, nothing that fetches) and shown or played by the same
//!   appsinks a file's player has. The pipeline's clock keeps sound and picture in step.
//! - **Reading**: a thread per track reads its segments in turn, ahead of what plays by what its `appsrc` holds
//!   ([`AHEAD`]): it pushes while the `appsrc` has room and waits while it says it has enough (a push never blocks, so
//!   a seek, which takes the push's lock, never waits on one); a read that failed on the way is made again ([`TRIES`] times in a row, a growing pause
//!   between), one that can't be had fails the player ([`Failure::Unavailable`]), a part that doesn't add up fails it
//!   as unsupported.
//! - **Seeking**: a seek reaches each `appsrc` as the time to play from; its thread lets go of the read it waits on and
//!   reads from the segment that holds that time (its first sample a key frame), the samples before it decoded and not
//!   shown.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::fmp4::{self, Codec, Sample, TrackInit};
use super::run::{Input, Playing, Sound, audio_sink, failure_of, outlast, start_gstreamer, tempo, video_sink};
use super::source::{Abandon, SourceError};
use super::{Command, Failure, PlayerOutput, PlayerSetup, State, Status};

/// How much of a track its `appsrc` holds ahead of what plays: what its thread reads ahead by.
pub const AHEAD: Duration = Duration::from_secs(30);
/// How many times in a row a part is read before the player fails.
pub const TRIES: u32 = 4;

/// What a track carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Video,
}

/// Where a media segment is in its track, in the track's timescale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u64,
    pub duration: u64,
}

/// One track of a presentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub kind: TrackKind,
    /// The units of a second its segments' times count.
    pub timescale: u64,
    pub segments: Vec<Span>,
}

impl Track {
    /// The segment that holds `at` (the first before it starts, the last after it ends).
    fn segment_at(&self, at: Duration) -> usize {
        let at = super::run::nanos(at);
        self.segments
            .partition_point(|segment| nanos_of(segment.start, self.timescale) <= at)
            .saturating_sub(1)
    }
}

/// A presentation: its length and its tracks (at most one of each kind is played).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presentation {
    pub duration: Duration,
    pub tracks: Vec<Track>,
}

/// A part of a track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// Its initialization segment.
    Init,
    /// Its media segment at this index.
    Media(usize),
}

/// Where a presentation and its parts come from. Called on the player's threads, and may block; a call waiting returns
/// [`SourceError::Stopped`] at once when its [`Abandon`] is set ([`Abandon::wait_for`]).
pub trait SegmentSource: Send + Sync {
    /// The presentation.
    ///
    /// # Errors
    /// [`SourceError::Unavailable`] when it can't be had, [`SourceError::Failed`] when its read failed on the way,
    /// [`SourceError::Stopped`] when abandoned.
    fn presentation(&self, abandon: &Abandon) -> Result<Presentation, SourceError>;

    /// The bytes of `part` of track `track` (its index in the presentation's tracks).
    ///
    /// # Errors
    /// As [`SegmentSource::presentation`].
    fn part(&self, track: usize, part: Part, abandon: &Abandon) -> Result<Vec<u8>, SourceError>;
}

/// `value` units of `timescale` in ns.
fn nanos_of(value: u64, timescale: u64) -> u64 {
    u64::try_from(u128::from(value) * 1_000_000_000 / u128::from(timescale.max(1))).unwrap_or(u64::MAX)
}

/// Where a track's thread reads: the seek it reads after (counted) and the time it reads from.
#[derive(Clone, Copy, Debug, Default)]
struct Place {
    seek: u64,
    at: Duration,
    /// A sample was pushed since that seek.
    pushed: bool,
}

/// One track's `appsrc` and its thread's state.
struct Lane {
    src: gst_app::AppSrc,
    place: Mutex<Place>,
    moved: Condvar,
    /// Held while a sample is pushed (never for long: a push doesn't block): a seek waits for it, so no sample of the
    /// place it left is pushed after it.
    push: Mutex<()>,
    /// Its `appsrc` holds enough ([`AHEAD`]): nothing is pushed until it asks for more.
    full: AtomicBool,
    /// Its read waits for the network.
    reading: AtomicBool,
    /// Its `appsrc` asked for data since it was last given some.
    hungry: AtomicBool,
    /// Set to let go of the read it waits on.
    abandon: Mutex<Abandon>,
}

impl Lane {
    fn place(&self) -> Place {
        *self.place.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A seek to `at`: the read waited on is let go and the thread reads from there. One to where it reads already,
    /// before anything was pushed (the `appsrc`'s own seek as it starts), changes nothing.
    fn seek(&self, at: Duration) {
        let _pushing = self.push.lock().unwrap_or_else(PoisonError::into_inner);
        let mut place = self.place.lock().unwrap_or_else(PoisonError::into_inner);
        if place.at == at && !place.pushed {
            return;
        }
        *place = Place {
            seek: place.seek + 1,
            at,
            pushed: false,
        };
        // the seek's flush emptied the `appsrc`
        self.full.store(false, Ordering::SeqCst);
        drop(place);
        self.abandon.lock().unwrap_or_else(PoisonError::into_inner).set();
        self.moved.notify_all();
    }

    /// The `appsrc` asked for data (it is under its low mark) or said it has enough.
    fn wants(&self, more: bool) {
        if more {
            self.hungry.store(true, Ordering::SeqCst);
        }
        let _place = self.place.lock().unwrap_or_else(PoisonError::into_inner);
        self.full.store(!more, Ordering::SeqCst);
        self.moved.notify_all();
    }

    /// Waits while the `appsrc` holds enough; whether there is room for the place `seek` put it (`false` once a seek
    /// came after it, or the player is closed).
    fn room(&self, seek: u64, streamed: &Streamed) -> bool {
        let mut place = self.place.lock().unwrap_or_else(PoisonError::into_inner);
        while self.full.load(Ordering::SeqCst) && place.seek == seek && !streamed.closed.load(Ordering::SeqCst) {
            // the `appsrc` asks for more only once it is empty: pushing starts again once half of what it held played
            if self.held().is_some_and(|held| held < AHEAD / 2) {
                self.full.store(false, Ordering::SeqCst);
                break;
            }
            place = self
                .moved
                .wait_timeout(place, Duration::from_millis(100))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        place.seek == seek && !streamed.closed.load(Ordering::SeqCst)
    }

    /// How much the `appsrc` holds, by its samples' times (`GStreamer` 1.20 on).
    fn held(&self) -> Option<Duration> {
        self.src
            .has_property("current-level-time")
            .then(|| Duration::from_nanos(self.src.property::<u64>("current-level-time")))
    }

    /// Waits until a seek after `seek`, or the player is closed.
    fn wait_for_seek(&self, seek: u64, streamed: &Streamed) {
        let mut place = self.place.lock().unwrap_or_else(PoisonError::into_inner);
        while place.seek == seek && !streamed.closed.load(Ordering::SeqCst) {
            place = self.moved.wait(place).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// What a streamed player's threads share: whether it was closed, what its reads wait on, why one failed.
#[derive(Default)]
pub(super) struct Streamed {
    closed: AtomicBool,
    /// The presentation's and the initialization segments' reads, before the tracks' threads start.
    opening: Abandon,
    lanes: Mutex<Vec<Arc<Lane>>>,
    failed: Mutex<Option<SourceError>>,
}

impl Streamed {
    fn lanes(&self) -> Vec<Arc<Lane>> {
        self.lanes.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The player fails: `error` kept, and an error posted on `src` so the loop hears it.
    fn fail(&self, error: SourceError, src: &gst_app::AppSrc) {
        *self.failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
        gst::element_error!(src, gst::ResourceError::Read, ["a part of the video couldn't be read"]);
    }
}

impl Input for Streamed {
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.opening.set();
        for lane in self.lanes() {
            lane.abandon.lock().unwrap_or_else(PoisonError::into_inner).set();
            lane.moved.notify_all();
        }
    }

    /// A seek reaches each `appsrc` as its own (see [`Lane::seek`]).
    fn interrupt(&self) {}

    fn waiting(&self) -> bool {
        self.lanes()
            .iter()
            .any(|lane| lane.reading.load(Ordering::SeqCst) && lane.hungry.load(Ordering::SeqCst))
    }

    fn failed(&self) -> Option<SourceError> {
        *self.failed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn refused(&self) -> bool {
        false
    }
}

impl super::Stop for Streamed {
    fn stop(&self) {
        self.close();
    }
}

/// A track to play: where it is in the presentation, and what its initialization segment says.
struct Opened {
    index: usize,
    track: Track,
    init: TrackInit,
}

/// The streamed player's thread: the presentation and its tracks' initialization read, the pipeline made and led
/// (see the module docs).
pub(super) fn run(
    streamed: &Arc<Streamed>,
    source: &Arc<dyn SegmentSource>,
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
    let sound = Arc::new(Mutex::new(Sound::default()));
    let made = open(streamed, source.as_ref())
        .and_then(|(presentation, tracks)| pipeline(streamed, source, &presentation, tracks, &sound, output));
    let made = match made {
        Ok(made) => made,
        Err(failure) => {
            status.state = State::Failed(failure);
            output.status(status);
            outlast(commands);
            return;
        }
    };
    let input: Arc<dyn Input> = Arc::clone(streamed) as Arc<dyn Input>;
    Playing::new(made, input, sound, setup, output, play).lead_to_end(commands);
}

/// The presentation and its tracks to play (an audio and a video one at most), each with its initialization segment.
fn open(streamed: &Streamed, source: &dyn SegmentSource) -> Result<(Presentation, Vec<Opened>), Failure> {
    let abandon = &streamed.opening;
    let presentation = again(abandon, || source.presentation(abandon)).map_err(failure_of)?;
    let mut opened: Vec<Opened> = Vec::new();
    for (index, track) in presentation.tracks.iter().enumerate() {
        if track.segments.is_empty() || opened.iter().any(|kept| kept.track.kind == track.kind) {
            continue;
        }
        let bytes = again(abandon, || source.part(index, Part::Init, abandon)).map_err(failure_of)?;
        let init = fmp4::read_init(&bytes).ok_or(Failure::Unsupported)?;
        let fits = matches!(
            (&init.codec, track.kind),
            (Codec::H264 { .. }, TrackKind::Video) | (Codec::Aac { .. }, TrackKind::Audio)
        );
        if !fits {
            return Err(Failure::Unsupported);
        }
        opened.push(Opened {
            index,
            track: track.clone(),
            init,
        });
    }
    if opened.is_empty() {
        return Err(Failure::Unsupported);
    }
    Ok((presentation, opened))
}

/// `read` made again while it fails on the way, up to [`TRIES`] times, a growing pause between (cut short when
/// `abandon` is set).
fn again<T>(abandon: &Abandon, mut read: impl FnMut() -> Result<T, SourceError>) -> Result<T, SourceError> {
    let mut pause = Duration::from_millis(250);
    for _ in 1..TRIES {
        match read() {
            Err(SourceError::Failed) => {
                if !rest(abandon, pause) {
                    return Err(SourceError::Stopped);
                }
                pause *= 2;
            }
            answer => return answer,
        }
    }
    read()
}

/// Waits `pause`, in short steps; `false` as soon as `abandon` is set.
fn rest(abandon: &Abandon, pause: Duration) -> bool {
    const STEP: Duration = Duration::from_millis(50);
    let mut left = pause;
    while !left.is_zero() {
        if abandon.is_set() {
            return false;
        }
        let step = left.min(STEP);
        thread::sleep(step);
        left -= step;
    }
    !abandon.is_set()
}

/// The pipeline: per track an `appsrc` of its caps, `decodebin`, and its sink (the picture converted to BGRA; the
/// sound through `scaletempo`, converted, resampled and through a `volume`, whose `mute` and `volume` the commands
/// set), paused; each
/// track's thread started.
fn pipeline(
    streamed: &Arc<Streamed>,
    source: &Arc<dyn SegmentSource>,
    presentation: &Presentation,
    tracks: Vec<Opened>,
    sound: &Arc<Mutex<Sound>>,
    output: &Arc<dyn PlayerOutput>,
) -> Result<(gst::Element, Option<gst::Element>), Failure> {
    start_gstreamer()?;
    let pipeline = gst::Pipeline::new();
    let mut mute = None;
    let mut lanes = Vec::new();
    for opened in &tracks {
        let sink: Vec<gst::Element> = match opened.track.kind {
            TrackKind::Video => vec![make("videoconvert")?, video_sink(output).upcast()],
            TrackKind::Audio => {
                let volume = make("volume")?;
                mute = Some(volume.clone());
                // the decoder's samples converted for `scaletempo` (which keeps the pitch at another speed), then for
                // the sink
                let mut chain = vec![make("audioconvert")?];
                chain.extend(tempo());
                chain.extend([
                    make("audioconvert")?,
                    make("audioresample")?,
                    volume,
                    audio_sink(sound).upcast(),
                ]);
                chain
            }
        };
        lanes.push(branch(&pipeline, &opened.init.codec, presentation.duration, &sink)?);
    }
    streamed
        .lanes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone_from(&lanes);
    let pipeline: gst::Element = pipeline.upcast();
    pipeline
        .set_state(gst::State::Paused)
        .map_err(|_| Failure::Unsupported)?;
    for (opened, lane) in tracks.into_iter().zip(lanes) {
        let (streamed, source) = (Arc::clone(streamed), Arc::clone(source));
        thread::Builder::new()
            .name("video-track".into())
            .spawn(move || feed(&streamed, &lane, source.as_ref(), &opened))
            .map_err(|_| Failure::Internal)?;
    }
    Ok((pipeline, mute))
}

/// One track's branch in `pipeline`: an `appsrc` of `codec`'s caps (its length `duration`), `decodebin`, then `sink`
/// (the elements after the decoder, in order), and the track's lane, its `appsrc`'s callbacks set.
fn branch(
    pipeline: &gst::Pipeline,
    codec: &Codec,
    duration: Duration,
    sink: &[gst::Element],
) -> Result<Arc<Lane>, Failure> {
    let src = gst_app::AppSrc::builder()
        .caps(&caps(codec))
        .format(gst::Format::Time)
        .stream_type(gst_app::AppStreamType::Seekable)
        .block(false)
        .max_bytes(64 * 1024 * 1024)
        .build();
    src.set_duration(gst::ClockTime::from_nseconds(super::run::nanos(duration)));
    // what it holds ahead (GStreamer 1.20 on): the bytes alone bound it on an older one
    if src.has_property("max-time") {
        src.set_property("max-time", super::run::nanos(AHEAD));
    }
    let decode = make("decodebin")?;
    pipeline
        .add_many([src.upcast_ref::<gst::Element>(), &decode])
        .map_err(|_| Failure::Internal)?;
    pipeline.add_many(sink).map_err(|_| Failure::Internal)?;
    src.link(&decode).map_err(|_| Failure::Internal)?;
    gst::Element::link_many(sink).map_err(|_| Failure::Internal)?;
    let head = sink
        .first()
        .and_then(|element| element.static_pad("sink"))
        .ok_or(Failure::Internal)?;
    decode.connect_pad_added(move |_, pad| {
        if !head.is_linked() {
            let _ = pad.link(&head);
        }
    });
    let lane = Arc::new(Lane {
        src: src.clone(),
        place: Mutex::new(Place::default()),
        moved: Condvar::new(),
        push: Mutex::new(()),
        full: AtomicBool::new(false),
        reading: AtomicBool::new(false),
        hungry: AtomicBool::new(false),
        abandon: Mutex::new(Abandon::new()),
    });
    let (hungry, enough, seeking) = (Arc::clone(&lane), Arc::clone(&lane), Arc::clone(&lane));
    src.set_callbacks(
        gst_app::AppSrcCallbacks::builder()
            .need_data(move |_, _| hungry.wants(true))
            .enough_data(move |_| enough.wants(false))
            .seek_data(move |_, at| {
                seeking.seek(Duration::from_nanos(at));
                true
            })
            .build(),
    );
    Ok(lane)
}

fn make(factory: &str) -> Result<gst::Element, Failure> {
    gst::ElementFactory::make(factory).build().map_err(|_| Failure::NoCodec)
}

/// The caps of a track's samples.
fn caps(codec: &Codec) -> gst::Caps {
    let size = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
    match codec {
        Codec::H264 {
            avcc,
            avc3,
            width,
            height,
        } => gst::Caps::builder("video/x-h264")
            .field("stream-format", if *avc3 { "avc3" } else { "avc" })
            .field("alignment", "au")
            .field("codec_data", gst::Buffer::from_slice(avcc.clone()))
            .field("width", size(*width))
            .field("height", size(*height))
            .build(),
        Codec::Aac { config, rate, channels } => {
            let mut caps = gst::Caps::builder("audio/mpeg")
                .field("mpegversion", 4)
                .field("stream-format", "raw")
                .field("codec_data", gst::Buffer::from_slice(config.clone()));
            // the sample entry's own (the config says it too): left out when it says nothing
            if *rate > 0 {
                caps = caps.field("rate", size(*rate));
            }
            if *channels > 0 {
                caps = caps.field("channels", size(*channels));
            }
            caps.build()
        }
    }
}

/// How a segment's samples went.
enum Pushed {
    /// All of them.
    All,
    /// A seek came (or the player was closed): they aren't wanted any more.
    Moved,
}

/// A track's thread: its segments read in turn from where the last seek put it, their samples pushed (see the module
/// docs); ends when the player is closed or fails.
fn feed(streamed: &Streamed, lane: &Lane, source: &dyn SegmentSource, opened: &Opened) {
    let segments = opened.track.segments.len();
    let mut seen: Option<u64> = None;
    let mut next = 0;
    let mut ended = false;
    while !streamed.closed.load(Ordering::SeqCst) {
        let place = lane.place();
        if seen != Some(place.seek) {
            seen = Some(place.seek);
            next = opened.track.segment_at(place.at);
            ended = false;
        }
        if next >= segments {
            if !ended {
                let _ = lane.src.end_of_stream();
                ended = true;
            }
            lane.wait_for_seek(place.seek, streamed);
            continue;
        }
        let abandon = Abandon::new();
        *lane.abandon.lock().unwrap_or_else(PoisonError::into_inner) = abandon.clone();
        if lane.place().seek != place.seek || streamed.closed.load(Ordering::SeqCst) {
            continue;
        }
        lane.reading.store(true, Ordering::SeqCst);
        let read = again(&abandon, || source.part(opened.index, Part::Media(next), &abandon));
        lane.reading.store(false, Ordering::SeqCst);
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(SourceError::Stopped) => continue,
            Err(error) => {
                streamed.fail(error, &lane.src);
                return;
            }
        };
        let Some(samples) = fmp4::read_samples(&bytes, &opened.init) else {
            gst::element_error!(
                lane.src,
                gst::StreamError::Format,
                ["a part of the video doesn't add up"]
            );
            return;
        };
        match push(streamed, lane, place.seek, &bytes, &samples, &opened.init) {
            Pushed::All => next += 1,
            Pushed::Moved => {}
        }
    }
}

/// Pushes a segment's samples, with their times, while no seek came after `seek`, each once the `appsrc` has room. One
/// the `appsrc` refuses (a seek's flush not over yet) is pushed again after a pause.
fn push(streamed: &Streamed, lane: &Lane, seek: u64, bytes: &[u8], samples: &[Sample], init: &TrackInit) -> Pushed {
    let scale = u64::from(init.timescale);
    for sample in samples {
        let Some(data) = bytes.get(sample.bytes.clone()) else {
            continue;
        };
        let decode = sample.decode.saturating_sub(init.shift);
        let shown = sample
            .decode
            .saturating_add_signed(sample.offset)
            .saturating_sub(init.shift);
        let mut buffer = gst::Buffer::from_slice(data.to_vec());
        if let Some(buffer) = buffer.get_mut() {
            buffer.set_dts(gst::ClockTime::from_nseconds(nanos_of(decode, scale)));
            buffer.set_pts(gst::ClockTime::from_nseconds(nanos_of(shown, scale)));
            buffer.set_duration(gst::ClockTime::from_nseconds(nanos_of(
                u64::from(sample.duration),
                scale,
            )));
            if !sample.key {
                buffer.set_flags(gst::BufferFlags::DELTA_UNIT);
            }
        }
        loop {
            if !lane.room(seek, streamed) {
                return Pushed::Moved;
            }
            let pushing = lane.push.lock().unwrap_or_else(PoisonError::into_inner);
            if lane.place().seek != seek {
                return Pushed::Moved;
            }
            if lane.src.push_buffer(buffer.clone()).is_ok() {
                break;
            }
            drop(pushing);
            thread::sleep(Duration::from_millis(10));
        }
        lane.hungry.store(false, Ordering::SeqCst);
        lane.place.lock().unwrap_or_else(PoisonError::into_inner).pushed = true;
    }
    Pushed::All
}
