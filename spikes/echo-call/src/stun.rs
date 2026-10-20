//! STUN messages (RFC 8489) with the TURN methods and attributes (RFC 8656), TURN `ChannelData`, and the framing of
//! both on a TCP stream.
//!
//! - [`Message::encode`] writes the attributes in order, then `MESSAGE-INTEGRITY` (HMAC-SHA1) when given a key, then
//!   `FINGERPRINT` when asked; [`Message::decode`] reads them back, checks a `FINGERPRINT` if there is one, and ignores
//!   every attribute after `MESSAGE-INTEGRITY` but the fingerprint, as RFC 8489 14.5 says.
//!   [`verify_integrity`] checks the `MESSAGE-INTEGRITY` of a raw message against a key.
//! - [`IntegrityKey::long_term`] is the long-term credential key, `MD5(username ":" realm ":" password)` (RFC 8489
//!   9.2.2). Credentials are taken as given: no `OpaqueString` preparation, so non-ASCII passwords are not supported.
//! - [`Framing`] and [`StreamDecoder`] carry messages on TCP: as TURN itself does (each message is self-delimiting,
//!   `ChannelData` padded to 4 bytes, RFC 8656 12.5), or with the 2-byte length prefix of RFC 4571.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use hmac::{Hmac, KeyInit as _, Mac as _};
use md5::{Digest as _, Md5};
use sha1::Sha1;

/// The fixed value in every STUN header (RFC 8489 5).
pub const MAGIC_COOKIE: u32 = 0x2112_A442;
/// Size of the STUN header.
pub const HEADER_LEN: usize = 20;
/// Size of the `ChannelData` header.
pub const CHANNEL_HEADER_LEN: usize = 4;
/// The lowest and highest channel numbers a client may bind (RFC 8656 12).
pub const CHANNEL_MIN: u16 = 0x4000;
pub const CHANNEL_MAX: u16 = 0x4FFF;
/// The protocol number `REQUESTED-TRANSPORT` carries for UDP.
pub const TRANSPORT_UDP: u8 = 17;

const FINGERPRINT_XOR: u32 = 0x5354_554E;
const INTEGRITY_LEN: usize = 20;
const ATTR_HEADER_LEN: usize = 4;

// attribute types
const MAPPED_ADDRESS: u16 = 0x0001;
const USERNAME: u16 = 0x0006;
const MESSAGE_INTEGRITY: u16 = 0x0008;
const ERROR_CODE: u16 = 0x0009;
const UNKNOWN_ATTRIBUTES: u16 = 0x000A;
const CHANNEL_NUMBER: u16 = 0x000C;
const LIFETIME: u16 = 0x000D;
const XOR_PEER_ADDRESS: u16 = 0x0012;
const DATA: u16 = 0x0013;
const REALM: u16 = 0x0014;
const NONCE: u16 = 0x0015;
const XOR_RELAYED_ADDRESS: u16 = 0x0016;
const REQUESTED_TRANSPORT: u16 = 0x0019;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;
const SOFTWARE: u16 = 0x8022;
const ALTERNATE_SERVER: u16 = 0x8023;
const FINGERPRINT: u16 = 0x8028;

/// Why bytes are not a valid STUN message, `ChannelData` or framed stream.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StunError {
    #[error("too short for a STUN message or ChannelData")]
    Truncated,
    #[error("not a STUN message (first bits or magic cookie)")]
    NotStun,
    #[error("the STUN length field disagrees with the message")]
    BadLength,
    #[error("a malformed {0:#06x} attribute")]
    BadAttribute(u16),
    #[error("the FINGERPRINT does not match")]
    BadFingerprint,
    #[error("not ChannelData")]
    NotChannelData,
    #[error("a ChannelData channel outside 0x4000-0x4FFF: {0:#06x}")]
    BadChannel(u16),
    #[error("the TCP stream carries neither STUN nor ChannelData")]
    Desync,
    #[error("a message too large to frame")]
    TooLarge,
}

/// A STUN transaction id: 96 random bits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransactionId(pub [u8; 12]);

impl TransactionId {
    /// A fresh id from the system's random source.
    ///
    /// # Errors
    /// When the random source fails.
    pub fn random() -> Result<Self, getrandom::Error> {
        let mut id = [0; 12];
        getrandom::fill(&mut id)?;
        Ok(Self(id))
    }
}

