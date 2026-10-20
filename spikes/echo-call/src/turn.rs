//! A sans-IO TURN client (RFC 8656) with long-term credentials: it takes the server's messages and `now` in, and hands
//! out the messages to send, its next deadline and [`TurnEvent`]s. [`crate::relay`] puts it on a UDP socket or a TCP
//! stream.
//!
//! - **Allocate** goes out first without credentials; the server's 401 brings the realm and nonce, and the request is
//!   sent again with `USERNAME`, `REALM`, `NONCE` and `MESSAGE-INTEGRITY`. Every later request is authenticated the
//!   same way. A 438 (stale nonce) takes the new nonce and sends the request again; a second 401 means the
//!   credentials are wrong.
//! - Responses to authenticated requests count only with a valid `MESSAGE-INTEGRITY`, except a 401 or 438 error,
//!   which a server sends without one. A response with a bad or missing one is dropped over UDP (the request is
//!   retransmitted and may still get a real answer); over TCP nothing is retransmitted, so it ends the request at once
//!   ([`TurnFailure::Unverified`]), as RFC 8489 9.2.5 asks of a reliable transport.
//! - **Failures**: only a failed Allocate or Refresh ends the allocation ([`TurnEvent::Failed`]). A refused or
//!   unanswered `CreatePermission` or `ChannelBind` is reported ([`TurnEvent::RequestRefused`]) and the allocation
//!   stays. A refused one is not asked again for that IP or peer (data goes by `Send` indication); an unanswered one
//!   is asked again on the next send, a `ChannelBind` with the same channel number, since the server may hold it. A
//!   peer keeps its channel number for the allocation's life, whatever happens to its binding. A 437 to the release
//!   means the allocation is already gone: it counts as closed.
//! - **Data**: [`TurnClient::send_to`] installs a permission for the peer's IP and binds a channel on first use, sends
//!   by `Send` indication until the channel is bound and by `ChannelData` after. Incoming `Data` indications and
//!   `ChannelData` come out as [`TurnEvent::Data`]; data from a peer without a permission never reaches us (the server
//!   drops it), and `ChannelData` on a channel no peer was ever bound to is dropped here.
//! - **Timers**: over UDP a request is retransmitted after 0.5 s, doubling, 7 sends in all (RFC 8489 6.2.1); over TCP
//!   it is sent once. The allocation is refreshed a minute before it runs out (or halfway through a shorter one),
//!   permissions after 4 of their 5 minutes, channel bindings after 9 of their 10.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use crate::stun::{
    Attribute, CHANNEL_MAX, CHANNEL_MIN, ChannelData, Class, IntegrityKey, Kind, Message, Method, StunError,
    TRANSPORT_UDP, TransactionId, classify, verify_integrity,
};

/// Teams' media relay, as the web client uses it (Task 18, `docs/protocol/calling-echo.md`): one host for every
/// account, no relay descriptor. The client lists it as `turn:<host>:3478?transport=udp`, `turn:<host>:443?transport=tcp`
/// and `turns:<host>:443`; TURN over TLS isn't implemented here ([`Transport`] has UDP and TCP).
pub const TEAMS_RELAY_HOST: &str = "gateway-world.az.relay.teams.cloud.microsoft";
/// The relay's port for TURN over UDP.
pub const TEAMS_RELAY_UDP_PORT: u16 = 3478;
/// The relay's port for TURN over TCP (and over TLS).
pub const TEAMS_RELAY_TCP_PORT: u16 = 443;

/// How long a permission lasts on the server (RFC 8656 9).
pub const PERMISSION_LIFETIME: Duration = Duration::from_secs(300);
/// How long a channel binding lasts on the server (RFC 8656 12).
pub const CHANNEL_LIFETIME: Duration = Duration::from_secs(600);
const PERMISSION_REFRESH: Duration = Duration::from_secs(240);
const CHANNEL_REFRESH: Duration = Duration::from_secs(540);
const ALLOCATION_MARGIN: Duration = Duration::from_secs(60);
/// 401/438 answers taken for one request before giving up.
const AUTH_RETRIES: u8 = 3;

/// The client's transport to the TURN server. The relayed side is always UDP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Udp,
    Tcp,
}

