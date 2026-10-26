//! A call's media session: the microphone through Opus to the relayed peer, and the peer through the jitter buffer and
//! Opus to the speaker, on threads of its own; and once its video started, the camera through H.264 to the peer and
//! the peer's H.264 to a sink ([`video`]).
//!
//! Three threads for the audio, and two more for the video once it started, none of them the caller's (and never an
//! async runtime's):
//!
//! - **the capture thread** (`teams-media-mic`) reads the microphone, runs each 10 ms frame through the audio
//!   processing (echo cancellation, noise suppression, gain control: [`SessionConfig::processing`]), encodes every two
//!   frames into a 20 ms packet and queues it for the network thread;
//! - **the network thread** (`teams-media-net`) owns the [`RelayedPeer`]: it turns the relay and `str0m`, sends the
//!   queued packets, puts arriving ones into the jitter buffer, and runs the caller's SDP commands;
//! - **the playout thread** (`teams-media-spk`) keeps the speaker's pace (one 10 ms frame every 10 ms), taking from
//!   the jitter buffer what to decode, recover or conceal; each frame it hands the speaker is the one it hands the
//!   audio processing as the far end, so the echo canceller hears exactly what is played;
//! - **the decoding thread** (`teams-media-vid`), once the video started ([`MediaSession::start_video`]), decodes the
//!   far end's H.264 the network thread hands it and shows each video line's on that line's sink (a group call
//!   receives one participant's video on each of its receiving lines);
//! - **the camera thread** (`teams-media-cam`), while the camera is on ([`MediaSession::set_camera`]), encodes the
//!   camera's frames and queues them for the network thread, which sends them while our video line is negotiated to
//!   send.
//!
//! The handoffs are bounded both ways: the capture queue keeps the newest [`SessionConfig::send_queue`] packets (a
//! network thread that falls behind loses the backlog, not the present), and the jitter buffer holds at most its
//! capacity. No thread waits on another's device: a stuck speaker fills only the jitter buffer, and a stuck
//! microphone only stops what is sent.
//!
//! A session may start without its devices ([`MediaSession::start_without_devices`]): the network thread makes and
//! takes the SDP and connects the media, and the capture and playout threads start only once
//! [`MediaSession::open_devices`] opened the microphone and the speaker (a call's devices open only once the call
//! went).
//!
//! SDP crosses as `str0m`'s own (`teams_media::sdp` converts it to and from Teams' dialect); each command is answered
//! by the network thread within [`COMMAND_TIMEOUT`]. The encoder starts as [`SessionConfig::opus`] says and turns DTX
//! on or off as the far end's Opus line asks once its offer or answer is applied. [`MediaSession::close`] (or dropping
//! the session) hangs up the peer, releases the allocation, and joins every thread. A thread that fails (the relay,
//! `str0m`, the codec, the audio processing) reports [`SessionEvent::Failed`] and stops the session: the other threads
//! end, and the network thread says goodbye and releases the allocation as well as it still can on its way out. The
//! camera failing (its device gone: [`SessionEvent::DeviceClosed`]) or its encoder failing
//! ([`SessionEvent::VideoFailed`]) stops only the camera thread and turns the camera off; the session goes on.
//!
//! A group call's media also runs its control plane on the network thread ([`MediaSession::start_control`],
//! [`control`]): the source requests the caller wants ([`MediaSession::request_sources`]) go to the call's mixer over
//! the data channel, and what the mixer tells comes back as events (the dominant speakers). The levels of what the
//! microphone hears and of what the speaker plays are kept ([`MediaSession::levels`]): who speaks in a 1:1 call.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};

use crate::Error;
use crate::codec::{Decoder, Encoder, OpusConfig, Packet, packet_samples};
use crate::devices::{AudioFrame, AudioSink, AudioSource, Devices, FRAME_DURATION, FRAME_SAMPLES};
use crate::devices::{VideoSink, VideoSource};
use crate::jitter::{JitterBuffer, JitterConfig, JitterStats, Playout};
use crate::peer::ReceivedVideo;
use crate::peer::{PeerEvent, RelayedPeer};
use crate::processing::{AudioProcessing, ProcessingConfig};
use crate::queue::Queue;
use crate::video::codec::{EncoderSettings, H264Library, VideoDecoder, VideoEncoder};
use crate::video::convert::MaxSize;
use control::Control;
pub use control::SourceRequest;
use video::{CameraLoop, DecodeLoop, OutgoingVideo, VideoShared};
pub use video::{RemoteSinks, VideoParts, VideoStats};

mod control;
mod video;

/// How long the network thread may take to answer an SDP command.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the network thread waits for a packet to send before it turns the relay again.
const NET_WAIT: Duration = Duration::from_millis(1);
/// How long the capture thread waits for the microphone before it checks whether to stop.
const MIC_WAIT: Duration = Duration::from_millis(100);
/// How far the playout may fall behind its clock (a stalled speaker or scheduler) before it starts its clock again
/// instead of catching up in a burst.
const MAX_PLAYOUT_LAG: Duration = Duration::from_millis(200);
/// Events kept for a caller that doesn't read them; beyond it the newest are dropped.
const EVENT_QUEUE: usize = 64;
/// SDP commands waiting for the network thread.
const COMMAND_QUEUE: usize = 8;

