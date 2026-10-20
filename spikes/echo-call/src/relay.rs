//! The TURN client on real sockets, and a `str0m` peer behind it.
//!
//! - [`RelayLink`] is one allocation over UDP or TCP ([`Framing`] picks how TCP carries the messages). Everything is
//!   non-blocking; the caller turns it with `now`, as `media-loop`'s `UdpPeer` is turned.
//! - [`allocate_any`] tries relay servers in order until one allocates: list the UDP ones first and TCP (port 443 in
//!   Teams' relay descriptors) after, and a blocked UDP falls back to TCP.
//! - [`RelayedPeer`] is a `media_loop` [`Peer`] whose only candidate is the allocation: what str0m sends from the
//!   allocation goes out through the server, what peers send to the allocation comes back in.

use std::fmt;
use std::io::{ErrorKind, Read as _, Write as _};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

use media_loop::peer::{Peer, UdpPeer};
use str0m::Candidate;
use str0m::net::Protocol;

use crate::Error;
use crate::stun::{Framing, StreamDecoder};
use crate::turn::{Credentials, Timing, Transport, TurnClient, TurnEvent};

/// Largest message we read at once.
const MAX_READ: usize = 4_096;
/// Nap between turns while waiting for the allocation.
const WAIT_NAP: Duration = Duration::from_millis(1);

/// One TURN server address and how to reach it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelayServer {
    pub addr: SocketAddr,
    pub transport: Transport,
}

impl fmt::Display for RelayServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let transport = match self.transport {
            Transport::Udp => "udp",
            Transport::Tcp => "tcp",
        };
        write!(f, "{transport}:{}", self.addr)
    }
}

/// The order to try a relay's addresses in: every address over UDP first, then every address over TCP.
#[must_use]
pub fn fallback_order(addresses: &[IpAddr], udp_port: u16, tcp_port: u16) -> Vec<RelayServer> {
    let udp = addresses.iter().map(|ip| RelayServer {
        addr: SocketAddr::new(*ip, udp_port),
        transport: Transport::Udp,
    });
    let tcp = addresses.iter().map(|ip| RelayServer {
        addr: SocketAddr::new(*ip, tcp_port),
        transport: Transport::Tcp,
    });
    udp.chain(tcp).collect()
}

/// How to allocate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelayOptions {
    /// How a TCP stream carries the messages.
    pub framing: Framing,
    /// How long one server may take to allocate before the next is tried.
    pub allocate_timeout: Duration,
    /// Retransmission over UDP (RFC 8489 by default).
    pub udp_timing: Timing,
}

impl Default for RelayOptions {
    fn default() -> Self {
        Self {
            framing: Framing::Native,
            allocate_timeout: Duration::from_secs(5),
            udp_timing: Timing::udp(),
        }
    }
}

enum Io {
    Udp(UdpSocket),
    Tcp {
        stream: TcpStream,
        decoder: StreamDecoder,
        framing: Framing,
        pending: Vec<u8>,
    },
}

/// One TURN allocation on a socket.
pub struct RelayLink {
    client: TurnClient,
    io: Io,
    server: RelayServer,
    local: SocketAddr,
    relayed: SocketAddr,
    buf: Vec<u8>,
    malformed: u64,
}

impl fmt::Debug for RelayLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayLink")
            .field("server", &self.server)
            .field("local", &self.local)
            .field("relayed", &self.relayed)
            .finish_non_exhaustive()
    }
}

