//! Tests only (`test-support`): a minimal TURN server on loopback (UDP and TCP, long-term credentials, permissions,
//! channels), for this crate's tests, the mock's media (`teams-mock`) and the engine's call tests (`teams-core`'s
//! `tests/calls`). It can impair the media it relays ([`Impairment`]): drop RTP packets and send some after the one
//! that followed them, as a lossy network would.
//!
//! The server is ours, built on the crate's own STUN codec, so it can share a misreading of the RFC with the client;
//! the codec is checked separately against the RFC 5769 test vectors (`tests/turn.rs`), and `tests/relay.rs` can also
//! run against an external server (coturn) given in the environment.

use std::collections::{HashMap, HashSet};
use std::io::{self, ErrorKind, Read as _, Write as _};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::stun::{
    Attribute, ChannelData, Class, Framing, IntegrityKey, Kind, Message, Method, StreamDecoder, TransactionId,
    classify, verify_integrity,
};

pub const USER: &str = "lab-user";
pub const PASSWORD: &str = "lab-password";
pub const REALM: &str = "turn.test";
const NONCE: &str = "test-nonce-0001";
const LIFETIME: u32 = 600;
/// How long a packet held back for reordering waits for the next one before it goes anyway.
const HOLD_LIMIT: Duration = Duration::from_millis(60);

/// What the server does to the media it relays between allocations: only RTP (a first byte of version 2 and a payload
/// type outside RTCP's range); STUN, DTLS and RTCP pass untouched, so the connection is made as on a clean network.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Impairment {
    /// RTP packets dropped, in percent.
    pub loss_percent: u8,
    /// RTP packets held back and sent after the next one to the same client, in percent.
    pub reorder_percent: u8,
    /// Seeds the choices, so a run drops and reorders the same packets.
    pub seed: u64,
}

/// What the server saw, for assertions.
#[derive(Debug, Default)]
pub struct Counters {
    pub allocations: AtomicUsize,
    pub send_indications: AtomicUsize,
    pub channel_data: AtomicUsize,
    /// RTP packets relayed between allocations (dropped ones included).
    pub rtp_relayed: AtomicUsize,
    pub rtp_dropped: AtomicUsize,
    pub rtp_reordered: AtomicUsize,
}

