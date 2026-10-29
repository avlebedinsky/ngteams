//! A group call's mixer on loopback (the `media` feature, M3 Task 19): what stands behind a group call's answers and
//! offers once a test turned group video on ([`crate::ChatServiceMock::start_group_video`]), one per call and account,
//! in the shape of the call's mixer as the captures show it (`docs/protocol/calling.md`, "Group video"):
//!
//! - **Its SDP**: offered to a callee, audio, the main video line, [`SLOTS`] video lines it sends on (both ways), the
//!   sharing line, and a data line; answering a caller's or a joiner's group offer, every video line as offered (it
//!   sends on each line the client receives on). Each line names its stream id (`a=x-source-streamid`), counted from
//!   the mixer's `base` in line order: the account's audio is `base`, its main video `base + 1`, as the roster the
//!   mock delivers names them.
//! - **Its control plane** on the client's data channel ([`teams_media::mcp`]): it answers `syn` with `ack` (it
//!   sends `dsh` and `sr_res`, takes `sr`), keeps every source request, and answers each one `ok`; it tells the
//!   dominant speakers a test names ([`MixerPeer::speak`]).
//! - **Its video**: a test pattern (frames carrying their index), sent on every line a source request gave a source,
//!   from the moment it did (a keyframe first); the video the client sends on its main line is counted.
//! - **Its audio**: the mock's tone, throughout, as the call's mix (who speaks in it the client hears only from the
//!   dominant speakers a test names).
//! - **Its renegotiation** ([`MixerPeer::reoffer`]): its session offered again (an ICE restart), as the live mixer
//!   renegotiates a callee's media now and then.
//!
//! It mixes nothing: what a line shows is its own pattern, whoever the request named.

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teams_media::codec::{Encoder, OpusConfig, PACKET_SAMPLES};
use teams_media::devices::pattern::TestPattern;
use teams_media::devices::{AudioFrame, FRAME_SAMPLES};
use teams_media::mcp::{Depacketizer, MCP_DATA_ID, Packetizer};
use teams_media::peer::{PeerEvent, RelayedPeer};
use teams_media::relay::{RelayLink, RelayOptions, RelayServer};
use teams_media::sdp::{NgcSdp, NgcSession, VideoLines};
use teams_media::test_turn::{PASSWORD, USER};
use teams_media::tone::{Tone, ToneGenerator};
use teams_media::turn::{Credentials, Transport};
use teams_media::video::codec::{EncoderSettings, VideoEncoder};
use teams_media::video::convert::{MaxSize, to_i420};

use crate::media::PeerError;

/// The video lines the mixer offers to send on, besides the main line and the sharing line (the live mixer's count).
pub const SLOTS: usize = 9;
/// How often it sends a frame on each line with a source.
const FRAME_EVERY: Duration = Duration::from_millis(66);
/// How often it sends an audio packet (20 ms of Opus).
const AUDIO_EVERY: Duration = Duration::from_millis(20);
/// How long its thread waits for a command before it turns the relay again.
const TURN_WAIT: Duration = Duration::from_millis(2);
/// How long a command may take.
const COMMAND_WAIT: Duration = Duration::from_secs(5);

/// What the mixer saw and did so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MixerStats {
    /// `syn`s it answered.
    pub handshakes: u64,
    /// Every source request, in order: the line's stream id and the source.
    pub requests: Vec<(u32, i64)>,
    /// Frames it sent, on every line together.
    pub sent: u64,
    /// Frames the client sent it on its main line.
    pub received: u64,
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    handshakes: AtomicU64,
    sent: AtomicU64,
    received: AtomicU64,
    requests: Mutex<Vec<(u32, i64)>>,
}

impl Shared {
    fn stats(&self) -> MixerStats {
        MixerStats {
            handshakes: self.handshakes.load(Ordering::Relaxed),
            requests: self.requests.lock().unwrap_or_else(PoisonError::into_inner).clone(),
            sent: self.sent.load(Ordering::Relaxed),
            received: self.received.load(Ordering::Relaxed),
        }
    }
}

type Reply<T> = Sender<Result<T, PeerError>>;

enum Command {
    Offer(Reply<NgcSdp>),
    Reoffer(Reply<NgcSdp>),
    Answer(NgcSdp, Reply<NgcSdp>),
    Accept(NgcSdp, Reply<()>),
    Speak(Vec<i64>),
}