impl RelayLink {
    /// Connects to `server` and allocates, waiting up to `options.allocate_timeout`.
    ///
    /// # Errors
    /// [`Error::Io`] when the socket fails (a UDP port answered by ICMP "unreachable" included), [`Error::Turn`] when
    /// the server refuses, [`Error::Timeout`] when it doesn't answer in time.
    pub fn allocate(server: RelayServer, credentials: Credentials, options: &RelayOptions) -> Result<Self, Error> {
        let deadline = Instant::now() + options.allocate_timeout;
        let (io, local, client) = match server.transport {
            Transport::Udp => {
                let socket = UdpSocket::bind(unspecified(server.addr))?;
                socket.connect(server.addr)?;
                socket.set_nonblocking(true)?;
                let local = socket.local_addr()?;
                let client = TurnClient::new(Transport::Udp, credentials).with_timing(options.udp_timing);
                (Io::Udp(socket), local, client)
            }
            Transport::Tcp => {
                let stream = TcpStream::connect_timeout(&server.addr, options.allocate_timeout)?;
                stream.set_nodelay(true)?;
                stream.set_nonblocking(true)?;
                let local = stream.local_addr()?;
                let io = Io::Tcp {
                    stream,
                    decoder: StreamDecoder::new(options.framing),
                    framing: options.framing,
                    pending: Vec::new(),
                };
                (io, local, TurnClient::new(Transport::Tcp, credentials))
            }
        };
        let mut link = Self {
            client: client.with_software("ms-teams echo-call spike"),
            io,
            server,
            local,
            relayed: local,
            buf: vec![0; MAX_READ],
            malformed: 0,
        };
        link.client.allocate(Instant::now())?;
        link.wait_allocated(deadline)?;
        Ok(link)
    }

    fn wait_allocated(&mut self, deadline: Instant) -> Result<(), Error> {
        loop {
            let now = Instant::now();
            for event in self.poll(now)? {
                match event {
                    TurnEvent::Allocated { relayed, .. } => {
                        self.relayed = relayed;
                        return Ok(());
                    }
                    TurnEvent::Failed(failure) => return Err(Error::Turn(failure)),
                    _ => {}
                }
            }
            if now >= deadline {
                return Err(Error::Timeout("TURN allocation"));
            }
            thread::sleep(WAIT_NAP);
        }
    }

    /// The server this allocation lives on.
    #[must_use]
    pub fn server(&self) -> RelayServer {
        self.server
    }

    /// Our end of the socket to the server.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// The allocation: where peers reach us.
    #[must_use]
    pub fn relayed_addr(&self) -> SocketAddr {
        self.relayed
    }

    /// Server messages that were neither STUN nor `ChannelData`, dropped.
    #[must_use]
    pub fn malformed(&self) -> u64 {
        self.malformed
    }

    /// Lets data from `ip` through.
    ///
    /// # Errors
    /// [`Error::TurnUse`] before the allocation exists.
    pub fn permit(&mut self, now: Instant, ip: IpAddr) -> Result<(), Error> {
        Ok(self.client.permit(now, ip)?)
    }

    /// Relays `payload` to `peer` (sent on the next [`RelayLink::poll`] or [`RelayLink::flush`]).
    ///
    /// # Errors
    /// [`Error::TurnUse`] when the allocation is gone or out of channels.
    pub fn send_to(&mut self, now: Instant, peer: SocketAddr, payload: &[u8]) -> Result<(), Error> {
        Ok(self.client.send_to(now, peer, payload)?)
    }

    /// Releases the allocation; the answer comes as [`TurnEvent::Closed`] on a later poll.
    ///
    /// # Errors
    /// [`Error::TurnUse`] before the allocation exists, [`Error::Io`] when the socket fails.
    pub fn close(&mut self, now: Instant) -> Result<(), Error> {
        self.client.close(now)?;
        self.flush()
    }

    /// One turn: reads everything waiting, fires due timers, sends what the client queued, and returns its events.
    ///
    /// # Errors
    /// [`Error::Io`] when the socket fails or the TCP stream closes or desynchronizes.
    pub fn poll(&mut self, now: Instant) -> Result<Vec<TurnEvent>, Error> {
        self.read(now)?;
        if self.client.poll_timeout().is_some_and(|at| at <= now) {
            self.client.handle_timeout(now)?;
        }
        self.flush()?;
        Ok(std::iter::from_fn(|| self.client.poll_event()).collect())
    }