impl fmt::Debug for TransactionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// The class of a STUN message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Request,
    Indication,
    Success,
    Error,
}

impl Class {
    fn bits(self) -> u16 {
        match self {
            Self::Request => 0b00,
            Self::Indication => 0b01,
            Self::Success => 0b10,
            Self::Error => 0b11,
        }
    }

    fn from_bits(bits: u16) -> Self {
        match bits & 0b11 {
            0b00 => Self::Request,
            0b01 => Self::Indication,
            0b10 => Self::Success,
            _ => Self::Error,
        }
    }
}

/// A STUN method: Binding and the TURN methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Binding,
    Allocate,
    Refresh,
    Send,
    Data,
    CreatePermission,
    ChannelBind,
    Other(u16),
}

impl Method {
    #[must_use]
    pub fn code(self) -> u16 {
        match self {
            Self::Binding => 0x001,
            Self::Allocate => 0x003,
            Self::Refresh => 0x004,
            Self::Send => 0x006,
            Self::Data => 0x007,
            Self::CreatePermission => 0x008,
            Self::ChannelBind => 0x009,
            Self::Other(code) => code & 0x0FFF,
        }
    }

    fn from_code(code: u16) -> Self {
        match code {
            0x001 => Self::Binding,
            0x003 => Self::Allocate,
            0x004 => Self::Refresh,
            0x006 => Self::Send,
            0x007 => Self::Data,
            0x008 => Self::CreatePermission,
            0x009 => Self::ChannelBind,
            other => Self::Other(other),
        }
    }
}

/// The 14-bit message type: the method's bits with the class's two bits interleaved (RFC 8489 5).
fn message_type(class: Class, method: Method) -> u16 {
    let m = method.code();
    let c = class.bits();
    (m & 0x000F) | ((m & 0x0070) << 1) | ((m & 0x0F80) << 2) | ((c & 0b01) << 4) | ((c & 0b10) << 7)
}

fn split_type(kind: u16) -> (Class, Method) {
    let class = ((kind >> 4) & 0b01) | ((kind >> 7) & 0b10);
    let method = (kind & 0x000F) | ((kind >> 1) & 0x0070) | ((kind >> 2) & 0x0F80);
    (Class::from_bits(class), Method::from_code(method))
}

/// One STUN attribute.
#[derive(Clone, PartialEq, Eq)]
pub enum Attribute {
    MappedAddress(SocketAddr),
    XorMappedAddress(SocketAddr),
    XorRelayedAddress(SocketAddr),
    XorPeerAddress(SocketAddr),
    AlternateServer(SocketAddr),
    Username(String),
    Realm(String),
    Nonce(String),
    Software(String),
    ErrorCode {
        code: u16,
        reason: String,
    },
    UnknownAttributes(Vec<u16>),
    /// Seconds.
    Lifetime(u32),
    /// An IP protocol number ([`TRANSPORT_UDP`]).
    RequestedTransport(u8),
    ChannelNumber(u16),
    Data(Vec<u8>),
    /// Only ever read; [`Message::encode`] computes it.
    MessageIntegrity([u8; INTEGRITY_LEN]),
    /// Only ever read; [`Message::encode`] computes it.
    Fingerprint(u32),
    /// Anything else, kept as it came.
    Other {
        kind: u16,
        value: Vec<u8>,
    },
}

impl fmt::Debug for Attribute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // relayed payloads are media and STUN checks: their size is what matters in a log
            Self::Data(data) => write!(f, "Data({} bytes)", data.len()),
            Self::MessageIntegrity(_) => f.write_str("MessageIntegrity(..)"),
            Self::MappedAddress(a) => write!(f, "MappedAddress({a})"),
            Self::XorMappedAddress(a) => write!(f, "XorMappedAddress({a})"),
            Self::XorRelayedAddress(a) => write!(f, "XorRelayedAddress({a})"),
            Self::XorPeerAddress(a) => write!(f, "XorPeerAddress({a})"),
            Self::AlternateServer(a) => write!(f, "AlternateServer({a})"),
            Self::Username(u) => write!(f, "Username({u:?})"),
            Self::Realm(r) => write!(f, "Realm({r:?})"),
            Self::Nonce(n) => write!(f, "Nonce({} chars)", n.len()),
            Self::Software(s) => write!(f, "Software({s:?})"),
            Self::ErrorCode { code, reason } => write!(f, "ErrorCode({code} {reason:?})"),
            Self::UnknownAttributes(kinds) => write!(f, "UnknownAttributes({kinds:x?})"),
            Self::Lifetime(s) => write!(f, "Lifetime({s})"),
            Self::RequestedTransport(p) => write!(f, "RequestedTransport({p})"),
            Self::ChannelNumber(c) => write!(f, "ChannelNumber({c:#06x})"),
            Self::Fingerprint(v) => write!(f, "Fingerprint({v:#010x})"),
            Self::Other { kind, value } => write!(f, "Other({kind:#06x}, {} bytes)", value.len()),
        }
    }
}

