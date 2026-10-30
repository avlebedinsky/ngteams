//! Tests only (`test-support`): the far end of a call's media as the tests' mixer plays it, on libwebrtc like the
//! client: a peer that sends a picture of its own on each of its video lines, plays what it is handed as audio, takes
//! the client's data channel, and counts the pictures the client sends on its main line.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use futures_executor::block_on;
use libwebrtc::audio_frame::AudioFrame as RtcAudioFrame;
use libwebrtc::data_channel::DataChannelInit;
use libwebrtc::media_stream_track::MediaStreamTrack;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt as _;
use libwebrtc::rtp_transceiver::RtpTransceiver;
use libwebrtc::video_frame::{VideoFrame as RtcVideoFrame, VideoRotation};
use libwebrtc::video_source::VideoResolution;
use libwebrtc::video_source::native::NativeVideoSource;
use libwebrtc::video_stream::native::NativeVideoStream;
use libwebrtc::video_track::RtcVideoTrack;

use super::{Next, Peer, PeerEvent, SdpAnswer, SdpOffer, factory, next_within, rtc, runtime};
use crate::Error;
use crate::devices::{AudioFrame, FRAME_SAMPLES, SAMPLE_RATE, VideoFrame};
use crate::mcp::MIXER_CHANNEL_LABEL;
use crate::processing::to_i16;
use crate::relay::RelayConfig;

/// The size of the pictures it sends on a line (the tests' pattern's).
const LINE_RESOLUTION: VideoResolution = VideoResolution {
    width: 320,
    height: 180,
};

/// The video lines by mid, as the peer names them.
type Lines = Arc<Mutex<Vec<String>>>;

/// How long a counting thread waits for a picture before it checks whether the far end hung up.
const COUNT_WAIT: Duration = Duration::from_millis(100);

/// What a counting thread counts with: the lines' names, the count, the far end's hang-up, and the lines a counting
/// thread runs for with this one's (taken out when it ends).
struct Counting {
    lines: Option<Lines>,
    counted: Arc<AtomicU64>,
    stopped: Arc<AtomicBool>,
    running: (Arc<Mutex<HashSet<String>>>, String),
}

/// Counts the pictures `track` brings while its transceiver is the main video line (line 0), until the far end hangs
/// up.
fn count_main(track: &RtcVideoTrack, transceiver: &RtpTransceiver, counting: &Counting) {
    let mut stream = NativeVideoStream::new(track.clone());
    while !counting.stopped.load(Ordering::Relaxed) {
        match next_within(&mut stream, COUNT_WAIT) {
            Ok(Next::Item(_)) => {}
            Ok(Next::Quiet) => continue,
            Ok(Next::Ended) | Err(_) => break,
        }
        let main = transceiver
            .mid()
            .zip(counting.lines.as_ref())
            .is_some_and(|(mid, lines)| Peer::video_line(lines, &mid) == Some(0));
        if main {
            counting.counted.fetch_add(1, Ordering::Relaxed);
        }
    }
    stream.close();
    let (running, line) = &counting.running;
    running.lock().unwrap_or_else(PoisonError::into_inner).remove(line);
}

/// What the far end hears from the client.
#[derive(Debug, PartialEq, Eq)]
pub enum FarEvent {
    Connected,
    /// A message on the client's data channel.
    Channel(Vec<u8>),
}

/// The far end's peer (see the module docs).
pub struct FarEnd {
    peer: Arc<Peer>,
    /// Each video line it sends on, in line order: its source.
    lines: Mutex<Vec<NativeVideoSource>>,
    events: Mutex<Receiver<FarEvent>>,
    /// Pictures the client sent on the main line (line 0).
    received: Arc<AtomicU64>,
    /// It hung up: its counting threads end.
    stopped: Arc<AtomicBool>,
}

impl FarEnd {
    /// A far end relayed through `relay`.
    ///
    /// # Errors
    /// [`Error::Rtc`] when libwebrtc refuses the peer.
    pub fn new(relay: &RelayConfig) -> Result<Self, Error> {
        let (tx, rx) = mpsc::channel();
        let received = Arc::new(AtomicU64::new(0));
        let lines_seen: Arc<Mutex<Option<Lines>>> = Arc::new(Mutex::new(None));
        let stopped = Arc::new(AtomicBool::new(false));
        let (counted, seen, halted) = (Arc::clone(&received), Arc::clone(&lines_seen), Arc::clone(&stopped));
        let sender = Mutex::new(tx);
        // libwebrtc tells a line's track again whenever the line receives again: one counting thread per line at a
        // time (a thread whose stream ended has left its line for the next)
        let counting_lines: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let events: super::Events = Arc::new(move |event| {
            let send = |event| {
                let _ = sender.lock().unwrap_or_else(PoisonError::into_inner).send(event);
            };
            match event {
                PeerEvent::Connected => send(FarEvent::Connected),
                PeerEvent::ChannelMessage(data) => send(FarEvent::Channel(data)),
                PeerEvent::Video(track, transceiver) => {
                    let line = transceiver.mid().unwrap_or_else(|| track.id());
                    if !counting_lines
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(line.clone())
                    {
                        return;
                    }
                    let counting = Counting {
                        lines: seen.lock().unwrap_or_else(PoisonError::into_inner).clone(),
                        counted: Arc::clone(&counted),
                        stopped: Arc::clone(&halted),
                        running: (Arc::clone(&counting_lines), line),
                    };
                    thread::spawn(move || count_main(&track, &transceiver, &counting));
                }
                _ => {}
            }
        });
        let peer = Arc::new(Peer::new(relay, events)?);
        *lines_seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(peer.lines());
        Ok(Self {
            peer,
            lines: Mutex::new(Vec::new()),
            events: Mutex::new(rx),
            received,
            stopped,
        })
    }

