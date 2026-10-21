//! What the service sends on a Trouter socket, read: socket.io 0.9 packets (`<type>:<id>:<endpoint>:<data>`) and the
//! events, acknowledgements and deliveries M1 Task 14 captured (`docs/protocol/trouter.md`).
//!
//! Only reading lives here. What the client sends is `teams_proto`'s reviewed own frames, written by its
//! [`FrameSink`](teams_proto::FrameSink): this module has nothing that makes frame text.

use serde_json::{Map, Value};
use teams_proto::{ConnectParams, DroppedIndicator};

/// One packet from the service.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    /// `0::`, or `0:::{"reason":…}`: the service closes the socket, with its reason when it gives one.
    Disconnect { reason: Option<String> },
    /// `1::`: the socket is open; the client authenticates now.
    Connect,
    /// `2::`, a socket.io heartbeat. None was seen; it is read and left unanswered.
    Heartbeat,
    /// `3:::{…}`: a delivery to one of the endpoint's push URLs.
    Delivery(RawDelivery),
    /// `5:<id>[+]::{"name":…,"args":[…]}`: an event; `acknowledged` when it asks for an acknowledgement (`+`).
    Event(Event),
    /// `6:::<id>+[…]`: the acknowledgement of the client's event `id`.
    Ack { id: u64, args: Value },
    /// Any other packet type (`4` JSON, `7` error, `8` no-op), by its type only.
    Other { kind: u8 },
}

/// A socket.io event from the service.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub id: Option<u64>,
    pub acknowledged: bool,
    pub name: String,
    pub args: Vec<Value>,
}

/// A delivery as it came: `{"id":…,"method":…,"url":…,"headers":{…},"body":"<text>"}`.
#[derive(Clone, PartialEq)]
pub struct RawDelivery {
    pub id: u64,
    pub method: String,
    /// The push URL's path (`/v4/f/<endpoint id>/messaging`).
    pub url: String,
    pub headers: Map<String, Value>,
    pub body: String,
}

impl std::fmt::Debug for RawDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawDelivery")
            .field("id", &self.id)
            .field("method", &self.method)
            .field("url", &self.url)
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl RawDelivery {
    /// A header's value as text (the names compared without case).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .and_then(|(_, value)| value.as_str())
    }

    /// A stored delivery, replayed on connect: its `trouter-request` says `"mstore":true`.
    #[must_use]
    pub fn is_stored(&self) -> bool {
        self.header("trouter-request")
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .is_some_and(|request| request["mstore"] == Value::Bool(true))
    }
}

/// Why a packet couldn't be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("not a socket.io 0.9 packet")]
    NotAPacket,
    #[error("a packet of type {0} whose data isn't what that type carries")]
    Data(u8),
}

/// Reads one text frame.
///
/// # Errors
/// [`DecodeError::NotAPacket`] when it isn't `<type>:<id>:<endpoint>[:<data>]`, [`DecodeError::Data`] when a known
/// type's data doesn't read.
pub fn decode(text: &str) -> Result<Incoming, DecodeError> {
    let mut parts = text.splitn(4, ':');
    let (Some(kind), Some(id), Some(_endpoint)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(DecodeError::NotAPacket);
    };
    let data = parts.next();
    let kind = match kind.as_bytes() {
        [digit @ b'0'..=b'9'] => digit - b'0',
        _ => return Err(DecodeError::NotAPacket),
    };
    let invalid = DecodeError::Data(kind);
    match kind {
        0 => Ok(Incoming::Disconnect {
            reason: data.and_then(disconnect_reason),
        }),
        1 => Ok(Incoming::Connect),
        2 => Ok(Incoming::Heartbeat),
        3 => data.and_then(delivery).map(Incoming::Delivery).ok_or(invalid),
        5 => event(id, data.ok_or(invalid)?).map(Incoming::Event).ok_or(invalid),
        6 => ack(data.ok_or(invalid)?).ok_or(invalid),
        other => Ok(Incoming::Other { kind: other }),
    }
}

fn disconnect_reason(data: &str) -> Option<String> {
    let value: Value = serde_json::from_str(data).ok()?;
    value["reason"].as_str().map(str::to_owned)
}

fn delivery(data: &str) -> Option<RawDelivery> {
    let Value::Object(mut fields) = serde_json::from_str(data).ok()? else {
        return None;
    };
    let text = |fields: &Map<String, Value>, key: &str| fields.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(RawDelivery {
        id: fields.get("id")?.as_u64()?,
        method: text(&fields, "method")?,
        url: text(&fields, "url")?,
        body: text(&fields, "body").unwrap_or_default(),
        headers: match fields.remove("headers") {
            Some(Value::Object(headers)) => headers,
            None => Map::new(),
            Some(_) => return None,
        },
    })
}

