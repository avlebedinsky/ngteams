//! The in-process loop: peer A sends an Opus tone in real time, peer B decodes it and echoes every packet back, and A
//! decodes the echo. Both peers run on loopback UDP sockets in one thread; we own the sockets and pass `now` in.

use std::collections::HashMap;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::thread;
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer};

use crate::codec::{OpusDecoder, OpusEncoder};
use crate::goertzel::dominant_frequency;
use crate::peer::{PeerEvent, ReceivedAudio, UdpPeer};
use crate::tone::ToneGenerator;
use crate::{Error, FRAME_DURATION, FRAME_SAMPLES, SAMPLE_RATE};

/// Decoded frames skipped before the frequency check: the decoder's warm-up.
const WARM_UP_FRAMES: usize = 5;
/// Most decoded frames the frequency check looks at (the last ones), to bound its cost.
const ANALYSIS_FRAMES: usize = 50;
/// Nap between run-loop turns when no datagram arrived.
const IDLE_NAP: Duration = Duration::from_micros(200);

/// Settings of one loop run.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopConfig {
    /// Frequency of the sent tone, in Hz.
    pub tone_hz: f64,
    /// Tone amplitude as a fraction of full scale.
    pub amplitude: f64,
    /// Number of 20 ms frames A sends.
    pub frames: usize,
    /// How long ICE + DTLS may take.
    pub connect_timeout: Duration,
    /// How long to wait for the last echoes after the last frame is sent.
    pub drain_timeout: Duration,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            tone_hz: 440.0,
            amplitude: 0.5,
            frames: 50,
            connect_timeout: Duration::from_secs(5),
            drain_timeout: Duration::from_secs(1),
        }
    }
}

/// Summary of a set of latency samples (nearest-rank percentiles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencyStats {
    /// Number of samples.
    pub count: usize,
    /// Smallest sample.
    pub min: Duration,
    /// Median.
    pub p50: Duration,
    /// 95th percentile.
    pub p95: Duration,
    /// Largest sample.
    pub max: Duration,
    /// Arithmetic mean.
    pub mean: Duration,
}

impl LatencyStats {
    /// Summarizes `samples`, or `None` if there are none.
    #[must_use]
    pub fn from_samples(samples: &[Duration]) -> Option<Self> {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let count = sorted.len();
        if count == 0 {
            return None;
        }
        let rank = |percent: usize| sorted[(percent * count).div_ceil(100).saturating_sub(1).min(count - 1)];
        let total: Duration = sorted.iter().sum();
        Some(Self {
            count,
            min: *sorted.first()?,
            p50: rank(50),
            p95: rank(95),
            max: *sorted.last()?,
            mean: total / u32::try_from(count).ok()?,
        })
    }
}

impl fmt::Display for LatencyStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = |d: Duration| d.as_secs_f64() * 1_000.0;
        write!(
            f,
            "n={} min={:.2} ms p50={:.2} ms p95={:.2} ms max={:.2} ms mean={:.2} ms",
            self.count,
            ms(self.min),
            ms(self.p50),
            ms(self.p95),
            ms(self.max),
            ms(self.mean)
        )
    }
}

/// What one loop run measured.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopReport {
    /// Frequency A sent, in Hz.
    pub sent_hz: f64,
    /// Frequency B detected in what it decoded.
    pub received_hz: Option<f64>,
    /// Frequency A detected in the echo.
    pub echoed_hz: Option<f64>,
    /// Frames A sent.
    pub frames_sent: usize,
    /// Frames B decoded.
    pub frames_received: usize,
    /// Echoed frames A decoded.
    pub frames_echoed: usize,
    /// Time from the SDP exchange to both peers connected (ICE + DTLS).
    pub connect_time: Duration,
    /// A's encode start → B's decode end.
    pub one_way: Option<LatencyStats>,
    /// A's encode start → A's decode end of the echo.
    pub round_trip: Option<LatencyStats>,
}

