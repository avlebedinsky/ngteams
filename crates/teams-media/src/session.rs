//! A call's audio session: the microphone through Opus to the relayed peer, and the peer through the jitter buffer and
//! Opus to the speaker, on threads of its own.
//!
//! Three threads, none of them the caller's (and never an async runtime's):
//!
//! - **the capture thread** (`teams-media-mic`) reads the microphone, encodes every two 10 ms frames into a 20 ms
//!   packet and queues it for the network thread;
//! - **the network thread** (`teams-media-net`) owns the [`RelayedPeer`]: it turns the relay and `str0m`, sends the
//!   queued packets, puts arriving ones into the jitter buffer, and runs the caller's SDP commands;
//! - **the playout thread** (`teams-media-spk`) keeps the speaker's pace (one 10 ms frame every 10 ms), taking from
//!   the jitter buffer what to decode, recover or conceal.
//!
//! The handoffs are bounded both ways: the capture queue keeps the newest [`SessionConfig::send_queue`] packets (a
//! network thread that falls behind loses the backlog, not the present), and the jitter buffer holds at most its
//! capacity. No thread waits on another's device: a stuck speaker fills only the jitter buffer, and a stuck
//! microphone only stops what is sent.
//!
//! SDP crosses as `str0m`'s own (`teams_media::sdp` converts it to and from Teams' dialect); each command is answered
//! by the network thread within [`COMMAND_TIMEOUT`]. The encoder starts as [`SessionConfig::opus`] says and turns DTX
//! on or off as the far end's Opus line asks once its offer or answer is applied. [`MediaSession::close`] (or dropping
//! the session) hangs up the peer, releases the allocation, and joins every thread; a network thread that fails says
//! goodbye and releases the allocation as well as it still can before it ends.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};

use crate::Error;
use crate::codec::{Decoder, Encoder, OpusConfig, Packet, packet_samples};
use crate::devices::{AudioFrame, AudioSink, AudioSource, Devices, FRAME_DURATION, FRAME_SAMPLES};
use crate::jitter::{JitterBuffer, JitterConfig, JitterStats, Playout};
use crate::peer::{PeerEvent, RelayedPeer};
use crate::queue::Queue;

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
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            opus: OpusConfig::default(),
            jitter: JitterConfig::default(),
            send_queue: 10,
        }
    }
}

/// A device of the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionDevice {
    Microphone,
    Speaker,
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
    /// The relay or `str0m` failed and the network thread ended. Final.
    Failed(String),
    /// A device went away (or failed); its side of the media stopped, the rest goes on.
    DeviceClosed(SessionDevice),
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
    counters: Counters,
    jitter: Mutex<JitterBuffer>,
    /// The encoder's configuration once the far end's SDP changed it, for the capture thread to take.
    negotiated: Mutex<Option<OpusConfig>>,
    events: SyncSender<SessionEvent>,
}

impl Shared {
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
    AcceptAnswer(SdpAnswer, Sender<Result<(), Error>>),
    AcceptOffer(SdpOffer, Sender<Result<SdpAnswer, Error>>),
}

/// A running audio session.
pub struct MediaSession {
    shared: Arc<Shared>,
    commands: SyncSender<Command>,
    events: Receiver<SessionEvent>,
    threads: Vec<JoinHandle<()>>,
}

impl MediaSession {
    /// Opens the microphone and the speaker of `devices` and starts the session's threads on `peer`. Nothing is sent
    /// until the SDP is exchanged and the media connects.
    ///
    /// # Errors
    /// [`Error::Device`] when a device can't be opened, [`Error::Opus`] when libopus refuses the setup, [`Error::Io`]
    /// when a thread can't be started.
    pub fn start(peer: RelayedPeer, devices: &dyn Devices, config: SessionConfig) -> Result<Self, Error> {
        let microphone = devices.microphone()?;
        let speaker = devices.speaker()?;
        let encoder = Encoder::new(&config.opus)?;
        let decoder = Decoder::new()?;
        let (events_tx, events) = mpsc::sync_channel(EVENT_QUEUE);
        let (commands, commands_rx) = mpsc::sync_channel(COMMAND_QUEUE);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            counters: Counters::default(),
            jitter: Mutex::new(JitterBuffer::new(config.jitter)),
            negotiated: Mutex::new(None),
            events: events_tx,
        });
        let outgoing = Queue::new(config.send_queue.max(1));
        let mut session = Self {
            shared: Arc::clone(&shared),
            commands,
            events,
            threads: Vec::with_capacity(3),
        };
        let network = Network {
            peer,
            shared: Arc::clone(&shared),
            commands: commands_rx,
            outgoing: outgoing.clone(),
            pending: None,
            opus: config.opus,
            opus_pending: false,
        };
        session.spawn("teams-media-net", move || network.run())?;
        let capture = Capture {
            microphone,
            encoder,
            outgoing,
            shared: Arc::clone(&shared),
        };
        session.spawn("teams-media-mic", move || capture.run())?;
        let playout = PlayoutLoop {
            speaker,
            decoder,
            shared,
            pcm: Vec::with_capacity(crate::codec::MAX_PACKET_SAMPLES + FRAME_SAMPLES),
        };
        session.spawn("teams-media-spk", move || playout.run())?;
        Ok(session)
    }

    fn spawn(&mut self, name: &str, run: impl FnOnce() + Send + 'static) -> Result<(), Error> {
        let handle = thread::Builder::new().name(name.to_owned()).spawn(run)?;
        self.threads.push(handle);
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
        self.events.recv_timeout(timeout).ok()
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
        for handle in self.threads.drain(..) {
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
}

impl Network {
    fn run(mut self) {
        while !self.shared.stopping() {
            if let Err(error) = self.turn() {
                self.shared.report(SessionEvent::Failed(error.to_string()));
                break;
            }
        }
        self.hang_up();
    }

    fn turn(&mut self) -> Result<(), Error> {
        while let Ok(command) = self.commands.try_recv() {
            self.command(command);
        }
        self.peer.turn(Instant::now())?;
        while let Some(event) = self.peer.peer().poll_event() {
            self.on_event(event);
        }
        self.negotiate_opus();
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
        }
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
    outgoing: Queue<Outgoing>,
    shared: Arc<Shared>,
}

impl Capture {
    fn run(mut self) {
        while !self.shared.stopping() {
            match self.microphone.next_frame(MIC_WAIT) {
                Ok(Some(frame)) => {
                    if let Err(error) = self.encode(&frame) {
                        self.shared.report(SessionEvent::Failed(error.to_string()));
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

    fn encode(&mut self, frame: &AudioFrame) -> Result<(), Error> {
        let captured = Instant::now();
        let negotiated = self.shared.negotiated().take();
        if let Some(config) = negotiated {
            self.encoder.configure(&config)?;
        }
        if let Some(packet) = self.encoder.push(frame)? {
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
