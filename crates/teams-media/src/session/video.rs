//! The session's video: the camera thread (`teams-media-cam`) and the decoding thread (`teams-media-vid`), and what
//! they share with the network thread.
//!
//! - **The camera thread** runs while the camera is on ([`super::MediaSession::set_camera`]): each frame turned into
//!   I420 at the size the call sends ([`crate::video::convert::to_i420`]), shown to the self view, encoded at the
//!   target bitrate (an IDR when one is wanted: the far end asked, or our line just began to send) and queued for the
//!   network thread, which keeps the newest [`SEND_QUEUE`] frames and sends them while our line is negotiated to send.
//!   A camera that fails (`DeviceClosed(Camera)`) or whose encoder fails (`VideoFailed`) ends the thread and turns
//!   the camera off; the rest of the session goes on.
//! - **The decoding thread** runs once the session's video started ([`super::MediaSession::start_video`]): each
//!   access unit the network thread hands over decoded and shown on the sink; a gap or a frame that doesn't decode
//!   asks the far end for a keyframe (at most every [`KEYFRAME_ASK_EVERY`]) and nothing is shown until one comes.
//! - **The bitrate** follows the bandwidth estimate: the network thread keeps the newest estimate, the camera thread
//!   sets the encoder to it less what the audio takes ([`AUDIO_HEADROOM`]), between the encoder's minimum and maximum.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::Error;
use crate::devices::{VideoSink, VideoSource};
use crate::peer::ReceivedVideo;
use crate::queue::Queue;
use crate::video::VIDEO_CLOCK;
use crate::video::codec::{EncodedVideo, EncoderSettings, H264Library, VideoDecoder, VideoEncoder};
use crate::video::convert::{MaxSize, to_i420};

use super::{SessionDevice, SessionEvent, Shared};

/// Encoded frames the network thread may hold before it sends them; more drops the oldest.
pub(super) const SEND_QUEUE: usize = 3;
/// Received access units the decoding thread may hold; more drops the oldest (and asks for a keyframe).
pub(super) const DECODE_QUEUE: usize = 30;
/// How long the camera and decoding threads wait for a frame before they check whether to stop.
const FRAME_WAIT: Duration = Duration::from_millis(100);
/// The least time between two keyframe requests of the decoding thread.
pub(super) const KEYFRAME_ASK_EVERY: Duration = Duration::from_millis(500);
/// What the estimate leaves to the audio, in bits per second.
pub(super) const AUDIO_HEADROOM: u64 = 64_000;

/// A call's video, as [`super::MediaSession::start_video`] takes it.
pub struct VideoParts {
    /// The OpenH264 library the encoder and decoder run on.
    pub library: H264Library,
    /// How the camera's frames are encoded.
    pub encoder: EncoderSettings,
    /// The largest picture sent: a bigger camera frame is scaled down to fit.
    pub max_size: MaxSize,
    /// Where the far end's decoded frames go (I420).
    pub remote: Box<dyn VideoSink>,
    /// Where our camera's frames go as they are sent (I420): the self view; `None` for none.
    pub preview: Option<Box<dyn VideoSink>>,
}

impl std::fmt::Debug for VideoParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoParts")
            .field("library", &self.library)
            .field("encoder", &self.encoder)
            .field("max_size", &self.max_size)
            .finish_non_exhaustive()
    }
}

/// The video's counts since the session started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoStats {
    /// Frames the camera handed over.
    pub captured: u64,
    /// Frames the encoder made.
    pub encoded: u64,
    /// IDRs among them.
    pub keyframes: u64,
    /// Encoded frames handed to the peer.
    pub sent: u64,
    /// Encoded frames never sent: our line wasn't negotiated to send, the media wasn't connected, or the queue was full.
    pub unsent: u64,
    /// Access units that arrived.
    pub received: u64,
    /// Pictures decoded and shown.
    pub shown: u64,
    /// Access units that didn't decode, or that were skipped while a keyframe was awaited.
    pub undecoded: u64,
    /// Keyframe requests the far end sent us.
    pub keyframes_asked: u64,
    /// Keyframe requests we sent.
    pub keyframes_requested: u64,
    /// The newest bandwidth estimate, in bits per second (0 before one).
    pub estimate: u64,
    /// The encoder's target bitrate now, in bits per second (0 while no camera runs).
    pub bitrate: u64,
}