/// How the session runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionConfig {
    /// How the encoder starts; DTX then follows the far end's Opus line (`usedtx=1`) once its SDP is applied.
    pub opus: OpusConfig,
    pub jitter: JitterConfig,
    /// Packets the capture thread may queue for the network thread (20 ms each); more drops the oldest.
    pub send_queue: usize,
    /// What the microphone's frames go through before they are encoded ([`ProcessingConfig::CALL`] by default;
    /// [`ProcessingConfig::OFF`] leaves them as they are).
    pub processing: ProcessingConfig,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            opus: OpusConfig::default(),
            jitter: JitterConfig::default(),
            send_queue: 10,
            processing: ProcessingConfig::CALL,
        }
    }
}

/// A device of the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionDevice {
    Microphone,
    Speaker,
    Camera,
}

/// What the session reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// ICE and DTLS are up (again).
    Connected,
    /// ICE lost the far end; it may come back.
    Disconnected,
    /// The far end closed the media. Final.
    Closed,
    /// The relay, `str0m`, the codec or the audio processing failed, and the session stopped: every thread ended, the
    /// peer said goodbye and released the allocation as well as it still could. Final.
    Failed(String),
    /// A device went away (or failed); its side of the media stopped, the rest goes on.
    DeviceClosed(SessionDevice),
    /// The camera's encoder failed: what the camera sends stopped, the rest goes on.
    VideoFailed(String),
    /// A group call's control plane is up: the mixer answered its handshake, naming what it sends and what it takes.
    ControlActive { send: Vec<String>, receive: Vec<String> },
    /// A source request went: numbered `sequence`, the line by its stream id, the source (`-1` for none).
    SourceRequested { sequence: u64, stream: u32, source: i64 },
    /// The mixer's answer to the source request numbered `sequence`.
    SourceResult { sequence: u64, result: String },
    /// A group call's mixer didn't answer the control plane's handshake: no source request goes.
    ControlTimedOut,
    /// Who spoke last in a group call, as its mixer tells it: the dominant speakers' audio source ids.
    DominantSpeakers(Vec<i64>),
}

/// How loud the call is on each side now: the RMS level (0 to 1) of the last 10 ms the microphone heard (after its
/// processing; 0 while muted) and of the last 10 ms the speaker played.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioLevels {
    pub near: f32,
    pub far: f32,
}

/// The RMS level of a frame's samples.
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let energy: f32 = samples.iter().map(|sample| sample * sample).sum();
    #[expect(clippy::cast_precision_loss, reason = "a frame's sample count is small")]
    let count = samples.len() as f32;
    (energy / count).sqrt()
}

/// The session's counts since it started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionStats {
    /// Packets the encoder made.
    pub encoded: u64,
    /// Packets handed to the peer.
    pub sent: u64,
    /// Packets never sent: the capture queue was full, or the media wasn't connected.
    pub unsent: u64,
    /// Audio packets that arrived.
    pub received: u64,
    /// Arrived packets that weren't readable Opus, or failed to decode.
    pub malformed: u64,
    /// 10 ms frames handed to the speaker.
    pub frames_played: u64,
    /// Packets in the jitter buffer now: the receiving side's delay, in packets.
    pub buffered: usize,
    /// The jitter buffer's counts.
    pub jitter: JitterStats,
    /// The video's counts.
    pub video: VideoStats,
}

#[derive(Default)]
struct Counters {
    encoded: AtomicU64,
    sent: AtomicU64,
    unsent: AtomicU64,
    received: AtomicU64,
    malformed: AtomicU64,
    frames_played: AtomicU64,
}

/// What the threads share.
struct Shared {
    stop: AtomicBool,
    /// The microphone is muted: the capture thread sends silence in its place.
    muted: AtomicBool,
    counters: Counters,
    jitter: Mutex<JitterBuffer>,
    /// The encoder's configuration once the far end's SDP changed it, for the capture thread to take.
    negotiated: Mutex<Option<OpusConfig>>,
    events: SyncSender<SessionEvent>,
    video: VideoShared,
    /// [`AudioLevels`], each an `f32`'s bits.
    near_level: AtomicU32,
    far_level: AtomicU32,
}

impl Shared {
    fn new(jitter: JitterConfig, events: SyncSender<SessionEvent>) -> Self {
        Self {
            stop: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            counters: Counters::default(),
            jitter: Mutex::new(JitterBuffer::new(jitter)),
            negotiated: Mutex::new(None),
            events,
            video: VideoShared::default(),
            near_level: AtomicU32::new(0),
            far_level: AtomicU32::new(0),
        }
    }

    fn levels(&self) -> AudioLevels {
        AudioLevels {
            near: f32::from_bits(self.near_level.load(Ordering::Relaxed)),
            far: f32::from_bits(self.far_level.load(Ordering::Relaxed)),
        }
    }

    fn jitter(&self) -> MutexGuard<'_, JitterBuffer> {
        self.jitter.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Reports `event`; dropped when nobody has read the last [`EVENT_QUEUE`] or the session is gone.
    fn report(&self, event: SessionEvent) {
        match self.events.try_send(event) {
            Ok(()) | Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {}
        }
    }