/// A STUN message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub class: Class,
    pub method: Method,
    pub transaction_id: TransactionId,
    pub attributes: Vec<Attribute>,
}

/// The key `MESSAGE-INTEGRITY` is computed with. `Debug` shows no key.
#[derive(Clone, PartialEq, Eq)]
pub struct IntegrityKey(Vec<u8>);

impl IntegrityKey {
    /// The long-term credential key: `MD5(username ":" realm ":" password)`.
    #[must_use]
    pub fn long_term(username: &str, realm: &str, password: &str) -> Self {
        let mut md5 = Md5::new();
        md5.update(format!("{username}:{realm}:{password}").as_bytes());
        Self(md5.finalize().to_vec())
    }

    /// The short-term credential key: the password itself (ICE uses these).
    #[must_use]
    pub fn short_term(password: &str) -> Self {
        Self(password.as_bytes().to_vec())
    }

    fn hmac(&self, parts: &[&[u8]]) -> [u8; INTEGRITY_LEN] {
        let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(&self.0) else {
            // HMAC takes a key of any length; this arm can't be reached
            return [0; INTEGRITY_LEN];
        };
        for part in parts {
            mac.update(part);
        }
        mac.finalize().into_bytes().into()
    }
}

impl fmt::Debug for IntegrityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IntegrityKey(..)")
    }
}

impl Message {
    #[must_use]
    pub fn new(class: Class, method: Method, transaction_id: TransactionId) -> Self {
        Self {
            class,
            method,
            transaction_id,
            attributes: Vec::new(),
        }
    }

    /// Adds an attribute.
    #[must_use]
    pub fn with(mut self, attribute: Attribute) -> Self {
        self.attributes.push(attribute);
        self
    }

    /// The first `ERROR-CODE`, as (code, reason).
    #[must_use]
    pub fn error_code(&self) -> Option<(u16, &str)> {
        self.attributes.iter().find_map(|a| match a {
            Attribute::ErrorCode { code, reason } => Some((*code, reason.as_str())),
            _ => None,
        })
    }

    /// The first attribute `pick` maps to something.
    pub fn find<'a, T>(&'a self, pick: impl FnMut(&'a Attribute) -> Option<T>) -> Option<T> {
        self.attributes.iter().find_map(pick)
    }

    /// Whether the message carries `MESSAGE-INTEGRITY`.
    #[must_use]
    pub fn has_integrity(&self) -> bool {
        self.attributes
            .iter()
            .any(|a| matches!(a, Attribute::MessageIntegrity(_)))
    }