/// What the video threads share with the network thread.
#[derive(Default)]
pub(super) struct VideoShared {
    /// The video started: received access units go to the decoding thread.
    pub(super) started: AtomicBool,
    /// The bitrate the estimate is to look for while the camera is on.
    pub(super) desired: AtomicU64,
    /// The camera is on (its thread runs).
    pub(super) camera_on: AtomicBool,
    /// The next frame encoded is to be an IDR.
    pub(super) keyframe_wanted: AtomicBool,
    /// The decoding thread wants a keyframe of the far end (the network thread asks).
    pub(super) keyframe_needed: AtomicBool,
    pub(super) captured: AtomicU64,
    pub(super) encoded: AtomicU64,
    pub(super) keyframes: AtomicU64,
    pub(super) sent: AtomicU64,
    pub(super) unsent: AtomicU64,
    pub(super) received: AtomicU64,
    pub(super) shown: AtomicU64,
    pub(super) undecoded: AtomicU64,
    pub(super) keyframes_asked: AtomicU64,
    pub(super) keyframes_requested: AtomicU64,
    pub(super) estimate: AtomicU64,
    pub(super) bitrate: AtomicU64,
}

impl VideoShared {
    pub(super) fn stats(&self) -> VideoStats {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        VideoStats {
            captured: read(&self.captured),
            encoded: read(&self.encoded),
            keyframes: read(&self.keyframes),
            sent: read(&self.sent),
            unsent: read(&self.unsent),
            received: read(&self.received),
            shown: read(&self.shown),
            undecoded: read(&self.undecoded),
            keyframes_asked: read(&self.keyframes_asked),
            keyframes_requested: read(&self.keyframes_requested),
            estimate: read(&self.estimate),
            bitrate: read(&self.bitrate),
        }
    }

    pub(super) fn count(counter: &AtomicU64, by: u64) {
        counter.fetch_add(by, Ordering::Relaxed);
    }
}

/// An encoded frame on its way from the camera thread to the network thread.
pub(super) struct OutgoingVideo {
    pub(super) frame: EncodedVideo,
    pub(super) rtp_time: u64,
    pub(super) captured: Instant,
}

/// The camera thread.
pub(super) struct CameraLoop {
    pub(super) camera: Box<dyn VideoSource>,
    pub(super) encoder: VideoEncoder,
    pub(super) max_size: MaxSize,
    pub(super) preview: Arc<Mutex<Option<Box<dyn VideoSink>>>>,
    pub(super) outgoing: Queue<OutgoingVideo>,
    pub(super) shared: Arc<Shared>,
    /// This camera's own stop (the session's stop ends it too).
    pub(super) stop: Arc<AtomicBool>,
    /// The session's video clock's start: RTP times count from it.
    pub(super) epoch: Instant,
}