/// A group call's mixer on loopback (see the module docs).
pub struct MixerPeer {
    commands: Sender<Command>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    base: u32,
}

impl fmt::Debug for MixerPeer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MixerPeer")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl MixerPeer {
    /// A mixer behind its own UDP allocation on the loopback TURN server's UDP port `turn`, its lines' stream ids
    /// counted from `base`, playing `tone`.
    ///
    /// # Errors
    /// [`PeerError::Media`] when the allocation or the codecs can't be made.
    pub fn start_at(turn: SocketAddr, base: u32, tone: Tone) -> Result<Self, PeerError> {
        let server = RelayServer {
            addr: turn,
            transport: Transport::Udp,
        };
        let credentials = Credentials {
            username: USER.into(),
            password: PASSWORD.into(),
        };
        let link = RelayLink::allocate(server, credentials, &RelayOptions::default())?;
        let peer = RelayedPeer::new(Instant::now(), link)?;
        let encoder = VideoEncoder::new(EncoderSettings::default())?;
        let audio = Audio {
            encoder: Encoder::new(&OpusConfig::default())?,
            tone: ToneGenerator::new(tone),
            next: Instant::now(),
        };
        let shared = Arc::new(Shared::default());
        let (commands, received) = mpsc::channel();
        let mixing = Mixing {
            peer,
            dialect: NgcSession::new(),
            encoder,
            audio,
            pattern: TestPattern::new(320, 180),
            index: 0,
            base,
            lines: Vec::new(),
            sources: BTreeMap::new(),
            keyframe: true,
            next_frame: Instant::now(),
            started: Instant::now(),
            depacketizer: Depacketizer::new(),
            packetizer: Packetizer::new(MCP_DATA_ID),
            shared: Arc::clone(&shared),
        };
        let thread = thread::Builder::new()
            .name("mock-mixer".to_owned())
            .spawn(move || mixing.run(&received))
            .map_err(teams_media::Error::from)?;
        Ok(Self {
            commands,
            shared,
            thread: Some(thread),
            base,
        })
    }

    /// The account's audio source id, as the roster names it.
    #[must_use]
    pub fn audio_source(&self) -> i64 {
        i64::from(self.base)
    }

    /// The account's main video source id, as the roster names it.
    #[must_use]
    pub fn video_source(&self) -> i64 {
        i64::from(self.base) + 1
    }

    /// Its offer to a callee, in `sdp-ngc-1.0`.
    ///
    /// # Errors
    /// [`PeerError`] when the offer can't be made or converted.
    pub fn offer(&self) -> Result<NgcSdp, PeerError> {
        self.ask(Command::Offer)
    }

    /// Its answer to a caller's or a joiner's group offer, in `sdp-ngc-1.0`.
    ///
    /// # Errors
    /// [`PeerError`] when the offer doesn't convert or the session refuses it.
    pub fn answer(&self, offer: &NgcSdp) -> Result<NgcSdp, PeerError> {
        let offer = offer.clone();
        self.ask(|reply| Command::Answer(offer, reply))
    }

    /// Its renegotiation of the callee's media it offered to (an ICE restart), in `sdp-ngc-1.0`; the answer goes to
    /// [`Self::accept_answer`].
    ///
    /// # Errors
    /// [`PeerError`] when the offer can't be made or converted.
    pub fn reoffer(&self) -> Result<NgcSdp, PeerError> {
        self.ask(Command::Reoffer)
    }

    /// Takes the callee's answer to its offer (its first, or its renegotiation).
    ///
    /// # Errors
    /// [`PeerError`] when the answer doesn't convert or the session refuses it.
    pub fn accept_answer(&self, answer: &NgcSdp) -> Result<(), PeerError> {
        let answer = answer.clone();
        self.ask(|reply| Command::Accept(answer, reply))
    }

    /// Tells the client the dominant speakers, newest first, by audio source id.
    pub fn speak(&self, sources: Vec<i64>) {
        let _ = self.commands.send(Command::Speak(sources));
    }

    /// What it saw and did so far.
    #[must_use]
    pub fn stats(&self) -> MixerStats {
        self.shared.stats()
    }

    fn ask<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, PeerError> {
        let (reply, answer) = mpsc::channel();
        self.commands
            .send(command(reply))
            .map_err(|_| teams_media::Error::SessionEnded)?;
        answer
            .recv_timeout(COMMAND_WAIT)
            .map_err(|_| teams_media::Error::Timeout("the mock's mixer"))?
    }
}