    /// The message on the wire: its attributes, then `MESSAGE-INTEGRITY` when `key` is given, then `FINGERPRINT` when
    /// `fingerprint` is set. `MessageIntegrity` and `Fingerprint` entries in [`Message::attributes`] are skipped.
    #[must_use]
    pub fn encode(&self, key: Option<&IntegrityKey>, fingerprint: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        out.extend_from_slice(&message_type(self.class, self.method).to_be_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        out.extend_from_slice(&self.transaction_id.0);
        for attribute in &self.attributes {
            encode_attribute(&mut out, attribute, &self.transaction_id);
        }
        if let Some(key) = key {
            let length = out.len() - HEADER_LEN + ATTR_HEADER_LEN + INTEGRITY_LEN;
            set_length(&mut out, length);
            let mac = key.hmac(&[&out]);
            push_attribute(&mut out, MESSAGE_INTEGRITY, &mac);
        }
        if fingerprint {
            let length = out.len() - HEADER_LEN + ATTR_HEADER_LEN + 4;
            set_length(&mut out, length);
            let crc = crc32fast::hash(&out) ^ FINGERPRINT_XOR;
            push_attribute(&mut out, FINGERPRINT, &crc.to_be_bytes());
        }
        let length = out.len() - HEADER_LEN;
        set_length(&mut out, length);
        out
    }

    /// Reads one whole message. A `FINGERPRINT`, when present, must match; attributes after `MESSAGE-INTEGRITY` other
    /// than `FINGERPRINT` are dropped. The integrity itself is checked with [`verify_integrity`].
    ///
    /// # Errors
    /// [`StunError`] when the bytes are not a well-formed STUN message.
    pub fn decode(bytes: &[u8]) -> Result<Self, StunError> {
        let header = Header::read(bytes)?;
        if bytes.len() != HEADER_LEN + header.length {
            return Err(StunError::BadLength);
        }
        let (class, method) = split_type(header.kind);
        let transaction_id = header.transaction_id;
        let mut attributes = Vec::new();
        let mut offset = HEADER_LEN;
        let mut after_integrity = false;
        while offset < bytes.len() {
            let (kind, value, next) = read_attribute(bytes, offset)?;
            if kind == FINGERPRINT {
                let crc = u32::from_be_bytes(value.try_into().map_err(|_| StunError::BadAttribute(kind))?);
                let mut prefix = bytes[..offset].to_vec();
                set_length(&mut prefix, offset - HEADER_LEN + ATTR_HEADER_LEN + 4);
                if crc32fast::hash(&prefix) ^ FINGERPRINT_XOR != crc {
                    return Err(StunError::BadFingerprint);
                }
                attributes.push(Attribute::Fingerprint(crc));
                break;
            }
            if !after_integrity {
                attributes.push(decode_attribute(kind, value, &transaction_id)?);
            }
            after_integrity |= kind == MESSAGE_INTEGRITY;
            offset = next;
        }
        Ok(Self {
            class,
            method,
            transaction_id,
            attributes,
        })
    }
}

/// Whether the raw message `bytes` carries a `MESSAGE-INTEGRITY` that matches `key`. False when it has none.
#[must_use]
pub fn verify_integrity(bytes: &[u8], key: &IntegrityKey) -> bool {
    let Ok(header) = Header::read(bytes) else {
        return false;
    };
    let end = (HEADER_LEN + header.length).min(bytes.len());
    let mut offset = HEADER_LEN;
    while offset < end {
        let Ok((kind, value, next)) = read_attribute(&bytes[..end], offset) else {
            return false;
        };
        if kind == MESSAGE_INTEGRITY {
            let mut head = bytes[..HEADER_LEN].to_vec();
            set_length(&mut head, offset - HEADER_LEN + ATTR_HEADER_LEN + INTEGRITY_LEN);
            let mac = key.hmac(&[&head, &bytes[HEADER_LEN..offset]]);
            return value == mac;
        }
        offset = next;
    }
    false
}

struct Header {
    kind: u16,
    length: usize,
    transaction_id: TransactionId,
}

impl Header {
    fn read(bytes: &[u8]) -> Result<Self, StunError> {
        if bytes.len() < HEADER_LEN {
            return Err(StunError::Truncated);
        }
        let kind = u16::from_be_bytes([bytes[0], bytes[1]]);
        let cookie = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        if kind & 0xC000 != 0 || cookie != MAGIC_COOKIE {
            return Err(StunError::NotStun);
        }
        let length = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
        if length % 4 != 0 {
            return Err(StunError::BadLength);
        }
        let mut id = [0; 12];
        id.copy_from_slice(&bytes[8..HEADER_LEN]);
        Ok(Self {
            kind,
            length,
            transaction_id: TransactionId(id),
        })
    }
}

/// (type, value, offset of the next attribute) of the attribute at `offset`.
fn read_attribute(bytes: &[u8], offset: usize) -> Result<(u16, &[u8], usize), StunError> {
    let head = bytes
        .get(offset..offset + ATTR_HEADER_LEN)
        .ok_or(StunError::BadLength)?;
    let kind = u16::from_be_bytes([head[0], head[1]]);
    let length = usize::from(u16::from_be_bytes([head[2], head[3]]));
    let start = offset + ATTR_HEADER_LEN;
    let value = bytes.get(start..start + length).ok_or(StunError::BadLength)?;
    let next = start + length.next_multiple_of(4);
    if next > bytes.len() {
        return Err(StunError::BadLength);
    }
    Ok((kind, value, next))
}

fn set_length(message: &mut [u8], length: usize) {
    let length = u16::try_from(length).unwrap_or(u16::MAX);
    message[2..4].copy_from_slice(&length.to_be_bytes());
}

fn push_attribute(out: &mut Vec<u8>, kind: u16, value: &[u8]) {
    let length = u16::try_from(value.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&value[..usize::from(length)]);
    out.resize(out.len().next_multiple_of(4), 0);
}

fn encode_attribute(out: &mut Vec<u8>, attribute: &Attribute, id: &TransactionId) {
    match attribute {
        Attribute::MappedAddress(addr) => push_attribute(out, MAPPED_ADDRESS, &address(*addr, None)),
        Attribute::AlternateServer(addr) => push_attribute(out, ALTERNATE_SERVER, &address(*addr, None)),
        Attribute::XorMappedAddress(addr) => push_attribute(out, XOR_MAPPED_ADDRESS, &address(*addr, Some(id))),
        Attribute::XorRelayedAddress(addr) => push_attribute(out, XOR_RELAYED_ADDRESS, &address(*addr, Some(id))),
        Attribute::XorPeerAddress(addr) => push_attribute(out, XOR_PEER_ADDRESS, &address(*addr, Some(id))),
        Attribute::Username(text) => push_attribute(out, USERNAME, text.as_bytes()),
        Attribute::Realm(text) => push_attribute(out, REALM, text.as_bytes()),
        Attribute::Nonce(text) => push_attribute(out, NONCE, text.as_bytes()),
        Attribute::Software(text) => push_attribute(out, SOFTWARE, text.as_bytes()),
        Attribute::ErrorCode { code, reason } => {
            let class = u8::try_from(code / 100).unwrap_or(0) & 0x07;
            let number = u8::try_from(code % 100).unwrap_or(0);
            let mut value = vec![0, 0, class, number];
            value.extend_from_slice(reason.as_bytes());
            push_attribute(out, ERROR_CODE, &value);
        }
        Attribute::UnknownAttributes(kinds) => {
            let value: Vec<u8> = kinds.iter().flat_map(|k| k.to_be_bytes()).collect();
            push_attribute(out, UNKNOWN_ATTRIBUTES, &value);
        }
        Attribute::Lifetime(seconds) => push_attribute(out, LIFETIME, &seconds.to_be_bytes()),
        Attribute::RequestedTransport(protocol) => push_attribute(out, REQUESTED_TRANSPORT, &[*protocol, 0, 0, 0]),
        Attribute::ChannelNumber(channel) => {
            let [hi, lo] = channel.to_be_bytes();
            push_attribute(out, CHANNEL_NUMBER, &[hi, lo, 0, 0]);
        }
        Attribute::Data(data) => push_attribute(out, DATA, data),
        Attribute::Other { kind, value } => push_attribute(out, *kind, value),
        Attribute::MessageIntegrity(_) | Attribute::Fingerprint(_) => {}
    }
}

fn decode_attribute(kind: u16, value: &[u8], id: &TransactionId) -> Result<Attribute, StunError> {
    let bad = || StunError::BadAttribute(kind);
    let text = || String::from_utf8(value.to_vec()).map_err(|_| bad());
    let word = || value.try_into().map(u32::from_be_bytes).map_err(|_| bad());
    Ok(match kind {
        MAPPED_ADDRESS => Attribute::MappedAddress(read_address(value, None).ok_or_else(bad)?),
        ALTERNATE_SERVER => Attribute::AlternateServer(read_address(value, None).ok_or_else(bad)?),
        XOR_MAPPED_ADDRESS => Attribute::XorMappedAddress(read_address(value, Some(id)).ok_or_else(bad)?),
        XOR_RELAYED_ADDRESS => Attribute::XorRelayedAddress(read_address(value, Some(id)).ok_or_else(bad)?),
        XOR_PEER_ADDRESS => Attribute::XorPeerAddress(read_address(value, Some(id)).ok_or_else(bad)?),
        USERNAME => Attribute::Username(text()?),
        REALM => Attribute::Realm(text()?),
        NONCE => Attribute::Nonce(text()?),
        SOFTWARE => Attribute::Software(text()?),
        ERROR_CODE => {
            let [_, _, class, number, reason @ ..] = value else {
                return Err(bad());
            };
            Attribute::ErrorCode {
                code: u16::from(class & 0x07) * 100 + u16::from(*number),
                reason: String::from_utf8_lossy(reason).into_owned(),
            }
        }
        UNKNOWN_ATTRIBUTES => Attribute::UnknownAttributes(
            value
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes(*pair))
                .collect(),
        ),
        LIFETIME => Attribute::Lifetime(word()?),
        REQUESTED_TRANSPORT => Attribute::RequestedTransport(*value.first().ok_or_else(bad)?),
        CHANNEL_NUMBER => {
            let [hi, lo, ..] = value else {
                return Err(bad());
            };
            Attribute::ChannelNumber(u16::from_be_bytes([*hi, *lo]))
        }
        DATA => Attribute::Data(value.to_vec()),
        MESSAGE_INTEGRITY => Attribute::MessageIntegrity(value.try_into().map_err(|_| bad())?),
        _ => Attribute::Other {
            kind,
            value: value.to_vec(),
        },
    })
}

