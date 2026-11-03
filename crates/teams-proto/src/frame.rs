//! Typed Trouter session frames and connection-bound call replies.

use std::error::Error;
use std::fmt;
use std::future::Future;

use serde_json::Value;
use teams_auth::secrets::Secret;

use crate::write::Links;

use self::sealed::Event;

/// A frame of the Trouter session (see the module docs). Sealed: no other crate can implement it, so no frame text is
/// made outside this crate.
pub trait OwnFrame: sealed::Sealed {
    /// The protocol frame kind.
    const KIND: &'static str;
}

mod sealed {
    /// The seal on [`OwnFrame`](super::OwnFrame): a public trait no other crate can name. It carries the frame's
    /// content, which only this crate makes and reads.
    pub trait Sealed {
        fn event(&self) -> Event;
    }

    /// The seal on [`DeliveryResponse`](super::DeliveryResponse): the delivery a response answers and its JSON, which
    /// only this crate makes and reads.
    pub trait Response {
        /// The id of the delivery it answers.
        fn delivery(&self) -> i64;
        fn connection(&self) -> &str;

        /// The socket.io message's JSON (after `3:::`), keys in the captured order.
        fn envelope(&self) -> String;
    }

    /// A frame's content, as the sink encodes it (socket.io 0.9): an event is its JSON text, keys in the captured
    /// order.
    pub enum Event {
        /// `5:::<json>`: an event that asks for no acknowledgement.
        Plain(String),
        /// `5:<n>+::<json>`: an event the service acknowledges with `6:::<n>+[…]`.
        Acknowledged(String),
        /// `0::`.
        Disconnect,
        /// `3:::<json>`: a response to a delivery, the JSON built by a typed response.
        Response(String),
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

impl ConnectParams {
    fn to_json(&self) -> String {
        Object::new()
            .string("issuer", &self.issuer)
            .string("scae", &self.scae)
            .string("sig", &self.sig)
            .string("sr", &self.sr)
            .string("sp", &self.sp)
            .string("se", &self.se)
            .string("st", &self.st)
            .finish()
    }
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
    /// The calling stack's socket's: without `X-Ms-Test-User`, as its sockets sent it.
    calling: bool,
}

impl Authenticate {
    /// Authenticates the messaging stack's socket with `token` (a bearer token for
    /// [`IC3_AUDIENCE`](crate::IC3_AUDIENCE)), sending back `params` when an earlier `trouter.connected` handed some
    /// over.
    #[must_use]
    pub fn new(token: &Secret, params: Option<ConnectParams>) -> Self {
        Self {
            token: token.clone(),
            params,
            calling: false,
        }
    }

    /// As [`Self::new`], for the calling stack's socket: its headers only `Authorization` and `X-MS-Migration`, as the
    /// calling stack's sockets sent them (`docs/protocol/trouter.md`).
    #[must_use]
    pub fn calling(token: &Secret, params: Option<ConnectParams>) -> Self {
        Self {
            calling: true,
            ..Self::new(token, params)
        }
    }
}

impl sealed::Sealed for Authenticate {
    fn event(&self) -> Event {
        let headers = if self.calling {
            Object::new()
        } else {
            Object::new().string("X-Ms-Test-User", "False")
        };
        let headers = headers
            .string("Authorization", &format!("Bearer {}", self.token.expose()))
            .string("X-MS-Migration", "True")
            .finish();
        let mut argument = Object::new().raw("headers", &headers);
        if let Some(params) = &self.params {
            argument = argument.raw("connectparams", &params.to_json());
        }
        Event::Plain(
            Object::new()
                .string("name", "user.authenticate")
                .raw("args", &array([argument.finish()]))
                .finish(),
        )
    }
}

impl OwnFrame for Authenticate {
    /// The protocol frame kind.
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
        Event::Acknowledged(Object::new().string("name", "ping").finish())
    }
}

impl OwnFrame for Ping {
    /// The protocol frame kind.
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
        let indicators = array(self.indicators.iter().map(|indicator| {
            Object::new()
                .string("tag", &indicator.tag)
                .string("etag", &indicator.etag)
                .finish()
        }));
        let argument = Object::new().raw("droppedIndicators", &indicators).finish();
        Event::Acknowledged(
            Object::new()
                .string("name", "trouter.processed_message_loss")
                .raw("args", &array([argument]))
                .finish(),
        )
    }
}