impl fmt::Display for LoopReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hz = |v: Option<f64>| v.map_or_else(|| "none".to_owned(), |v| format!("{v} Hz"));
        let stats = |v: Option<LatencyStats>| v.map_or_else(|| "none".to_owned(), |v| v.to_string());
        writeln!(
            f,
            "sent {} Hz, received {}, echoed {}",
            self.sent_hz,
            hz(self.received_hz),
            hz(self.echoed_hz)
        )?;
        writeln!(
            f,
            "frames sent {} received {} echoed {}; connect {:.2} ms",
            self.frames_sent,
            self.frames_received,
            self.frames_echoed,
            self.connect_time.as_secs_f64() * 1_000.0
        )?;
        writeln!(f, "one-way:    {}", stats(self.one_way))?;
        write!(f, "round trip: {}", stats(self.round_trip))
    }
}

/// Runs the loop: connect A and B over loopback UDP, send `config.frames` frames of the tone from A, echo them from
/// B, and measure.
///
/// # Errors
/// [`Error::Config`] for unusable settings, [`Error::Timeout`] if the peers don't connect, and any socket, `str0m` or
/// Opus failure.
pub fn run_loopback(config: &LoopConfig) -> Result<LoopReport, Error> {
    check(config)?;
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let mut a = UdpPeer::bind(Instant::now(), loopback)?;
    let mut b = UdpPeer::bind(Instant::now(), loopback)?;
    let connect_time = connect(&mut a, &mut b, config.connect_timeout)?;

    let mut sender = Sender::new(config)?;
    let mut received = Listener::new()?;
    let mut echoed = Listener::new()?;
    let mut last_sent = Instant::now();
    loop {
        let now = Instant::now();
        let arrived = a.turn(now)? | b.turn(now)?;
        echo(&mut b, &mut received, &sender.sent_at, now)?;
        while let Some(event) = a.peer().poll_event() {
            if let Some(frame) = audio(event)? {
                echoed.on_frame(&frame, &sender.sent_at)?;
            }
        }
        if let Some(sent) = sender.send_due(&mut a, now)? {
            last_sent = sent;
        }
        let all_back = echoed.frames >= config.frames;
        if sender.done() && (all_back || now.duration_since(last_sent) > config.drain_timeout) {
            break;
        }
        if !arrived {
            thread::sleep(IDLE_NAP);
        }
    }
    Ok(LoopReport {
        sent_hz: config.tone_hz,
        received_hz: received.frequency(),
        echoed_hz: echoed.frequency(),
        frames_sent: sender.index,
        frames_received: received.frames,
        frames_echoed: echoed.frames,
        connect_time,
        one_way: LatencyStats::from_samples(&received.latencies),
        round_trip: LatencyStats::from_samples(&echoed.latencies),
    })
}

fn check(config: &LoopConfig) -> Result<(), Error> {
    if config.frames == 0 {
        return Err(Error::Config("frames must be at least 1".into()));
    }
    let nyquist = f64::from(SAMPLE_RATE) / 2.0;
    if !(config.tone_hz > 0.0 && config.tone_hz < nyquist) {
        return Err(Error::Config(format!(
            "tone {} Hz is outside (0, {nyquist}) Hz",
            config.tone_hz
        )));
    }
    Ok(())
}

/// Exchanges SDP as text (as a signaling channel would carry it) and turns both peers until both are connected.
fn connect(a: &mut UdpPeer, b: &mut UdpPeer, timeout: Duration) -> Result<Duration, Error> {
    let start = Instant::now();
    let (offer, pending) = a.peer().offer_audio()?;
    let offer = SdpOffer::from_sdp_string(&offer.to_sdp_string()).map_err(|e| Error::Rtc(e.to_string()))?;
    let answer = b.peer().accept_offer(offer)?;
    let answer = SdpAnswer::from_sdp_string(&answer.to_sdp_string()).map_err(|e| Error::Rtc(e.to_string()))?;
    a.peer().accept_answer(pending, answer)?;
    while !(a.peer().is_connected() && b.peer().is_connected()) {
        if start.elapsed() > timeout {
            return Err(Error::Timeout("ICE + DTLS between the loopback peers"));
        }
        let now = Instant::now();
        if !(a.turn(now)? | b.turn(now)?) {
            thread::sleep(IDLE_NAP);
        }
    }
    // the Connected events are consumed here; media events start after this point
    while a.peer().poll_event().is_some() {}
    while b.peer().poll_event().is_some() {}
    Ok(start.elapsed())
}

