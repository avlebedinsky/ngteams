//! One participant's leg of a room: its str0m `Rtc` on the node's socket, its SDP in Teams' dialect, its Opus decoder
//! and encoder and its control plane.
//!
//! - **One exchange makes a leg**: the mixer answers a caller's or joiner's offer, or offers to a callee and takes
//!   its answer. The mixer never offers again: every line a call uses is in that exchange.
//! - **An answer is a function of the leg and the offer**: the same offer again gets the same answer (its first, kept),
//!   and a renegotiation's answer keeps the leg's ICE, DTLS, candidates and streams; only the origin's version moves.
//! - **The 186/187 seam**: the video lines a leg takes are one switch here ([`VIDEO`]).

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer as Str0mAnswer, SdpOffer as Str0mOffer, SdpPendingOffer};
use str0m::channel::ChannelId;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid, Pt};
use str0m::net::Receive;
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use teams_sdp::mcp::{CHANNEL_LABEL, MIXER_CHANNEL_LABEL};
use teams_sdp::sdp::{NgcSdp, NgcSession, StreamIds, VideoLines};
use teams_sdp::{SdpAnswer, SdpOffer};

use super::audio::{Buffer, FRAME, RATE, Took};
use super::control::Control;
use crate::calling::{Sdp, Sources};

/// The video lines a leg takes: none in an audio mix (every offered video line is answered rejected).
const VIDEO: VideoLines = VideoLines::None;
/// The most renegotiations a leg takes in [`RENEGOTIATION_WINDOW`].
const RENEGOTIATIONS: usize = 20;
const RENEGOTIATION_WINDOW: Duration = Duration::from_mins(1);
/// The encoder's bit rate.
const BITRATE: i32 = 32_000;
/// The largest Opus packet written.
const MAX_PACKET: usize = 1500;
/// An Opus packet this short is a DTX frame: nothing to send.
const DTX_PACKET: usize = 2;

/// Why a leg didn't do what it was asked.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LegError {
    #[error("the SDP isn't usable: {0}")]
    Sdp(String),
    #[error("the leg has no offer of the mixer's waiting for an answer")]
    NoOffer,
    #[error("the leg's offer of the mixer's is still waiting for its answer")]
    Waiting,
    #[error("the leg renegotiated too often")]
    TooOften,
    #[error("the audio codec failed: {0}")]
    Codec(String),
}

impl From<teams_sdp::sdp::SdpError> for LegError {
    fn from(error: teams_sdp::sdp::SdpError) -> Self {
        Self::Sdp(error.to_string())
    }
}

impl From<str0m::RtcError> for LegError {
    fn from(error: str0m::RtcError) -> Self {
        Self::Sdp(error.to_string())
    }
}

impl From<str0m::error::SdpError> for LegError {
    fn from(error: str0m::error::SdpError) -> Self {
        Self::Sdp(error.to_string())
    }
}

impl From<opus::Error> for LegError {
    fn from(error: opus::Error) -> Self {
        Self::Codec(error.to_string())
    }
}

/// What every leg of a node is made with.
#[derive(Clone, Debug)]
pub(crate) struct Making {
    /// The node's host candidates.
    pub candidates: Vec<SocketAddr>,
    pub ice_lite: bool,
}

/// Where a leg's connection is, for the node's judgement of whether its media is lost.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Health {
    /// Since when it has had an exchange done (its answer made or taken); none while the mixer's offer waits.
    pub exchanged: Option<Instant>,
    /// Whether DTLS came up.
    pub connected: bool,
    /// Since when ICE is down, once it was up.
    pub down_since: Option<Instant>,
}

/// One participant's leg.
pub(crate) struct Leg {
    pub sources: Sources,
    rtc: Rtc,
    dialect: NgcSession,
    /// The mixer's offer, until the callee's answer comes.
    pending: Option<SdpPendingOffer>,
    /// What the first exchange gave: a repeated make gets it again.
    pub first: Sdp,
    /// The last renegotiation's offer and answer: the same offer again gets the same answer.
    last: Option<(String, Sdp)>,
    renegotiated: VecDeque<Instant>,
    audio: Option<Mid>,
    channel: Option<ChannelId>,
    control: Control,
    decoder: opus::Decoder,
    encoder: opus::Encoder,
    clip: opus::SoftClip,
    buffer: Buffer,
    /// Whether a packet was decoded yet: a gap before the first is no loss.
    decoded: bool,
    sent_samples: u64,
    pub health: Health,
    /// When the `Rtc` wants its next timeout.
    pub due: Instant,
}

