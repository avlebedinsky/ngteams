//! The echo probe: once the call's media is connected, send a tone as Opus in real time, decode whatever comes back,
//! and report the frequency heard in the echo and the round-trip delay of each frame.
//!
//! It runs over any [`MediaPath`] (direct UDP or a TURN relay). The SDP exchange is not part of it: in a real call it
//! goes through signaling, in the tests straight from peer to peer.

use std::collections::HashMap;
use std::fmt;
use std::thread;
use std::time::{Duration, Instant};

use media_loop::codec::{OpusDecoder, OpusEncoder};
use media_loop::goertzel::dominant_frequency;
use media_loop::loopback::LatencyStats;
use media_loop::peer::PeerEvent;
use media_loop::tone::ToneGenerator;
use media_loop::{FRAME_DURATION, FRAME_SAMPLES, SAMPLE_RATE};

use crate::Error;
use crate::relay::MediaPath;

/// Decoded frames skipped before the frequency check (the decoder's warm-up, and the echo service's own start).
const WARM_UP_FRAMES: usize = 5;
/// Most decoded frames the frequency check looks at (the last ones), to bound its cost.
const ANALYSIS_FRAMES: usize = 50;
/// Nap between run-loop turns when nothing arrived.
const IDLE_NAP: Duration = Duration::from_micros(200);

/// What the probe sends and how long it waits.
#[derive(Clone, Debug, PartialEq)]
pub struct EchoConfig {
    pub tone_hz: f64,
    pub amplitude: f64,
    /// 20 ms frames to send.
    pub frames: usize,
    /// How long ICE + DTLS may take.
    pub connect_timeout: Duration,
    /// How long to wait for echoes after the last frame. Teams' echo service plays back what it recorded after the
    /// caller stops talking, so a real run needs seconds here; loopback needs one.
    pub drain_timeout: Duration,
}

impl Default for EchoConfig {
    fn default() -> Self {
        Self {
            tone_hz: 440.0,
            amplitude: 0.5,
            frames: 50,
            connect_timeout: Duration::from_secs(10),
            drain_timeout: Duration::from_secs(1),
        }
    }
}

/// What the probe measured.
#[derive(Clone, Debug, PartialEq)]
pub struct EchoReport {
    pub sent_hz: f64,
    /// The dominant frequency of the echo after the warm-up.
    pub echoed_hz: Option<f64>,
    pub frames_sent: usize,
    pub frames_echoed: usize,
    /// SDP applied → connected.
    pub connect_time: Duration,
    /// Encode start of a frame → decode end of its echo, matched by RTP time. An echo service that re-times its
    /// playback gives no matches; then only the frequency tells.
    pub round_trip: Option<LatencyStats>,
}

impl fmt::Display for EchoReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hz = self.echoed_hz.map_or_else(|| "none".to_owned(), |v| format!("{v} Hz"));
        let rtt = self.round_trip.map_or_else(|| "none".to_owned(), |v| v.to_string());
        writeln!(f, "sent {} Hz, echoed {hz}", self.sent_hz)?;
        writeln!(
            f,
            "frames sent {} echoed {}; connect {:.2} ms",
            self.frames_sent,
            self.frames_echoed,
            self.connect_time.as_secs_f64() * 1_000.0
        )?;
        write!(f, "round trip: {rtt}")
    }
}

/// Turns `path` (and `side`, anything else the loop must keep running, such as a test's echo stand-in) until the
/// peer is connected. Returns how long it took.
///
/// # Errors
/// [`Error::Timeout`] when it doesn't connect within `timeout`, and any failure of the path or of `side`.
pub fn connect(
    path: &mut impl MediaPath,
    side: &mut impl FnMut(Instant) -> Result<(), Error>,
    timeout: Duration,
) -> Result<Duration, Error> {
    let start = Instant::now();
    while !path.peer().is_connected() {
        if start.elapsed() > timeout {
            return Err(Error::Timeout("ICE + DTLS of the call's media"));
        }
        let now = Instant::now();
        let arrived = path.turn(now)?;
        side(now)?;
        if !arrived {
            thread::sleep(IDLE_NAP);
        }
    }
    while path.peer().poll_event().is_some() {}
    Ok(start.elapsed())
}

