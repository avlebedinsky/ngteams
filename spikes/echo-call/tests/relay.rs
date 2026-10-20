//! The TURN client on real loopback sockets against the stand-in server (`support`), and a str0m peer behind the
//! relay carrying the echo probe's tone to the echo stand-in and back, over UDP and over TCP.
//!
//! Set `ECHO_CALL_TURN_SERVER=<ip:port>`, `ECHO_CALL_TURN_USER` and `ECHO_CALL_TURN_PASSWORD` to run the same echo
//! through an external TURN server on loopback (coturn, started by hand with loopback peers allowed); without them
//! that test only says it was skipped. Never point it at a public server.

mod support;

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use echo_call::Error;
use echo_call::echo::{EchoConfig, EchoReport, run_echo};
use echo_call::relay::{MediaPath, RelayLink, RelayOptions, RelayServer, RelayedPeer, allocate_any, teams_relay};
use echo_call::stun::Framing;
use echo_call::turn::{Credentials, Transport, TurnEvent, TurnFailure};
use str0m::change::{SdpAnswer, SdpOffer};
use support::{EchoBot, PASSWORD, TurnServer, USER};

const TOLERANCE_HZ: f64 = 15.0;

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

fn relays_both_ways(server: RelayServer, turn: &TurnServer) -> TestResult<()> {
    let mut link = RelayLink::allocate(server, credentials(), &RelayOptions::default())?;
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
    assert!(turn.counters.channel_data.load(Ordering::Relaxed) >= 1);
    assert_eq!(turn.counters.send_indications.load(Ordering::Relaxed), 1);

    link.close(Instant::now())?;
    poll_until(&mut link, seen, |e| *e == TurnEvent::Closed)?;
    Ok(())
}

#[test]
fn allocates_over_udp_and_relays_both_ways() {
    let turn = TurnServer::start().unwrap();
    relays_both_ways(udp(turn.udp), &turn).unwrap();
}

#[test]
fn allocates_over_tcp_and_relays_both_ways() {
    let turn = TurnServer::start().unwrap();
    relays_both_ways(tcp(turn.tcp), &turn).unwrap();
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

/// Our peer behind `server`, the echo stand-in on plain UDP, SDP exchanged as text; runs the probe.
fn echo_through(server: RelayServer, credentials: Credentials, framing: Framing) -> TestResult<EchoReport> {
    let options = RelayOptions {
        framing,
        ..RelayOptions::default()
    };
    let link = RelayLink::allocate(server, credentials, &options)?;
    let mut ours = RelayedPeer::new(Instant::now(), link)?;
    let mut bot = EchoBot::bind()?;
    let (offer, pending) = ours.peer().offer_audio()?;
    let offer = SdpOffer::from_sdp_string(&offer.to_sdp_string())?;
    assert!(offer.to_sdp_string().contains("typ relay"));
    let answer = bot.peer.peer().accept_offer(offer)?;
    let answer = SdpAnswer::from_sdp_string(&answer.to_sdp_string())?;
    ours.peer().accept_answer(pending, answer)?;

    let config = EchoConfig {
        frames: 50,
        connect_timeout: Duration::from_secs(10),
        ..EchoConfig::default()
    };
    let report = run_echo(&mut ours, &mut |now| bot.step(now), &config)?;
    eprintln!("echo through {server}:\n{report}");
    Ok(report)
}

fn assert_echo(report: &EchoReport) {
    assert_eq!(report.frames_sent, 50);
    assert!(report.frames_echoed >= 45, "{report}");
    assert!(
        report
            .echoed_hz
            .is_some_and(|hz| (hz - report.sent_hz).abs() <= TOLERANCE_HZ),
        "{report}"
    );
    assert!(
        report
            .round_trip
            .is_some_and(|round_trip| round_trip.p95 < Duration::from_millis(200)),
        "{report}"
    );
}

#[test]
fn the_probe_refuses_unusable_settings() {
    let mut direct = media_loop::peer::UdpPeer::bind(Instant::now(), "127.0.0.1:0".parse().unwrap()).unwrap();
    for config in [
        EchoConfig {
            frames: 0,
            ..EchoConfig::default()
        },
        EchoConfig {
            tone_hz: 30_000.0,
            ..EchoConfig::default()
        },
    ] {
        let error = run_echo(&mut direct, &mut |_| Ok(()), &config).unwrap_err();
        assert!(matches!(error, Error::Config(_)), "{error}");
    }
}

#[test]
fn the_probe_times_out_when_the_media_never_connects() {
    let mut lonely = media_loop::peer::UdpPeer::bind(Instant::now(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let config = EchoConfig {
        connect_timeout: Duration::from_millis(100),
        ..EchoConfig::default()
    };
    let error = run_echo(&mut lonely, &mut |_| Ok(()), &config).unwrap_err();
    assert!(matches!(error, Error::Timeout(_)), "{error}");
}

#[test]
fn the_tone_comes_back_through_a_udp_relay() {
    let turn = TurnServer::start().unwrap();
    assert_echo(&echo_through(udp(turn.udp), credentials(), Framing::Native).unwrap());
    assert!(
        turn.counters.channel_data.load(Ordering::Relaxed) > 0,
        "media went by ChannelData"
    );
}

#[test]
fn the_tone_comes_back_through_a_tcp_relay() {
    let turn = TurnServer::start().unwrap();
    assert_echo(&echo_through(tcp(turn.tcp), credentials(), Framing::Native).unwrap());
}

#[test]
fn the_tone_comes_back_through_an_external_turn_server() {
    let (Ok(addr), Ok(username), Ok(password)) = (
        std::env::var("ECHO_CALL_TURN_SERVER"),
        std::env::var("ECHO_CALL_TURN_USER"),
        std::env::var("ECHO_CALL_TURN_PASSWORD"),
    ) else {
        eprintln!("skipped: ECHO_CALL_TURN_SERVER, ECHO_CALL_TURN_USER and ECHO_CALL_TURN_PASSWORD are not set");
        return;
    };
    let addr: SocketAddr = addr.parse().unwrap();
    assert!(addr.ip().is_loopback(), "only a TURN server on loopback");
    let credentials = Credentials { username, password };
    assert_echo(&echo_through(udp(addr), credentials.clone(), Framing::Native).unwrap());
    assert_echo(&echo_through(tcp(addr), credentials, Framing::Native).unwrap());
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
        echo_call::turn::TEAMS_RELAY_HOST,
        "gateway-world.az.relay.teams.cloud.microsoft"
    );
}