    /// Reports that a thread failed with `error` and stops the session: the other threads end at their next turn, the
    /// network thread hanging up on its way out.
    fn fail(&self, error: &Error) {
        self.report(SessionEvent::Failed(error.to_string()));
        self.stop.store(true, Ordering::Relaxed);
    }

    fn count(counter: &AtomicU64, by: u64) {
        counter.fetch_add(by, Ordering::Relaxed);
    }

    fn stats(&self) -> SessionStats {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let counters = &self.counters;
        let jitter = self.jitter();
        SessionStats {
            encoded: read(&counters.encoded),
            sent: read(&counters.sent),
            unsent: read(&counters.unsent),
            received: read(&counters.received),
            malformed: read(&counters.malformed),
            frames_played: read(&counters.frames_played),
            buffered: jitter.len(),
            jitter: jitter.stats(),
            video: self.video.stats(),
        }
    }

    fn negotiated(&self) -> MutexGuard<'_, Option<OpusConfig>> {
        self.negotiated.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A packet on its way from the capture thread to the network thread.
struct Outgoing {
    packet: Packet,
    captured: Instant,
}

enum Command {
    Offer(Sender<Result<SdpOffer, Error>>),
    Reoffer(Sender<Result<SdpOffer, Error>>),
    AcceptAnswer(SdpAnswer, Sender<Result<(), Error>>),
    AcceptOffer(SdpOffer, Sender<Result<SdpAnswer, Error>>),
    OfferVideo(bool, Sender<Result<Option<SdpOffer>, Error>>),
    OfferGroup(usize, Sender<Result<SdpOffer, Error>>),
    TakeSlots(Sender<Result<(), Error>>),
    StartControl(Option<u32>, Sender<Result<(), Error>>),
    RequestSources(Vec<SourceRequest>),
}

/// A running media session.
pub struct MediaSession {
    shared: Arc<Shared>,
    commands: SyncSender<Command>,
    /// Behind a lock so a session is shared between threads (`Sync`): an engine asks it from one thread and runs its
    /// SDP commands from another.
    events: Mutex<Receiver<SessionEvent>>,
    /// Behind a lock so the devices open on a shared session ([`MediaSession::open_devices`]).
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// What the capture and playout threads take once the devices open; `None` once they did.
    unopened: Mutex<Option<Unopened>>,
    /// The video's queues, and what its camera runs with once it started ([`MediaSession::start_video`]).
    video: Mutex<SessionVideo>,
}

/// The session's video: its queues, and once it started, what a camera runs with and the camera running now.
struct SessionVideo {
    /// Encoded frames, from the camera thread to the network thread.
    outgoing: Queue<OutgoingVideo>,
    /// Received access units, from the network thread to the decoding thread.
    incoming: Queue<ReceivedVideo>,
    /// What a camera runs with, once the video started.
    camera_parts: Option<CameraParts>,
    /// The camera thread running now, with its own stop.
    camera: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
    /// The session's video clock's start.
    epoch: Instant,
}

/// What each camera the session turns on runs with; the self view is handed from one camera thread to the next.
struct CameraParts {
    library: H264Library,
    encoder: EncoderSettings,
    max_size: MaxSize,
    preview: Arc<Mutex<Option<Box<dyn VideoSink>>>>,
}

/// The capture and playout threads' parts, made when the session starts, waiting for the devices.
struct Unopened {
    encoder: Encoder,
    decoder: Decoder,
    processing: Option<Arc<AudioProcessing>>,
    outgoing: Queue<Outgoing>,
}

impl MediaSession {
    /// Opens the microphone and the speaker of `devices` and starts the session's threads on `peer`: as
    /// [`MediaSession::start_without_devices`], then [`MediaSession::open_devices`]. Nothing is sent until the SDP is
    /// exchanged and the media connects.
    ///
    /// # Errors
    /// As those two's.
    pub fn start(peer: RelayedPeer, devices: &dyn Devices, config: SessionConfig) -> Result<Self, Error> {
        let session = Self::start_without_devices(peer, config)?;
        session.open_devices(devices)?;
        Ok(session)
    }

