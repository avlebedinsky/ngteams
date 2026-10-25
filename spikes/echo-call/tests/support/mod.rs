//! Test stand-in: an echo peer that plays back every audio frame it receives, as Teams' echo service would (only
//! sooner). The TURN server it is reached through is `teams_media::test_turn`.

use std::net::SocketAddr;
use std::time::Instant;

use media_loop::peer::{PeerEvent, UdpPeer};

/// The echo stand-in: a plain UDP peer that sends every audio frame straight back.
pub struct EchoBot {
    pub peer: UdpPeer,
}

impl EchoBot {
    pub fn bind() -> Result<Self, echo_call::Error> {
        Ok(Self {
            peer: UdpPeer::bind(Instant::now(), SocketAddr::from(([127, 0, 0, 1], 0)))?,
        })
    }

    pub fn step(&mut self, now: Instant) -> Result<(), echo_call::Error> {
        self.peer.turn(now)?;
        while let Some(event) = self.peer.peer().poll_event() {
            if let PeerEvent::Audio(frame) = event {
                self.peer
                    .peer()
                    .write_audio(Instant::now(), now, frame.rtp_time, &frame.payload)?;
            }
        }
        self.peer.flush()?;
        Ok(())
    }
}