/// An address attribute's value; XOR-ed with the cookie (and the transaction id for IPv6) when `xor` is given.
fn address(addr: SocketAddr, xor: Option<&TransactionId>) -> Vec<u8> {
    let mask = xor_mask(xor);
    let port = addr.port() ^ if xor.is_some() { 0x2112 } else { 0 };
    let (family, ip): (u8, Vec<u8>) = match addr.ip() {
        IpAddr::V4(ip) => (0x01, ip.octets().to_vec()),
        IpAddr::V6(ip) => (0x02, ip.octets().to_vec()),
    };
    let mut value = vec![0, family];
    value.extend_from_slice(&port.to_be_bytes());
    value.extend(ip.iter().zip(mask.iter()).map(|(byte, mask)| byte ^ mask));
    value
}

fn read_address(value: &[u8], xor: Option<&TransactionId>) -> Option<SocketAddr> {
    let [_, family, p0, p1, ip @ ..] = value else {
        return None;
    };
    let mask = xor_mask(xor);
    let port = u16::from_be_bytes([*p0, *p1]) ^ if xor.is_some() { 0x2112 } else { 0 };
    let bytes: Vec<u8> = ip.iter().zip(mask.iter()).map(|(byte, mask)| byte ^ mask).collect();
    let ip = match (family, bytes.len()) {
        (0x01, 4) => IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(bytes).ok()?)),
        (0x02, 16) => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(bytes).ok()?)),
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// The cookie followed by the transaction id, or zeros when the address isn't XOR-ed.
fn xor_mask(xor: Option<&TransactionId>) -> [u8; 16] {
    let mut mask = [0; 16];
    if let Some(id) = xor {
        mask[..4].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
        mask[4..].copy_from_slice(&id.0);
    }
    mask
}

