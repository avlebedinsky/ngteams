//! One participant's leg of a room: its str0m `Rtc` on the node's socket, its SDP in Teams' dialect, its Opus decoder
//! and encoder and its control plane.
//!
//! - **One exchange makes a leg**: the mixer answers a caller's or joiner's offer, or offers to a callee and takes
//!   its answer. The mixer never offers again: every line a call uses is in that exchange.
//! - **An answer is a function of the leg and the offer**: the same offer again gets the same answer (its first, kept),
//!   and a renegotiation's answer keeps the leg's ICE, DTLS, candidates and streams; only the origin's version moves.
//! - **Video** (`super::video`): every video line is taken as offered, H.264 at the mixer's payload numbers (`107`, its
//!   retransmissions `99`). What the participant's camera and screen send is handed to the room as frames; the room
//!   writes the frames a receiving line is to show onto it ([`Leg::write_video`]), only once the leg seeded its
//!   bandwidth (`applyChannelParameters`' `sendSideBWSeed`): Teams' mixer sends no video before the seed either.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer as Str0mAnswer, SdpOffer as Str0mOffer, SdpPendingOffer};
use str0m::channel::ChannelId;
use str0m::format::CodecExtra;
use str0m::media::{Direction, Frequency, KeyframeRequestKind, MediaData, MediaKind, MediaTime, Mid, Pt};
use str0m::net::Receive;
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use teams_sdp::mcp::{CHANNEL_LABEL, MIXER_CHANNEL_LABEL};
use teams_sdp::sdp::{NgcSdp, NgcSession, StreamIds, VideoLines};
use teams_sdp::{SdpAnswer, SdpOffer};

use super::audio::{Buffer, FRAME, RATE, Took};
use super::control::{Control, SourceRequest};
use super::video::{Frame, LineKind, VideoLine, video_lines};
use crate::calling::{Sdp, Sources};