    /// Starts the session's network thread on `peer`, its devices not opened: the SDP and the media's connection run,
    /// nothing is captured, sent or played until [`MediaSession::open_devices`] (a call's offer or answer is made
    /// before the call goes, its devices opened only once it went).
    ///
    /// # Errors
    /// [`Error::Opus`] when libopus refuses the setup, [`Error::Processing`] when the audio processing can't be set
    /// up, [`Error::Io`] when the thread can't be started.
    pub fn start_without_devices(peer: RelayedPeer, config: SessionConfig) -> Result<Self, Error> {
        let encoder = Encoder::new(&config.opus)?;
        let decoder = Decoder::new()?;
        let processing = if config.processing.any() {
            Some(Arc::new(AudioProcessing::new(config.processing)?))
        } else {
            None
        };
        let (events_tx, events) = mpsc::sync_channel(EVENT_QUEUE);
        let (commands, commands_rx) = mpsc::sync_channel(COMMAND_QUEUE);
        let shared = Arc::new(Shared::new(config.jitter, events_tx));
        let outgoing = Queue::new(config.send_queue.max(1));
        let video_out = Queue::new(video::SEND_QUEUE);
        let video_in = Queue::new(video::DECODE_QUEUE);
        let session = Self {
            shared: Arc::clone(&shared),
            commands,
            events: Mutex::new(events),
            threads: Mutex::new(Vec::with_capacity(3)),
            unopened: Mutex::new(Some(Unopened {
                encoder,
                decoder,
                processing,
                outgoing: outgoing.clone(),
            })),
            video: Mutex::new(SessionVideo {
                outgoing: video_out.clone(),
                incoming: video_in.clone(),
                camera_parts: None,
                camera: None,
                epoch: Instant::now(),
            }),
        };
        let network = Network {
            peer,
            shared,
            commands: commands_rx,
            outgoing,
            pending: None,
            opus: config.opus,
            opus_pending: false,
            video_out,
            video_in,
            was_sending: false,
            desired: 0,
            control: None,
        };
        session.spawn("teams-media-net", move || network.run())?;
        Ok(session)
    }

    /// Opens the microphone and the speaker of `devices` and starts the capture and playout threads; once only (a
    /// later call opens nothing).
    ///
    /// # Errors
    /// [`Error::Device`] when a device can't be opened (the session goes on without them, and they may be opened
    /// again), [`Error::Io`] when a thread can't be started.
    pub fn open_devices(&self, devices: &dyn Devices) -> Result<(), Error> {
        let mut unopened = lock(&self.unopened);
        if unopened.is_none() {
            return Ok(());
        }
        let microphone = devices.microphone()?;
        let speaker = devices.speaker()?;
        let Some(parts) = unopened.take() else {
            return Ok(());
        };
        let capture = Capture {
            microphone,
            encoder: parts.encoder,
            processing: parts.processing.clone(),
            outgoing: parts.outgoing,
            shared: Arc::clone(&self.shared),
        };
        self.spawn("teams-media-mic", move || capture.run())?;
        let playout = PlayoutLoop {
            speaker,
            decoder: parts.decoder,
            processing: parts.processing,
            shared: Arc::clone(&self.shared),
            pcm: Vec::with_capacity(crate::codec::MAX_PACKET_SAMPLES + FRAME_SAMPLES),
        };
        self.spawn("teams-media-spk", move || playout.run())?;
        Ok(())
    }

    fn spawn(&self, name: &str, run: impl FnOnce() + Send + 'static) -> Result<(), Error> {
        let handle = thread::Builder::new().name(name.to_owned()).spawn(run)?;
        lock(&self.threads).push(handle);
        Ok(())
    }

    /// Offers one send-and-receive audio line; the answer goes to [`MediaSession::accept_answer`].
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` fails, [`Error::Timeout`] or [`Error::SessionEnded`] when the network thread doesn't
    /// answer.
    pub fn offer_audio(&self) -> Result<SdpOffer, Error> {
        self.request(Command::Offer)
    }

    /// Offers the running line again with fresh ICE credentials (an ICE restart, [`crate::peer::Peer::reoffer`]): a
    /// renegotiation of the call's media; the answer goes to [`MediaSession::accept_answer`].
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` fails, [`Error::Timeout`] or [`Error::SessionEnded`] when the network thread doesn't
    /// answer.
    pub fn reoffer(&self) -> Result<SdpOffer, Error> {
        self.request(Command::Reoffer)
    }

    /// Offers a group call's media ([`crate::peer::Peer::offer_group`]: audio, the main video line, `slots` receiving
    /// video lines, the sharing line and the data channel); the answer goes to [`MediaSession::accept_answer`].
    ///
    /// # Errors
    /// As [`MediaSession::offer_audio`]'s.
    pub fn offer_group(&self, slots: usize) -> Result<SdpOffer, Error> {
        self.request(|reply| Command::OfferGroup(slots, reply))
    }

    /// From the far end's next offer on, the video lines after its main one are receiving lines (a group call's
    /// mixer's offer, [`crate::peer::Peer::take_slots`]).
    ///
    /// # Errors
    /// [`Error::Timeout`] or [`Error::SessionEnded`] when the network thread doesn't answer.
    pub fn take_slots(&self) -> Result<(), Error> {
        self.request(Command::TakeSlots)
    }