/// TURN `ChannelData`: application data on a bound channel, with a 4-byte header instead of a STUN message.
#[derive(Clone, PartialEq, Eq)]
pub struct ChannelData {
    pub channel: u16,
    pub data: Vec<u8>,
}

impl fmt::Debug for ChannelData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChannelData({:#06x}, {} bytes)", self.channel, self.data.len())
    }
}

impl ChannelData {
    /// The message on the wire; `pad` rounds it up to 4 bytes, as TCP requires (RFC 8656 12.5).
    #[must_use]
    pub fn encode(&self, pad: bool) -> Vec<u8> {
        let length = u16::try_from(self.data.len()).unwrap_or(u16::MAX);
        let mut out = Vec::with_capacity(CHANNEL_HEADER_LEN + self.data.len() + 3);
        out.extend_from_slice(&self.channel.to_be_bytes());
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(&self.data[..usize::from(length)]);
        if pad {
            out.resize(out.len().next_multiple_of(4), 0);
        }
        out
    }

    /// Reads one `ChannelData` message; padding after the data is allowed.
    ///
    /// # Errors
    /// [`StunError`] when the bytes are not `ChannelData` on a valid channel.
    pub fn decode(bytes: &[u8]) -> Result<Self, StunError> {
        let [c0, c1, l0, l1, rest @ ..] = bytes else {
            return Err(StunError::Truncated);
        };
        if c0 & 0xC0 != 0x40 {
            return Err(StunError::NotChannelData);
        }
        let channel = u16::from_be_bytes([*c0, *c1]);
        if !(CHANNEL_MIN..=CHANNEL_MAX).contains(&channel) {
            return Err(StunError::BadChannel(channel));
        }
        let length = usize::from(u16::from_be_bytes([*l0, *l1]));
        let data = rest.get(..length).ok_or(StunError::Truncated)?;
        if rest.len() > length.next_multiple_of(4) {
            return Err(StunError::BadLength);
        }
        Ok(Self {
            channel,
            data: data.to_vec(),
        })
    }
}