/// Long-term TURN credentials, as `GET https://teams.microsoft.com/trap/tokens` hands them out
/// (`crate::signaling::TrapTokens`; 7 days, realm `rtcmedia`). The realm isn't part of them: the client takes the one
/// the server names in its 401. `Debug` shows no password.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// Retransmission settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// First retransmission timeout over UDP.
    pub rto: Duration,
    /// Sends of one request over UDP, the first included.
    pub max_sends: u32,
    /// How long a request waits for its answer after its last send (UDP: a multiple of the RTO; TCP: the whole wait).
    pub last_wait: Duration,
}

impl Timing {
    /// RFC 8489 6.2.1 over UDP: RTO 500 ms, Rc = 7, Rm = 16 (39.5 s in all).
    #[must_use]
    pub fn udp() -> Self {
        Self {
            rto: Duration::from_millis(500),
            max_sends: 7,
            last_wait: Duration::from_secs(8),
        }
    }

    /// RFC 8489 6.2.2 over TCP: one send, 39.5 s to answer.
    #[must_use]
    pub fn tcp() -> Self {
        Self {
            rto: Duration::from_millis(39_500),
            max_sends: 1,
            last_wait: Duration::from_millis(39_500),
        }
    }
}

/// Why the allocation failed or was lost. Final: the client does nothing more.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TurnFailure {
    #[error("the TURN server refused the credentials")]
    Unauthorized,
    #[error("the TURN server answered {code} {reason}")]
    Rejected { code: u16, reason: String },
    #[error("the TURN server asks to use {0} instead (300 Try Alternate)")]
    TryAlternate(SocketAddr),
    #[error("the TURN server did not answer the {0:?} request")]
    Timeout(Method),
    #[error("the TURN server's success answer lacks XOR-RELAYED-ADDRESS")]
    NoRelayedAddress,
    /// Over TCP, the answer to an authenticated request had a bad or missing `MESSAGE-INTEGRITY`; `code` is the error
    /// code it claimed, unverified.
    #[error(
        "the TURN server's answer to the {method:?} request has no valid MESSAGE-INTEGRITY (claimed code {code:?})"
    )]
    Unverified { method: Method, code: Option<u16> },
}

/// What the client reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnEvent {
    /// The allocation exists: peers reach us at `relayed`; the server saw us at `mapped`.
    Allocated {
        relayed: SocketAddr,
        mapped: Option<SocketAddr>,
        lifetime: Duration,
    },
    /// A refresh kept the allocation for `lifetime` more.
    Refreshed { lifetime: Duration },
    /// Data from `peer`'s IP is let through now.
    PermissionCreated(IpAddr),
    /// `peer` is bound to `channel`: data goes as `ChannelData` from now on.
    ChannelBound { peer: SocketAddr, channel: u16 },
    /// A permission or channel request was refused or went unanswered (not final: data still goes by indication where
    /// it can). `code` is the server's error code, 0 when no trusted answer came.
    RequestRefused { method: Method, code: u16 },
    /// Data relayed from `peer`.
    Data { peer: SocketAddr, payload: Vec<u8> },
    /// The allocation failed or was lost.
    Failed(TurnFailure),
    /// The allocation was released ([`TurnClient::close`]).
    Closed,
}

/// Misuse of the client.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TurnError {
    #[error("no allocation yet")]
    NotAllocated,
    #[error("the allocation has failed or closed")]
    Finished,
    #[error("no free channel number")]
    NoChannel,
    #[error("the system random source failed")]
    Random,
    #[error("a malformed message from the server: {0}")]
    Malformed(#[from] StunError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Allocating,
    Allocated { relayed: SocketAddr, refresh_at: Instant },
    Closing,
    Done,
}

/// A request the client may have to send again (with a new nonce, or as a refresh).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Request {
    Allocate,
    Refresh { lifetime: Option<u32> },
    CreatePermission(IpAddr),
    ChannelBind { peer: SocketAddr, channel: u16 },
}

impl Request {
    fn method(&self) -> Method {
        match self {
            Self::Allocate => Method::Allocate,
            Self::Refresh { .. } => Method::Refresh,
            Self::CreatePermission(_) => Method::CreatePermission,
            Self::ChannelBind { .. } => Method::ChannelBind,
        }
    }
}

#[derive(Debug)]
struct Transaction {
    request: Request,
    bytes: Vec<u8>,
    authenticated: bool,
    auth_retries: u8,
    sends: u32,
    rto: Duration,
    deadline: Instant,
}

