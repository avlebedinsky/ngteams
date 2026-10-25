//! The call's WebRTC peer on `str0m`: its audio, and the one video line a 1:1 call carries.
//!
//! - [`Peer`] is sans-IO: it owns no socket and no clock, takes datagrams and `now` in and hands datagrams, events and
//!   its next deadline out. Every mutation of the `Rtc` (input, write, SDP change) is followed by a full drain of
//!   `poll_output` to its next `Timeout`, as `str0m` requires; the drain queues datagrams and events for the caller.
//! - [`RelayedPeer`] puts it behind a TURN allocation ([`RelayLink`]): its one candidate is the allocation, what it
//!   sends leaves through the server and what peers send to the allocation comes back in. A call's media goes only this
//!   way (relayed candidates only, no local address in an offer).
//!
//! A Teams offer carries video, sharing and data lines beside the audio: the peer answers them as `str0m` does (which
//! lines it takes is the dialect's to say: [`crate::sdp::NgcSession::set_video`]), but its audio is the line its own
//! offer added or the offer's first audio line, and only that line's frames come out as [`PeerEvent::Audio`]; its
//! video is the line its own offer added or the first video line it took, whose frames come out as
//! [`PeerEvent::Video`]. H.264 is offered as the web client offers constrained baseline ([`H264_PT`]); the bandwidth
//! estimate (`str0m`'s, on the transport-wide congestion control feedback) is told as [`PeerEvent::Bitrate`].
//!
//! `str0m` hands every audio frame over the moment it arrives, in arrival order: the receiving side's jitter buffer
//! ([`crate::jitter`]) puts frames back in order and decides what a gap means, so `str0m` holds nothing back waiting
//! for a lost packet (its audio reordering window is 0).

use std::collections::VecDeque;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use str0m::bwe::{Bitrate, BweKind};
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::format::{Codec, FormatParams};
use str0m::media::{Direction, Frequency, KeyframeRequestKind, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};

use crate::Error;
use crate::codec::OpusConfig;
use crate::relay::RelayLink;
use crate::turn::TurnEvent;

/// The payload type our offers give Opus: the media controller's own number for it. Answering an Opus-only offer
/// that numbered it `111` (the web client's number), the media controller answered `102`, sent Opus to us as `111`
/// and took none of ours as `111`; `str0m` keeps one number per codec for both ways, so the offer names Opus as the
/// media controller does and both ways agree (`docs/protocol/calling-echo.md`, "The native echo call"). A far end that
/// answers another number is read back with ours ([`crate::sdp::NgcSession::read_answer`]); an offer we answer keeps
/// its own numbers.
pub const OPUS_PT: u8 = 102;
/// The payload type our offers give H.264 (constrained baseline, `profile-level-id=42e01f`, `packetization-mode=1`),
/// and [`H264_RTX_PT`] its retransmissions: the web client's own numbers for it (its offers give `102` to another
/// H.264 profile, which ours already gives Opus).
pub const H264_PT: u8 = 108;
pub const H264_RTX_PT: u8 = 109;
/// The bandwidth estimate a call starts from, in bits per second.
pub const START_BITRATE: u64 = 300_000;

/// What a [`Peer`] reports to the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerEvent {
    /// ICE and DTLS are up: media can flow. Comes again when ICE recovers after a [`PeerEvent::Disconnected`].
    Connected,
    /// ICE lost the remote side and media can't flow. This may be transient: while the remote still sends checks,
    /// ICE rebuilds the pair and a later [`PeerEvent::Connected`] means the link is back. (Once both sides have
    /// pruned the pair, `str0m` sends nothing more and only an ICE restart would help.)
    Disconnected,
    /// The remote closed the session (DTLS close or SCTP loss). Final: nothing flows after it.
    Closed,
    /// One audio frame (an Opus packet) arrived.
    Audio(ReceivedAudio),
    /// One video frame (an H.264 access unit) arrived.
    Video(ReceivedVideo),
    /// The far end asks for a keyframe of what we send (PLI or FIR).
    KeyframeRequested,
    /// The bandwidth estimate of what we send, in bits per second.
    Bitrate(u64),
}

