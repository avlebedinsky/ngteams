//! Where a call's media is relayed: the TURN servers libwebrtc's ICE agent allocates on, with the credentials the
//! relay handed out (`RelayTokens`, read by the engine).
//!
//! The media goes through Teams' relay only (`docs/protocol/calling-echo.md`, "The relay"): the peer's ICE policy is
//! relay-only, so libwebrtc gathers relayed candidates alone and no local address goes into an offer. Teams' relay is
//! named by its host ([`TEAMS_RELAY_HOST`]) in the three URLs the web client gives its own peer: TURN over UDP on
//! [`TEAMS_RELAY_UDP_PORT`], over TCP on [`TEAMS_RELAY_TCP_PORT`] and over TLS on the same port. libwebrtc resolves
//! the host, allocates over every URL it can reach, follows a `300 Try Alternate`, and pairs over the best allocation.

use std::fmt;

/// Teams' relay host: one name for every region (the web client's relay URLs name it).
pub const TEAMS_RELAY_HOST: &str = "gateway-world.az.relay.teams.cloud.microsoft";
/// The relay's port for TURN over UDP.
pub const TEAMS_RELAY_UDP_PORT: u16 = 3478;
/// The relay's port for TURN over TCP and over TLS.
pub const TEAMS_RELAY_TCP_PORT: u16 = 443;

/// How a TURN server is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Udp,
    Tcp,
    /// TCP with TLS (`turns:`).
    Tls,
}

/// The long-term credentials of a TURN server.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // a credential never reaches a log
        f.debug_struct("Credentials").finish_non_exhaustive()
    }
}

/// One TURN server: its host (a name or an address) and port, and how it is reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayServer {
    pub host: String,
    pub port: u16,
    pub transport: Transport,
}

impl RelayServer {
    /// TURN over UDP at `addr` (a test's loopback server).
    #[must_use]
    pub fn udp(addr: std::net::SocketAddr) -> Self {
        Self {
            host: addr.ip().to_string(),
            port: addr.port(),
            transport: Transport::Udp,
        }
    }

    /// The server as libwebrtc names it: `turn:<host>:<port>?transport=udp|tcp`, or `turns:<host>:<port>`.
    #[must_use]
    pub fn url(&self) -> String {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        match self.transport {
            Transport::Udp => format!("turn:{host}:{}?transport=udp", self.port),
            Transport::Tcp => format!("turn:{host}:{}?transport=tcp", self.port),
            Transport::Tls => format!("turns:{host}:{}?transport=tcp", self.port),
        }
    }
}

impl fmt::Display for RelayServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.url())
    }
}

/// Teams' relay, as the web client names it: UDP 3478, TCP 443 and TLS 443 on [`TEAMS_RELAY_HOST`].
#[must_use]
pub fn teams_relay() -> Vec<RelayServer> {
    let server = |port, transport| RelayServer {
        host: TEAMS_RELAY_HOST.to_owned(),
        port,
        transport,
    };
    vec![
        server(TEAMS_RELAY_UDP_PORT, Transport::Udp),
        server(TEAMS_RELAY_TCP_PORT, Transport::Tcp),
        server(TEAMS_RELAY_TCP_PORT, Transport::Tls),
    ]
}

/// Where a call's media is relayed: the servers and their credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayConfig {
    pub servers: Vec<RelayServer>,
    pub credentials: Credentials,
}