    /// Adds a video track of its own, sending, on a new line or the far end's next free one.
    fn add_line(&self) -> Result<(), Error> {
        let source = {
            let _runtime = runtime()?.enter();
            // the size of the tests' pattern: libwebrtc sends black frames of it until the line's first picture
            NativeVideoSource::new(LINE_RESOLUTION, false)
        };
        // one picture now stops the bindings' keep-alive (a black picture every 100 ms until the first), so a line
        // nobody chose a source for encodes nothing
        let black = libwebrtc::video_frame::I420Buffer::new_black(LINE_RESOLUTION.width, LINE_RESOLUTION.height);
        source.capture_frame(&RtcVideoFrame::new(VideoRotation::VideoRotation0, black));
        let track = factory().create_video_track("far-end-video", source.clone());
        self.peer
            .pc
            .add_track(MediaStreamTrack::Video(track), &[super::STREAM_ID])
            .map_err(rtc)?;
        self.lines.lock().unwrap_or_else(PoisonError::into_inner).push(source);
        Ok(())
    }

    /// Its offer: audio, `video` video lines it sends on, and a data line (its own channel, [`MIXER_CHANNEL_LABEL`];
    /// the client's is the one it takes).
    ///
    /// # Errors
    /// [`Error::Rtc`] when libwebrtc makes no offer.
    pub fn offer(&self, video: usize) -> Result<SdpOffer, Error> {
        self.peer.attach_audio()?;
        for _ in 0..video {
            self.add_line()?;
        }
        let channel = self
            .peer
            .pc
            .create_data_channel(MIXER_CHANNEL_LABEL, DataChannelInit::default())
            .map_err(rtc)?;
        *self.peer.channel.lock().unwrap_or_else(PoisonError::into_inner) = Some(channel);
        self.peer.offer()
    }

    /// Its offer of the session as it is.
    ///
    /// # Errors
    /// [`Error::Rtc`] when libwebrtc makes no offer.
    pub fn reoffer(&self) -> Result<SdpOffer, Error> {
        self.peer.offer()
    }

    /// Answers the client's offer, sending on each of its video lines.
    ///
    /// # Errors
    /// [`Error::Rtc`] when libwebrtc refuses the offer.
    pub fn answer(&self, offer: &SdpOffer) -> Result<SdpAnswer, Error> {
        let video_lines = super::video_mids(offer.as_str()).len();
        self.peer.roll_back()?;
        block_on(self.peer.pc.set_remote_description(offer.clone().description()?)).map_err(rtc)?;
        self.peer.attach_audio()?;
        while self.lines.lock().unwrap_or_else(PoisonError::into_inner).len() < video_lines {
            self.add_line()?;
        }
        self.peer.prefer_codecs();
        let answer = block_on(
            self.peer
                .pc
                .create_answer(libwebrtc::peer_connection::AnswerOptions::default()),
        )
        .map_err(rtc)?;
        let text = answer.to_string();
        block_on(self.peer.pc.set_local_description(answer)).map_err(rtc)?;
        self.peer.negotiated();
        Ok(SdpAnswer(self.peer.local_sdp(&text)?))
    }

    /// Takes the client's answer to its offer.
    ///
    /// # Errors
    /// [`Error::Rtc`] when libwebrtc refuses it.
    pub fn accept_answer(&self, answer: &SdpAnswer) -> Result<(), Error> {
        self.peer.accept_answer(answer.clone())
    }

    /// Plays one 10 ms frame.
    pub fn send_audio(&self, frame: &AudioFrame) {
        let samples = to_i16(frame);
        let sent = RtcAudioFrame {
            data: samples.as_slice().into(),
            sample_rate: SAMPLE_RATE,
            num_channels: 1,
            samples_per_channel: u32::try_from(FRAME_SAMPLES).unwrap_or(u32::MAX),
        };
        let _ = block_on(self.peer.audio().capture_frame(&sent));
    }

    /// Sends an I420 picture on its video line `line` (in the order its offer or answer added them); whether there is
    /// such a line.
    pub fn send_video(&self, line: usize, frame: &VideoFrame) -> bool {
        let lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(source) = lines.get(line) else {
            return false;
        };
        let picture = RtcVideoFrame::new(VideoRotation::VideoRotation0, crate::session::to_rtc_buffer(frame));
        source.capture_frame(&picture)
    }

    /// Sends `data` on the client's data channel; whether it went.
    pub fn send_channel(&self, data: &[u8]) -> bool {
        self.peer.send_channel(data)
    }

    /// The next event within `wait`.
    pub fn next_event(&self, wait: Duration) -> Option<FarEvent> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv_timeout(wait)
            .ok()
    }

    /// Pictures the client sent on its main line so far.
    #[must_use]
    pub fn received(&self) -> u64 {
        self.received.load(Ordering::Relaxed)
    }

    /// Hangs up.
    pub fn close(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.peer.close();
    }
}

impl Drop for FarEnd {
    fn drop(&mut self) {
        self.close();
    }
}