    /// Starts a group call's control plane, sending as `source` (the call's data line's stream id, when known): the
    /// data channel opened on the far end's data line (or the one our group offer made), its handshake once it opens.
    /// Once only (a later call changes nothing).
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` fails, [`Error::Timeout`] or [`Error::SessionEnded`] when the network thread doesn't
    /// answer.
    pub fn start_control(&self, source: Option<u32>) -> Result<(), Error> {
        self.request(|reply| Command::StartControl(source, reply))
    }

    /// Which participant's video to receive on which receiving line ([`SourceRequest`]s, the lines by their stream
    /// ids): sent to the mixer once its control plane is up, each line only when it changed (a line left out gets no
    /// source). It doesn't wait for the network thread (an engine asks it from its own thread).
    ///
    /// # Errors
    /// [`Error::Timeout`] when the network thread's commands are full (the next request carries what this one would
    /// have), [`Error::SessionEnded`] when it is gone.
    pub fn request_sources(&self, wanted: Vec<SourceRequest>) -> Result<(), Error> {
        match self.commands.try_send(Command::RequestSources(wanted)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(Error::Timeout("the media session's network thread")),
            Err(TrySendError::Disconnected(_)) => Err(Error::SessionEnded),
        }
    }

    /// How loud the call is on each side now ([`AudioLevels`]).
    #[must_use]
    pub fn levels(&self) -> AudioLevels {
        self.shared.levels()
    }

    /// Starts the session's video ([`VideoParts`]): the decoding thread, showing what the far end sends on `remote`,
    /// and what each camera turned on later runs with ([`MediaSession::set_camera`]). Once only (a later call starts
    /// nothing).
    ///
    /// # Errors
    /// [`Error::Video`] when the codec's library doesn't load, [`Error::Io`] when the thread can't be started.
    pub fn start_video(&self, parts: VideoParts) -> Result<(), Error> {
        let mut video = lock(&self.video);
        if video.camera_parts.is_some() {
            return Ok(());
        }
        // the library loads (or the video doesn't start): each line's decoder is made as its first frame comes
        drop(VideoDecoder::new(&parts.library)?);
        let decoding = DecodeLoop {
            library: parts.library.clone(),
            sinks: parts.remote,
            incoming: video.incoming.clone(),
            shared: Arc::clone(&self.shared),
            lines: HashMap::new(),
        };
        self.spawn("teams-media-vid", move || decoding.run())?;
        video.camera_parts = Some(CameraParts {
            library: parts.library,
            encoder: parts.encoder,
            max_size: parts.max_size,
            preview: Arc::new(Mutex::new(parts.preview)),
        });
        self.shared.video.started.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Turns the camera on with `camera` (its frames encoded and sent while our video line is negotiated to send), or
    /// off with `None` (its thread stopped and joined; nothing more is sent). A camera already on is replaced.
    ///
    /// # Errors
    /// [`Error::Video`] before the video started or when the encoder can't be set up, [`Error::Io`] when the thread
    /// can't be started.
    pub fn set_camera(&self, camera: Option<Box<dyn VideoSource>>) -> Result<(), Error> {
        let mut video = lock(&self.video);
        if let Some((stop, thread)) = video.camera.take() {
            stop.store(true, Ordering::Relaxed);
            let _ = thread.join();
        }
        let shared = &self.shared.video;
        shared.camera_on.store(false, Ordering::Relaxed);
        let Some(camera) = camera else {
            return Ok(());
        };
        let parts = video
            .camera_parts
            .as_ref()
            .ok_or_else(|| Error::Video("the session's video hasn't started".into()))?;
        let encoder = VideoEncoder::new(&parts.library, parts.encoder)?;
        let stop = Arc::new(AtomicBool::new(false));
        let camera_loop = CameraLoop {
            camera,
            encoder,
            max_size: parts.max_size,
            preview: Arc::clone(&parts.preview),
            outgoing: video.outgoing.clone(),
            shared: Arc::clone(&self.shared),
            stop: Arc::clone(&stop),
            epoch: video.epoch,
        };
        shared.desired.store(
            u64::from(parts.encoder.max_bitrate) + video::AUDIO_HEADROOM,
            Ordering::Relaxed,
        );
        let thread = video::spawn("teams-media-cam", move || camera_loop.run())?;
        shared.camera_on.store(true, Ordering::Relaxed);
        video.camera = Some((stop, thread));
        Ok(())
    }

    /// Whether the camera is on ([`MediaSession::set_camera`]).
    #[must_use]
    pub fn camera_on(&self) -> bool {
        self.shared.video.camera_on.load(Ordering::Relaxed)
    }

    /// Offers the session again with our video line sending (`true`, the camera on) or only receiving
    /// ([`crate::peer::Peer::offer_video`]); the answer goes to [`MediaSession::accept_answer`]. `None` when the line
    /// already goes that way: nothing to offer.
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` fails, [`Error::Timeout`] or [`Error::SessionEnded`] when the network thread doesn't
    /// answer.
    pub fn offer_video(&self, sending: bool) -> Result<Option<SdpOffer>, Error> {
        self.request(|reply| Command::OfferVideo(sending, reply))
    }

    /// Mutes (`true`) or unmutes the microphone: while muted the capture thread encodes and sends silence in place of
    /// what it hears (the line and its timing go on), and the device stays open.
    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }

    /// Whether the microphone is muted ([`MediaSession::set_muted`]).
    #[must_use]
    pub fn is_muted(&self) -> bool {
        self.shared.muted.load(Ordering::Relaxed)
    }

    /// Applies the far end's answer to our last offer.
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` refuses it or no offer is pending, [`Error::Timeout`] or [`Error::SessionEnded`] when
    /// the network thread doesn't answer.
    pub fn accept_answer(&self, answer: SdpAnswer) -> Result<(), Error> {
        self.request(|reply| Command::AcceptAnswer(answer, reply))
    }

    /// Answers the far end's offer.
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` refuses it, [`Error::Timeout`] or [`Error::SessionEnded`] when the network thread
    /// doesn't answer.
    pub fn accept_offer(&self, offer: SdpOffer) -> Result<SdpAnswer, Error> {
        self.request(|reply| Command::AcceptOffer(offer, reply))
    }

