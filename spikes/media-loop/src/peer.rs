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
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};

use crate::Error;

/// Largest datagram we read; str0m keeps its own output under the MTU.
const MAX_DATAGRAM: usize = 2_000;

/// What a [`Peer`] reports to the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerEvent {
    /// ICE and DTLS are up: media can flow.
    Connected,
    /// ICE lost the remote side.
    Disconnected,
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
    connected: bool,
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
        let mut rtc = Rtc::builder()
            .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
            .build(now);
        let candidate = Candidate::host(host, proto).map_err(|e| Error::Rtc(e.to_string()))?;
        rtc.add_local_candidate(candidate);
        let mut peer = Self {
            rtc,
            mid: None,
            connected: false,
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

    /// Whether ICE and DTLS are up.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// When the peer next wants [`Peer::handle_timeout`].
    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Feeds a datagram that arrived at `destination` from `source`.
    ///
    /// # Errors
    /// [`Error::Rtc`] if the datagram can't be parsed or str0m fails.
    pub fn handle_datagram(
        &mut self,
        now: Instant,
        proto: Protocol,
        source: SocketAddr,
        destination: SocketAddr,
        contents: &[u8],
    ) -> Result<(), Error> {
        let contents = contents.try_into().map_err(rtc_error)?;
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

    /// Sends one Opus frame with RTP media time `rtp_time` (48 kHz units), sampled at `wallclock`.
    ///
    /// # Errors
    /// [`Error::NotConnected`] before the audio m-line is negotiated and connected, [`Error::Rtc`] if str0m fails.
    pub fn write_audio(&mut self, wallclock: Instant, rtp_time: u64, payload: &[u8]) -> Result<(), Error> {
        let mid = self.mid.filter(|_| self.connected).ok_or(Error::NotConnected)?;
        let writer = self.rtc.writer(mid).ok_or(Error::NotConnected)?;
        let pt = writer
            .payload_params()
            .find(|params| params.spec().codec == Codec::Opus)
            .map(str0m::format::PayloadParams::pt)
            .ok_or_else(|| Error::Rtc("Opus was not negotiated".into()))?;
        let time = MediaTime::new(rtp_time, Frequency::FORTY_EIGHT_KHZ);
        writer.write(pt, wallclock, time, payload.to_vec()).map_err(rtc_error)?;
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
            Event::Connected => {
                self.connected = true;
                self.events.push_back(PeerEvent::Connected);
            }
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                self.connected = false;
                self.events.push_back(PeerEvent::Disconnected);
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

    /// One turn of the run loop: feeds every datagram waiting on the socket, fires the timeout if it is due, and sends
    /// everything the peer queued. Returns whether any datagram arrived.
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