/// The TURN server, running on its own thread until dropped.
pub struct TurnServer {
    pub udp: SocketAddr,
    pub tcp: SocketAddr,
    pub counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TurnServer {
    /// Binds a UDP and a TCP port on loopback and serves both from a thread of its own until dropped.
    ///
    /// # Errors
    /// When a port can't be bound.
    pub fn start() -> io::Result<Self> {
        Self::start_impaired(Impairment::default())
    }

    /// The same, impairing the RTP it relays as `impairment` says.
    ///
    /// # Errors
    /// When a port can't be bound.
    pub fn start_impaired(impairment: Impairment) -> io::Result<Self> {
        let udp = UdpSocket::bind("127.0.0.1:0")?;
        udp.set_nonblocking(true)?;
        let tcp = TcpListener::bind("127.0.0.1:0")?;
        tcp.set_nonblocking(true)?;
        let (udp_addr, tcp_addr) = (udp.local_addr()?, tcp.local_addr()?);
        let counters = Arc::new(Counters::default());
        let stop = Arc::new(AtomicBool::new(false));
        let mut state = State {
            udp,
            tcp,
            conns: Vec::new(),
            allocations: HashMap::new(),
            key: IntegrityKey::long_term(USER, REALM, PASSWORD),
            counters: Arc::clone(&counters),
            impairment,
            dice: Dice::new(impairment.seed),
            held: HashMap::new(),
        };
        let flag = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                if !state.turn() {
                    thread::sleep(Duration::from_micros(200));
                }
            }
        });
        Ok(Self {
            udp: udp_addr,
            tcp: tcp_addr,
            counters,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for TurnServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Client {
    Udp(SocketAddr),
    Tcp(usize),
}

struct Conn {
    stream: TcpStream,
    peer: SocketAddr,
    decoder: StreamDecoder,
    out: Vec<u8>,
    alive: bool,
}

struct Allocation {
    relay: UdpSocket,
    permissions: HashSet<IpAddr>,
    channels: HashMap<u16, SocketAddr>,
}

struct State {
    udp: UdpSocket,
    tcp: TcpListener,
    conns: Vec<Conn>,
    allocations: HashMap<Client, Allocation>,
    key: IntegrityKey,
    counters: Arc<Counters>,
    impairment: Impairment,
    dice: Dice,
    /// A packet held back for reordering, per client, and since when.
    held: HashMap<Client, (Vec<u8>, Instant)>,
}

/// The impairment's choices: xorshift64*, from the seed.
struct Dice(u64);

impl Dice {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// Whether a choice of `percent` in a hundred comes up.
    fn roll(&mut self, percent: u8) -> bool {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        percent > 0 && self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % 100 < u64::from(percent)
    }
}

/// Whether a relayed datagram is RTP (version 2, a payload type outside RTCP's 64..=95 after the marker bit).
fn is_rtp(data: &[u8]) -> bool {
    data.len() >= 12 && data[0] >> 6 == 2 && !(64..=95).contains(&(data[1] & 0x7F))
}

impl State {
    /// One turn; whether anything happened.
    fn turn(&mut self) -> bool {
        let mut busy = false;
        let mut buf = vec![0; 4096];
        while let Ok((len, from)) = self.udp.recv_from(&mut buf) {
            busy = true;
            self.on_client(Client::Udp(from), from, &buf[..len]);
        }
        while let Ok((stream, peer)) = self.tcp.accept() {
            if stream
                .set_nonblocking(true)
                .and_then(|()| stream.set_nodelay(true))
                .is_err()
            {
                continue;
            }
            self.conns.push(Conn {
                stream,
                peer,
                decoder: StreamDecoder::new(Framing::Native),
                out: Vec::new(),
                alive: true,
            });
        }
        for index in 0..self.conns.len() {
            busy |= self.read_conn(index, &mut buf);
        }
        busy |= self.relay_in(&mut buf);
        for conn in &mut self.conns {
            flush(conn);
        }
        busy
    }

    fn read_conn(&mut self, index: usize, buf: &mut [u8]) -> bool {
        let conn = &mut self.conns[index];
        if !conn.alive {
            return false;
        }
        match conn.stream.read(buf) {
            Ok(0) => {
                conn.alive = false;
                false
            }
            Ok(len) => {
                conn.decoder.push(&buf[..len]);
                let peer = conn.peer;
                while let Ok(Some(message)) = self.conns[index].decoder.next_message() {
                    self.on_client(Client::Tcp(index), peer, &message);
                }
                true
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => false,
            Err(_) => {
                conn.alive = false;
                false
            }
        }
    }

    fn on_client(&mut self, client: Client, from: SocketAddr, bytes: &[u8]) {
        match classify(bytes) {
            Kind::ChannelData => {
                let Ok(data) = ChannelData::decode(bytes) else { return };
                self.counters.channel_data.fetch_add(1, Ordering::Relaxed);
                if let Some(allocation) = self.allocations.get(&client)
                    && let Some(peer) = allocation.channels.get(&data.channel)
                {
                    let _ = allocation.relay.send_to(&data.data, peer);
                }
            }
            Kind::Stun => {
                let Ok(message) = Message::decode(bytes) else { return };
                match message.class {
                    Class::Request => self.on_request(client, from, &message, bytes),
                    Class::Indication if message.method == Method::Send => self.on_send(client, &message),
                    _ => {}
                }
            }
            Kind::Other => {}
        }
    }

    fn on_send(&mut self, client: Client, message: &Message) {
        self.counters.send_indications.fetch_add(1, Ordering::Relaxed);
        let peer = message.find(|a| match a {
            Attribute::XorPeerAddress(addr) => Some(*addr),
            _ => None,
        });
        let data = message.find(|a| match a {
            Attribute::Data(data) => Some(data.clone()),
            _ => None,
        });
        if let (Some(allocation), Some(peer), Some(data)) = (self.allocations.get(&client), peer, data)
            && allocation.permissions.contains(&peer.ip())
        {
            let _ = allocation.relay.send_to(&data, peer);
        }
    }

    fn on_request(&mut self, client: Client, from: SocketAddr, message: &Message, raw: &[u8]) {
        let reply = |class| Message::new(class, message.method, message.transaction_id);
        if !message.has_integrity() || !verify_integrity(raw, &self.key) {
            let challenge = reply(Class::Error)
                .with(Attribute::ErrorCode {
                    code: 401,
                    reason: "Unauthorized".into(),
                })
                .with(Attribute::Realm(REALM.into()))
                .with(Attribute::Nonce(NONCE.into()));
            self.send(client, &challenge.encode(None, false));
            return;
        }
        let peer = message.find(|a| match a {
            Attribute::XorPeerAddress(addr) => Some(*addr),
            _ => None,
        });
        let response = match message.method {
            Method::Allocate => self.allocate(client, from, reply(Class::Success)),
            Method::Refresh => {
                let lifetime = message
                    .find(|a| match a {
                        Attribute::Lifetime(s) => Some(*s),
                        _ => None,
                    })
                    .unwrap_or(LIFETIME);
                if lifetime == 0 {
                    self.allocations.remove(&client);
                }
                Some(reply(Class::Success).with(Attribute::Lifetime(lifetime)))
            }
            Method::CreatePermission => self.allocations.get_mut(&client).zip(peer).map(|(allocation, peer)| {
                allocation.permissions.insert(peer.ip());
                reply(Class::Success)
            }),
            Method::ChannelBind => {
                let channel = message.find(|a| match a {
                    Attribute::ChannelNumber(c) => Some(*c),
                    _ => None,
                });
                self.allocations
                    .get_mut(&client)
                    .zip(peer.zip(channel))
                    .map(|(allocation, (peer, channel))| {
                        allocation.permissions.insert(peer.ip());
                        allocation.channels.insert(channel, peer);
                        reply(Class::Success)
                    })
            }
            _ => None,
        };
        let response = response.unwrap_or_else(|| {
            reply(Class::Error).with(Attribute::ErrorCode {
                code: 437,
                reason: "Allocation Mismatch".into(),
            })
        });
        let bytes = response.encode(Some(&self.key), true);
        self.send(client, &bytes);
    }

    fn allocate(&mut self, client: Client, from: SocketAddr, success: Message) -> Option<Message> {
        if self.allocations.contains_key(&client) {
            return None;
        }
        let relay = UdpSocket::bind("127.0.0.1:0").ok()?;
        relay.set_nonblocking(true).ok()?;
        let relayed = relay.local_addr().ok()?;
        self.allocations.insert(
            client,
            Allocation {
                relay,
                permissions: HashSet::new(),
                channels: HashMap::new(),
            },
        );
        self.counters.allocations.fetch_add(1, Ordering::Relaxed);
        Some(
            success
                .with(Attribute::XorRelayedAddress(relayed))
                .with(Attribute::XorMappedAddress(from))
                .with(Attribute::Lifetime(LIFETIME)),
        )
    }

    /// Data from peers to the allocations, let through only from permitted IPs.
    fn relay_in(&mut self, buf: &mut [u8]) -> bool {
        let mut out = Vec::new();
        for (client, allocation) in &self.allocations {
            while let Ok((len, peer)) = allocation.relay.recv_from(buf) {
                if !allocation.permissions.contains(&peer.ip()) {
                    continue;
                }
                let channel = allocation.channels.iter().find(|(_, p)| **p == peer).map(|(c, _)| *c);
                let bytes = match channel {
                    Some(channel) => ChannelData {
                        channel,
                        data: buf[..len].to_vec(),
                    }
                    .encode(false),
                    None => Message::new(Class::Indication, Method::Data, TransactionId([7; 12]))
                        .with(Attribute::XorPeerAddress(peer))
                        .with(Attribute::Data(buf[..len].to_vec()))
                        .encode(None, false),
                };
                out.push((*client, is_rtp(&buf[..len]), bytes));
            }
        }
        let busy = !out.is_empty();
        for (client, rtp, bytes) in out {
            if rtp {
                self.impair(client, bytes);
            } else {
                self.send(client, &bytes);
            }
        }
        self.release_held();
        busy
    }

    /// Relays one RTP packet to `client`: dropped, held back until the next one, or sent (with the one held back
    /// before it after it).
    fn impair(&mut self, client: Client, bytes: Vec<u8>) {
        self.counters.rtp_relayed.fetch_add(1, Ordering::Relaxed);
        if self.dice.roll(self.impairment.loss_percent) {
            self.counters.rtp_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if !self.held.contains_key(&client) && self.dice.roll(self.impairment.reorder_percent) {
            self.counters.rtp_reordered.fetch_add(1, Ordering::Relaxed);
            self.held.insert(client, (bytes, Instant::now()));
            return;
        }
        self.send(client, &bytes);
        if let Some((held, _)) = self.held.remove(&client) {
            self.send(client, &held);
        }
    }

    /// Sends the packets held back longer than [`HOLD_LIMIT`].
    fn release_held(&mut self) {
        let due: Vec<Client> = self
            .held
            .iter()
            .filter(|(_, (_, since))| since.elapsed() >= HOLD_LIMIT)
            .map(|(client, _)| *client)
            .collect();
        for client in due {
            if let Some((held, _)) = self.held.remove(&client) {
                self.send(client, &held);
            }
        }
    }

    fn send(&mut self, client: Client, bytes: &[u8]) {
        match client {
            Client::Udp(addr) => {
                let _ = self.udp.send_to(bytes, addr);
            }
            Client::Tcp(index) => {
                let conn = &mut self.conns[index];
                if let Ok(framed) = Framing::Native.frame(bytes) {
                    conn.out.extend(framed);
                }
                flush(conn);
            }
        }
    }
}

fn flush(conn: &mut Conn) {
    while conn.alive && !conn.out.is_empty() {
        match conn.stream.write(&conn.out) {
            Ok(n) => drop(conn.out.drain(..n)),
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(_) => conn.alive = false,
        }
    }
}