/// The video lines a leg takes: every one offered (the mixer sends on each line the client receives on).
const VIDEO: VideoLines = VideoLines::Offered;
/// The video lines of the mixer's offer to a callee: the main line, nine receiving lines and the sharing line.
const OFFERED_VIDEO: usize = 11;
/// H.264's payload numbers, and its retransmissions', as Teams' mixer numbers them.
const H264_PT: u8 = 107;
const H264_RTX_PT: u8 = 99;
/// Constrained baseline H.264, level 3.1: what the client sends.
const H264_PROFILE: u32 = 0x42_e0_1f;
/// The most renegotiations a leg takes in [`RENEGOTIATION_WINDOW`].
const RENEGOTIATIONS: usize = 20;
const RENEGOTIATION_WINDOW: Duration = Duration::from_mins(1);
/// The encoder's bit rate.
const BITRATE: i32 = 32_000;
/// The largest Opus packet written.
const MAX_PACKET: usize = 1500;
/// The most samples one Opus packet decodes to (120 ms).
const MOST_DECODED: usize = 5760;
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
    /// The video lines of the leg's exchange.
    video: Vec<VideoLine>,
    /// The bandwidth the leg seeded (bits a second): no video goes to it before.
    pub seed: Option<u64>,
    channel: Option<ChannelId>,
    control: Control,
    decoder: opus::Decoder,
    encoder: opus::Encoder,
    clip: opus::SoftClip,
    buffer: Buffer,
    /// What a packet decodes into.
    pcm: Vec<f32>,
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
    /// The frames its camera and screen sent.
    pub frames: Vec<Frame>,
    /// Its source requests, in order.
    pub requests: Vec<SourceRequest>,
    /// The receiving lines (by stream id) whose decoder asked for a keyframe.
    pub keyframes: Vec<u32>,
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
        let mut config = RtcConfig::new()
            .clear_codecs()
            .enable_opus(true, false)
            .set_ice_lite(making.ice_lite && offer.is_some());
        config
            .codec_config()
            .add_h264(H264_PT.into(), Some(H264_RTX_PT.into()), true, H264_PROFILE);
        let mut rtc = config.build(now);
        for address in &making.candidates {
            let candidate = Candidate::host(*address, "udp").map_err(|error| LegError::Sdp(error.to_string()))?;
            rtc.add_local_candidate(candidate);
        }
        let mut dialect = NgcSession::mixer();
        dialect.set_stream_ids(stream_ids(sources));
        dialect.set_slot_stream_ids(sources.first_slot());
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
            video: Vec::new(),
            seed: None,
            channel: None,
            control: Control::default(),
            decoder: opus::Decoder::new(RATE, opus::Channels::Mono)?,
            encoder,
            clip: opus::SoftClip::new(opus::Channels::Mono),
            buffer: Buffer::default(),
            pcm: vec![0.0; MOST_DECODED],
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

    /// The mixer's offer: its audio line, the video lines (each both ways) and a data channel of its own, on which the
    /// client opens its control plane.
    fn offer(&mut self) -> Result<Sdp, LegError> {
        let mut change = self.rtc.sdp_api();
        let audio = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        for _ in 0..OFFERED_VIDEO {
            change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None);
        }
        change.add_channel(MIXER_CHANNEL_LABEL.to_owned());
        let (offer, pending) = change.apply().ok_or(LegError::NoOffer)?;
        self.audio = Some(audio);
        self.pending = Some(pending);
        let offer = offer.to_sdp_string();
        let written = self.dialect.write_offer(&SdpOffer::new(offer.clone()))?;
        self.video = video_lines(&offer, written.as_str());
        Ok(ours(&written))
    }

    /// The mixer's answer to `offer`.
    fn answer(&mut self, offer: &Sdp) -> Result<Sdp, LegError> {
        let offer = self.dialect.read_offer(&standard(offer)?)?;
        let offer = Str0mOffer::from_sdp_string(&offer.to_sdp_string())?;
        let answer = self.rtc.sdp_api().accept_offer(offer)?.to_sdp_string();
        let written = self.dialect.write_answer(&SdpAnswer::new(answer.clone()))?;
        self.video = video_lines(&answer, written.as_str());
        Ok(ours(&written))
    }

    /// The callee's answer to the mixer's offer. A leg whose exchange is done takes an answer as a no-op: signaling
    /// sends a callee's answer again only as a retry. A leg whose offer an answer failed has no offer left to answer.
    pub(crate) fn accept(&mut self, answer: &Sdp, now: Instant) -> Result<(), LegError> {
        if self.pending.is_none() {
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
            Event::MediaData(data) => driven.frames.extend(self.frame(data)),
            Event::KeyframeRequest(request) => driven.keyframes.extend(self.slot_of(request.mid)),
            Event::ChannelOpen(id, label) if label == CHANNEL_LABEL => self.channel = Some(id),
            Event::ChannelData(data) if Some(data.id) == self.channel => {
                let was = self.control.acked();
                let received = self.control.receive(&data.data);
                for packet in received.answers {
                    self.write_channel(&packet);
                }
                driven.requests.extend(received.requests);
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
        let pcm = &mut self.pcm;
        if !contiguous
            && self.decoded
            && let Ok(recovered) = self.decoder.decode_float(packet, &mut pcm[..FRAME], true)
        {
            self.buffer.push(&pcm[..recovered]);
        }
        match self.decoder.decode_float(packet, pcm, false) {
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
    pub(crate) fn send(&mut self, mix: Option<&[f32; FRAME]>, now: Instant) {
        let mut frame = mix.copied().unwrap_or([0.0; FRAME]);
        self.clip.apply(&mut frame);
        let time = MediaTime::new(self.sent_samples, Frequency::FORTY_EIGHT_KHZ);
        self.sent_samples += FRAME as u64;
        let Some(mid) = self.audio else { return };
        let Some(packet) = self.encode(&frame) else { return };
        let Some(pt) = self.opus_pt(mid) else { return };
        let Some(writer) = self.rtc.writer(mid) else { return };
        if let Err(error) = writer.write(pt, now, time, packet) {
            tracing::debug!(%error, "a mix wasn't sent");
        }
    }

    /// `frame` in Opus; `None` for a DTX frame, which isn't sent.
    fn encode(&mut self, frame: &[f32; FRAME]) -> Option<Vec<u8>> {
        let mut packet = vec![0_u8; MAX_PACKET];
        match self.encoder.encode_float(frame, &mut packet) {
            Ok(length) if length > DTX_PACKET => {
                packet.truncate(length);
                Some(packet)
            }
            Ok(_) => None,
            Err(error) => {
                tracing::debug!(%error, "a mix didn't encode");
                None
            }
        }
    }

    fn opus_pt(&mut self, mid: Mid) -> Option<Pt> {
        let writer = self.rtc.writer(mid)?;
        writer
            .payload_params()
            .find(|params| params.spec().codec == str0m::format::Codec::Opus)
            .map(str0m::format::PayloadParams::pt)
    }

    /// What the line `mid` carries, if it is a video line of the leg's.
    fn line(&self, mid: Mid) -> Option<LineKind> {
        self.video.iter().find(|line| line.mid == mid).map(|line| line.kind)
    }

    /// The stream id of the receiving line `mid`.
    fn slot_of(&self, mid: Mid) -> Option<u32> {
        match self.line(mid)? {
            LineKind::Slot(stream) => Some(stream),
            LineKind::Main | LineKind::Sharing => None,
        }
    }

    /// A frame its camera (main line) or its screen (sharing line) sent, as its source.
    fn frame(&self, data: MediaData) -> Option<Frame> {
        let source = match self.line(data.mid)? {
            LineKind::Main => self.sources.video,
            LineKind::Sharing => self.sources.sharing,
            LineKind::Slot(_) => return None,
        };
        let keyframe = matches!(data.codec_extra, CodecExtra::H264(extra) if extra.is_keyframe);
        Some(Frame {
            source,
            time: data.time.numer(),
            keyframe,
            params: data.params,
            data: data.data,
        })
    }

    /// Writes `frame` at RTP time `time` onto the line `stream` names: a receiving line, or the leg's sharing line
    /// (someone's screen). Nothing goes before the leg seeded its bandwidth; whether it went.
    pub(crate) fn write_video(&mut self, stream: u32, time: u64, frame: &Frame, now: Instant) -> bool {
        if self.seed.is_none() {
            return false;
        }
        let kind = if stream == self.sources.sharing {
            LineKind::Sharing
        } else {
            LineKind::Slot(stream)
        };
        let line = self.video.iter().find(|line| line.kind == kind);
        let Some(mid) = line.map(|line| line.mid) else {
            return false;
        };
        let Some(writer) = self.rtc.writer(mid) else {
            return false;
        };
        let Some(pt) = writer.match_params(frame.params) else {
            return false;
        };
        let data: Arc<[u8]> = Arc::clone(&frame.data);
        match writer.write(pt, now, MediaTime::new(time, Frequency::NINETY_KHZ), data) {
            Ok(()) => true,
            Err(error) => {
                tracing::debug!(%error, "a video frame wasn't forwarded");
                false
            }
        }
    }

    /// Asks the participant for a keyframe of its source `source` (its camera's or its screen's); whether it went.
    pub(crate) fn request_keyframe(&mut self, source: u32) -> bool {
        let kind = if source == self.sources.video {
            LineKind::Main
        } else if source == self.sources.sharing {
            LineKind::Sharing
        } else {
            return false;
        };
        let Some(mid) = self.video.iter().find(|line| line.kind == kind).map(|line| line.mid) else {
            return false;
        };
        let Some(mut writer) = self.rtc.writer(mid) else {
            return false;
        };
        let kind = if writer.is_request_keyframe_possible(KeyframeRequestKind::Pli) {
            KeyframeRequestKind::Pli
        } else {
            KeyframeRequestKind::Fir
        };
        writer.request_keyframe(None, kind).is_ok()
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
            offer.blob = format!("{}a=x-test:{n}\r\n", offer.blob);
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
    fn a_leg_whose_answer_failed_refuses_the_next_answer() {
        let now = Instant::now();
        let mut leg = Leg::new(&making(), SOURCES, None, now).unwrap();
        let offer = leg.first.clone();
        let mut rtc = RtcConfig::new().build(now);
        rtc.add_local_candidate(Candidate::host(SocketAddr::from(([192, 0, 2, 20], 50_000)), "udp").unwrap());
        let mut dialect = NgcSession::mixer();
        let theirs = dialect.read_offer(&standard(&offer).unwrap()).unwrap();
        let theirs = Str0mOffer::from_sdp_string(&theirs.to_sdp_string()).unwrap();
        let answer = rtc.sdp_api().accept_offer(theirs).unwrap();
        let answer = ours(&dialect.write_answer(&SdpAnswer::new(answer.to_sdp_string())).unwrap());
        let unsigned = Sdp {
            blob: answer
                .blob
                .split_inclusive("\r\n")
                .filter(|line| !line.starts_with("a=fingerprint"))
                .collect(),
            content_type: answer.content_type.clone(),
        };
        assert!(leg.accept(&unsigned, now).is_err());
        assert!(leg.pending.is_none(), "the failed answer used the offer");
        assert!(matches!(leg.accept(&answer, now), Err(LegError::NoOffer)));
        assert!(leg.health.exchanged.is_none());
    }

    #[test]
    fn a_leg_whose_exchange_is_done_takes_an_answer_as_a_no_op_and_a_bad_offer_is_refused() {
        let now = Instant::now();
        let (_, _, offer, _) = client();
        let mut answered = Leg::new(&making(), SOURCES, Some(&offer), now).unwrap();
        let exchanged = answered.health.exchanged;
        let later = now + Duration::from_secs(1);
        answered.accept(&offer, later).unwrap();
        assert_eq!(answered.health.exchanged, exchanged, "nothing changed");
        let bad = Sdp {
            blob: "not sdp".into(),
            content_type: "application/sdp-ngc-1.0".into(),
        };
        assert!(Leg::new(&making(), SOURCES, Some(&bad), now).is_err());
    }

    /// A client's group offer in str0m: audio, the main video line, nine receiving lines, the sharing line and its
    /// control plane's channel, in the dialect.
    fn group_client() -> Sdp {
        let mut rtc = RtcConfig::new().build(Instant::now());
        rtc.add_local_candidate(Candidate::host(SocketAddr::from(([192, 0, 2, 20], 50_000)), "udp").unwrap());
        let mut change = rtc.sdp_api();
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        for _ in 0..OFFERED_VIDEO {
            change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None);
        }
        change.add_channel(CHANNEL_LABEL.to_owned());
        let (offer, _) = change.apply().unwrap();
        let mut dialect = NgcSession::mixer();
        ours(&dialect.write_offer(&SdpOffer::new(offer.to_sdp_string())).unwrap())
    }

    /// The stream ids the video lines of `sdp` name, in line order.
    fn video_streams(sdp: &Sdp) -> Vec<u32> {
        sdp.blob
            .split("m=")
            .filter(|section| section.starts_with("video") && !section.starts_with("video 0 "))
            .filter_map(|section| {
                section
                    .split("\r\n")
                    .find_map(|line| line.strip_prefix("a=x-source-streamid:"))
                    .and_then(|id| id.parse().ok())
            })
            .collect()
    }

    #[test]
    fn a_group_offer_is_answered_on_every_video_line_each_with_a_stream_of_its_own() {
        let now = Instant::now();
        let mut leg = Leg::new(&making(), SOURCES, Some(&group_client()), now).unwrap();
        let mut wanted = vec![1001];
        wanted.extend(1004..1013);
        wanted.push(1002);
        assert_eq!(video_streams(&leg.first), wanted, "{}", leg.first.blob);
        assert!(leg.first.blob.contains("H264/90000"), "{}", leg.first.blob);
        let kinds: Vec<LineKind> = leg.video.iter().map(|line| line.kind).collect();
        assert_eq!(kinds.first(), Some(&LineKind::Main));
        assert_eq!(kinds.last(), Some(&LineKind::Sharing));
        assert_eq!(kinds.get(1), Some(&LineKind::Slot(1004)));
        // a frame for a receiving line goes only once the leg seeded its bandwidth
        let main = leg.video[0].mid;
        let params = *leg.rtc.writer(main).unwrap().payload_params().next().unwrap();
        let frame = Frame {
            source: 1021,
            time: 90_000,
            keyframe: true,
            params,
            data: Arc::from(&[0_u8, 0, 0, 1, 0x65][..]),
        };
        assert!(!leg.write_video(1004, 90_000, &frame, now));
        leg.seed = Some(4_000_000);
        assert!(leg.write_video(1004, 90_000, &frame, now));
        assert!(
            leg.write_video(SOURCES.sharing, 90_000, &frame, now),
            "someone's screen"
        );
        assert!(!leg.write_video(1003, 90_000, &frame, now), "no line of that stream");
        // its own camera and screen are asked for keyframes; nothing else of its is
        assert!(leg.request_keyframe(SOURCES.video));
        assert!(leg.request_keyframe(SOURCES.sharing));
        assert!(!leg.request_keyframe(1021));
        assert_eq!(leg.slot_of(leg.video[1].mid), Some(1004));
        assert_eq!(leg.slot_of(main), None);
    }

    #[test]
    fn a_callees_leg_is_offered_every_video_line() {
        let leg = Leg::new(&making(), SOURCES, None, Instant::now()).unwrap();
        let streams = video_streams(&leg.first);
        assert_eq!(streams.len(), OFFERED_VIDEO, "{}", leg.first.blob);
        assert_eq!(streams.first(), Some(&1001));
        assert_eq!(streams.last(), Some(&1002));
        assert!(leg.first.blob.contains("a=rtpmap:107 H264/90000"), "{}", leg.first.blob);
        assert_eq!(leg.video.len(), OFFERED_VIDEO);
    }
}