/// An incoming video frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedVideo {
    /// RTP media time in 90 kHz units.
    pub rtp_time: u64,
    /// The access unit, Annex B (start codes before each NAL unit).
    pub data: Arc<[u8]>,
    /// It is an IDR.
    pub keyframe: bool,
    /// It follows the frame before without a gap (`false`: a keyframe is likely wanted).
    pub contiguous: bool,
}

/// An incoming audio frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedAudio {
    /// RTP media time in 48 kHz units, as the sender wrote it (extended past 32-bit rollover).
    pub rtp_time: u64,
    /// When the first datagram of the frame was handed to the peer.
    pub network_time: Instant,
    /// The codec payload.
    pub payload: Arc<[u8]>,
}

/// A datagram the peer wants sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datagram {
    /// Transport the ICE pair uses.
    pub proto: Protocol,
    /// Local address it leaves from.
    pub source: SocketAddr,
    /// Remote address it goes to.
    pub destination: SocketAddr,
    /// Bytes on the wire (STUN, DTLS or SRTP).
    pub contents: Vec<u8>,
}

/// A sans-IO peer: one `str0m::Rtc` with one Opus audio m-line and at most one H.264 video m-line.
pub struct Peer {
    rtc: Rtc,
    /// The audio line: the one our offer added, or the first audio line of the far end's offer.
    mid: Option<Mid>,
    /// The video line: the one our offer added, once the answer to that offer committed it, or the first video line
    /// of the far end's offer we took.
    video: Option<Mid>,
    /// The video line our latest offer adds, until the answer to that offer commits it (`str0m` has no media for it
    /// before, and an offer made meanwhile, or the far end's taken, replaces that offer).
    offered_video: Option<Mid>,
    /// The last offer or answer the far end sent, as text.
    remote_sdp: Option<String>,
    /// The state last reported through [`PeerEvent::Connected`] / [`PeerEvent::Disconnected`].
    reported_connected: bool,
    closed: bool,
    dropped: u64,
    deadline: Instant,
    outgoing: VecDeque<Datagram>,
    events: VecDeque<PeerEvent>,
}

impl Peer {
    /// A peer whose only local ICE candidate is `candidate`: a call's is relayed ([`RelayedPeer`]); a host one serves
    /// a test's stand-in for the far end. `str0m` sends a relayed pair's datagrams with the allocation as their
    /// source, and takes datagrams for it with the allocation as their destination: the caller carries them through
    /// the TURN server.
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` fails while draining.
    pub fn with_candidate(now: Instant, candidate: Candidate) -> Result<Self, Error> {
        // Opus and H.264 alone: which video lines are taken is the dialect's to say (a line it reads rejected is
        // answered rejected, as an audio-only client does)
        let mut config = Rtc::builder()
            .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
            .set_reordering_size_audio(0)
            .enable_bwe(Some(Bitrate::bps(START_BITRATE)))
            .clear_codecs();
        // Opus as `str0m` offers it by default (`minptime=10;useinbandfec=1`), on the payload type the media
        // controller numbers it with
        config.codec_config().add_config(
            OPUS_PT.into(),
            None,
            Codec::Opus,
            Frequency::FORTY_EIGHT_KHZ,
            Some(2),
            FormatParams {
                min_p_time: Some(10),
                use_inband_fec: Some(true),
                ..FormatParams::default()
            },
        );
        config
            .codec_config()
            .add_h264(H264_PT.into(), Some(H264_RTX_PT.into()), true, 0x0042_e01f);
        let mut rtc = config.build(now);
        rtc.add_local_candidate(candidate);
        let mut peer = Self {
            rtc,
            mid: None,
            video: None,
            offered_video: None,
            remote_sdp: None,
            reported_connected: false,
            closed: false,
            dropped: 0,
            deadline: now,
            outgoing: VecDeque::new(),
            events: VecDeque::new(),
        };
        peer.drain()?;
        Ok(peer)
    }

    /// Creates the offer for one send-and-receive audio m-line. Pass the pending half to [`Peer::accept_answer`].
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` fails while draining.
    pub fn offer_audio(&mut self) -> Result<(SdpOffer, SdpPendingOffer), Error> {
        let mut change = self.rtc.sdp_api();
        let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        let offer = change
            .apply()
            .ok_or_else(|| Error::Rtc("the SDP change produced no offer".into()))?;
        self.mid = Some(mid);
        self.offered_video = None;
        self.drain()?;
        Ok(offer)
    }

