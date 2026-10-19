//! The sans-IO peer on a virtual clock: two `Peer`s wired back to back with no sockets, so a test can cut one
//! direction of the link, advance time by minutes in microseconds, and feed junk.

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use media_loop::Error;
use media_loop::peer::{Peer, PeerEvent, UdpPeer};
use str0m::net::Protocol;

/// One virtual run-loop step.
const STEP: Duration = Duration::from_millis(10);
/// Longest any phase may take, in virtual time.
const PHASE_LIMIT: Duration = Duration::from_secs(120);
/// A payload str0m carries as is (an Opus silence frame).
const FRAME: [u8; 3] = [0xF8, 0xFF, 0xFE];

/// Two peers whose datagrams are carried by the test; B's are dropped while `deaf_a`.
struct Link {
    a: Peer,
    b: Peer,
    now: Instant,
    deaf_a: bool,
    a_events: Vec<PeerEvent>,
    b_events: Vec<PeerEvent>,
}

impl Link {
    fn connected() -> Result<Self, Error> {
        let now = Instant::now();
        let mut a = Peer::new(now, SocketAddr::from(([127, 0, 0, 1], 10_000)), Protocol::Udp)?;
        let mut b = Peer::new(now, SocketAddr::from(([127, 0, 0, 1], 20_000)), Protocol::Udp)?;
        let (offer, pending) = a.offer_audio()?;
        let answer = b.accept_offer(offer)?;
        a.accept_answer(pending, answer)?;
        let mut link = Self {
            a,
            b,
            now,
            deaf_a: false,
            a_events: Vec::new(),
            b_events: Vec::new(),
        };
        link.run_until("connect", |l| l.a.is_connected() && l.b.is_connected())?;
        assert_eq!(link.a_events, [PeerEvent::Connected]);
        assert_eq!(link.b_events, [PeerEvent::Connected]);
        link.a_events.clear();
        link.b_events.clear();
        Ok(link)
    }

    /// Carries every queued datagram (B's only while A is not deaf), fires due timeouts, collects events, then
    /// advances the clock.
    fn step(&mut self) -> Result<(), Error> {
        while let Some(d) = self.a.poll_datagram() {
            self.b
                .handle_datagram(self.now, d.proto, d.source, d.destination, &d.contents)?;
        }
        while let Some(d) = self.b.poll_datagram() {
            if !self.deaf_a {
                self.a
                    .handle_datagram(self.now, d.proto, d.source, d.destination, &d.contents)?;
            }
        }
        for peer in [&mut self.a, &mut self.b] {
            if self.now >= peer.deadline() {
                peer.handle_timeout(self.now)?;
            }
        }
        self.a_events.extend(std::iter::from_fn(|| self.a.poll_event()));
        self.b_events.extend(std::iter::from_fn(|| self.b.poll_event()));
        self.now += STEP;
        Ok(())
    }

    fn run_until(&mut self, what: &str, done: impl Fn(&Self) -> bool) -> Result<(), Error> {
        let limit = self.now + PHASE_LIMIT;
        while !done(self) {
            assert!(
                self.now < limit,
                "{what}: no progress within {PHASE_LIMIT:?} of virtual time; A {:?}, B {:?}",
                self.a_events,
                self.b_events
            );
            self.step()?;
        }
        Ok(())
    }

    /// Writes one frame from A and returns the payloads B received within a few steps.
    fn send_a_to_b(&mut self, payload: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
        self.a.write_audio(self.now, self.now, 0, payload)?;
        for _ in 0..5 {
            self.step()?;
        }
        Ok(self
            .b_events
            .drain(..)
            .filter_map(|event| match event {
                PeerEvent::Audio(frame) => Some(frame.payload.to_vec()),
                _ => None,
            })
            .collect())
    }
}

#[test]
fn ice_recovers_after_a_transient_disconnect() {
    // A hears nothing until its ICE pair times out. B still hears A, so B keeps its pair and its checks, which is what
    // lets A rebuild the pair once the gap ends (str0m prunes a pair both sides lost, and only an ICE restart helps).
    let mut link = Link::connected().unwrap();
    link.deaf_a = true;
    link.run_until("ICE disconnect", |l| l.a_events.contains(&PeerEvent::Disconnected))
        .unwrap();
    assert!(!link.a.is_connected());
    assert!(matches!(
        link.a.write_audio(link.now, link.now, 0, &FRAME),
        Err(Error::NotConnected)
    ));

    link.deaf_a = false;
    link.a_events.clear();
    link.b_events.clear();
    link.run_until("ICE recovery", |l| l.a.is_connected() && l.b.is_connected())
        .unwrap();
    assert_eq!(link.a_events, [PeerEvent::Connected], "A reports the recovery");
    let received = link.send_a_to_b(&FRAME).unwrap();
    assert_eq!(received, [FRAME.to_vec()], "media flows again after the recovery");
}

#[test]
fn a_remote_close_ends_the_session() {
    let mut link = Link::connected().unwrap();
    link.a.close().unwrap();
    assert!(!link.a.is_connected());
    link.run_until("remote close", |l| l.b_events.contains(&PeerEvent::Closed))
        .unwrap();
    assert!(!link.b.is_connected());
    assert!(matches!(
        link.b.write_audio(link.now, link.now, 0, &FRAME),
        Err(Error::NotConnected)
    ));
    assert!(
        !link.a_events.contains(&PeerEvent::Closed),
        "only the remote side reports Closed"
    );
}

#[test]
fn write_audio_queues_the_rtp_datagram_at_once() {
    let mut link = Link::connected().unwrap();
    while link.a.poll_datagram().is_some() {}
    link.a.write_audio(link.now, link.now, 0, &FRAME).unwrap();
    let rtp = std::iter::from_fn(|| link.a.poll_datagram())
        .any(|d| d.contents.first().is_some_and(|b| (128..=191).contains(b)));
    assert!(rtp, "the frame left without waiting for a timeout");
}

#[test]
fn unclassifiable_datagrams_are_dropped_and_counted() {
    let mut link = Link::connected().unwrap();
    let from = SocketAddr::from(([127, 0, 0, 1], 30_000));
    let to = SocketAddr::from(([127, 0, 0, 1], 10_000));
    let junk: [&[u8]; 4] = [
        &[],                          // empty
        &[0, 1, 0, 0],                // STUN range, shorter than a header
        &[0x40, 0x00, 0x00, 0x02, 1], // TURN ChannelData
        &[200; 8],                    // first byte above the RTP range
    ];
    for contents in junk {
        link.a
            .handle_datagram(link.now, Protocol::Udp, from, to, contents)
            .unwrap();
    }
    assert_eq!(link.a.dropped_datagrams(), 4);
    let received = link.send_a_to_b(&FRAME).unwrap();
    assert_eq!(received, [FRAME.to_vec()], "the session is unaffected");
}

#[test]
fn udp_peer_keeps_reading_past_junk() {
    let mut peer = UdpPeer::bind(Instant::now(), SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
    let probe = UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
    for _ in 0..3 {
        probe.send_to(&[], peer.local_addr()).unwrap();
    }
    let limit = Instant::now() + Duration::from_secs(5);
    while peer.peer().dropped_datagrams() < 3 {
        assert!(Instant::now() < limit, "the junk never arrived");
        peer.turn(Instant::now()).unwrap();
    }
    assert_eq!(peer.peer().dropped_datagrams(), 3);
}
