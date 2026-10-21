//! The Trouter frame policy (M1 Task 15): what the native client may send on its push channel's socket.
//!
//! A socket's write half is handed to a [`FrameSink`], and the sink sends only an [`OwnFrame`]: the sealed, reviewed
//! list of the Trouter session's own frames, each of which touches only the account's own session and reaches no one
//! else (`docs/protocol/trouter.md`, the shapes the messaging stack sent in M1 Task 14's captures):
//!
//! - [`Authenticate`], the first frame on every socket: `5:::{"name":"user.authenticate","args":[{"headers":{…},
//!   "connectparams":{…}}]}`, the headers exactly `X-Ms-Test-User: False`, `Authorization: Bearer <ic3>` and
//!   `X-MS-Migration: True`, and the reconnect parameters an earlier `trouter.connected` handed over, when there are
//!   some, sent back as they came;
//! - [`Ping`], the keep-alive: `5:<n>+::{"name":"ping"}`;
//! - [`MessageLossAck`], the acknowledgement of the service's message-loss report:
//!   `5:<n>+::{"name":"trouter.processed_message_loss","args":[{"droppedIndicators":[{"tag":…,"etag":…},…]}]}`;
//! - [`Disconnect`], the client's disconnect: `0::`.
//!
//! Nothing else. Not `user.activity` (Task 14: deliveries come without it, so the native client doesn't report its
//! activity in M1), and no answer to a delivery (`3:::`): deliveries stay unanswered (Task 14, decided with Codex), so
//! no response type exists. A frame is built only by this crate's constructors, each of which gives exactly its
//! reviewed shape; no constructor takes frame text, and [`OwnFrame`] is sealed, so no other crate adds to the list.
//!
//! The sink numbers the events that ask for an acknowledgement itself (`<n>` above, from 1 per socket) and logs every
//! frame to `fence.log` ([`FrameRecord`]: the socket, the frame's kind, never its text) before it hands it to the
//! writer; a frame that can't be logged isn't sent.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use teams_auth::secrets::Secret;
use teams_fence::{Fence, FrameRecord};

use self::sealed::Event;

/// A reviewed own frame of the Trouter session (see the module docs). Sealed: no other crate can implement it.
pub trait OwnFrame: sealed::Sealed {
    /// The entry's name, logged to `fence.log` with every frame of it.
    const KIND: &'static str;
}

mod sealed {
    use serde_json::Value;

    /// The seal on [`OwnFrame`](super::OwnFrame): a public trait no other crate can name. Its implementations are the
    /// reviewed list, and it carries the frame's content, which only this crate reads.
    pub trait Sealed {
        fn event(&self) -> Event;
    }

    /// A frame's content, as the sink encodes it (socket.io 0.9).
    pub enum Event {
        /// `5:::<json>`: an event that asks for no acknowledgement.
        Plain(Value),
        /// `5:<n>+::<json>`: an event the service acknowledges with `6:::<n>+[…]`.
        Acknowledged(Value),
        /// `0::`.
        Disconnect,
    }
}

/// The reconnect parameters a `trouter.connected` handed over (its `connectparams`), sent back as they came in the
/// next [`Authenticate`]. `Debug` shows `sig` by its length only.
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectParams {
    pub issuer: String,
    pub scae: String,
    pub sig: String,
    /// The endpoint id.
    pub sr: String,
    /// The regional name.
    pub sp: String,
    /// The expiry, in ms.
    pub se: String,
    /// The start, in ms.
    pub st: String,
}

impl fmt::Debug for ConnectParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectParams")
            .field("issuer", &self.issuer)
            .field("scae", &self.scae)
            .field("sig_len", &self.sig.len())
            .field("sr", &self.sr)
            .field("sp", &self.sp)
            .field("se", &self.se)
            .field("st", &self.st)
            .finish()
    }
}

/// `user.authenticate`, the first frame on every socket: the account's `ic3` token and, on a reconnect, the
/// parameters the service handed over. `Debug` shows no token.
#[derive(Clone, Debug)]
pub struct Authenticate {
    token: Secret,
    params: Option<ConnectParams>,
}

impl Authenticate {
    /// Authenticates with `token` (a bearer token for [`IC3_AUDIENCE`](crate::IC3_AUDIENCE)), sending back `params`
    /// when an earlier `trouter.connected` handed some over.
    #[must_use]
    pub fn new(token: &Secret, params: Option<ConnectParams>) -> Self {
        Self {
            token: token.clone(),
            params,
        }
    }
}

impl sealed::Sealed for Authenticate {
    fn event(&self) -> Event {
        let mut argument = Map::new();
        argument.insert(
            "headers".to_owned(),
            json!({
                "X-Ms-Test-User": "False",
                "Authorization": format!("Bearer {}", self.token.expose()),
                "X-MS-Migration": "True",
            }),
        );
        if let Some(params) = &self.params {
            argument.insert(
                "connectparams".to_owned(),
                json!({
                    "issuer": params.issuer,
                    "scae": params.scae,
                    "sig": params.sig,
                    "sr": params.sr,
                    "sp": params.sp,
                    "se": params.se,
                    "st": params.st,
                }),
            );
        }
        Event::Plain(json!({"name": "user.authenticate", "args": [argument]}))
    }
}

impl OwnFrame for Authenticate {
    const KIND: &'static str = "trouter_authenticate";
}