    /// Offers the session again with our video line sending (`true`) or only receiving: the line added to the session
    /// when it has none, its direction changed when it has. Pass the pending half to [`Peer::accept_answer`]. `None`
    /// when nothing would change: the line already goes that way (an offer we answered asked for it).
    ///
    /// Only a line an answer committed is changed: a line an earlier offer of ours added and no answer committed (that
    /// offer turned away) has no media yet, and is offered anew in its place.
    ///
    /// # Errors
    /// [`Error::Rtc`] when `str0m` fails while draining.
    pub fn offer_video(&mut self, sending: bool) -> Result<Option<(SdpOffer, SdpPendingOffer)>, Error> {
        let direction = if sending {
            Direction::SendRecv
        } else {
            Direction::RecvOnly
        };
        if let Some(mid) = self.video.filter(|_| sending) {
            self.declare_sending(mid);
        }
        let mut change = self.rtc.sdp_api();
        let added = match self.video {
            Some(mid) => {
                change.set_direction(mid, direction);
                None
            }
            None => Some(change.add_media(MediaKind::Video, direction, None, None, None)),
        };
        let Some(offer) = change.apply() else {
            return Ok(None);
        };
        // the line is ours once the answer to this offer commits it ([`Peer::accept_answer`])
        self.offered_video = added;
        self.drain()?;
        Ok(Some(offer))
    }

    /// Gives the line `mid` a stream to send on, when it has none (a line the far end's offer added, answered while
    /// we only received): `str0m` makes one only once the answer to our offer came, and so names none in that offer,
    /// while our offers name no line-naming header extension (the dialect renames our lines' mids), so the far end
    /// would know the stream by nothing. Declared now, its SSRCs (and its retransmissions') go in the offer.
    fn declare_sending(&mut self, mid: Mid) {
        let mut api = self.rtc.direct_api();
        if api.stream_tx_by_mid(mid, None).is_some() {
            return;
        }
        let ssrc = api.new_ssrc();
        let rtx = std::iter::repeat_with(|| api.new_ssrc())
            .find(|rtx| *rtx != ssrc)
            .unwrap_or(ssrc);
        api.declare_stream_tx(ssrc, Some(rtx), mid, None);
    }

    /// Offers the session again with fresh ICE credentials, its lines as they are and its candidate kept (an ICE
    /// restart): a renegotiation that changes nothing else, for a call whose media is offered again. Pass the pending
    /// half to [`Peer::accept_answer`].
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` makes no offer of it or fails while draining.
    pub fn reoffer(&mut self) -> Result<(SdpOffer, SdpPendingOffer), Error> {
        let mut change = self.rtc.sdp_api();
        change.ice_restart(true);
        let offer = change
            .apply()
            .ok_or_else(|| Error::Rtc("the ICE restart produced no offer".into()))?;
        self.offered_video = None;
        self.drain()?;
        Ok(offer)
    }

    /// Answers a remote offer.
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` refuses the offer.
    pub fn accept_offer(&mut self, offer: SdpOffer) -> Result<SdpAnswer, Error> {
        let text = offer.to_sdp_string();
        let answer = self.rtc.sdp_api().accept_offer(offer).map_err(rtc_error)?;
        // an offer of ours still waiting for its answer can't take one now
        self.offered_video = None;
        self.remote_sdp = Some(text);
        self.drain()?;
        Ok(answer)
    }