/// The realm, nonce and key once the server's first 401 gave them.
#[derive(Debug)]
struct Auth {
    realm: String,
    nonce: String,
    key: IntegrityKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Permission {
    /// The first `CreatePermission` is out.
    Pending,
    Installed {
        refresh_at: Instant,
    },
    /// The server refused it: not asked again in this allocation.
    Refused,
}

/// A peer's channel. The number stays the peer's for the allocation's life, whatever happens to the binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Binding {
    Pending(u16),
    Bound {
        channel: u16,
        refresh_at: Instant,
    },
    /// The last `ChannelBind` went unanswered: the server may hold the binding, so the next send asks for the same
    /// number again.
    Unconfirmed(u16),
    /// The server refused it: data goes by indication, and the channel isn't asked again in this allocation.
    Refused(u16),
}

/// A sans-IO TURN client for one allocation.
#[derive(Debug)]
pub struct TurnClient {
    transport: Transport,
    timing: Timing,
    credentials: Credentials,
    software: Option<String>,
    auth: Option<Auth>,
    state: State,
    transactions: HashMap<TransactionId, Transaction>,
    permissions: HashMap<IpAddr, Permission>,
    bindings: HashMap<SocketAddr, Binding>,
    peers_by_channel: HashMap<u16, SocketAddr>,
    next_channel: u16,
    outgoing: VecDeque<Vec<u8>>,
    events: VecDeque<TurnEvent>,
}

impl TurnClient {
    /// A client that will allocate over `transport` with `credentials`.
    #[must_use]
    pub fn new(transport: Transport, credentials: Credentials) -> Self {
        Self {
            transport,
            timing: match transport {
                Transport::Udp => Timing::udp(),
                Transport::Tcp => Timing::tcp(),
            },
            credentials,
            software: None,
            auth: None,
            state: State::Idle,
            transactions: HashMap::new(),
            permissions: HashMap::new(),
            bindings: HashMap::new(),
            peers_by_channel: HashMap::new(),
            next_channel: CHANNEL_MIN,
            outgoing: VecDeque::new(),
            events: VecDeque::new(),
        }
    }

