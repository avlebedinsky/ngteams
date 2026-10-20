//! A WebRTC audio peer on `str0m`, sans-IO: [`Peer`] owns no socket and no clock, it takes datagrams and `now` in and
//! hands datagrams, events and its next deadline out. [`UdpPeer`] puts it on a UDP socket.
//!
//! Every mutation of the `Rtc` (input, write, SDP change) is followed by a full drain of `poll_output` to its next
//! `Timeout`, as str0m requires; the drain queues datagrams and events for the caller.

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Instant;

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::format::Codec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};

use crate::Error;

/// Largest datagram we read; str0m keeps its own output under the MTU.
const MAX_DATAGRAM: usize = 2_000;

/// What a [`Peer`] reports to the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerEvent {
    /// ICE and DTLS are up: media can flow. Comes again when ICE recovers after a [`PeerEvent::Disconnected`].
    Connected,
    /// ICE lost the remote side and media can't flow. This may be transient: while the remote still sends checks,
    /// ICE rebuilds the pair and a later [`PeerEvent::Connected`] means the link is back. (Once both sides have
    /// pruned the pair, str0m sends nothing more and only an ICE restart would help.)
    Disconnected,
    /// The remote closed the session (DTLS close or SCTP loss). Final: nothing flows after it.
    Closed,
    /// One audio frame (an Opus packet) arrived.
    Audio(AudioFrame),
}

/// An incoming audio frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
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
    mid: Option<Mid>,
    /// The state last reported through [`PeerEvent::Connected`] / [`PeerEvent::Disconnected`].
    reported_connected: bool,
    closed: bool,
    dropped: u64,
    deadline: Instant,
    outgoing: VecDeque<Datagram>,
    events: VecDeque<PeerEvent>,
}

impl Peer {
    /// A peer whose only local ICE candidate is the host address `host` on `proto`.
    ///
    /// # Errors
    /// [`Error::Rtc`] if `host` is not a usable candidate address.
    pub fn new(now: Instant, host: SocketAddr, proto: Protocol) -> Result<Self, Error> {
        let candidate = Candidate::host(host, proto).map_err(|e| Error::Rtc(e.to_string()))?;
        Self::with_candidate(now, candidate)
    }

    /// A peer whose only local ICE candidate is `candidate`, such as a relayed one
    /// (`Candidate::relayed(<allocation>, <socket to the TURN server>, "udp")`). str0m sends a relayed pair's datagrams
    /// with the allocation as their source, and takes datagrams for it with the allocation as their destination: the
    /// caller carries them through the TURN server.
    ///
    /// # Errors
    /// [`Error::Rtc`] if str0m fails while draining.
    pub fn with_candidate(now: Instant, candidate: Candidate) -> Result<Self, Error> {
        let mut rtc = Rtc::builder()
            .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
            .build(now);
        rtc.add_local_candidate(candidate);
        let mut peer = Self {
            rtc,
            mid: None,
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
    /// [`Error::Rtc`] if str0m fails while draining.
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
    /// [`Error::Rtc`] if str0m refuses the offer.
    pub fn accept_offer(&mut self, offer: SdpOffer) -> Result<SdpAnswer, Error> {
        let answer = self.rtc.sdp_api().accept_offer(offer).map_err(rtc_error)?;
        self.drain()?;
        Ok(answer)
    }

    /// Applies the remote answer to our offer.
    ///
    /// # Errors
    /// [`Error::Rtc`] if str0m refuses the answer.
    pub fn accept_answer(&mut self, pending: SdpPendingOffer, answer: SdpAnswer) -> Result<(), Error> {
        self.rtc.sdp_api().accept_answer(pending, answer).map_err(rtc_error)?;
        self.drain()
    }

    /// Whether ICE, DTLS and SRTP are up right now: false while ICE is disconnected, true again once it recovers,
    /// false for good after the remote closed the session.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        !self.closed && self.rtc.is_connected()
    }

    /// How many datagrams were dropped because str0m can't classify them (see [`Peer::handle_datagram`]).
    #[must_use]
    pub fn dropped_datagrams(&self) -> u64 {
        self.dropped
    }

    /// When the peer next wants [`Peer::handle_timeout`].
    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Feeds a datagram that arrived at `destination` from `source`. A datagram str0m can't classify (empty, short or
    /// malformed STUN, a first byte outside the STUN/DTLS/RTP ranges such as TURN `ChannelData`) is dropped and counted
    /// in [`Peer::dropped_datagrams`]: a stray packet must not end the call.
    ///
    /// # Errors
    /// [`Error::Rtc`] if str0m fails on a datagram it did classify.
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
    /// [`Error::Rtc`] if str0m fails.
    pub fn handle_timeout(&mut self, now: Instant) -> Result<(), Error> {
        self.rtc.handle_input(Input::Timeout(now)).map_err(rtc_error)?;
        self.drain()
    }

    /// Sends one Opus frame with RTP media time `rtp_time` (48 kHz units), sampled at `wallclock`. str0m only queues a
    /// written frame and packetizes it on its next timeout, so this also advances the peer's clock to `now`: when it
    /// returns, the frame's RTP datagrams wait in [`Peer::poll_datagram`].
    ///
    /// # Errors
    /// [`Error::NotConnected`] before the audio m-line is negotiated or while the peer is not connected,
    /// [`Error::Rtc`] if str0m fails.
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
    /// [`Error::Rtc`] if str0m fails.
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
            Event::MediaAdded(added) => self.mid = Some(added.mid),
            Event::MediaData(data) => self.events.push_back(PeerEvent::Audio(AudioFrame {
                rtp_time: data.time.rebase(Frequency::FORTY_EIGHT_KHZ).numer(),
                network_time: data.network_time,
                payload: data.data,
            })),
            _ => {}
        }
    }

    /// Reports a change of [`Peer::is_connected`]. ICE can drop and come back while DTLS stays up, and str0m's own
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