    fn request<T>(&self, command: impl FnOnce(Sender<Result<T, Error>>) -> Command) -> Result<T, Error> {
        let (reply, answer) = mpsc::channel();
        self.commands.send(command(reply)).map_err(|_| Error::SessionEnded)?;
        match answer.recv_timeout(COMMAND_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(Error::Timeout("the media session's network thread")),
            Err(RecvTimeoutError::Disconnected) => Err(Error::SessionEnded),
        }
    }

    /// The next event, waiting up to `timeout`.
    #[must_use]
    pub fn next_event(&self, timeout: Duration) -> Option<SessionEvent> {
        let events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        events.recv_timeout(timeout).ok()
    }

    #[must_use]
    pub fn stats(&self) -> SessionStats {
        self.shared.stats()
    }

    /// Hangs up: the peer says goodbye, the allocation is released, every thread is joined. Returns the final counts.
    #[must_use]
    pub fn close(mut self) -> SessionStats {
        self.stop();
        self.shared.stats()
    }

    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let camera = lock(&self.video).camera.take();
        if let Some((stop, thread)) = camera {
            stop.store(true, Ordering::Relaxed);
            let _ = thread.join();
        }
        let threads = std::mem::take(self.threads.get_mut().unwrap_or_else(PoisonError::into_inner));
        for handle in threads {
            // a thread that panicked has nothing left to stop
            let _ = handle.join();
        }
    }
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The network thread.
struct Network {
    peer: RelayedPeer,
    shared: Arc<Shared>,
    commands: Receiver<Command>,
    outgoing: Queue<Outgoing>,
    pending: Option<SdpPendingOffer>,
    /// The encoder's starting configuration, which the far end's SDP changes only in DTX.
    opus: OpusConfig,
    /// The far end's SDP was applied and its Opus line not yet read.
    opus_pending: bool,
    /// Encoded frames from the camera thread.
    video_out: Queue<OutgoingVideo>,
    /// Received access units for the decoding thread.
    video_in: Queue<ReceivedVideo>,
    /// Our video line was negotiated to send at the last turn.
    was_sending: bool,
    /// The bitrate the bandwidth estimate was last asked to look for.
    desired: u64,
    /// A group call's control plane, once started.
    control: Option<Control>,
}

impl Network {
    fn run(mut self) {
        while !self.shared.stopping() {
            if let Err(error) = self.turn() {
                self.shared.fail(&error);
                break;
            }
        }
        self.hang_up();
    }

    fn turn(&mut self) -> Result<(), Error> {
        while let Ok(command) = self.commands.try_recv() {
            self.command(command);
        }
        let now = Instant::now();
        self.peer.turn(now)?;
        while let Some(event) = self.peer.peer().poll_event() {
            match (event, &mut self.control) {
                (PeerEvent::ChannelData(data), Some(control)) => {
                    control.receive(&data, self.peer.peer(), &self.shared)?;
                }
                (event, _) => self.on_event(event),
            }
        }
        if let Some(control) = &mut self.control {
            control.turn(now, self.peer.peer(), &self.shared)?;
            self.peer.flush(now)?;
        }
        self.negotiate_opus();
        self.video_turn()?;
        match self.outgoing.recv(NET_WAIT) {
            Ok(Some(first)) => {
                self.send(&first)?;
                while let Ok(Some(next)) = self.outgoing.recv(Duration::ZERO) {
                    self.send(&next)?;
                }
            }
            Ok(None) => {}
            // the microphone is gone: keep receiving, at the same pace
            Err(_) => thread::sleep(NET_WAIT),
        }
        Ok(())
    }

    fn command(&mut self, command: Command) {
        let peer = self.peer.peer();
        // a caller that stopped waiting has nothing to hear: an answer it can't take is dropped
        match command {
            Command::Offer(reply) => {
                let offer = peer.offer_audio().map(|(offer, pending)| {
                    self.pending = Some(pending);
                    offer
                });
                let _ = reply.send(offer);
            }
            Command::Reoffer(reply) => {
                let offer = peer.reoffer().map(|(offer, pending)| {
                    self.pending = Some(pending);
                    offer
                });
                let _ = reply.send(offer);
            }
            Command::AcceptAnswer(answer, reply) => {
                let result = match self.pending.take() {
                    Some(pending) => peer.accept_answer(pending, answer),
                    None => Err(Error::Rtc("no offer is waiting for an answer".into())),
                };
                self.opus_pending |= result.is_ok();
                let _ = reply.send(result);
            }
            Command::AcceptOffer(offer, reply) => {
                let result = peer.accept_offer(offer);
                self.opus_pending |= result.is_ok();
                let _ = reply.send(result);
            }
            Command::OfferVideo(sending, reply) => {
                let offer = peer.offer_video(sending).map(|offered| {
                    offered.map(|(offer, pending)| {
                        self.pending = Some(pending);
                        offer
                    })
                });
                let _ = reply.send(offer);
            }
            Command::OfferGroup(slots, reply) => {
                let offer = peer.offer_group(slots).map(|(offer, pending)| {
                    self.pending = Some(pending);
                    offer
                });
                let _ = reply.send(offer);
            }
            Command::TakeSlots(reply) => {
                peer.take_slots();
                let _ = reply.send(Ok(()));
            }
            Command::StartControl(source, reply) => {
                let result = peer.open_channel();
                if result.is_ok() && self.control.is_none() {
                    self.control = Some(Control::new(source));
                }
                let _ = reply.send(result);
            }
            Command::RequestSources(wanted) => {
                // without a control plane there is nobody to ask: a 1:1 call's media has none
                if let Some(control) = &mut self.control
                    && let Err(error) = control.request(wanted, peer, &self.shared)
                {
                    self.shared.fail(&error);
                }
            }
        }
    }