    /// Applies the remote answer to our offer.
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` refuses the answer.
    pub fn accept_answer(&mut self, pending: SdpPendingOffer, answer: SdpAnswer) -> Result<(), Error> {
        let text = answer.to_sdp_string();
        self.rtc.sdp_api().accept_answer(pending, answer).map_err(rtc_error)?;
        if let Some(mid) = self.offered_video.take()
            && self.video.is_none()
            && self.rtc.media(mid).is_some()
        {
            self.video = Some(mid);
        }
        self.remote_sdp = Some(text);
        self.drain()
    }

    /// What the far end's Opus payload on the audio line asked for in the last offer or answer it sent
    /// ([`OpusConfig::from_sdp`]); `None` before one, while the audio line isn't known yet (`str0m` names the lines of
    /// a remote offer once the media connects), or when it carries no Opus.
    #[must_use]
    pub fn remote_opus(&self) -> Option<OpusConfig> {
        let mid = self.mid?;
        OpusConfig::from_sdp(self.remote_sdp.as_deref()?, &mid)
    }

    /// The audio line's mid: the one our offer added, or the first audio line of the far end's offer once the media
    /// connects; `None` before.
    #[must_use]
    pub fn audio_mid(&self) -> Option<Mid> {
        self.mid
    }

    /// The video line's mid, once there is one.
    #[must_use]
    pub fn video_mid(&self) -> Option<Mid> {
        self.video
    }

    /// Whether our video line is negotiated to send.
    #[must_use]
    pub fn video_sending(&self) -> bool {
        self.video
            .and_then(|mid| self.rtc.media(mid))
            .is_some_and(|media| media.direction().is_sending())
    }

    /// Asks the bandwidth estimate to look for `bps` (probing up to it while we send video), in bits per second.
    pub fn set_desired_bitrate(&mut self, bps: u64) {
        self.rtc.bwe().set_desired_bitrate(Bitrate::bps(bps));
    }

    /// Whether ICE, DTLS and SRTP are up right now: false while ICE is disconnected, true again once it recovers,
    /// false for good after the remote closed the session.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        !self.closed && self.rtc.is_connected()
    }

    /// How many datagrams were dropped because `str0m` can't classify them (see [`Peer::handle_datagram`]).
    #[must_use]
    pub fn dropped_datagrams(&self) -> u64 {
        self.dropped
    }

    /// When the peer next wants [`Peer::handle_timeout`].
    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Feeds a datagram that arrived at `destination` from `source`. A datagram `str0m` can't classify (empty, short
    /// or malformed STUN, a first byte outside the STUN/DTLS/RTP ranges such as TURN `ChannelData`) is dropped and
    /// counted in [`Peer::dropped_datagrams`]: a stray packet must not end the call.
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` fails on a datagram it did classify.
    pub fn handle_datagram(
        &mut self,
        now: Instant,
        proto: Protocol,
        source: SocketAddr,
        destination: SocketAddr,
        contents: &[u8],
    ) -> Result<(), Error> {
        let Ok(contents) = contents.try_into() else {
            self.dropped += 1;
            return Ok(());
        };
        self.rtc
            .handle_input(Input::Receive(
                now,
                Receive {
                    proto,
                    source,
                    destination,
                    contents,
                },
            ))
            .map_err(rtc_error)?;
        self.drain()
    }

