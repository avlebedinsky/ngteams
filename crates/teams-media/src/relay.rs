//! Where a call's media is relayed: the TURN servers libwebrtc's ICE agent allocates on, with the credentials the
//! relay handed out (`RelayTokens`, read by the engine).
//!
//! The media goes through the relay of the account's own service only: the peer's ICE policy is relay-only, so
//! libwebrtc gathers relayed candidates alone and no local address goes into an offer. A Microsoft account's relay is
//! Teams' (`docs/protocol/calling-echo.md`, "The relay"), named by its host ([`TEAMS_RELAY_HOST`]) in the three URLs
//! the web client gives its own peer: TURN over UDP on [`TEAMS_RELAY_UDP_PORT`], over TCP on
//! [`TEAMS_RELAY_TCP_PORT`] and over TLS on the same port. An own-server account's is the TURN URLs its server names
//! ([`RelayServer::parse`], `docs/server.md`); a test's is its own TURN server. libwebrtc resolves the hosts,
//! allocates over every URL it can reach, follows a `300 Try Alternate`, and pairs over the best allocation.

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

/// Why [`RelayServer::parse`] took no server from a URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a turn: or turns: URL with a host, an optional port and an optional transport of udp or tcp")]
pub struct InvalidRelayUrl;

/// The port of a `turn:` URL that names none (RFC 7065).
pub const TURN_PORT: u16 = 3478;
/// The port of a `turns:` URL that names none (RFC 7065).
pub const TURNS_PORT: u16 = 5349;

impl RelayServer {
    /// The server a TURN URL names (RFC 7065), as an own server hands them out: `turn:<host>[:<port>]
    /// [?transport=udp|tcp]` (UDP by default) or `turns:<host>[:<port>][?transport=tcp]` (TLS), on the default port
    /// when it names none. The inverse of [`RelayServer::url`].
    ///
    /// # Errors
    /// [`InvalidRelayUrl`] for any other text.
    pub fn parse(url: &str) -> Result<Self, InvalidRelayUrl> {
        let (secure, rest) = match (url.strip_prefix("turns:"), url.strip_prefix("turn:")) {
            (Some(rest), _) => (true, rest),
            (None, Some(rest)) => (false, rest),
            (None, None) => return Err(InvalidRelayUrl),
        };
        if rest.starts_with('/') {
            return Err(InvalidRelayUrl);
        }
        // the authority and query read as a hierarchical URL's
        let parsed = url::Url::parse(&format!("turn://{rest}")).map_err(|_| InvalidRelayUrl)?;
        let host = match parsed.host() {
            Some(url::Host::Ipv6(address)) => address.to_string(),
            Some(url::Host::Ipv4(address)) => address.to_string(),
            Some(url::Host::Domain(domain)) if !domain.is_empty() => domain.to_owned(),
            _ => return Err(InvalidRelayUrl),
        };
        let bare = parsed.username().is_empty()
            && parsed.password().is_none()
            && matches!(parsed.path(), "" | "/")
            && parsed.fragment().is_none();
        let pairs: Vec<_> = parsed.query_pairs().collect();
        let transport = match (pairs.as_slice(), secure) {
            ([], false) => Transport::Udp,
            ([], true) => Transport::Tls,
            ([(key, value)], false) if key == "transport" && value == "udp" => Transport::Udp,
            ([(key, value)], false) if key == "transport" && value == "tcp" => Transport::Tcp,
            ([(key, value)], true) if key == "transport" && value == "tcp" => Transport::Tls,
            _ => return Err(InvalidRelayUrl),
        };
        if !bare {
            return Err(InvalidRelayUrl);
        }
        let port = parsed.port().unwrap_or(if secure { TURNS_PORT } else { TURN_PORT });
        Ok(Self { host, port, transport })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(host: &str, port: u16, transport: Transport) -> RelayServer {
        RelayServer {
            host: host.to_owned(),
            port,
            transport,
        }
    }

    #[test]
    fn a_turn_url_names_its_server_with_rfc_7065s_defaults() {
        let parsed = |url: &str| RelayServer::parse(url).unwrap();
        assert_eq!(
            parsed("turn:turn.own.example.test:3479?transport=udp"),
            server("turn.own.example.test", 3479, Transport::Udp)
        );
        assert_eq!(
            parsed("turn:turn.own.example.test?transport=tcp"),
            server("turn.own.example.test", TURN_PORT, Transport::Tcp)
        );
        assert_eq!(
            parsed("turn:turn.own.example.test"),
            server("turn.own.example.test", TURN_PORT, Transport::Udp)
        );
        assert_eq!(
            parsed("turns:turn.own.example.test"),
            server("turn.own.example.test", TURNS_PORT, Transport::Tls)
        );
        assert_eq!(
            parsed("turns:turn.own.example.test:443?transport=tcp"),
            server("turn.own.example.test", 443, Transport::Tls)
        );
        assert_eq!(
            parsed("turn:127.0.0.1:53478"),
            server("127.0.0.1", 53_478, Transport::Udp)
        );
        assert_eq!(
            parsed("turn:[2001:db8::7]:3478"),
            server("2001:db8::7", 3478, Transport::Udp)
        );
        for written in [
            server("turn.own.example.test", 3478, Transport::Udp),
            server("turn.own.example.test", 443, Transport::Tcp),
            server("turn.own.example.test", 443, Transport::Tls),
            server("2001:db8::7", 5349, Transport::Tls),
        ] {
            assert_eq!(parsed(&written.url()), written);
        }
    }

    #[test]
    fn anything_else_names_no_server() {
        for url in [
            "",
            "stun:turn.own.example.test",
            "https://turn.own.example.test",
            "turn:",
            "turn:?transport=udp",
            "turn://turn.own.example.test",
            "turn: turn.own.example.test",
            "turn:turn.own.example.test:port",
            "turn:turn.own.example.test?transport=sctp",
            "turns:turn.own.example.test?transport=udp",
            "turn:turn.own.example.test?transport=udp&transport=tcp",
            "turn:user@turn.own.example.test",
            "turn:turn.own.example.test/path",
            "turn:turn.own.example.test#part",
        ] {
            assert_eq!(RelayServer::parse(url), Err(InvalidRelayUrl), "{url}");
        }
    }
}