    /// The video's part of a turn: the bitrate the estimate looks for, a keyframe the decoding thread wants, and the
    /// camera's encoded frames sent while our line is negotiated to send (the first one after it began to send asked
    /// to be an IDR).
    fn video_turn(&mut self) -> Result<(), Error> {
        let video = &self.shared.video;
        let desired = if video.camera_on.load(Ordering::Relaxed) {
            video.desired.load(Ordering::Relaxed)
        } else {
            video::AUDIO_HEADROOM
        };
        let peer = self.peer.peer();
        if desired != self.desired {
            self.desired = desired;
            peer.set_desired_bitrate(desired);
        }
        let needed = video.keyframe_needed.swap(0, Ordering::Relaxed);
        for line in (0..64).filter(|line| needed & (1 << line) != 0) {
            if peer.request_keyframe(line) {
                VideoShared::count(&video.keyframes_requested, 1);
            }
        }
        let sending = peer.is_connected() && peer.video_sending();
        if sending && !self.was_sending {
            video.keyframe_wanted.store(true, Ordering::Relaxed);
        }
        self.was_sending = sending;
        let now = Instant::now();
        while let Ok(Some(frame)) = self.video_out.recv(Duration::ZERO) {
            let peer = self.peer.peer();
            // a frame `str0m` won't take (our line's new direction offered and not yet answered: no sending stream
            // yet) isn't sent; the call's audio goes on
            if peer
                .write_video(now, frame.captured, frame.rtp_time, &frame.frame.data)
                .is_ok()
            {
                VideoShared::count(&video.sent, 1);
            } else {
                VideoShared::count(&video.unsent, 1);
            }
            self.peer.flush(now)?;
        }
        Ok(())
    }

    /// Once the far end's SDP was applied and the audio line is known (at once for our offer, once the media connects
    /// for theirs), hands the capture thread the encoder's configuration with DTX as the far end's Opus line asks.
    fn negotiate_opus(&mut self) {
        let peer = self.peer.peer();
        if !self.opus_pending || peer.audio_mid().is_none() {
            return;
        }
        self.opus_pending = false;
        if let Some(remote) = peer.remote_opus() {
            *self.shared.negotiated() = Some(OpusConfig {
                dtx: remote.dtx,
                ..self.opus
            });
        }
    }

    fn on_event(&self, event: PeerEvent) {
        let video = &self.shared.video;
        match event {
            PeerEvent::Connected => self.shared.report(SessionEvent::Connected),
            PeerEvent::Disconnected => self.shared.report(SessionEvent::Disconnected),
            PeerEvent::Closed => self.shared.report(SessionEvent::Closed),
            PeerEvent::Audio(audio) => {
                Shared::count(&self.shared.counters.received, 1);
                match packet_samples(&audio.payload) {
                    Some(samples) => {
                        let ticks = u64::try_from(samples).unwrap_or(u64::MAX);
                        self.shared.jitter().push(audio.rtp_time, ticks, audio.payload);
                    }
                    None => Shared::count(&self.shared.counters.malformed, 1),
                }
            }
            PeerEvent::Video(unit) => {
                VideoShared::count(&video.received, 1);
                let line = unit.line;
                // nobody decodes before the video started: what comes is dropped
                if video.started.load(Ordering::Relaxed) && self.video_in.push(unit) > 0 {
                    video.want_keyframe(line);
                }
            }
            PeerEvent::KeyframeRequested => {
                VideoShared::count(&video.keyframes_asked, 1);
                video.keyframe_wanted.store(true, Ordering::Relaxed);
            }
            PeerEvent::Bitrate(bps) => video.estimate.store(bps, Ordering::Relaxed),
            // a data channel with no control plane started carries nothing the session reads
            PeerEvent::ChannelOpen | PeerEvent::ChannelData(_) => {}
        }
    }

    fn send(&mut self, outgoing: &Outgoing) -> Result<(), Error> {
        let counters = &self.shared.counters;
        let now = Instant::now();
        let peer = self.peer.peer();
        if !peer.is_connected() {
            Shared::count(&counters.unsent, 1);
            return Ok(());
        }
        match peer.write_audio(
            now,
            outgoing.captured,
            outgoing.packet.rtp_time,
            &outgoing.packet.payload,
        ) {
            Ok(()) => Shared::count(&counters.sent, 1),
            Err(Error::NotConnected) => Shared::count(&counters.unsent, 1),
            Err(error) => return Err(error),
        }
        self.peer.flush(now)
    }

