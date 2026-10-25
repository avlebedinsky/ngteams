//! A `media_loop` [`Peer`] behind a TURN allocation ([`teams_media::relay::RelayLink`]): what str0m sends from the
//! allocation goes out through the server, what peers send to the allocation comes back in. The TURN client and the
//! relay link themselves live in `teams-media`.

use std::fmt;
use std::time::Instant;

use media_loop::peer::{Peer, UdpPeer};
use str0m::Candidate;
use str0m::net::Protocol;
use teams_media::relay::RelayLink;
use teams_media::turn::TurnEvent;

use crate::Error;

/// Something a call's media runs over: a `media_loop` peer and the sockets that carry it.
pub trait MediaPath {
    /// The sans-IO peer, for SDP and media.
    fn peer(&mut self) -> &mut Peer;
    /// One turn of the run loop; returns whether any datagram arrived.
    ///
    /// # Errors
    /// When a socket, the relay or str0m fails.
    fn turn(&mut self, now: Instant) -> Result<bool, Error>;
    /// Sends everything the peer queued.
    ///
    /// # Errors
    /// When a socket or the relay fails.
    fn flush(&mut self, now: Instant) -> Result<(), Error>;
}

impl MediaPath for UdpPeer {
    fn peer(&mut self) -> &mut Peer {
        UdpPeer::peer(self)
    }

    fn turn(&mut self, now: Instant) -> Result<bool, Error> {
        Ok(UdpPeer::turn(self, now)?)
    }

    fn flush(&mut self, _now: Instant) -> Result<(), Error> {
        Ok(UdpPeer::flush(self)?)
    }
}

/// A peer reached only through a TURN allocation.
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
    /// [`Error::Media`] when str0m refuses the candidate.
    pub fn new(now: Instant, link: RelayLink) -> Result<Self, Error> {
        let candidate = Candidate::relayed(link.relayed_addr(), link.local_addr(), Protocol::Udp)
            .map_err(|e| Error::Media(media_loop::Error::Rtc(e.to_string())))?;
        let peer = Peer::with_candidate(now, candidate)?;
        Ok(Self { peer, link })
    }

    /// The relay link.
    pub fn link(&mut self) -> &mut RelayLink {
        &mut self.link
    }
}

impl MediaPath for RelayedPeer {
    fn peer(&mut self) -> &mut Peer {
        &mut self.peer
    }

    fn turn(&mut self, now: Instant) -> Result<bool, Error> {
        let relayed = self.link.relayed_addr();
        let mut received = false;
        for event in self.link.poll(now)? {
            match event {
                TurnEvent::Data { peer, payload } => {
                    received = true;
                    self.peer.handle_datagram(now, Protocol::Udp, peer, relayed, &payload)?;
                }
                TurnEvent::Failed(failure) => return Err(Error::Relay(teams_media::Error::Turn(failure))),
                _ => {}
            }
        }
        if now >= self.peer.deadline() {
            self.peer.handle_timeout(now)?;
        }
        MediaPath::flush(self, now)?;
        Ok(received)
    }

    fn flush(&mut self, now: Instant) -> Result<(), Error> {
        let relayed = self.link.relayed_addr();
        while let Some(datagram) = self.peer.poll_datagram() {
            // the peer has no other candidate, so everything leaves from the allocation
            if datagram.source == relayed {
                self.link.send_to(now, datagram.destination, &datagram.contents)?;
            }
        }
        Ok(self.link.flush()?)
    }
}