impl OwnFrame for MessageLossAck {
    /// The protocol frame kind.
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
    /// The protocol frame kind.
    const KIND: &'static str = "trouter_disconnect";
}

/// A JSON object written key by key, in the order the keys are given (see the module docs).
struct Object(String);

impl Object {
    fn new() -> Self {
        Self(String::from("{"))
    }

    fn key(mut self, key: &str) -> Self {
        if self.0.len() > 1 {
            self.0.push(',');
        }
        push_string(&mut self.0, key);
        self.0.push(':');
        self
    }

    /// `key` with a string `value`.
    fn string(self, key: &str, value: &str) -> Self {
        let mut object = self.key(key);
        push_string(&mut object.0, value);
        object
    }

    /// `key` with a number `value`.
    fn number(self, key: &str, value: i64) -> Self {
        let mut object = self.key(key);
        object.0.push_str(&value.to_string());
        object
    }

    /// `key` with `json`, a value already written.
    fn raw(self, key: &str, json: &str) -> Self {
        let mut object = self.key(key);
        object.0.push_str(json);
        object
    }

    fn finish(mut self) -> String {
        self.0.push('}');
        self.0
    }
}

/// Appends `value` as a JSON string, escaped by `serde_json` (a lone string has no key order to lose).
fn push_string(out: &mut String, value: &str) {
    out.push_str(&Value::from(value).to_string());
}

/// A JSON array of `items`, each a value already written.
fn array(items: impl IntoIterator<Item = String>) -> String {
    let items: Vec<String> = items.into_iter().collect();
    format!("[{}]", items.join(","))
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
    /// Why a frame wasn't sent. It may hold the unsent frame, token and all (tungstenite's `WriteBufferFull` hands the
    /// message back), so [`FrameError`] never shows it: the caller takes it out of [`FrameError::Write`] by name and
    /// mustn't print it.
    type Error: Error + Send + Sync + 'static;

    /// Sends one text frame.
    fn write(&mut self, text: FrameText) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// Why a frame wasn't sent. Its `Debug`, `Display` and `source` never show a frame's text: the writer's error in
/// [`FrameError::Write`] may hold the unsent frame ([`FrameWriter::Error`]), so none of them shows it.
#[derive(thiserror::Error)]
pub enum FrameError<E: Error + 'static> {
    /// The socket failed to send it: the writer's error, reached only by matching the variant.
    #[error("the socket failed to send the frame")]
    Write(E),
}

impl<E: Error + 'static> fmt::Debug for FrameError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Write(_) => f.write_str("Write(<the writer's error, not shown: it may hold the frame>)"),
        }
    }
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

pub struct FrameSink<W> {
    writer: W,
    last_ack: u64,
    connection: Option<String>,
}

impl<W> fmt::Debug for FrameSink<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSink")
            .field("last_ack", &self.last_ack)
            .finish_non_exhaustive()
    }
}