impl std::fmt::Debug for Leg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Leg")
            .field("sources", &self.sources)
            .field("health", &self.health)
            .finish_non_exhaustive()
    }
}

/// What a leg's `Rtc` gave while it was driven.
#[derive(Debug, Default)]
pub(crate) struct Driven {
    /// Its control plane came up (the client's handshake answered).
    pub control_up: bool,
}

fn standard(sdp: &Sdp) -> Result<NgcSdp, LegError> {
    Ok(NgcSdp::new(&sdp.content_type, &sdp.blob)?)
}

fn ours(written: &NgcSdp) -> Sdp {
    Sdp {
        blob: written.as_str().to_owned(),
        content_type: written.content_type().to_owned(),
    }
}

fn stream_ids(sources: Sources) -> StreamIds {
    StreamIds {
        audio: sources.audio,
        video: sources.video,
        sharing: sources.sharing,
        data: sources.data,
    }
}

impl Leg {
    /// A new leg with `sources`: the answer to `offer` (a caller's or a joiner's), or without one the mixer's offer
    /// (to a callee).
    pub(crate) fn new(making: &Making, sources: Sources, offer: Option<&Sdp>, now: Instant) -> Result<Self, LegError> {
        // where the mixer offers it checks too: its SDP can't say it is lite (the client reads no `a=ice-lite`), and
        // two controlled agents never nominate a pair
        let mut rtc = RtcConfig::new()
            .set_ice_lite(making.ice_lite && offer.is_some())
            .build(now);
        for address in &making.candidates {
            let candidate = Candidate::host(*address, "udp").map_err(|error| LegError::Sdp(error.to_string()))?;
            rtc.add_local_candidate(candidate);
        }
        let mut dialect = NgcSession::mixer();
        dialect.set_stream_ids(stream_ids(sources));
        dialect.set_video(VIDEO);
        let mut encoder = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)?;
        encoder.set_bitrate(opus::Bitrate::Bits(BITRATE))?;
        encoder.set_inband_fec(true)?;
        encoder.set_dtx(true)?;
        let mut leg = Self {
            sources,
            rtc,
            dialect,
            pending: None,
            first: Sdp {
                blob: String::new(),
                content_type: String::new(),
            },
            last: None,
            renegotiated: VecDeque::new(),
            audio: None,
            channel: None,
            control: Control::default(),
            decoder: opus::Decoder::new(RATE, opus::Channels::Mono)?,
            encoder,
            clip: opus::SoftClip::new(opus::Channels::Mono),
            buffer: Buffer::default(),
            decoded: false,
            sent_samples: 0,
            health: Health {
                exchanged: None,
                connected: false,
                down_since: None,
            },
            due: now,
        };
        leg.first = match offer {
            Some(offer) => {
                let answer = leg.answer(offer)?;
                leg.health.exchanged = Some(now);
                answer
            }
            None => leg.offer()?,
        };
        Ok(leg)
    }

    /// The mixer's offer: its audio line and a data channel of its own, on which the client opens its control plane.
    fn offer(&mut self) -> Result<Sdp, LegError> {
        let mut change = self.rtc.sdp_api();
        let audio = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        change.add_channel(MIXER_CHANNEL_LABEL.to_owned());
        let (offer, pending) = change.apply().ok_or(LegError::NoOffer)?;
        self.audio = Some(audio);
        self.pending = Some(pending);
        let written = self.dialect.write_offer(&SdpOffer::new(offer.to_sdp_string()))?;
        Ok(ours(&written))
    }

    /// The mixer's answer to `offer`.
    fn answer(&mut self, offer: &Sdp) -> Result<Sdp, LegError> {
        let offer = self.dialect.read_offer(&standard(offer)?)?;
        let offer = Str0mOffer::from_sdp_string(&offer.to_sdp_string())?;
        let answer = self.rtc.sdp_api().accept_offer(offer)?;
        let written = self.dialect.write_answer(&SdpAnswer::new(answer.to_sdp_string()))?;
        Ok(ours(&written))
    }

    /// The callee's answer to the mixer's offer.
    pub(crate) fn accept(&mut self, answer: &Sdp, now: Instant) -> Result<(), LegError> {
        if self.pending.is_none() {
            // the same answer again, once taken
            return if self.health.exchanged.is_some() {
                Ok(())
            } else {
                Err(LegError::NoOffer)
            };
        }
        let read = self.dialect.read_answer(&standard(answer)?)?;
        let parsed = Str0mAnswer::from_sdp_string(&read.to_sdp_string())?;
        let pending = self.pending.take().ok_or(LegError::NoOffer)?;
        self.rtc.sdp_api().accept_answer(pending, parsed)?;
        self.health.exchanged = Some(now);
        Ok(())
    }

    /// A client's renegotiation: the mixer's answer.
    pub(crate) fn renegotiate(&mut self, offer: &Sdp, now: Instant) -> Result<Sdp, LegError> {
        if let Some((last, answer)) = &self.last
            && *last == offer.blob
        {
            return Ok(answer.clone());
        }
        if self.pending.is_some() {
            return Err(LegError::Waiting);
        }
        while self
            .renegotiated
            .front()
            .is_some_and(|at| now.duration_since(*at) > RENEGOTIATION_WINDOW)
        {
            self.renegotiated.pop_front();
        }
        if self.renegotiated.len() >= RENEGOTIATIONS {
            return Err(LegError::TooOften);
        }
        let answer = self.answer(offer)?;
        self.renegotiated.push_back(now);
        self.last = Some((offer.blob.clone(), answer.clone()));
        Ok(answer)
    }

    /// Whether `input` (a datagram the node's socket got) is this leg's.
    pub(crate) fn accepts(&self, input: &Input<'_>) -> bool {
        self.rtc.accepts(input)
    }

    /// A datagram for this leg.
    pub(crate) fn receive(&mut self, now: Instant, receive: Receive<'_>) {
        if let Err(error) = self.rtc.handle_input(Input::Receive(now, receive)) {
            tracing::debug!(%error, "a media datagram wasn't taken");
        }
    }

    /// Lets the `Rtc` act on time passing.
    pub(crate) fn tick(&mut self, now: Instant) {
        if now >= self.due
            && let Err(error) = self.rtc.handle_input(Input::Timeout(now))
        {
            tracing::debug!(%error, "a leg's timeout failed");
        }
    }

    /// Everything the `Rtc` has to give: datagrams out through `send`, events taken in.
    pub(crate) fn drive(&mut self, now: Instant, send: &mut dyn FnMut(SocketAddr, &[u8])) -> Driven {
        let mut driven = Driven::default();
        loop {
            match self.rtc.poll_output() {
                Ok(Output::Timeout(at)) => {
                    self.due = at;
                    return driven;
                }
                Ok(Output::Transmit(transmit)) => send(transmit.destination, &transmit.contents),
                Ok(Output::Event(event)) => self.event(event, now, &mut driven),
                Err(error) => {
                    tracing::debug!(%error, "a leg's connection failed");
                    self.due = now + Duration::from_millis(100);
                    return driven;
                }
            }
        }
    }

    fn event(&mut self, event: Event, now: Instant, driven: &mut Driven) {
        match event {
            Event::Connected => self.health.connected = true,
            Event::IceConnectionStateChange(state) => self.ice(state, now),
            Event::MediaAdded(added) if added.kind == MediaKind::Audio => self.audio = Some(added.mid),
            Event::MediaData(data) if Some(data.mid) == self.audio => self.decode(&data.data, data.contiguous),
            Event::ChannelOpen(id, label) if label == CHANNEL_LABEL => self.channel = Some(id),
            Event::ChannelData(data) if Some(data.id) == self.channel => {
                let was = self.control.acked();
                for packet in self.control.receive(&data.data) {
                    self.write_channel(&packet);
                }
                driven.control_up |= !was && self.control.acked();
            }
            _ => {}
        }
    }

    fn ice(&mut self, state: IceConnectionState, now: Instant) {
        match state {
            IceConnectionState::Connected | IceConnectionState::Completed => self.health.down_since = None,
            IceConnectionState::Disconnected => {
                self.health.down_since.get_or_insert(now);
            }
            IceConnectionState::New | IceConnectionState::Checking => {}
        }
    }

    /// An Opus packet that came: decoded into the buffer, the frame lost before it first from its in-band FEC.
    fn decode(&mut self, packet: &[u8], contiguous: bool) {
        let mut pcm = [0.0_f32; 5760];
        if !contiguous
            && self.decoded
            && let Ok(recovered) = self.decoder.decode_float(packet, &mut pcm[..FRAME], true)
        {
            self.buffer.push(&pcm[..recovered]);
        }
        match self.decoder.decode_float(packet, &mut pcm, false) {
            Ok(decoded) => {
                self.decoded = true;
                self.buffer.push(&pcm[..decoded]);
            }
            Err(error) => tracing::debug!(%error, "an Opus packet didn't decode"),
        }
    }

    /// This tick's frame of what the leg's participant says.
    pub(crate) fn take(&mut self, frame: &mut [f32; FRAME]) {
        if self.buffer.take(frame) == Took::Conceal && self.decoder.decode_float(&[], frame, false).is_err() {
            frame.fill(0.0);
        }
    }

    /// This tick's mix for the leg's participant (`None`: silence), encoded and sent.
    pub(crate) fn send(&mut self, mix: Option<[f32; FRAME]>, now: Instant) {
        let mut frame = mix.unwrap_or([0.0; FRAME]);
        self.clip.apply(&mut frame);
        let time = MediaTime::new(self.sent_samples, Frequency::FORTY_EIGHT_KHZ);
        self.sent_samples += FRAME as u64;
        let Some(mid) = self.audio else { return };
        let mut packet = [0_u8; MAX_PACKET];
        let length = match self.encoder.encode_float(&frame, &mut packet) {
            Ok(length) if length > DTX_PACKET => length,
            Ok(_) => return,
            Err(error) => {
                tracing::debug!(%error, "a mix didn't encode");
                return;
            }
        };
        let Some(pt) = self.opus_pt(mid) else { return };
        let Some(writer) = self.rtc.writer(mid) else { return };
        if let Err(error) = writer.write(pt, now, time, packet[..length].to_vec()) {
            tracing::debug!(%error, "a mix wasn't sent");
        }
    }

    fn opus_pt(&mut self, mid: Mid) -> Option<Pt> {
        let writer = self.rtc.writer(mid)?;
        writer
            .payload_params()
            .find(|params| params.spec().codec == str0m::format::Codec::Opus)
            .map(|params| params.pt())
    }

    /// Tells the leg's participant the dominant speakers, once its control plane is up.
    pub(crate) fn tell_speakers(&mut self, history: &[u32]) {
        if self.control.acked() {
            let packet = self.control.dominant(history);
            self.write_channel(&packet);
        }
    }

    pub(crate) fn control_up(&self) -> bool {
        self.control.acked()
    }

    fn write_channel(&mut self, packet: &[u8]) {
        let Some(id) = self.channel else { return };
        if let Some(mut channel) = self.rtc.channel(id)
            && let Err(error) = channel.write(true, packet)
        {
            tracing::debug!(%error, "a control message wasn't sent");
        }
    }

    /// Closes the connection.
    pub(crate) fn close(&mut self) {
        self.rtc.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCES: Sources = Sources {
        audio: 1000,
        video: 1001,
        sharing: 1002,
        data: 1003,
    };

    fn making() -> Making {
        super::super::install_crypto();
        Making {
            candidates: vec![SocketAddr::from(([192, 0, 2, 10], 3480))],
            ice_lite: true,
        }
    }

    /// A client's side in str0m: its offer of audio and its control plane's channel, in the dialect.
    fn client() -> (Rtc, NgcSession, Sdp, SdpPendingOffer) {
        let mut rtc = RtcConfig::new().build(Instant::now());
        rtc.add_local_candidate(Candidate::host(SocketAddr::from(([192, 0, 2, 20], 50_000)), "udp").unwrap());
        let mut change = rtc.sdp_api();
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        change.add_channel(CHANNEL_LABEL.to_owned());
        let (offer, pending) = change.apply().unwrap();
        let mut dialect = NgcSession::mixer();
        let written = dialect.write_offer(&SdpOffer::new(offer.to_sdp_string())).unwrap();
        (rtc, dialect, ours(&written), pending)
    }

    /// The lines of `sdp` but its origin's.
    fn lines(sdp: &Sdp) -> Vec<&str> {
        sdp.blob.split("\r\n").filter(|line| !line.starts_with("o=")).collect()
    }

    #[test]
    fn the_same_offer_is_answered_with_the_same_lines() {
        let (_, _, offer, _) = client();
        let now = Instant::now();
        let mut leg = Leg::new(&making(), SOURCES, Some(&offer), now).unwrap();
        let first = leg.first.clone();
        assert!(first.blob.contains("typ host"), "{}", first.blob);
        assert!(first.blob.contains("a=x-source-streamid:1000"), "{}", first.blob);
        assert!(first.blob.contains("a=x-source-streamid:1003"), "{}", first.blob);
        let again = leg.renegotiate(&offer, now).unwrap();
        assert_eq!(lines(&again), lines(&first));
        // the same renegotiation once more is the same answer, to the byte
        assert_eq!(leg.renegotiate(&offer, now).unwrap(), again);
    }

    #[test]
    fn a_leg_renegotiates_at_most_twenty_times_a_minute() {
        let now = Instant::now();
        let mut leg = Leg::new(&making(), SOURCES, Some(&client().2), now).unwrap();
        for n in 0..RENEGOTIATIONS {
            let mut offer = client().2;
            offer.blob.push_str(&format!("a=x-test:{n}\r\n"));
            leg.renegotiate(&offer, now).unwrap();
        }
        assert!(matches!(leg.renegotiate(&client().2, now), Err(LegError::TooOften)));
        let later = now + RENEGOTIATION_WINDOW + Duration::from_secs(1);
        leg.renegotiate(&client().2, later).unwrap();
    }

    #[test]
    fn a_callees_leg_offers_once_and_takes_its_answer() {
        let now = Instant::now();
        let mut leg = Leg::new(&making(), SOURCES, None, now).unwrap();
        let offer = leg.first.clone();
        assert!(offer.blob.contains("m=audio 3480"), "{}", offer.blob);
        assert!(leg.health.exchanged.is_none());
        assert!(matches!(leg.renegotiate(&client().2, now), Err(LegError::Waiting)));
        // the client answers as a callee
        let mut rtc = RtcConfig::new().build(now);
        rtc.add_local_candidate(Candidate::host(SocketAddr::from(([192, 0, 2, 20], 50_000)), "udp").unwrap());
        let mut dialect = NgcSession::mixer();
        let theirs = dialect.read_offer(&standard(&offer).unwrap()).unwrap();
        let theirs = Str0mOffer::from_sdp_string(&theirs.to_sdp_string()).unwrap();
        let answer = rtc.sdp_api().accept_offer(theirs).unwrap();
        let answer = ours(&dialect.write_answer(&SdpAnswer::new(answer.to_sdp_string())).unwrap());
        let unreadable = Sdp {
            blob: "v=0\r\n".into(),
            content_type: answer.content_type.clone(),
        };
        assert!(leg.accept(&unreadable, now).is_err());
        leg.accept(&answer, now).unwrap();
        assert!(leg.health.exchanged.is_some());
        leg.accept(&answer, now).unwrap();
        assert_eq!(leg.first, offer, "a repeated make gets the same offer");
    }

    #[test]
    fn an_answer_without_an_offer_of_the_mixers_is_refused() {
        let now = Instant::now();
        let (_, _, offer, _) = client();
        let mut fresh = Leg::new(&making(), SOURCES, Some(&offer), now).unwrap();
        fresh.health.exchanged = None;
        assert!(matches!(fresh.accept(&offer, now), Err(LegError::NoOffer)));
        let bad = Sdp {
            blob: "not sdp".into(),
            content_type: "application/sdp-ngc-1.0".into(),
        };
        assert!(Leg::new(&making(), SOURCES, Some(&bad), now).is_err());
    }
}