/// `<id>[+]` and the event's JSON.
fn event(id: &str, data: &str) -> Option<Event> {
    let (id, acknowledged) = match id.strip_suffix('+') {
        Some(id) => (Some(number(id)?), true),
        None if id.is_empty() => (None, false),
        None => (Some(number(id)?), false),
    };
    let value: Value = serde_json::from_str(data).ok()?;
    let name = value["name"].as_str()?.to_owned();
    let args = match &value["args"] {
        Value::Array(args) => args.clone(),
        Value::Null => Vec::new(),
        _ => return None,
    };
    Some(Event {
        id,
        acknowledged,
        name,
        args,
    })
}

/// `<id>+<json>`.
fn ack(data: &str) -> Option<Incoming> {
    let (id, args) = data.split_once('+')?;
    Some(Incoming::Ack {
        id: number(id)?,
        args: if args.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(args).ok()?
        },
    })
}

fn number(text: &str) -> Option<u64> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// What a `trouter.connected` hands over: the endpoint, its push URLs and where to reconnect, with the parameters to
/// send back then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connected {
    /// The endpoint (connection) id, the last part of the push URLs.
    pub id: String,
    /// The push URL the registration names (`surl`).
    pub push_url: String,
    /// Where the next connect goes (`reconnectUrl`), as it came.
    pub reconnect_url: Option<String>,
    /// Seconds the parameters stay valid (`ttl`, a string of digits as captured).
    pub ttl: Option<u64>,
    /// The reconnect parameters (`connectparams`), all seven or none.
    pub params: Option<ConnectParams>,
}

impl Connected {
    /// Reads a `trouter.connected` event's first argument; `None` without an `id` or a `surl`.
    #[must_use]
    pub fn from_event(event: &Event) -> Option<Self> {
        if event.name != "trouter.connected" {
            return None;
        }
        let info = event.args.first()?;
        let text = |key: &str| info[key].as_str().map(str::to_owned);
        Some(Self {
            id: text("id")?,
            push_url: text("surl")?,
            reconnect_url: text("reconnectUrl"),
            ttl: match &info["ttl"] {
                Value::String(ttl) => number(ttl),
                other => other.as_u64(),
            },
            params: connect_params(&info["connectparams"]),
        })
    }
}

fn connect_params(value: &Value) -> Option<ConnectParams> {
    let text = |key: &str| value[key].as_str().map(str::to_owned);
    Some(ConnectParams {
        issuer: text("issuer")?,
        scae: text("scae")?,
        sig: text("sig")?,
        sr: text("sr")?,
        sp: text("sp")?,
        se: text("se")?,
        st: text("st")?,
    })
}

/// A `trouter.message_loss` event's indicators (`droppedIndicators`), each `tag` and `etag`; `None` for another event
/// or a report that isn't that shape.
#[must_use]
pub fn message_loss(event: &Event) -> Option<Vec<DroppedIndicator>> {
    if event.name != "trouter.message_loss" {
        return None;
    }
    event.args.first()?["droppedIndicators"]
        .as_array()?
        .iter()
        .map(|indicator| {
            Some(DroppedIndicator {
                tag: indicator["tag"].as_str()?.to_owned(),
                etag: indicator["etag"].as_str()?.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{DecodeError, Incoming, decode};

    #[test]
    fn packets_are_read_by_their_type() {
        assert_eq!(decode("1::"), Ok(Incoming::Connect));
        assert_eq!(decode("2::"), Ok(Incoming::Heartbeat));
        assert_eq!(decode("0::"), Ok(Incoming::Disconnect { reason: None }));
        assert_eq!(decode("8::"), Ok(Incoming::Other { kind: 8 }));
        assert_eq!(decode(""), Err(DecodeError::NotAPacket));
        assert_eq!(decode("1:"), Err(DecodeError::NotAPacket));
        assert_eq!(decode("12::"), Err(DecodeError::NotAPacket));
        assert_eq!(decode("5:::not json"), Err(DecodeError::Data(5)));
        assert_eq!(decode("5:x+::{\"name\":\"ping\"}"), Err(DecodeError::Data(5)));
        assert_eq!(decode("6:::x+[]"), Err(DecodeError::Data(6)));
        assert_eq!(decode("3:::{\"id\":\"1\"}"), Err(DecodeError::Data(3)));
    }
}
