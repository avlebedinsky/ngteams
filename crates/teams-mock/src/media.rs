//! The loopback media peer (the `media` feature, M3 Task 13): the far end of a call's media on loopback, for the
//! calls that have no native client at their other end (the test call's media controller, a caller or a callee the
//! mock plays). It is a `teams-media` session on the file backend behind its own allocation on a loopback TURN server
//! (`teams_media::test_turn`), speaking Teams' dialect as the call controller and media controller do: it answers an
//! offer in `sdp-ngc-1.0` and offers in it, and takes the answer to its offer (`teams_media::sdp::NgcSession` keeping
//! the lines' two namings for the call's life). With the feature on, the calling service puts one behind its answers
//! once a test turned its media on ([`crate::ChatServiceMock::start_media`]): the test call's media controller, a
//! caller the mock plays, and the renegotiations of their calls.

use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use teams_media::devices::Devices;
use teams_media::peer::RelayedPeer;
use teams_media::processing::ProcessingConfig;
use teams_media::relay::{RelayLink, RelayOptions, RelayServer};
use teams_media::sdp::{NgcSdp, NgcSession, SdpError};
use teams_media::session::{MediaSession, SessionConfig, SessionEvent, SessionStats};
use teams_media::test_turn::{PASSWORD, TurnServer, USER};
use teams_media::turn::{Credentials, Transport};

/// Why the loopback peer couldn't do it.
#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error(transparent)]
    Media(#[from] teams_media::Error),
    #[error(transparent)]
    Sdp(#[from] SdpError),
}

/// The far end of a call's media on loopback (see the module docs).
pub struct LoopbackPeer {
    session: MediaSession,
    dialect: NgcSession,
}

impl fmt::Debug for LoopbackPeer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopbackPeer").finish_non_exhaustive()
    }
}

impl LoopbackPeer {
    /// A peer behind its own UDP allocation on `turn`, its media on `devices`, its microphone unprocessed (a test's
    /// steady tone goes out as it is).
    ///
    /// # Errors
    /// [`PeerError::Media`] when the allocation or the session can't be made.
    pub fn start(turn: &TurnServer, devices: &dyn Devices) -> Result<Self, PeerError> {
        Self::start_at(turn.udp, devices)
    }

    /// As [`Self::start`], on the loopback TURN server's UDP port `turn`.
    ///
    /// # Errors
    /// [`PeerError::Media`] when the allocation or the session can't be made.
    pub fn start_at(turn: SocketAddr, devices: &dyn Devices) -> Result<Self, PeerError> {
        let server = RelayServer {
            addr: turn,
            transport: Transport::Udp,
        };
        let credentials = Credentials {
            username: USER.into(),
            password: PASSWORD.into(),
        };
        let link = RelayLink::allocate(server, credentials, &RelayOptions::default())?;
        let config = SessionConfig {
            processing: ProcessingConfig::OFF,
            ..SessionConfig::default()
        };
        let session = MediaSession::start(RelayedPeer::new(Instant::now(), link)?, devices, config)?;
        Ok(Self {
            session,
            dialect: NgcSession::new(),
        })
    }

    /// Answers `offer` (either dialect): its answer in `sdp-ngc-1.0`.
    ///
    /// # Errors
    /// [`PeerError`] when the offer doesn't convert or the session refuses it.
    pub fn answer(&mut self, offer: &NgcSdp) -> Result<NgcSdp, PeerError> {
        let offer = self.dialect.read_offer(offer)?;
        let answer = self.session.accept_offer(offer)?;
        Ok(self.dialect.write_answer(&answer)?)
    }

    /// Its offer of the call's audio, in `sdp-ngc-1.0`.
    ///
    /// # Errors
    /// [`PeerError`] when the session can't offer or the offer doesn't convert.
    pub fn offer(&mut self) -> Result<NgcSdp, PeerError> {
        let offer = self.session.offer_audio()?;
        Ok(self.dialect.write_offer(&offer)?)
    }

    /// Its offer of the running call's media again (an ICE restart, `MediaSession::reoffer`), in `sdp-ngc-1.0`: a
    /// renegotiation of its own.
    ///
    /// # Errors
    /// [`PeerError`] when the session can't offer or the offer doesn't convert.
    pub fn reoffer(&mut self) -> Result<NgcSdp, PeerError> {
        let offer = self.session.reoffer()?;
        Ok(self.dialect.write_offer(&offer)?)
    }

    /// Takes the answer to its offer.
    ///
    /// # Errors
    /// [`PeerError`] when the answer doesn't convert or the session refuses it.
    pub fn accept_answer(&mut self, answer: &NgcSdp) -> Result<(), PeerError> {
        let answer = self.dialect.read_answer(answer)?;
        Ok(self.session.accept_answer(answer)?)
    }

    /// Whether the session reports `wanted` within `wait` (`false` on a final event or when the time is up).
    #[must_use]
    pub fn reports(&self, wanted: &SessionEvent, wait: Duration) -> bool {
        let limit = Instant::now() + wait;
        loop {
            let left = limit.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            match self.session.next_event(left) {
                Some(event) if &event == wanted => return true,
                Some(SessionEvent::Failed(_) | SessionEvent::Closed) => return false,
                _ => {}
            }
        }
    }

    /// The session's counts so far.
    #[must_use]
    pub fn stats(&self) -> SessionStats {
        self.session.stats()
    }

    /// Hangs up and closes the session: its final counts.
    #[must_use]
    pub fn close(self) -> SessionStats {
        self.session.close()
    }
}
