//! The media paths the echo probe runs over: `teams-media`'s peer behind a TURN allocation
//! ([`teams_media::peer::RelayedPeer`], moved there from this spike), or `media_loop`'s peer on a plain UDP socket (the
//! echo stand-in's). The TURN client and the relay link live in `teams-media` too.

use std::time::Instant;

use media_loop::peer::UdpPeer;
use teams_media::peer::{Peer, RelayedPeer};

use crate::Error;

/// Something a call's media runs over: a peer and the sockets that carry it.
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

impl MediaPath for RelayedPeer {
    fn peer(&mut self) -> &mut Peer {
        RelayedPeer::peer(self)
    }

    fn turn(&mut self, now: Instant) -> Result<bool, Error> {
        Ok(RelayedPeer::turn(self, now)?)
    }

    fn flush(&mut self, now: Instant) -> Result<(), Error> {
        Ok(RelayedPeer::flush(self, now)?)
    }
}