/// The keep-alive, `{"name":"ping"}`, acknowledged by the service with a `pong`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ping {
    _sealed: (),
}

impl Ping {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl sealed::Sealed for Ping {
    fn event(&self) -> Event {
        Event::Acknowledged(json!({"name": "ping"}))
    }
}

impl OwnFrame for Ping {
    const KIND: &'static str = "trouter_ping";
}

/// One entry of the service's message-loss report (`droppedIndicators`), handed back as it came.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DroppedIndicator {
    pub tag: String,
    pub etag: String,
}

/// `trouter.processed_message_loss`: the acknowledgement of the service's `trouter.message_loss` report, its
/// indicators handed back as they came.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageLossAck {
    indicators: Vec<DroppedIndicator>,
}

impl MessageLossAck {
    /// The acknowledgement of a report of `indicators`; `None` for an empty list, which no report showed.
    #[must_use]
    pub fn new(indicators: Vec<DroppedIndicator>) -> Option<Self> {
        (!indicators.is_empty()).then_some(Self { indicators })
    }
}

impl sealed::Sealed for MessageLossAck {
    fn event(&self) -> Event {
        let indicators: Vec<Value> = self
            .indicators
            .iter()
            .map(|indicator| json!({"tag": indicator.tag, "etag": indicator.etag}))
            .collect();
        Event::Acknowledged(json!({
            "name": "trouter.processed_message_loss",
            "args": [{"droppedIndicators": indicators}],
        }))
    }
}

impl OwnFrame for MessageLossAck {
    const KIND: &'static str = "trouter_message_loss_ack";
}

/// The client's disconnect, `0::`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Disconnect {
    _sealed: (),
}

impl Disconnect {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl sealed::Sealed for Disconnect {
    fn event(&self) -> Event {
        Event::Disconnect
    }
}

impl OwnFrame for Disconnect {
    const KIND: &'static str = "trouter_disconnect";
}

/// A frame's text as it goes on the socket, made only by a [`FrameSink`]. `Debug` shows its length only: the
/// authentication frame carries the account's token.
pub struct FrameText(String);

impl FrameText {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for FrameText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameText").field("len", &self.0.len()).finish()
    }
}

/// A socket's write half, as a [`FrameSink`] uses it: it takes only the [`FrameText`] the sink makes. The socket's
/// owner hands it to the sink and keeps no other way to send.
pub trait FrameWriter: Send {
    type Error: Error + Send + Sync + 'static;

    /// Sends one text frame.
    fn write(&mut self, text: FrameText) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// Why a frame wasn't sent. No variant carries a frame's text.
#[derive(Debug, thiserror::Error)]
pub enum FrameError<E: Error + 'static> {
    /// Its `fence.log` entry couldn't be written; it wasn't sent.
    #[error("fence.log can't be written, so the frame wasn't sent: {0}")]
    LogUnavailable(#[source] io::Error),
    /// The socket failed to send it (after it was logged).
    #[error("the socket failed to send the frame: {0}")]
    Write(#[source] E),
}

/// What the sink did with a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sent {
    ack: Option<u64>,
}

impl Sent {
    /// The id the sink numbered an event that asks for an acknowledgement with (the `<n>` of `5:<n>+::`), which the
    /// service's `6:::<n>+[…]` names; `None` for a frame that asks for none.
    #[must_use]
    pub fn ack_id(&self) -> Option<u64> {
        self.ack
    }
}

/// The one way the native client sends on a Trouter socket: only an [`OwnFrame`], each logged to `fence.log` before
/// it is sent. It has no method that takes text, and it doesn't give its writer back.
pub struct FrameSink<W> {
    writer: W,
    fence: Arc<Fence>,
    url: String,
    last_ack: u64,
}

impl<W> fmt::Debug for FrameSink<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSink")
            .field("last_ack", &self.last_ack)
            .finish_non_exhaustive()
    }
}

impl<W: FrameWriter> FrameSink<W> {
    /// The sink for the socket at `url` (logged with every frame, without its query), sending through `writer` and
    /// logging to `fence`'s log.
    #[must_use]
    pub fn new(writer: W, fence: Arc<Fence>, url: &str) -> Self {
        Self {
            writer,
            fence,
            url: url.to_owned(),
            last_ack: 0,
        }
    }

    /// Logs `frame` to `fence.log`, then sends it. An event that asks for an acknowledgement takes the socket's next
    /// id, logged or not, so an id is never used twice.
    ///
    /// # Errors
    /// [`FrameError::LogUnavailable`] when the log can't take the entry (nothing was sent), [`FrameError::Write`] when
    /// the socket failed.
    pub async fn send<F: OwnFrame + Sync>(&mut self, frame: &F) -> Result<Sent, FrameError<W::Error>> {
        let (text, ack) = match frame.event() {
            Event::Plain(event) => (format!("5:::{event}"), None),
            Event::Acknowledged(event) => {
                self.last_ack += 1;
                (format!("5:{}+::{event}", self.last_ack), Some(self.last_ack))
            }
            Event::Disconnect => ("0::".to_owned(), None),
        };
        let record = FrameRecord {
            url: &self.url,
            kind: F::KIND,
        };
        self.fence
            .log()
            .record_frame(&record)
            .map_err(FrameError::LogUnavailable)?;
        self.writer.write(FrameText(text)).await.map_err(FrameError::Write)?;
        Ok(Sent { ack })
    }
}