impl<W: FrameWriter> FrameSink<W> {
    /// A sink for the socket's write half.
    #[must_use]
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            last_ack: 0,
            connection: None,
        }
    }

    /// Allows typed call replies on this delivery connection.
    #[must_use]
    pub fn with_call_responses(mut self, connection: impl Into<String>) -> Self {
        self.connection = Some(connection.into());
        self
    }

    /// Sends a typed reply only on the connection its delivery arrived on.
    ///
    /// # Errors
    /// No reply connection, a different connection, or a socket write failure.
    pub async fn respond<R: DeliveryResponse + Send>(&mut self, response: R) -> Result<Sent, RespondError<W::Error>> {
        let connection = self.connection.as_deref().ok_or(RespondError::NoCallResponses)?;
        if connection != response.connection() {
            return Err(RespondError::OtherConnection);
        }
        let text = format!("3:::{}", response.envelope());
        self.writer
            .write(FrameText(text))
            .await
            .map_err(|error| RespondError::Frame(FrameError::Write(error)))?;
        Ok(Sent { ack: None })
    }

    /// Sends one typed session frame, numbering acknowledgement requests monotonically.
    ///
    /// # Errors
    /// A socket write failure.
    pub async fn send<F: OwnFrame + Sync>(&mut self, frame: &F) -> Result<Sent, FrameError<W::Error>> {
        let (text, ack) = match frame.event() {
            Event::Plain(event) => (format!("5:::{event}"), None),
            Event::Acknowledged(event) => {
                self.last_ack += 1;
                (format!("5:{}+::{event}", self.last_ack), Some(self.last_ack))
            }
            Event::Disconnect => ("0::".to_owned(), None),
            Event::Response(json) => (format!("3:::{json}"), None),
        };
        self.writer.write(FrameText(text)).await.map_err(FrameError::Write)?;
        Ok(Sent { ack })
    }
}

pub trait DeliveryResponse: sealed::Response {
    /// The frame's kind, as a personal sink logs it.
    /// The protocol frame kind.
    const KIND: &'static str;
}

/// Why a typed reply could not be sent.
#[derive(Debug, thiserror::Error)]
pub enum RespondError<E: Error + 'static> {
    #[error("this socket answers no call delivery")]
    NoCallResponses,
    #[error("the delivery belongs to another connection")]
    OtherConnection,
    #[error(transparent)]
    Frame(FrameError<E>),
}

/// The minimal delivery context required to encode a typed call response.
/// Full call deliveries and tracking belong to teams-call.
pub trait ResponseDelivery {
    fn connection(&self) -> &str;
    fn id(&self) -> i64;
    fn trouter_request(&self) -> Option<&str>;
    fn chain_id(&self) -> Option<&str>;
    fn message_id(&self) -> Option<&str>;
}

/// What a call delivery response echoes of its delivery: its id and its `trouter-request`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Answered {
    connection: String,
    id: i64,
    trouter_request: String,
}

impl Answered {
    fn of(delivery: &impl ResponseDelivery) -> Option<Self> {
        Some(Self {
            connection: delivery.connection().to_owned(),
            id: delivery.id(),
            trouter_request: delivery.trouter_request()?.to_owned(),
        })
    }
}

/// `trouter-client` as the captures send it: `{"cd":<n>}`.
fn trouter_client(client_delay: u64) -> String {
    Object::new()
        .number("cd", i64::try_from(client_delay).unwrap_or(i64::MAX))
        .finish()
}

/// The response to the acceptance of a call the account started: `{"id": <the delivery's>, "status": 200, "headers":
/// {"MS-CV", "trouter-request", "trouter-client", "X-Microsoft-Skype-Chain-ID", "X-Microsoft-Skype-Message-ID"},
/// "body": "<the acknowledgement>"}`, the request, chain and message ids the delivery's own, the acknowledgement
/// `{"callAcceptanceAcknowledgement":{"links":{…}}}` handing the call controller the endpoint's callbacks for the call.
/// `Debug` shows the delivery's id only.
#[derive(Clone, PartialEq, Eq)]
pub struct AcceptanceAcknowledgement {
    answered: Answered,
    chain_id: String,
    message_id: String,
    correlation: String,
    client_delay: u64,
    links: Links,
}