/// B's side: decode every arriving frame and send its packet straight back.
fn echo(b: &mut UdpPeer, received: &mut Listener, sent_at: &HashMap<u64, Instant>, now: Instant) -> Result<(), Error> {
    while let Some(event) = b.peer().poll_event() {
        if let Some(frame) = audio(event)? {
            received.on_frame(&frame, sent_at)?;
            b.peer()
                .write_audio(Instant::now(), now, frame.rtp_time, &frame.payload)?;
        }
    }
    b.flush()
}

/// The audio frame of an event. A disconnect fails the run even though ICE may recover from it: on loopback it means
/// something broke, and the gap would skew the latency figures.
fn audio(event: PeerEvent) -> Result<Option<ReceivedAudio>, Error> {
    match event {
        PeerEvent::Audio(frame) => Ok(Some(frame)),
        PeerEvent::Disconnected => Err(Error::Rtc("ICE disconnected mid-loop".into())),
        PeerEvent::Closed => Err(Error::Rtc("the session closed mid-loop".into())),
        // the loop carries audio alone: its bandwidth estimates (and no video) change nothing of it
        PeerEvent::Connected
        | PeerEvent::Video(_)
        | PeerEvent::KeyframeRequested
        | PeerEvent::Bitrate(_)
        | PeerEvent::ChannelOpen
        | PeerEvent::ChannelData(_) => Ok(None),
    }
}

/// A's sending side: one frame of the tone every 20 ms of real time.
struct Sender {
    generator: ToneGenerator,
    encoder: OpusEncoder,
    frames: usize,
    index: usize,
    start: Option<Instant>,
    sent_at: HashMap<u64, Instant>,
}

impl Sender {
    fn new(config: &LoopConfig) -> Result<Self, Error> {
        Ok(Self {
            generator: ToneGenerator::new(config.tone_hz, SAMPLE_RATE, config.amplitude),
            encoder: OpusEncoder::new()?,
            frames: config.frames,
            index: 0,
            start: None,
            sent_at: HashMap::new(),
        })
    }

    fn done(&self) -> bool {
        self.index >= self.frames
    }

    /// Sends the next frame if its time has come; returns when it was sent.
    fn send_due(&mut self, a: &mut UdpPeer, now: Instant) -> Result<Option<Instant>, Error> {
        let start = *self.start.get_or_insert(now);
        let due = start + FRAME_DURATION * u32::try_from(self.index).map_err(|e| Error::Config(e.to_string()))?;
        if self.done() || now < due {
            return Ok(None);
        }
        let rtp_time = u64::try_from(self.index * FRAME_SAMPLES).map_err(|e| Error::Config(e.to_string()))?;
        let mut frame = vec![0; FRAME_SAMPLES];
        self.generator.fill(&mut frame);
        let encode_start = Instant::now();
        let packet = self.encoder.encode(&frame)?;
        self.sent_at.insert(rtp_time, encode_start);
        a.peer().write_audio(Instant::now(), encode_start, rtp_time, &packet)?;
        a.flush()?;
        self.index += 1;
        Ok(Some(encode_start))
    }
}

/// A receiving side: decodes frames, keeps the PCM and the latency of each frame.
struct Listener {
    decoder: OpusDecoder,
    pcm: Vec<i16>,
    frames: usize,
    latencies: Vec<Duration>,
}

impl Listener {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            decoder: OpusDecoder::new()?,
            pcm: Vec::new(),
            frames: 0,
            latencies: Vec::new(),
        })
    }

    fn on_frame(&mut self, frame: &ReceivedAudio, sent_at: &HashMap<u64, Instant>) -> Result<(), Error> {
        let pcm = self.decoder.decode(&frame.payload)?;
        let decoded = Instant::now();
        if let Some(sent) = sent_at.get(&frame.rtp_time) {
            self.latencies.push(decoded.duration_since(*sent));
        }
        self.pcm.extend_from_slice(&pcm);
        self.frames += 1;
        Ok(())
    }

    /// The tone in the decoded audio after the warm-up, over at most the last [`ANALYSIS_FRAMES`] frames.
    fn frequency(&self) -> Option<f64> {
        let skip = (WARM_UP_FRAMES * FRAME_SAMPLES).min(self.pcm.len());
        let steady = &self.pcm[skip..];
        let tail = steady.len().saturating_sub(ANALYSIS_FRAMES * FRAME_SAMPLES);
        dominant_frequency(&steady[tail..], SAMPLE_RATE)
    }
}
