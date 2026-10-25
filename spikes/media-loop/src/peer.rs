//! [`UdpPeer`]: `teams-media`'s sans-IO peer ([`teams_media::peer::Peer`], moved there from this spike) on a UDP
//! socket of its own with a host candidate, for the loopback harness and the echo spike's stand-in for the far end. A
//! call's own media never takes this path: it goes only through a relay ([`teams_media::peer::RelayedPeer`]).

use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::time::Instant;

use str0m::Candidate;
use str0m::net::Protocol;
pub use teams_media::peer::{Peer, PeerEvent, ReceivedAudio};

use crate::Error;

/// Largest datagram we read; `str0m` keeps its own output under the MTU.
const MAX_DATAGRAM: usize = 2_000;

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
        let candidate = Candidate::host(local, Protocol::Udp).map_err(|e| Error::Rtc(e.to_string()))?;
        let peer = Peer::with_candidate(now, candidate)?;
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

    /// One turn of the run loop: feeds every datagram waiting on the socket (one `str0m` can't classify is dropped,
    /// see [`Peer::handle_datagram`]), fires the timeout if it is due, and sends everything the peer queued. Returns
    /// whether any datagram arrived.
    ///
    /// # Errors
    /// [`Error::Io`] if the socket fails, [`Error::Peer`] if `str0m` does.
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