impl Drop for MixerPeer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The mixer's thread.
struct Mixing {
    peer: RelayedPeer,
    dialect: NgcSession,
    encoder: VideoEncoder,
    audio: Audio,
    pattern: TestPattern,
    index: u32,
    base: u32,
    /// Each video line's stream id and its line number (0 the main line).
    lines: Vec<(u32, usize)>,
    /// The source each line sends, by stream id (`-1`: none).
    sources: BTreeMap<u32, i64>,
    keyframe: bool,
    next_frame: Instant,
    started: Instant,
    depacketizer: Depacketizer,
    packetizer: Packetizer,
    shared: Arc<Shared>,
}

/// The tone it plays, as Opus.
struct Audio {
    encoder: Encoder,
    tone: ToneGenerator,
    next: Instant,
}

impl Mixing {
    fn run(mut self, commands: &Receiver<Command>) {
        while !self.shared.stop.load(Ordering::Relaxed) {
            match commands.recv_timeout(TURN_WAIT) {
                Ok(command) => self.command(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if self.turn().is_err() {
                break;
            }
        }
        let now = Instant::now();
        if self.peer.peer().close().is_ok() {
            let _ = self.peer.flush(now);
        }
        let _ = self.peer.link().close(now);
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Offer(reply) => {
                let _ = reply.send(self.offer());
            }
            Command::Reoffer(reply) => {
                let _ = reply.send(self.reoffer());
            }
            Command::Answer(offer, reply) => {
                let _ = reply.send(self.answer(&offer));
            }
            Command::Accept(answer, reply) => {
                let result = self
                    .dialect
                    .read_answer(&answer)
                    .map_err(PeerError::from)
                    .and_then(|answer| Ok(self.peer.peer().accept_mixer_answer(answer)?));
                let _ = reply.send(result);
            }
            Command::Speak(sources) => {
                let message = json!([{"type": "dsh", "history": sources}]);
                let _ = self.send_mcp(&message);
            }
        }
    }

    fn offer(&mut self) -> Result<NgcSdp, PeerError> {
        let offer = self.peer.peer().offer_mixer(SLOTS)?;
        let written = self.dialect.write_offer(&offer)?;
        Ok(self.named(&written))
    }

    fn reoffer(&mut self) -> Result<NgcSdp, PeerError> {
        let offer = self.peer.peer().reoffer_mixer()?;
        let written = self.dialect.write_offer(&offer)?;
        Ok(self.named(&written))
    }

    fn answer(&mut self, offer: &NgcSdp) -> Result<NgcSdp, PeerError> {
        self.dialect.set_video(VideoLines::Offered);
        let peer = self.peer.peer();
        peer.take_slots();
        peer.take_remote_channel();
        let offer = self.dialect.read_offer(offer)?;
        let answer = self.peer.peer().accept_offer(offer)?;
        let written = self.dialect.write_answer(&answer)?;
        Ok(self.named(&written))
    }

    /// `sdp` with each line's stream id named (`a=x-source-streamid`, counted from the base in line order), and the
    /// video lines' kept.
    fn named(&mut self, sdp: &NgcSdp) -> NgcSdp {
        let mut out = Vec::new();
        let mut line = 0;
        let mut video = 0;
        self.lines.clear();
        for text in sdp.as_str().split("\r\n").filter(|text| !text.is_empty()) {
            out.push(text.to_owned());
            if !text.starts_with("m=") {
                continue;
            }
            let stream = self.base + line;
            line += 1;
            if text.split(' ').nth(1) == Some("0") {
                continue;
            }
            out.push(format!("a=x-source-streamid:{stream}"));
            if text.starts_with("m=video ") {
                self.lines.push((stream, video));
                video += 1;
            }
        }
        let mut text = out.join("\r\n");
        text.push_str("\r\n");
        NgcSdp::new(sdp.content_type(), &text).unwrap_or_else(|_| sdp.clone())
    }

    fn turn(&mut self) -> Result<(), teams_media::Error> {
        let now = Instant::now();
        self.peer.turn(now)?;
        while let Some(event) = self.peer.peer().poll_event() {
            match event {
                PeerEvent::ChannelData(data) => self.control(&data)?,
                PeerEvent::Video(unit) if unit.line == 0 => {
                    self.shared.received.fetch_add(1, Ordering::Relaxed);
                }
                PeerEvent::KeyframeRequested => self.keyframe = true,
                _ => {}
            }
        }
        if now >= self.next_frame {
            self.next_frame = now + FRAME_EVERY;
            self.send_frame(now)?;
        }
        if now >= self.audio.next {
            self.audio.next = (self.audio.next + AUDIO_EVERY).max(now);
            self.send_audio(now)?;
        }
        self.peer.flush(now)
    }

    /// The next 20 ms of the tone, while the peer is connected.
    fn send_audio(&mut self, now: Instant) -> Result<(), teams_media::Error> {
        if !self.peer.peer().is_connected() {
            return Ok(());
        }
        let mut samples = [0.0; PACKET_SAMPLES];
        self.audio.tone.fill(&mut samples);
        for frame in samples.chunks(FRAME_SAMPLES).filter_map(AudioFrame::from_samples) {
            if let Some(packet) = self.audio.encoder.push(&frame)? {
                // a line that doesn't take audio yet (the answer still on its way) drops it
                let _ = self.peer.peer().write_audio(now, now, packet.rtp_time, &packet.payload);
            }
        }
        Ok(())
    }

    /// One pattern frame on every line with a source.
    fn send_frame(&mut self, now: Instant) -> Result<(), teams_media::Error> {
        let lines: Vec<usize> = self
            .lines
            .iter()
            .filter(|(stream, _)| self.sources.get(stream).is_some_and(|source| *source != -1))
            .map(|(_, line)| *line)
            .collect();
        if lines.is_empty() || !self.peer.peer().is_connected() {
            return Ok(());
        }
        self.index += 1;
        let Some(frame) = to_i420(&self.pattern.frame(self.index), MaxSize::HD) else {
            return Ok(());
        };
        let at = now.saturating_duration_since(self.started);
        let keyframe = std::mem::take(&mut self.keyframe);
        let Some(encoded) = self.encoder.encode(&frame, at, keyframe)? else {
            return Ok(());
        };
        let rtp_time = u64::try_from(at.as_micros() * 90 / 1000).unwrap_or(u64::MAX);
        for line in lines {
            if self
                .peer
                .peer()
                .write_video_on(line, now, (now, rtp_time), &encoded.data)
                .is_ok()
            {
                self.shared.sent.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// A message of the client's control plane: `syn` answered, a source request kept and answered.
    fn control(&mut self, data: &[u8]) -> Result<(), teams_media::Error> {
        let Ok(Some(message)) = self.depacketizer.depacketize(data) else {
            return Ok(());
        };
        let Some((1, json)) = message.data.split_first().map(|(version, json)| (*version, json)) else {
            return Ok(());
        };
        let Ok(Value::Array(messages)) = serde_json::from_slice::<Value>(json) else {
            return Ok(());
        };
        for message in messages {
            match message.get("type").and_then(Value::as_str) {
                Some("syn") => {
                    self.shared.handshakes.fetch_add(1, Ordering::Relaxed);
                    let ack = json!([{"type": "ack", "send_capabilities": ["dsh", "sr_res"],
                        "receive_capabilities": ["sr"]}]);
                    self.send_mcp(&ack)?;
                }
                Some("sr") => self.source_request(&message)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn source_request(&mut self, message: &Value) -> Result<(), teams_media::Error> {
        let info = message.pointer("/controlVideoStreaming/controlInfo");
        let stream = info
            .and_then(|info| info.get("streamMsid")?.as_u64())
            .and_then(|stream| u32::try_from(stream).ok());
        let source = info.and_then(|info| info.get("sourceId")?.as_i64());
        let sequence = message
            .pointer("/controlVideoStreaming/sequenceNumber")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if let (Some(stream), Some(source)) = (stream, source) {
            self.shared
                .requests
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((stream, source));
            self.sources.insert(stream, source);
            self.keyframe = true;
        }
        self.send_mcp(&json!([{"type": "sr_res", "sequenceNumber": sequence, "result": "ok"}]))
    }

    fn send_mcp(&mut self, messages: &Value) -> Result<(), teams_media::Error> {
        let mut payload = vec![1];
        payload.extend_from_slice(messages.to_string().as_bytes());
        let packet = self
            .packetizer
            .packetize(teams_media::mcp::MIXER_RECIPIENT, &[], &payload);
        self.peer.peer().write_channel(&packet)?;
        Ok(())
    }
}
