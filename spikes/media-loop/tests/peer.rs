//! The UDP peer on a real socket. The sans-IO peer's own tests moved with it to `teams-media` (`tests/peer.rs`).

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use media_loop::peer::UdpPeer;

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