impl AcceptanceAcknowledgement {
    /// The acknowledgement of the acceptance `delivery`, with the correlation vector `correlation` (`MS-CV`), the
    /// client's delay `client_delay` (`trouter-client`'s `cd`) and the callbacks `links`; `None` when the delivery
    /// carries no `trouter-request`, chain id or message id (its response can't echo them).
    #[must_use]
    pub fn new(
        delivery: &impl ResponseDelivery,
        correlation: impl Into<String>,
        client_delay: u64,
        links: Links,
    ) -> Option<Self> {
        Some(Self {
            answered: Answered::of(delivery)?,
            chain_id: delivery.chain_id()?.to_owned(),
            message_id: delivery.message_id()?.to_owned(),
            correlation: correlation.into(),
            client_delay,
            links,
        })
    }
}

impl fmt::Debug for AcceptanceAcknowledgement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcceptanceAcknowledgement")
            .field("delivery", &self.answered.id)
            .finish_non_exhaustive()
    }
}

impl sealed::Response for AcceptanceAcknowledgement {
    fn delivery(&self) -> i64 {
        self.answered.id
    }
    fn connection(&self) -> &str {
        &self.answered.connection
    }

    fn envelope(&self) -> String {
        let headers = Object::new()
            .string("MS-CV", &self.correlation)
            .string("trouter-request", &self.answered.trouter_request)
            .string("trouter-client", &trouter_client(self.client_delay))
            .string("X-Microsoft-Skype-Chain-ID", &self.chain_id)
            .string("X-Microsoft-Skype-Message-ID", &self.message_id)
            .finish();
        let links = self
            .links
            .iter()
            .fold(Object::new(), |links, (name, url)| links.string(name, url))
            .finish();
        let acknowledgement = Object::new()
            .raw(
                "callAcceptanceAcknowledgement",
                &Object::new().raw("links", &links).finish(),
            )
            .finish();
        Object::new()
            .number("id", self.answered.id)
            .number("status", 200)
            .raw("headers", &headers)
            .string("body", &acknowledgement)
            .finish()
    }
}

impl DeliveryResponse for AcceptanceAcknowledgement {
    /// The protocol frame kind.
    const KIND: &'static str = "call_acceptance_acknowledgement";
}

/// The receipt of a call delivery: an offered invitation's, sent at once while it rings, and a live call's
/// renegotiation, answer or acknowledgement delivered at its own callback, as the web client sends it for each:
/// `{"id": <the delivery's>, "status": 200, "headers": {"trouter-request", "trouter-client"}, "body": ""}`, the
/// request id the delivery's own (the captures show it with and without `MS-CV`; this sends none). `Debug` shows the
/// delivery's id only.
#[derive(Clone, PartialEq, Eq)]
pub struct InvitationReceipt {
    answered: Answered,
    client_delay: u64,
}

impl InvitationReceipt {
    /// The receipt of the invitation `delivery`, with the client's delay `client_delay`; `None` when the delivery
    /// carries no `trouter-request`.
    #[must_use]
    pub fn new(delivery: &impl ResponseDelivery, client_delay: u64) -> Option<Self> {
        Some(Self {
            answered: Answered::of(delivery)?,
            client_delay,
        })
    }
}

impl fmt::Debug for InvitationReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InvitationReceipt")
            .field("delivery", &self.answered.id)
            .finish_non_exhaustive()
    }
}

impl sealed::Response for InvitationReceipt {
    fn delivery(&self) -> i64 {
        self.answered.id
    }
    fn connection(&self) -> &str {
        &self.answered.connection
    }

    fn envelope(&self) -> String {
        let headers = Object::new()
            .string("trouter-request", &self.answered.trouter_request)
            .string("trouter-client", &trouter_client(self.client_delay))
            .finish();
        Object::new()
            .number("id", self.answered.id)
            .number("status", 200)
            .raw("headers", &headers)
            .string("body", "")
            .finish()
    }
}

impl DeliveryResponse for InvitationReceipt {
    /// The protocol frame kind.
    const KIND: &'static str = "call_invitation_receipt";
}