    /// Advances the peer's clock to `now`.
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` fails.
    pub fn handle_timeout(&mut self, now: Instant) -> Result<(), Error> {
        self.rtc.handle_input(Input::Timeout(now)).map_err(rtc_error)?;
        self.drain()
    }

    /// Sends one Opus frame with RTP media time `rtp_time` (48 kHz units), sampled at `wallclock`. `str0m` only queues
    /// a written frame and packetizes it on its next timeout, so this also advances the peer's clock to `now`: when it
    /// returns, the frame's RTP datagrams wait in [`Peer::poll_datagram`].
    ///
    /// # Errors
    /// [`Error::NotConnected`] before the audio m-line is negotiated or while the peer is not connected,
    /// [`Error::Rtc`] if `str0m` fails.
    pub fn write_audio(
        &mut self,
        now: Instant,
        wallclock: Instant,
        rtp_time: u64,
        payload: &[u8],
    ) -> Result<(), Error> {
        let connected = self.is_connected();
        let mid = self.mid.filter(|_| connected).ok_or(Error::NotConnected)?;
        let writer = self.rtc.writer(mid).ok_or(Error::NotConnected)?;
        let pt = writer
            .payload_params()
            .find(|params| params.spec().codec == Codec::Opus)
            .map(str0m::format::PayloadParams::pt)
            .ok_or_else(|| Error::Rtc("Opus was not negotiated".into()))?;
        let time = MediaTime::new(rtp_time, Frequency::FORTY_EIGHT_KHZ);
        writer.write(pt, wallclock, time, payload.to_vec()).map_err(rtc_error)?;
        self.rtc.handle_input(Input::Timeout(now)).map_err(rtc_error)?;
        self.drain()
    }

    /// Sends one H.264 access unit (Annex B) with RTP media time `rtp_time` (90 kHz units), captured at `wallclock`;
    /// as [`Peer::write_audio`], the peer's clock advances to `now` and its datagrams wait in [`Peer::poll_datagram`].
    ///
    /// # Errors
    /// [`Error::NotConnected`] while the peer isn't connected or the video line isn't negotiated to send,
    /// [`Error::Rtc`] if `str0m` fails.
    pub fn write_video(&mut self, now: Instant, wallclock: Instant, rtp_time: u64, data: &[u8]) -> Result<(), Error> {
        if !self.is_connected() || !self.video_sending() {
            return Err(Error::NotConnected);
        }
        let mid = self.video.ok_or(Error::NotConnected)?;
        let writer = self.rtc.writer(mid).ok_or(Error::NotConnected)?;
        let pt = writer
            .payload_params()
            .find(|params| params.spec().codec == Codec::H264)
            .map(str0m::format::PayloadParams::pt)
            .ok_or_else(|| Error::Rtc("H.264 was not negotiated".into()))?;
        let time = MediaTime::new(rtp_time, Frequency::NINETY_KHZ);
        writer.write(pt, wallclock, time, data.to_vec()).map_err(rtc_error)?;
        self.rtc.handle_input(Input::Timeout(now)).map_err(rtc_error)?;
        self.drain()
    }

    /// Asks the far end for a keyframe of the video it sends (a PLI), when the line negotiated one; whether it went.
    pub fn request_keyframe(&mut self) -> bool {
        let Some(mut writer) = self.video.and_then(|mid| self.rtc.writer(mid)) else {
            return false;
        };
        let asked = writer.is_request_keyframe_possible(KeyframeRequestKind::Pli)
            && writer.request_keyframe(None, KeyframeRequestKind::Pli).is_ok();
        asked && self.drain().is_ok()
    }

    /// Hangs up: queues the RTCP BYE and DTLS close for [`Peer::poll_datagram`]; the remote then reports
    /// [`PeerEvent::Closed`]. The peer is not connected afterwards.
    ///
    /// # Errors
    /// [`Error::Rtc`] if `str0m` fails.
    pub fn close(&mut self) -> Result<(), Error> {
        self.rtc.close().map_err(rtc_error)?;
        self.closed = true;
        self.reported_connected = false;
        self.drain()
    }

    /// Takes the next datagram to send.
    pub fn poll_datagram(&mut self) -> Option<Datagram> {
        self.outgoing.pop_front()
    }

    /// Takes the next event for the application.
    pub fn poll_event(&mut self) -> Option<PeerEvent> {
        self.events.pop_front()
    }

    /// Drains `poll_output` to the next timeout, queueing datagrams and events.
    fn drain(&mut self) -> Result<(), Error> {
        loop {
            match self.rtc.poll_output().map_err(rtc_error)? {
                Output::Timeout(deadline) => {
                    self.deadline = deadline;
                    return Ok(());
                }
                Output::Transmit(t) => self.outgoing.push_back(Datagram {
                    proto: t.proto,
                    source: t.source,
                    destination: t.destination,
                    contents: t.contents.to_vec(),
                }),
                Output::Event(event) => self.on_event(event),
            }
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Connected | Event::IceConnectionStateChange(_) => self.report_connection(),
            Event::Closed => {
                if !self.closed {
                    self.closed = true;
                    self.reported_connected = false;
                    self.events.push_back(PeerEvent::Closed);
                }
            }
            // a remote offer adds every line it carries that isn't rejected: the audio is the first audio line, the
            // video the first video line
            Event::MediaAdded(added) => match added.kind {
                MediaKind::Audio if self.mid.is_none() => self.mid = Some(added.mid),
                MediaKind::Video if self.video.is_none() => self.video = Some(added.mid),
                _ => {}
            },
            Event::MediaData(data) if Some(data.mid) == self.mid => {
                self.events.push_back(PeerEvent::Audio(ReceivedAudio {
                    rtp_time: data.time.rebase(Frequency::FORTY_EIGHT_KHZ).numer(),
                    network_time: data.network_time,
                    payload: data.data,
                }));
            }
            Event::MediaData(data) if Some(data.mid) == self.video => {
                let keyframe = data.is_keyframe();
                self.events.push_back(PeerEvent::Video(ReceivedVideo {
                    rtp_time: data.time.rebase(Frequency::NINETY_KHZ).numer(),
                    data: data.data,
                    keyframe,
                    contiguous: data.contiguous,
                }));
            }
            Event::KeyframeRequest(request) if Some(request.mid) == self.video => {
                self.events.push_back(PeerEvent::KeyframeRequested);
            }
            Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. } | BweKind::Remb { estimate, .. }) => {
                self.events.push_back(PeerEvent::Bitrate(estimate.as_u64()));
            }
            _ => {}
        }
    }

    /// Reports a change of [`Peer::is_connected`]. ICE can drop and come back while DTLS stays up, and `str0m`'s own
    /// `Event::Connected` fires only once per session, so every ICE state change is checked against the live state.
    fn report_connection(&mut self) {
        let connected = self.is_connected();
        if self.closed || connected == self.reported_connected {
            return;
        }
        self.reported_connected = connected;
        self.events.push_back(if connected {
            PeerEvent::Connected
        } else {
            PeerEvent::Disconnected
        });
    }
}

/// A [`Peer`] reached only through a TURN allocation.
pub struct RelayedPeer {
    peer: Peer,
    link: RelayLink,
}

impl fmt::Debug for RelayedPeer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayedPeer")
            .field("link", &self.link)
            .field("connected", &self.peer.is_connected())
            .finish_non_exhaustive()
    }
}

impl RelayedPeer {
    /// A peer whose only candidate is `link`'s allocation.
    ///
    /// # Errors
    /// [`Error::Rtc`] when `str0m` refuses the candidate.
    pub fn new(now: Instant, link: RelayLink) -> Result<Self, Error> {
        let candidate = Candidate::relayed(link.relayed_addr(), link.local_addr(), Protocol::Udp).map_err(rtc_error)?;
        let peer = Peer::with_candidate(now, candidate)?;
        Ok(Self { peer, link })
    }

    /// The sans-IO peer, for SDP and media.
    pub fn peer(&mut self) -> &mut Peer {
        &mut self.peer
    }

    /// The relay link.
    pub fn link(&mut self) -> &mut RelayLink {
        &mut self.link
    }

    /// One turn of the run loop: feeds whatever the relay brought in, fires the peer's timeout if it is due, and sends
    /// everything the peer queued. Returns whether any datagram arrived.
    ///
    /// # Errors
    /// [`Error::Turn`] when the allocation fails, [`Error::Io`] when its socket does, [`Error::Rtc`] when `str0m`
    /// does.
    pub fn turn(&mut self, now: Instant) -> Result<bool, Error> {
        let relayed = self.link.relayed_addr();
        let mut received = false;
        for event in self.link.poll(now)? {
            match event {
                TurnEvent::Data { peer, payload } => {
                    received = true;
                    self.peer.handle_datagram(now, Protocol::Udp, peer, relayed, &payload)?;
                }
                TurnEvent::Failed(failure) => return Err(Error::Turn(failure)),
                _ => {}
            }
        }
        if now >= self.peer.deadline() {
            self.peer.handle_timeout(now)?;
        }
        self.flush(now)?;
        Ok(received)
    }

    /// Sends everything the peer queued through the relay.
    ///
    /// # Errors
    /// [`Error::TurnUse`] when the allocation is gone or out of channels, [`Error::Io`] when its socket fails.
    pub fn flush(&mut self, now: Instant) -> Result<(), Error> {
        let relayed = self.link.relayed_addr();
        while let Some(datagram) = self.peer.poll_datagram() {
            // the peer has no other candidate, so everything leaves from the allocation
            if datagram.source == relayed {
                self.link.send_to(now, datagram.destination, &datagram.contents)?;
            }
        }
        self.link.flush()
    }
}

/// Turns any `str0m` error into [`Error::Rtc`]; a `map_err` adapter.
fn rtc_error(error: impl fmt::Display) -> Error {
    Error::Rtc(error.to_string())
}
