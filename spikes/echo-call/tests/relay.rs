//! A str0m peer behind the relay (`teams_media::test_turn`, our loopback TURN server) carrying the echo probe's tone to
//! the echo stand-in and back, over UDP and over TCP. The TURN client's own tests are `teams-media`'s.
//!
//! Set `ECHO_CALL_TURN_SERVER=<ip:port>`, `ECHO_CALL_TURN_USER` and `ECHO_CALL_TURN_PASSWORD` to run the same echo
//! through an external TURN server on loopback (coturn, started by hand with loopback peers allowed); without them
//! that test only says it was skipped. Never point it at a public server.

mod support;

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use echo_call::Error;
use echo_call::echo::{EchoConfig, EchoReport, run_echo};
use echo_call::relay::{MediaPath, RelayedPeer};
use str0m::change::{SdpAnswer, SdpOffer};
use support::EchoBot;
use teams_media::relay::{RelayLink, RelayOptions, RelayServer};
use teams_media::stun::Framing;
use teams_media::test_turn::{PASSWORD, TurnServer, USER};
use teams_media::turn::{Credentials, Transport};

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