/// What a datagram or framed message from a TURN server is, by its first byte (RFC 7983 demultiplexing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Stun,
    ChannelData,
    Other,
}

#[must_use]
pub fn classify(bytes: &[u8]) -> Kind {
    match bytes.first().map(|b| b >> 6) {
        Some(0) => Kind::Stun,
        Some(1) => Kind::ChannelData,
        _ => Kind::Other,
    }
}

/// How STUN and `ChannelData` are delimited on a TCP stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    /// As TURN defines it (RFC 8656 12.5): every message is self-delimiting, `ChannelData` padded to 4 bytes. What
    /// coturn and browsers speak.
    Native,
    /// Each message behind a 2-byte length (RFC 4571), as ICE-TCP frames media.
    Rfc4571,
}

impl Framing {
    /// The bytes that carry `message` on the stream.
    ///
    /// # Errors
    /// [`StunError::TooLarge`] when an RFC 4571 frame can't hold the message.
    pub fn frame(self, message: &[u8]) -> Result<Vec<u8>, StunError> {
        match self {
            Self::Native => {
                let mut out = message.to_vec();
                if classify(message) == Kind::ChannelData {
                    out.resize(out.len().next_multiple_of(4), 0);
                }
                Ok(out)
            }
            Self::Rfc4571 => {
                let length = u16::try_from(message.len()).map_err(|_| StunError::TooLarge)?;
                let mut out = Vec::with_capacity(message.len() + 2);
                out.extend_from_slice(&length.to_be_bytes());
                out.extend_from_slice(message);
                Ok(out)
            }
        }
    }
}

/// Splits a TCP byte stream into messages, however the reads cut it.
#[derive(Debug)]
pub struct StreamDecoder {
    framing: Framing,
    buf: Vec<u8>,
}

impl StreamDecoder {
    #[must_use]
    pub fn new(framing: Framing) -> Self {
        Self {
            framing,
            buf: Vec::new(),
        }
    }

    /// Adds bytes read from the stream.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole message, without framing (a `ChannelData`'s padding stays: [`ChannelData::decode`] allows it).
    ///
    /// # Errors
    /// [`StunError::Desync`] when the stream carries something that is neither STUN nor `ChannelData` (natively
    /// framed); the stream can't be read further.
    pub fn next_message(&mut self) -> Result<Option<Vec<u8>>, StunError> {
        let Some(total) = self.frame_len()? else {
            return Ok(None);
        };
        if self.buf.len() < total {
            return Ok(None);
        }
        let frame: Vec<u8> = self.buf.drain(..total).collect();
        Ok(Some(match self.framing {
            Framing::Native => frame,
            Framing::Rfc4571 => frame[2..].to_vec(),
        }))
    }

    /// The length of the frame at the head of the buffer, once its header is in.
    fn frame_len(&self) -> Result<Option<usize>, StunError> {
        let [b0, b1, b2, b3, ..] = self.buf[..] else {
            return Ok(None);
        };
        Ok(Some(match self.framing {
            Framing::Rfc4571 => 2 + usize::from(u16::from_be_bytes([b0, b1])),
            Framing::Native => {
                let length = usize::from(u16::from_be_bytes([b2, b3]));
                match classify(&self.buf) {
                    Kind::Stun => HEADER_LEN + length,
                    Kind::ChannelData => CHANNEL_HEADER_LEN + length.next_multiple_of(4),
                    Kind::Other => return Err(StunError::Desync),
                }
            }
        }))
    }
}