    /// Sends every queued message.
    ///
    /// # Errors
    /// [`Error::Io`] when the socket fails.
    pub fn flush(&mut self) -> Result<(), Error> {
        while let Some(message) = self.client.poll_transmit() {
            match &mut self.io {
                Io::Udp(socket) => match socket.send(&message) {
                    Ok(_) => {}
                    // a full socket buffer drops the datagram, as the network would
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.into()),
                },
                Io::Tcp { framing, pending, .. } => pending.extend(framing.frame(&message)?),
            }
        }
        if let Io::Tcp { stream, pending, .. } = &mut self.io {
            while !pending.is_empty() {
                match stream.write(pending) {
                    Ok(0) => return Err(Error::Io(ErrorKind::WriteZero.into())),
                    Ok(written) => drop(pending.drain(..written)),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Ok(())
    }

    fn read(&mut self, now: Instant) -> Result<(), Error> {
        loop {
            let message = match &mut self.io {
                Io::Udp(socket) => match socket.recv(&mut self.buf) {
                    Ok(len) => self.buf[..len].to_vec(),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
                    Err(e) => return Err(e.into()),
                },
                Io::Tcp { stream, decoder, .. } => match decoder.next_message()? {
                    Some(message) => message,
                    None => match stream.read(&mut self.buf) {
                        Ok(0) => return Err(Error::Io(ErrorKind::UnexpectedEof.into())),
                        Ok(len) => {
                            decoder.push(&self.buf[..len]);
                            continue;
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
                        Err(e) => return Err(e.into()),
                    },
                },
            };
            if self.client.handle_input(now, &message).is_err() {
                self.malformed += 1;
            }
        }
    }
}

/// The unspecified address of `server`'s family, port 0.
fn unspecified(server: SocketAddr) -> SocketAddr {
    match server {
        SocketAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
        SocketAddr::V6(_) => SocketAddr::from(([0u16; 8], 0)),
    }
}

/// Allocates on the first server of `servers` that works, in order. Returns the link and every failure before it.
///
/// # Errors
/// [`Error::NoRelay`] with each server's failure when none allocates.
pub fn allocate_any(
    servers: &[RelayServer],
    credentials: &Credentials,
    options: &RelayOptions,
) -> Result<(RelayLink, Vec<(RelayServer, String)>), Error> {
    let mut failures = Vec::new();
    for server in servers {
        match RelayLink::allocate(*server, credentials.clone(), options) {
            Ok(link) => return Ok((link, failures)),
            Err(error) => failures.push((*server, error.to_string())),
        }
    }
    Err(Error::NoRelay(failures))
}

/// Something a call's media runs over: a `media_loop` peer and the sockets that carry it.
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

/// A peer reached only through a TURN allocation.
pub struct RelayedPeer {
    peer: Peer,
    link: RelayLink,
}

impl fmt::Debug for RelayedPeer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayedPeer")
            .field("link", &self.link)
            .field("connected", &self.peer.is_connected())
            .finish_non_exhaustive()
    }
}

impl RelayedPeer {
    /// A peer whose only candidate is `link`'s allocation.
    ///
    /// # Errors
    /// [`Error::Media`] when str0m refuses the candidate.
    pub fn new(now: Instant, link: RelayLink) -> Result<Self, Error> {
        let candidate = Candidate::relayed(link.relayed_addr(), link.local_addr(), Protocol::Udp)
            .map_err(|e| Error::Media(media_loop::Error::Rtc(e.to_string())))?;
        let peer = Peer::with_candidate(now, candidate)?;
        Ok(Self { peer, link })
    }

    /// The relay link.
    pub fn link(&mut self) -> &mut RelayLink {
        &mut self.link
    }
}

impl MediaPath for RelayedPeer {
    fn peer(&mut self) -> &mut Peer {
        &mut self.peer
    }

    fn turn(&mut self, now: Instant) -> Result<bool, Error> {
        let relayed = self.link.relayed_addr();
        let mut received = false;
        for event in self.link.poll(now)? {
            match event {
                TurnEvent::Data { peer, payload } => {
                    received = true;
                    self.peer.handle_datagram(now, Protocol::Udp, peer, relayed, &payload)?;
                }
                TurnEvent::Failed(failure) => return Err(Error::Turn(failure)),
                _ => {}
            }
        }
        if now >= self.peer.deadline() {
            self.peer.handle_timeout(now)?;
        }
        MediaPath::flush(self, now)?;
        Ok(received)
    }

    fn flush(&mut self, now: Instant) -> Result<(), Error> {
        let relayed = self.link.relayed_addr();
        while let Some(datagram) = self.peer.poll_datagram() {
            // the peer has no other candidate, so everything leaves from the allocation
            if datagram.source == relayed {
                self.link.send_to(now, datagram.destination, &datagram.contents)?;
            }
        }
        self.link.flush()
    }
}
