//! The call's WebRTC audio peer on `str0m`.
//!
//! - [`Peer`] is sans-IO: it owns no socket and no clock, takes datagrams and `now` in and hands datagrams, events and
//!   its next deadline out. Every mutation of the `Rtc` (input, write, SDP change) is followed by a full drain of
//!   `poll_output` to its next `Timeout`, as `str0m` requires; the drain queues datagrams and events for the caller.
//! - [`RelayedPeer`] puts it behind a TURN allocation ([`RelayLink`]): its one candidate is the allocation, what it
//!   sends leaves through the server and what peers send to the allocation comes back in. A call's media goes only this
//!   way (relayed candidates only, no local address in an offer).
//!
//! A Teams offer carries video, sharing and data lines beside the audio: the peer answers them as `str0m` does, but
//! its audio is the line its own offer added or the offer's first audio line, and only that line's frames come out as
//! [`PeerEvent::Audio`].
//!
//! `str0m` hands every audio frame over the moment it arrives, in arrival order: the receiving side's jitter buffer
//! ([`crate::jitter`]) puts frames back in order and decides what a gap means, so `str0m` holds nothing back waiting
//! for a lost packet (its audio reordering window is 0).

use std::collections::VecDeque;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::format::Codec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};

use crate::Error;
use crate::codec::OpusConfig;
use crate::relay::RelayLink;
use crate::turn::TurnEvent;

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

/// A sans-IO audio peer: one `str0m::Rtc` with one Opus audio m-line.
pub struct Peer {
    rtc: Rtc,
    /// The audio line: the one our offer added, or the first audio line of the far end's offer.
    mid: Option<Mid>,
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
        let mut rtc = Rtc::builder()
            .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
            .set_reordering_size_audio(0)
            .build(now);
        rtc.add_local_candidate(candidate);
        let mut peer = Self {
            rtc,
            mid: None,
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
            // a remote offer adds every line it carries, video and sharing too: the audio is the first audio line
            Event::MediaAdded(added) => {
                if added.kind == MediaKind::Audio && self.mid.is_none() {
                    self.mid = Some(added.mid);
                }
            }
            Event::MediaData(data) if Some(data.mid) == self.mid => {
                self.events.push_back(PeerEvent::Audio(ReceivedAudio {
                    rtp_time: data.time.rebase(Frequency::FORTY_EIGHT_KHZ).numer(),
                    network_time: data.network_time,
                    payload: data.data,
                }));
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