    /// Other retransmission settings (tests, a quicker fallback).
    #[must_use]
    pub fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = timing;
        self
    }

    /// A `SOFTWARE` attribute on every request.
    #[must_use]
    pub fn with_software(mut self, software: impl Into<String>) -> Self {
        self.software = Some(software.into());
        self
    }

    #[must_use]
    pub fn transport(&self) -> Transport {
        self.transport
    }

    /// The relayed address, once allocated.
    #[must_use]
    pub fn relayed(&self) -> Option<SocketAddr> {
        match self.state {
            State::Allocated { relayed, .. } => Some(relayed),
            _ => None,
        }
    }

    /// Whether the client has failed or closed for good.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    /// Starts the allocation (unauthenticated, to learn the realm and nonce).
    ///
    /// # Errors
    /// [`TurnError::Finished`] when the client already allocated or finished, [`TurnError::Random`] when no
    /// transaction id can be made.
    pub fn allocate(&mut self, now: Instant) -> Result<(), TurnError> {
        if self.state != State::Idle {
            return Err(TurnError::Finished);
        }
        self.state = State::Allocating;
        self.start(now, Request::Allocate, 0)
    }

    /// Releases the allocation (a Refresh with lifetime 0); [`TurnEvent::Closed`] follows its answer.
    ///
    /// # Errors
    /// [`TurnError::NotAllocated`] before the allocation exists.
    pub fn close(&mut self, now: Instant) -> Result<(), TurnError> {
        if self.relayed().is_none() {
            return Err(TurnError::NotAllocated);
        }
        self.state = State::Closing;
        self.start(now, Request::Refresh { lifetime: Some(0) }, 0)
    }

    /// Lets data from `ip` through (`CreatePermission`), unless a permission exists, is pending, or was refused.
    ///
    /// # Errors
    /// [`TurnError::NotAllocated`] before the allocation exists.
    pub fn permit(&mut self, now: Instant, ip: IpAddr) -> Result<(), TurnError> {
        self.relayed().ok_or(TurnError::NotAllocated)?;
        if self.permissions.contains_key(&ip) {
            return Ok(());
        }
        self.permissions.insert(ip, Permission::Pending);
        self.start(now, Request::CreatePermission(ip), 0)
    }

    /// Relays `payload` to `peer`: by `ChannelData` once its channel is bound, else by a `Send` indication. The first
    /// send to a peer also asks for its permission and binds it a channel.
    ///
    /// # Errors
    /// [`TurnError::NotAllocated`] before the allocation exists, [`TurnError::NoChannel`] when every channel is in use.
    pub fn send_to(&mut self, now: Instant, peer: SocketAddr, payload: &[u8]) -> Result<(), TurnError> {
        self.relayed().ok_or(TurnError::NotAllocated)?;
        self.permit(now, peer.ip())?;
        match self.bindings.get(&peer).copied() {
            Some(Binding::Bound { channel, .. }) => {
                let message = ChannelData {
                    channel,
                    data: payload.to_vec(),
                };
                self.outgoing.push_back(message.encode(false));
                return Ok(());
            }
            Some(Binding::Pending(_) | Binding::Refused(_)) => {}
            Some(Binding::Unconfirmed(channel)) => self.bind_channel(now, peer, channel)?,
            None => self.bind(now, peer)?,
        }
        let indication = Message::new(Class::Indication, Method::Send, Self::transaction_id()?)
            .with(Attribute::XorPeerAddress(peer))
            .with(Attribute::Data(payload.to_vec()));
        self.outgoing.push_back(indication.encode(None, false));
        Ok(())
    }

    /// Feeds one message from the server (a datagram over UDP, a deframed message over TCP).
    ///
    /// # Errors
    /// [`TurnError::Malformed`] when the bytes are neither STUN nor `ChannelData`. Other stray input is dropped.
    pub fn handle_input(&mut self, now: Instant, bytes: &[u8]) -> Result<(), TurnError> {
        match classify(bytes) {
            Kind::ChannelData => {
                let message = ChannelData::decode(bytes)?;
                if let Some(peer) = self.peers_by_channel.get(&message.channel) {
                    self.events.push_back(TurnEvent::Data {
                        peer: *peer,
                        payload: message.data,
                    });
                }
                Ok(())
            }
            Kind::Stun => {
                let message = Message::decode(bytes)?;
                self.on_message(now, &message, bytes);
                Ok(())
            }
            Kind::Other => Err(TurnError::Malformed(StunError::NotStun)),
        }
    }

    /// Retransmits, times out, and refreshes what is due at `now`.
    ///
    /// # Errors
    /// [`TurnError::Random`] when no transaction id can be made.
    pub fn handle_timeout(&mut self, now: Instant) -> Result<(), TurnError> {
        self.retransmit(now);
        if self.is_done() {
            return Ok(());
        }
        self.refresh_due(now)
    }

    /// When the client next wants [`TurnClient::handle_timeout`].
    #[must_use]
    pub fn poll_timeout(&self) -> Option<Instant> {
        let transactions = self.transactions.values().map(|t| t.deadline);
        let allocation = match self.state {
            State::Allocated { refresh_at, .. } => Some(refresh_at),
            _ => None,
        };
        let permissions = self.permissions.values().filter_map(|permission| match permission {
            Permission::Installed { refresh_at } => Some(*refresh_at),
            Permission::Pending | Permission::Refused => None,
        });
        let bindings = self.bindings.values().filter_map(|binding| match binding {
            Binding::Bound { refresh_at, .. } => Some(*refresh_at),
            Binding::Pending(_) | Binding::Unconfirmed(_) | Binding::Refused(_) => None,
        });
        transactions.chain(allocation).chain(permissions).chain(bindings).min()
    }

    /// The next message to send to the server (unframed).
    pub fn poll_transmit(&mut self) -> Option<Vec<u8>> {
        self.outgoing.pop_front()
    }

    /// The next event.
    pub fn poll_event(&mut self) -> Option<TurnEvent> {
        self.events.pop_front()
    }

    fn transaction_id() -> Result<TransactionId, TurnError> {
        TransactionId::random().map_err(|_| TurnError::Random)
    }

    /// Binds `peer` a new channel number.
    fn bind(&mut self, now: Instant, peer: SocketAddr) -> Result<(), TurnError> {
        let channel = self.next_channel;
        if channel > CHANNEL_MAX {
            return Err(TurnError::NoChannel);
        }
        self.next_channel += 1;
        self.bind_channel(now, peer, channel)
    }

    fn bind_channel(&mut self, now: Instant, peer: SocketAddr, channel: u16) -> Result<(), TurnError> {
        self.bindings.insert(peer, Binding::Pending(channel));
        self.start(now, Request::ChannelBind { peer, channel }, 0)
    }

    /// Sends `request` as a new transaction.
    fn start(&mut self, now: Instant, request: Request, auth_retries: u8) -> Result<(), TurnError> {
        let id = Self::transaction_id()?;
        let message = self.build(&request, id);
        let authenticated = self.auth.is_some();
        let bytes = message.encode(self.auth.as_ref().map(|a| &a.key), false);
        self.outgoing.push_back(bytes.clone());
        self.transactions.insert(
            id,
            Transaction {
                request,
                bytes,
                authenticated,
                auth_retries,
                sends: 1,
                rto: self.timing.rto,
                deadline: now + self.first_wait(),
            },
        );
        Ok(())
    }

    fn first_wait(&self) -> Duration {
        if self.timing.max_sends <= 1 {
            self.timing.last_wait
        } else {
            self.timing.rto
        }
    }

    fn build(&self, request: &Request, id: TransactionId) -> Message {
        let mut message = Message::new(Class::Request, request.method(), id);
        match request {
            Request::Allocate => message = message.with(Attribute::RequestedTransport(TRANSPORT_UDP)),
            Request::Refresh { lifetime } => {
                if let Some(seconds) = lifetime {
                    message = message.with(Attribute::Lifetime(*seconds));
                }
            }
            Request::CreatePermission(ip) => message = message.with(Attribute::XorPeerAddress(SocketAddr::new(*ip, 0))),
            Request::ChannelBind { peer, channel } => {
                message = message
                    .with(Attribute::ChannelNumber(*channel))
                    .with(Attribute::XorPeerAddress(*peer));
            }
        }
        if let Some(software) = &self.software {
            message = message.with(Attribute::Software(software.clone()));
        }
        if let Some(auth) = &self.auth {
            message = message
                .with(Attribute::Username(self.credentials.username.clone()))
                .with(Attribute::Realm(auth.realm.clone()))
                .with(Attribute::Nonce(auth.nonce.clone()));
        }
        message
    }

    fn on_message(&mut self, now: Instant, message: &Message, raw: &[u8]) {
        match message.class {
            Class::Indication => self.on_indication(message),
            Class::Success | Class::Error => self.on_response(now, message, raw),
            Class::Request => {}
        }
    }

    fn on_indication(&mut self, message: &Message) {
        if message.method != Method::Data {
            return;
        }
        let peer = message.find(|a| match a {
            Attribute::XorPeerAddress(addr) => Some(*addr),
            _ => None,
        });
        let data = message.find(|a| match a {
            Attribute::Data(data) => Some(data.clone()),
            _ => None,
        });
        if let (Some(peer), Some(payload)) = (peer, data) {
            self.events.push_back(TurnEvent::Data { peer, payload });
        }
    }

    fn on_response(&mut self, now: Instant, message: &Message, raw: &[u8]) {
        let Some(transaction) = self.transactions.get(&message.transaction_id) else {
            return;
        };
        let trusted = self.trusted(transaction, message, raw);
        if !trusted && self.transport == Transport::Udp {
            // a retransmission may still bring the real answer
            return;
        }
        let Some(transaction) = self.transactions.remove(&message.transaction_id) else {
            return;
        };
        if !trusted {
            // over TCP nothing is retransmitted: the request ends here
            let method = transaction.request.method();
            let code = message.error_code().map(|(code, _)| code);
            self.on_request_failed(&transaction.request, TurnFailure::Unverified { method, code }, 0);
            return;
        }
        if message.class == Class::Success {
            self.on_success(now, &transaction.request, message);
        } else {
            self.on_error(now, &transaction, message);
        }
    }

    /// Whether a response may be acted on: a request sent with integrity needs it back, except for a 401 or 438 error.
    fn trusted(&self, transaction: &Transaction, message: &Message, raw: &[u8]) -> bool {
        if !transaction.authenticated {
            return true;
        }
        let auth_challenge = message.class == Class::Error && matches!(message.error_code(), Some((401 | 438, _)));
        match &self.auth {
            Some(auth) if message.has_integrity() => verify_integrity(raw, &auth.key),
            _ => auth_challenge,
        }
    }

    fn on_success(&mut self, now: Instant, request: &Request, message: &Message) {
        let lifetime = message.find(|a| match a {
            Attribute::Lifetime(seconds) => Some(Duration::from_secs(u64::from(*seconds))),
            _ => None,
        });
        match request {
            Request::Allocate => self.on_allocated(now, message, lifetime.unwrap_or(Duration::from_secs(600))),
            Request::Refresh { lifetime: Some(0) } => self.closed(),
            Request::Refresh { .. } => {
                let lifetime = lifetime.unwrap_or(Duration::from_secs(600));
                if let State::Allocated { relayed, .. } = self.state {
                    self.state = State::Allocated {
                        relayed,
                        refresh_at: now + refresh_after(lifetime),
                    };
                }
                self.events.push_back(TurnEvent::Refreshed { lifetime });
            }
            Request::CreatePermission(ip) => {
                let installed = Permission::Installed {
                    refresh_at: now + PERMISSION_REFRESH,
                };
                let first = self.permissions.insert(*ip, installed) == Some(Permission::Pending);
                if first {
                    self.events.push_back(TurnEvent::PermissionCreated(*ip));
                }
            }
            Request::ChannelBind { peer, channel } => self.on_bound(now, *peer, *channel),
        }
    }

    fn on_allocated(&mut self, now: Instant, message: &Message, lifetime: Duration) {
        let relayed = message.find(|a| match a {
            Attribute::XorRelayedAddress(addr) => Some(*addr),
            _ => None,
        });
        let Some(relayed) = relayed else {
            self.fail(TurnFailure::NoRelayedAddress);
            return;
        };
        let mapped = message.find(|a| match a {
            Attribute::XorMappedAddress(addr) => Some(*addr),
            _ => None,
        });
        self.state = State::Allocated {
            relayed,
            refresh_at: now + refresh_after(lifetime),
        };
        self.events.push_back(TurnEvent::Allocated {
            relayed,
            mapped,
            lifetime,
        });
    }

    fn on_bound(&mut self, now: Instant, peer: SocketAddr, channel: u16) {
        let first = matches!(self.bindings.get(&peer), Some(Binding::Pending(_)));
        self.bindings.insert(
            peer,
            Binding::Bound {
                channel,
                refresh_at: now + CHANNEL_REFRESH,
            },
        );
        self.peers_by_channel.insert(channel, peer);
        if first {
            self.events.push_back(TurnEvent::ChannelBound { peer, channel });
        }
    }

    fn on_error(&mut self, now: Instant, transaction: &Transaction, message: &Message) {
        let (code, reason) = message.error_code().unwrap_or((0, ""));
        let challenge = code == 401 && !transaction.authenticated || code == 438;
        if challenge && transaction.auth_retries < AUTH_RETRIES && self.take_challenge(message) {
            let method = transaction.request.method();
            if self
                .start(now, transaction.request.clone(), transaction.auth_retries + 1)
                .is_err()
            {
                self.on_request_failed(&transaction.request, TurnFailure::Timeout(method), 0);
            }
            return;
        }
        if code == 437 && transaction.request == (Request::Refresh { lifetime: Some(0) }) {
            // Allocation Mismatch to the release: it is gone already (RFC 8656 7.3), say a retransmission after the
            // first one's answer was lost
            self.closed();
            return;
        }
        let failure = match code {
            401 | 438 => TurnFailure::Unauthorized,
            300 => match message.find(|a| match a {
                Attribute::AlternateServer(addr) => Some(*addr),
                _ => None,
            }) {
                Some(addr) => TurnFailure::TryAlternate(addr),
                None => TurnFailure::Rejected {
                    code,
                    reason: reason.to_owned(),
                },
            },
            _ => TurnFailure::Rejected {
                code,
                reason: reason.to_owned(),
            },
        };
        self.on_request_failed(&transaction.request, failure, code);
    }

    /// Takes the realm and nonce of a 401/438; false when either is missing.
    fn take_challenge(&mut self, message: &Message) -> bool {
        let realm = message
            .find(|a| match a {
                Attribute::Realm(realm) => Some(realm.clone()),
                _ => None,
            })
            .or_else(|| self.auth.as_ref().map(|a| a.realm.clone()));
        let nonce = message.find(|a| match a {
            Attribute::Nonce(nonce) => Some(nonce.clone()),
            _ => None,
        });
        let (Some(realm), Some(nonce)) = (realm, nonce) else {
            return false;
        };
        let key = IntegrityKey::long_term(&self.credentials.username, &realm, &self.credentials.password);
        self.auth = Some(Auth { realm, nonce, key });
        true
    }

    /// A request that won't succeed: the allocation's own requests fail the client, the others are reported. An
    /// unanswered permission or channel is asked again on the next send (the channel under the same number); a refused
    /// one is not asked again.
    fn on_request_failed(&mut self, request: &Request, failure: TurnFailure, code: u16) {
        let unanswered = matches!(failure, TurnFailure::Timeout(_));
        match request {
            Request::Allocate | Request::Refresh { .. } => self.fail(failure),
            Request::CreatePermission(ip) => {
                if unanswered {
                    self.permissions.remove(ip);
                } else {
                    self.permissions.insert(*ip, Permission::Refused);
                }
                self.events.push_back(TurnEvent::RequestRefused {
                    method: Method::CreatePermission,
                    code,
                });
            }
            Request::ChannelBind { peer, channel } => {
                let binding = if unanswered {
                    Binding::Unconfirmed(*channel)
                } else {
                    Binding::Refused(*channel)
                };
                self.bindings.insert(*peer, binding);
                self.events.push_back(TurnEvent::RequestRefused {
                    method: Method::ChannelBind,
                    code,
                });
            }
        }
    }

    fn closed(&mut self) {
        self.state = State::Done;
        self.transactions.clear();
        self.events.push_back(TurnEvent::Closed);
    }

    fn fail(&mut self, failure: TurnFailure) {
        if self.state == State::Done {
            return;
        }
        self.state = State::Done;
        self.transactions.clear();
        self.events.push_back(TurnEvent::Failed(failure));
    }

    fn retransmit(&mut self, now: Instant) {
        let due: Vec<TransactionId> = self
            .transactions
            .iter()
            .filter(|(_, t)| t.deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            let Some(transaction) = self.transactions.get_mut(&id) else {
                continue;
            };
            if transaction.sends < self.timing.max_sends {
                transaction.sends += 1;
                transaction.rto *= 2;
                transaction.deadline = now
                    + if transaction.sends == self.timing.max_sends {
                        self.timing.last_wait
                    } else {
                        transaction.rto
                    };
                self.outgoing.push_back(transaction.bytes.clone());
            } else if let Some(transaction) = self.transactions.remove(&id) {
                let method = transaction.request.method();
                self.on_request_failed(&transaction.request, TurnFailure::Timeout(method), 0);
            }
        }
    }

    fn refresh_due(&mut self, now: Instant) -> Result<(), TurnError> {
        if let State::Allocated { relayed, refresh_at } = self.state
            && refresh_at <= now
            && !self.pending(|r| matches!(r, Request::Refresh { .. }))
        {
            // the next refresh time is set by the answer; until then, don't ask again
            self.state = State::Allocated {
                relayed,
                refresh_at: now + self.timing.last_wait + self.timing.rto,
            };
            self.start(now, Request::Refresh { lifetime: None }, 0)?;
        }
        let permissions: Vec<IpAddr> = self
            .permissions
            .iter()
            .filter(|(_, permission)| matches!(permission, Permission::Installed { refresh_at } if *refresh_at <= now))
            .map(|(ip, _)| *ip)
            .collect();
        for ip in permissions {
            let installed = Permission::Installed {
                refresh_at: now + PERMISSION_REFRESH,
            };
            self.permissions.insert(ip, installed);
            self.start(now, Request::CreatePermission(ip), 0)?;
        }
        let bindings: Vec<(SocketAddr, u16)> = self
            .bindings
            .iter()
            .filter_map(|(peer, binding)| match binding {
                Binding::Bound { channel, refresh_at } if *refresh_at <= now => Some((*peer, *channel)),
                _ => None,
            })
            .collect();
        for (peer, channel) in bindings {
            self.bindings.insert(
                peer,
                Binding::Bound {
                    channel,
                    refresh_at: now + CHANNEL_REFRESH,
                },
            );
            self.start(now, Request::ChannelBind { peer, channel }, 0)?;
        }
        Ok(())
    }

    fn pending(&self, matches: impl Fn(&Request) -> bool) -> bool {
        self.transactions.values().any(|t| matches(&t.request))
    }
}

/// When to refresh an allocation granted for `lifetime`: a minute early, or halfway through a short one.
fn refresh_after(lifetime: Duration) -> Duration {
    lifetime.saturating_sub(ALLOCATION_MARGIN.min(lifetime / 2))
}