/// A [`Peer`] on a non-blocking UDP socket bound to a loopback (or any local) address.
pub struct UdpPeer {
    peer: Peer,
    socket: UdpSocket,
    local: SocketAddr,
    buf: Vec<u8>,
}

impl UdpPeer {
    /// Binds `addr` (port 0 picks a free one) and makes it the peer's host candidate.
    ///
    /// # Errors
    /// [`Error::Io`] if the socket can't be bound, [`Error::Rtc`] if its address is not a usable candidate.
    pub fn bind(now: Instant, addr: SocketAddr) -> Result<Self, Error> {
        let socket = UdpSocket::bind(addr)?;
        socket.set_nonblocking(true)?;
        let local = socket.local_addr()?;
        let peer = Peer::new(now, local, Protocol::Udp)?;
        Ok(Self {
            peer,
            socket,
            local,
            buf: vec![0; MAX_DATAGRAM],
        })
    }

    /// The bound address.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// The sans-IO peer, for SDP and media.
    pub fn peer(&mut self) -> &mut Peer {
        &mut self.peer
    }

    /// One turn of the run loop: feeds every datagram waiting on the socket (one str0m can't classify is dropped, see
    /// [`Peer::handle_datagram`]), fires the timeout if it is due, and sends everything the peer queued. Returns whether
    /// any datagram arrived.
    ///
    /// # Errors
    /// [`Error::Io`] if the socket fails, [`Error::Rtc`] if str0m does.
    pub fn turn(&mut self, now: Instant) -> Result<bool, Error> {
        let mut received = false;
        loop {
            match self.socket.recv_from(&mut self.buf) {
                Ok((len, source)) => {
                    received = true;
                    let contents = &self.buf[..len];
                    self.peer
                        .handle_datagram(now, Protocol::Udp, source, self.local, contents)?;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        if now >= self.peer.deadline() {
            self.peer.handle_timeout(now)?;
        }
        self.flush()?;
        Ok(received)
    }

    /// Sends every datagram the peer queued.
    ///
    /// # Errors
    /// [`Error::Io`] if the socket fails.
    pub fn flush(&mut self) -> Result<(), Error> {
        while let Some(datagram) = self.peer.poll_datagram() {
            self.socket.send_to(&datagram.contents, datagram.destination)?;
        }
        Ok(())
    }
}

/// Turns any `str0m` error into [`Error::Rtc`]; a `map_err` adapter.
fn rtc_error(error: impl std::fmt::Display) -> Error {
    Error::Rtc(error.to_string())
}