/// Connects, sends the tone and listens to the echo.
///
/// # Errors
/// [`Error::Config`] for unusable settings, [`Error::Timeout`] when the media doesn't connect, a disconnect or close
/// mid-probe, and any failure of the path, of `side` or of Opus.
pub fn run_echo(
    path: &mut impl MediaPath,
    side: &mut impl FnMut(Instant) -> Result<(), Error>,
    config: &EchoConfig,
) -> Result<EchoReport, Error> {
    if config.frames == 0 || !(config.tone_hz > 0.0 && config.tone_hz < f64::from(SAMPLE_RATE) / 2.0) {
        return Err(Error::Config(
            "the tone must be inside (0, 24000) Hz and at least one frame long".into(),
        ));
    }
    let connect_time = connect(path, side, config.connect_timeout)?;
    let mut probe = Probe::new(config)?;
    let mut last_sent = Instant::now();
    loop {
        let now = Instant::now();
        let arrived = path.turn(now)?;
        side(now)?;
        while let Some(event) = path.peer().poll_event() {
            probe.on_event(event)?;
        }
        if let Some(sent) = probe.send_due(path, now)? {
            last_sent = sent;
        }
        let all_back = probe.echoed >= config.frames;
        if probe.done() && (all_back || now.duration_since(last_sent) > config.drain_timeout) {
            break;
        }
        if !arrived {
            thread::sleep(IDLE_NAP);
        }
    }
    Ok(probe.report(config.tone_hz, connect_time))
}

struct Probe {
    generator: ToneGenerator,
    encoder: OpusEncoder,
    decoder: OpusDecoder,
    frames: usize,
    index: usize,
    start: Option<Instant>,
    sent_at: HashMap<u64, Instant>,
    pcm: Vec<i16>,
    echoed: usize,
    latencies: Vec<Duration>,
}

impl Probe {
    fn new(config: &EchoConfig) -> Result<Self, Error> {
        Ok(Self {
            generator: ToneGenerator::new(config.tone_hz, SAMPLE_RATE, config.amplitude),
            encoder: OpusEncoder::new()?,
            decoder: OpusDecoder::new()?,
            frames: config.frames,
            index: 0,
            start: None,
            sent_at: HashMap::new(),
            pcm: Vec::new(),
            echoed: 0,
            latencies: Vec::new(),
        })
    }

    fn done(&self) -> bool {
        self.index >= self.frames
    }

    fn send_due(&mut self, path: &mut impl MediaPath, now: Instant) -> Result<Option<Instant>, Error> {
        let start = *self.start.get_or_insert(now);
        let index = u32::try_from(self.index).map_err(|e| Error::Config(e.to_string()))?;
        if self.done() || now < start + FRAME_DURATION * index {
            return Ok(None);
        }
        let rtp_time = u64::from(index) * u64::try_from(FRAME_SAMPLES).map_err(|e| Error::Config(e.to_string()))?;
        let mut frame = vec![0; FRAME_SAMPLES];
        self.generator.fill(&mut frame);
        let encode_start = Instant::now();
        let packet = self.encoder.encode(&frame)?;
        self.sent_at.insert(rtp_time, encode_start);
        path.peer()
            .write_audio(Instant::now(), encode_start, rtp_time, &packet)?;
        path.flush(now)?;
        self.index += 1;
        Ok(Some(encode_start))
    }

    fn on_event(&mut self, event: PeerEvent) -> Result<(), Error> {
        match event {
            PeerEvent::Audio(frame) => {
                let pcm = self.decoder.decode(&frame.payload)?;
                let decoded = Instant::now();
                if let Some(sent) = self.sent_at.get(&frame.rtp_time) {
                    self.latencies.push(decoded.duration_since(*sent));
                }
                self.pcm.extend_from_slice(&pcm);
                self.echoed += 1;
                Ok(())
            }
            PeerEvent::Connected => Ok(()),
            PeerEvent::Disconnected => Err(Error::Media(media_loop::Error::Rtc(
                "ICE disconnected mid-probe".into(),
            ))),
            PeerEvent::Closed => Err(Error::Media(media_loop::Error::Rtc(
                "the call's media closed mid-probe".into(),
            ))),
        }
    }

    fn report(&self, sent_hz: f64, connect_time: Duration) -> EchoReport {
        let skip = (WARM_UP_FRAMES * FRAME_SAMPLES).min(self.pcm.len());
        let steady = &self.pcm[skip..];
        let tail = steady.len().saturating_sub(ANALYSIS_FRAMES * FRAME_SAMPLES);
        EchoReport {
            sent_hz,
            echoed_hz: dominant_frequency(&steady[tail..], SAMPLE_RATE),
            frames_sent: self.index,
            frames_echoed: self.echoed,
            connect_time,
            round_trip: LatencyStats::from_samples(&self.latencies),
        }
    }
}