    /// Says goodbye (RTCP BYE, DTLS close) and releases the allocation, best effort: the session ends either way.
    fn hang_up(&mut self) {
        let now = Instant::now();
        if self.peer.peer().close().is_ok() {
            let _ = self.peer.flush(now);
        }
        let _ = self.peer.link().close(now);
    }
}

/// The capture thread.
struct Capture {
    microphone: Box<dyn AudioSource>,
    encoder: Encoder,
    /// The near end's processing, shared with the playout thread, which feeds it the far end.
    processing: Option<Arc<AudioProcessing>>,
    outgoing: Queue<Outgoing>,
    shared: Arc<Shared>,
}

impl Capture {
    fn run(mut self) {
        while !self.shared.stopping() {
            match self.microphone.next_frame(MIC_WAIT) {
                Ok(Some(frame)) => {
                    if let Err(error) = self.encode(frame) {
                        self.shared.fail(&error);
                        break;
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    self.shared
                        .report(SessionEvent::DeviceClosed(SessionDevice::Microphone));
                    break;
                }
            }
        }
        self.outgoing.close();
    }

    fn encode(&mut self, mut frame: AudioFrame) -> Result<(), Error> {
        let captured = Instant::now();
        if let Some(processing) = &self.processing {
            processing.near_end(&mut frame)?;
        }
        if self.shared.muted.load(Ordering::Relaxed) {
            frame = AudioFrame::silence();
        }
        self.shared
            .near_level
            .store(rms(frame.samples()).to_bits(), Ordering::Relaxed);
        let negotiated = self.shared.negotiated().take();
        if let Some(config) = negotiated {
            self.encoder.configure(&config)?;
        }
        if let Some(packet) = self.encoder.push(&frame)? {
            let counters = &self.shared.counters;
            Shared::count(&counters.encoded, 1);
            let dropped = self.outgoing.push(Outgoing { packet, captured });
            Shared::count(&counters.unsent, u64::try_from(dropped).unwrap_or(u64::MAX));
        }
        Ok(())
    }
}

/// The playout thread.
struct PlayoutLoop {
    speaker: Box<dyn AudioSink>,
    decoder: Decoder,
    /// The audio processing the capture thread runs, which hears every played frame as the far end.
    processing: Option<Arc<AudioProcessing>>,
    shared: Arc<Shared>,
    /// Decoded samples not yet played.
    pcm: Vec<f32>,
}

impl PlayoutLoop {
    fn run(mut self) {
        let mut start = Instant::now();
        let mut played: u32 = 0;
        while !self.shared.stopping() {
            let due = start + FRAME_DURATION * played;
            let now = Instant::now();
            if due > now {
                thread::sleep(due - now);
                continue;
            }
            if now - due > MAX_PLAYOUT_LAG {
                start = now;
                played = 0;
            }
            let frame = self.next_frame();
            if let Some(processing) = &self.processing
                && let Err(error) = processing.far_end(&frame)
            {
                self.shared.fail(&error);
                return;
            }
            self.shared
                .far_level
                .store(rms(frame.samples()).to_bits(), Ordering::Relaxed);
            if self.speaker.play(&frame).is_err() {
                self.shared.report(SessionEvent::DeviceClosed(SessionDevice::Speaker));
                return;
            }
            Shared::count(&self.shared.counters.frames_played, 1);
            played = played.saturating_add(1);
        }
    }

    /// The next 10 ms to play, decoding (or recovering, or concealing) as much as it takes.
    fn next_frame(&mut self) -> AudioFrame {
        while self.pcm.len() < FRAME_SAMPLES {
            let playout = self.shared.jitter().pop();
            let decoded = match playout {
                Playout::Packet(packet) => self.decoder.decode(&packet, &mut self.pcm),
                Playout::Recover { next, samples } => self.decoder.recover(&next, samples, &mut self.pcm),
                Playout::Conceal { samples } => self.decoder.conceal(samples, &mut self.pcm),
                Playout::Silence => {
                    self.pcm.resize(self.pcm.len() + FRAME_SAMPLES, 0.0);
                    Ok(FRAME_SAMPLES)
                }
            };
            if decoded.is_err() {
                Shared::count(&self.shared.counters.malformed, 1);
                self.pcm.resize(self.pcm.len() + FRAME_SAMPLES, 0.0);
            }
        }
        let frame = AudioFrame::from_samples(&self.pcm[..FRAME_SAMPLES]).unwrap_or_else(AudioFrame::silence);
        self.pcm.drain(..FRAME_SAMPLES);
        frame
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_that_fails_reports_it_and_stops_the_session() {
        let (events, reported) = mpsc::sync_channel(EVENT_QUEUE);
        let shared = Shared::new(JitterConfig::default(), events);
        shared.fail(&Error::Processing("a refused frame".into()));
        assert_eq!(
            reported.try_recv(),
            Ok(SessionEvent::Failed("audio processing: a refused frame".into()))
        );
        assert!(shared.stopping(), "the other threads go on");
    }
}
