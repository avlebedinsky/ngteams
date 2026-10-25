//! The TURN client on real loopback sockets against the stand-in server (`teams_media::test_turn`): allocations over
//! UDP and TCP, data relayed both ways, refused credentials, the fallback from a silent UDP relay to TCP.
//!
//! Set `TEAMS_MEDIA_TURN_SERVER=<ip:port>`, `TEAMS_MEDIA_TURN_USER` and `TEAMS_MEDIA_TURN_PASSWORD` to run the same
//! relay through an external TURN server on loopback (coturn, started by hand with loopback peers allowed); without
//! them that test only says it was skipped. Never point it at a public server.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use teams_media::Error;
use teams_media::relay::{RelayLink, RelayOptions, RelayServer, allocate_any, teams_relay};
use teams_media::test_turn::{PASSWORD, TurnServer, USER};
use teams_media::turn::{Credentials, Transport, TurnEvent, TurnFailure};

fn credentials() -> Credentials {
    Credentials {
        username: USER.into(),
        password: PASSWORD.into(),
    }
}

fn udp(addr: SocketAddr) -> RelayServer {
    RelayServer {
        addr,
        transport: Transport::Udp,
    }
}

fn tcp(addr: SocketAddr) -> RelayServer {
    RelayServer {
        addr,
        transport: Transport::Tcp,
    }
}

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Polls `link` until `done` holds for one of its events (or one already in `seen`); returns every event seen.
fn poll_until(
    link: &mut RelayLink,
    mut seen: Vec<TurnEvent>,
    done: impl Fn(&TurnEvent) -> bool,
) -> TestResult<Vec<TurnEvent>> {
    let limit = Instant::now() + Duration::from_secs(5);
    if seen.iter().any(&done) {
        return Ok(seen);
    }
    loop {
        assert!(Instant::now() < limit, "no matching event; saw {seen:?}");
        let events = link.poll(Instant::now())?;
        let hit = events.iter().any(&done);
        seen.extend(events);
        if hit {
            return Ok(seen);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Reads one datagram on `socket` within a few seconds, polling `link` meanwhile (its events go to `seen`).
fn receive(socket: &UdpSocket, link: &mut RelayLink, seen: &mut Vec<TurnEvent>) -> TestResult<(Vec<u8>, SocketAddr)> {
    socket.set_nonblocking(true)?;
    let limit = Instant::now() + Duration::from_secs(5);
    let mut buf = [0; 1500];
    loop {
        assert!(Instant::now() < limit, "nothing was relayed to the peer");
        seen.extend(link.poll(Instant::now())?);
        if let Ok((len, from)) = socket.recv_from(&mut buf) {
            return Ok((buf[..len].to_vec(), from));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Allocates on `server`, relays to a loopback peer and back (by `Send` indication, then by `ChannelData` once the
/// channel is bound), and releases. With `turn`, our stand-in, also checks how the data travelled.
fn relays_both_ways(server: RelayServer, credentials: Credentials, turn: Option<&TurnServer>) -> TestResult<()> {
    let mut link = RelayLink::allocate(server, credentials, &RelayOptions::default())?;
    let relayed = link.relayed_addr();
    assert_ne!(relayed, server.addr, "the allocation is a port of its own");
    let peer = UdpSocket::bind("127.0.0.1:0")?;
    let peer_addr = peer.local_addr()?;

    let mut seen = Vec::new();
    link.send_to(Instant::now(), peer_addr, b"hello")?;
    let (payload, from) = receive(&peer, &mut link, &mut seen)?;
    assert_eq!(
        (payload.as_slice(), from),
        (&b"hello"[..], relayed),
        "sent from the allocation"
    );

    peer.send_to(b"world", relayed)?;
    let seen = poll_until(&mut link, seen, |e| matches!(e, TurnEvent::Data { .. }))?;
    assert!(seen.contains(&TurnEvent::Data {
        peer: peer_addr,
        payload: b"world".to_vec()
    }));
    assert!(seen.contains(&TurnEvent::PermissionCreated(peer_addr.ip())), "{seen:?}");

    // once the channel is bound, data goes as ChannelData both ways
    let mut seen = poll_until(&mut link, seen, |e| matches!(e, TurnEvent::ChannelBound { .. }))?;
    link.send_to(Instant::now(), peer_addr, b"again")?;
    assert_eq!(receive(&peer, &mut link, &mut seen)?.0, b"again");
    if let Some(turn) = turn {
        assert!(turn.counters.channel_data.load(Ordering::Relaxed) >= 1);
        assert_eq!(turn.counters.send_indications.load(Ordering::Relaxed), 1);
    }

    link.close(Instant::now())?;
    poll_until(&mut link, seen, |e| *e == TurnEvent::Closed)?;
    Ok(())
}

#[test]
fn allocates_over_udp_and_relays_both_ways() {
    let turn = TurnServer::start().unwrap();
    relays_both_ways(udp(turn.udp), credentials(), Some(&turn)).unwrap();
}

#[test]
fn allocates_over_tcp_and_relays_both_ways() {
    let turn = TurnServer::start().unwrap();
    relays_both_ways(tcp(turn.tcp), credentials(), Some(&turn)).unwrap();
}

#[test]
fn wrong_credentials_fail_the_allocation() {
    let turn = TurnServer::start().unwrap();
    let wrong = Credentials {
        username: USER.into(),
        password: "not-the-password".into(),
    };
    for server in [udp(turn.udp), tcp(turn.tcp)] {
        let error = RelayLink::allocate(server, wrong.clone(), &RelayOptions::default()).unwrap_err();
        assert!(
            matches!(error, Error::Turn(TurnFailure::Unauthorized)),
            "{server}: {error}"
        );
    }
}

#[test]
fn a_silent_udp_relay_falls_back_to_tcp() {
    let turn = TurnServer::start().unwrap();
    // a bound socket that never answers: UDP to the relay is "blocked"
    let black_hole = UdpSocket::bind("127.0.0.1:0").unwrap();
    let options = RelayOptions {
        allocate_timeout: Duration::from_millis(300),
        ..RelayOptions::default()
    };
    let servers = [udp(black_hole.local_addr().unwrap()), tcp(turn.tcp)];

    let (link, failures) = allocate_any(&servers, &credentials(), &options).unwrap();

    assert_eq!(link.server(), servers[1], "TCP took over");
    assert_eq!(failures.len(), 1);
    assert!(failures[0].1.contains("timed out"), "{failures:?}");
}

#[test]
fn no_working_relay_is_an_error_listing_every_attempt() {
    let black_hole = UdpSocket::bind("127.0.0.1:0").unwrap();
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let options = RelayOptions {
        allocate_timeout: Duration::from_millis(200),
        ..RelayOptions::default()
    };
    let servers = [udp(black_hole.local_addr().unwrap()), tcp(closed)];
    let Err(Error::NoRelay(failures)) = allocate_any(&servers, &credentials(), &options) else {
        panic!("expected NoRelay");
    };
    assert_eq!(failures.len(), 2);
}

#[test]
fn relays_both_ways_through_an_external_turn_server() {
    let (Ok(addr), Ok(username), Ok(password)) = (
        std::env::var("TEAMS_MEDIA_TURN_SERVER"),
        std::env::var("TEAMS_MEDIA_TURN_USER"),
        std::env::var("TEAMS_MEDIA_TURN_PASSWORD"),
    ) else {
        eprintln!("skipped: TEAMS_MEDIA_TURN_SERVER, TEAMS_MEDIA_TURN_USER and TEAMS_MEDIA_TURN_PASSWORD are not set");
        return;
    };
    let addr: SocketAddr = addr.parse().unwrap();
    assert!(addr.ip().is_loopback(), "only a TURN server on loopback");
    let credentials = Credentials { username, password };
    relays_both_ways(udp(addr), credentials.clone(), None).unwrap();
    relays_both_ways(tcp(addr), credentials, None).unwrap();
}

#[test]
fn teams_relay_is_tried_over_udp_3478_first_then_tcp_443() {
    // documentation addresses standing in for what the relay host resolves to; nothing is sent
    let addresses = ["192.0.2.10".parse().unwrap(), "2001:db8::10".parse().unwrap()];
    let servers: Vec<(Transport, SocketAddr)> = teams_relay(&addresses)
        .iter()
        .map(|server| (server.transport, server.addr))
        .collect();
    assert_eq!(
        servers,
        [
            (Transport::Udp, "192.0.2.10:3478".parse().unwrap()),
            (Transport::Udp, "[2001:db8::10]:3478".parse().unwrap()),
            (Transport::Tcp, "192.0.2.10:443".parse().unwrap()),
            (Transport::Tcp, "[2001:db8::10]:443".parse().unwrap()),
        ]
    );
    assert_eq!(
        teams_media::turn::TEAMS_RELAY_HOST,
        "gateway-world.az.relay.teams.cloud.microsoft"
    );
}