impl CameraLoop {
    pub(super) fn run(mut self) {
        let shared = Arc::clone(&self.shared);
        let video = &shared.video;
        video.keyframe_wanted.store(true, Ordering::Relaxed);
        while !self.shared.stopping() && !self.stop.load(Ordering::Relaxed) {
            match self.camera.next_frame(FRAME_WAIT) {
                Ok(Some(frame)) => {
                    VideoShared::count(&video.captured, 1);
                    if let Err(error) = self.encode(&frame) {
                        video.camera_on.store(false, Ordering::Relaxed);
                        self.shared.report(SessionEvent::VideoFailed(error.to_string()));
                        break;
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    video.camera_on.store(false, Ordering::Relaxed);
                    self.shared.report(SessionEvent::DeviceClosed(SessionDevice::Camera));
                    break;
                }
            }
        }
        video.bitrate.store(0, Ordering::Relaxed);
    }

    fn encode(&mut self, frame: &crate::devices::VideoFrame) -> Result<(), Error> {
        let captured = Instant::now();
        let Some(frame) = to_i420(frame, self.max_size) else {
            return Ok(());
        };
        if let Some(preview) = self.preview.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            // a self view that can't take a frame misses it; the call goes on
            let _ = preview.show(&frame);
        }
        let video = &self.shared.video;
        let estimate = video.estimate.load(Ordering::Relaxed);
        if estimate > 0 {
            self.encoder.set_bitrate(estimate.saturating_sub(AUDIO_HEADROOM));
        }
        video
            .bitrate
            .store(u64::from(self.encoder.bitrate()), Ordering::Relaxed);
        let keyframe = video.keyframe_wanted.swap(false, Ordering::Relaxed);
        let at = captured.saturating_duration_since(self.epoch);
        let Some(encoded) = self.encoder.encode(&frame, at, keyframe)? else {
            return Ok(());
        };
        VideoShared::count(&video.encoded, 1);
        VideoShared::count(&video.keyframes, u64::from(encoded.keyframe));
        let rtp_time = rtp_time(self.epoch, captured);
        let dropped = self.outgoing.push(OutgoingVideo {
            frame: encoded,
            rtp_time,
            captured,
        });
        VideoShared::count(&video.unsent, u64::try_from(dropped).unwrap_or(u64::MAX));
        Ok(())
    }
}

/// The 90 kHz media time of `at`, counted from `epoch`.
fn rtp_time(epoch: Instant, at: Instant) -> u64 {
    let micros = at.saturating_duration_since(epoch).as_micros();
    u64::try_from(micros * u128::from(VIDEO_CLOCK) / 1_000_000).unwrap_or(u64::MAX)
}

/// The decoding thread.
pub(super) struct DecodeLoop {
    pub(super) decoder: VideoDecoder,
    pub(super) sink: Box<dyn VideoSink>,
    pub(super) incoming: Queue<ReceivedVideo>,
    pub(super) shared: Arc<Shared>,
    /// Waiting for a keyframe: what comes before it isn't decoded.
    pub(super) broken: bool,
    pub(super) asked: Option<Instant>,
}

impl DecodeLoop {
    pub(super) fn run(mut self) {
        while !self.shared.stopping() {
            match self.incoming.recv(FRAME_WAIT) {
                Ok(Some(unit)) => self.decode(&unit),
                Ok(None) => {}
                Err(_) => break,
            }
        }
    }

    fn decode(&mut self, unit: &ReceivedVideo) {
        let video = &self.shared.video;
        if !unit.contiguous && !unit.keyframe {
            self.broken = true;
        }
        if unit.keyframe {
            self.broken = false;
        }
        if self.broken {
            VideoShared::count(&video.undecoded, 1);
            self.ask_keyframe();
            return;
        }
        match self.decoder.decode(&unit.data) {
            Ok(Some(frame)) => {
                if self.sink.show(&frame).is_ok() {
                    VideoShared::count(&video.shown, 1);
                }
            }
            Ok(None) => {}
            Err(_) => {
                VideoShared::count(&video.undecoded, 1);
                self.broken = true;
                self.ask_keyframe();
            }
        }
    }

    /// Asks the network thread to request a keyframe of the far end, at most every [`KEYFRAME_ASK_EVERY`].
    fn ask_keyframe(&mut self) {
        let now = Instant::now();
        if self.asked.is_some_and(|asked| now < asked + KEYFRAME_ASK_EVERY) {
            return;
        }
        self.asked = Some(now);
        self.shared.video.keyframe_needed.store(true, Ordering::Relaxed);
    }
}

/// Starts a thread named `name` running `run`.
pub(super) fn spawn(name: &str, run: impl FnOnce() + Send + 'static) -> Result<thread::JoinHandle<()>, Error> {
    Ok(thread::Builder::new().name(name.to_owned()).spawn(run)?)
}
