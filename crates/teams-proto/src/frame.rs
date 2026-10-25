//! The Trouter frames the native client sends on its push channel's socket.
//!
//! A socket's write half is handed to a [`FrameSink`], and the sink sends only an [`OwnFrame`]: a typed frame of this
//! crate's, built by its constructor in the shape the messaging stack sent (`docs/protocol/trouter.md`, M1 Task 14's
//! captures):
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
//! [`OwnFrame`] is sealed only to keep frame text out of other crates' hands: no constructor takes frame text, and no
//! other crate makes a frame. Which frames exist isn't a reviewed list: the session's own frames reach nobody, so adding
//! one here (`user.activity`, say) is an ordinary change. The only answer to a delivery (`3:::`) is a typed call
//! delivery response ([`DeliveryResponse`]: [`AcceptanceAcknowledgement`], [`InvitationReceipt`]), which is no frame by
//! itself: only [`FrameSink::respond`] sends one, once `Fence::check_delivery_response` permitted exactly its JSON for
//! the connection the sink was given ([`FrameSink::with_call_responses`]) (`CLAUDE.md`, "Calls go only to the lab").
//! Every other delivery stays unanswered.
//!
//! The sink numbers the events that ask for an acknowledgement itself (`<n>` above, from 1 per socket). A lab sink
//! writes nothing to `fence.log`: a frame isn't a contacting write. The personal build's sink ([`FrameSink::logging`],
//! for a transport whose fence is the personal one, [`Transport::frame_log`](crate::Transport::frame_log)) logs each
//! frame by its [`OwnFrame::KIND`] before it goes ([`FrameRecord`](teams_fence::FrameRecord):
//! `docs/personal-build.md`), and sends none it couldn't log.
//!
//! The frames' JSON is written by this module's own writer ([`Object`]) with its keys in the order the messaging stack
//! sent them, byte for byte: `serde_json`'s `Map` keeps insertion order only with its `preserve_order` feature, which
//! another crate in a build may or may not turn on, so the bytes on the socket would change with what else is compiled.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io;
use std::marker::PhantomData;
use std::sync::Arc;

use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_fence::{Delivery, DeliveryDecision, DeliveryDenyReason, Fence, FrameRecord, MembershipSource};

use crate::write::Links;

use self::sealed::Event;

/// A frame of the Trouter session (see the module docs). Sealed: no other crate can implement it, so no frame text is
/// made outside this crate.
pub trait OwnFrame: sealed::Sealed {
    /// The frame's kind, the name a `fence.log` record of it carries ([`FrameRecord`](teams_fence::FrameRecord)): the
    /// personal build logs its frames (`docs/personal-build.md`); the lab's sink logs none.
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
        /// `3:::<json>`: a response to a delivery, the JSON already checked by the fence.
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
        let headers = Object::new()
            .string("X-Ms-Test-User", "False")
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
    /// The personal build's sink couldn't log the frame to `fence.log`: it wasn't sent.
    #[error("fence.log can't be written, so the frame wasn't sent: {0}")]
    LogUnavailable(io::Error),
}

impl<E: Error + 'static> fmt::Debug for FrameError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Write(_) => f.write_str("Write(<the writer's error, not shown: it may hold the frame>)"),
            Self::LogUnavailable(error) => f.debug_tuple("LogUnavailable").field(error).finish(),
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

/// The one way the native client sends on a Trouter socket: only an [`OwnFrame`]. It has no method that takes text,
/// and it doesn't give its writer back. A lab sink writes nothing to `fence.log`; the personal build's logs each frame
/// first ([`FrameSink::logging`]).
pub struct FrameSink<W> {
    writer: W,
    last_ack: u64,
    /// The personal build's: the fence whose log each frame goes to first, and the socket's URL.
    log: Option<(Arc<Fence>, String)>,
    /// The calling socket's: the fence that decides its delivery responses, and the connection's key, the one its
    /// deliveries were observed under (`Fence::observe_delivery`).
    calls: Option<(Arc<Fence>, String)>,
}

impl<W> fmt::Debug for FrameSink<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSink")
            .field("last_ack", &self.last_ack)
            .finish_non_exhaustive()
    }
}

impl<W: FrameWriter> FrameSink<W> {
    /// The sink for a socket's write half, `writer`.
    #[must_use]
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            last_ack: 0,
            log: None,
            calls: None,
        }
    }

    /// The same sink answering call deliveries ([`Self::respond`]): each response decided by `fence` for the
    /// connection `connection`, the key this socket's deliveries are observed under. A sink without it answers none.
    #[must_use]
    pub fn with_call_responses(mut self, fence: Arc<Fence>, connection: impl Into<String>) -> Self {
        self.calls = Some((fence, connection.into()));
        self
    }

    /// Answers a call delivery with `response`, once: its JSON made once, decided by the fence for this sink's
    /// connection (`Fence::check_delivery_response`, `source` reading an offered group call's chat fresh), and only
    /// when permitted sent as `3:::` and exactly that JSON, as a typed frame (logged by its kind on a logging sink).
    ///
    /// # Errors
    /// [`RespondError::NoCallResponses`] for a sink that answers none; [`RespondError::NotPending`] when no delivery
    /// of this connection waits for it; [`RespondError::Denied`] when the fence denied it; [`RespondError::Frame`] when
    /// it couldn't be logged or sent. Nothing is sent but on success.
    pub async fn respond<R: DeliveryResponse + Send>(
        &mut self,
        response: R,
        source: &dyn MembershipSource,
    ) -> Result<Sent, RespondError<W::Error>> {
        let (fence, connection) = self.calls.as_ref().ok_or(RespondError::NoCallResponses)?;
        let json = response.envelope();
        let decision = fence.check_delivery_response(connection, json.as_bytes(), source).await;
        match decision {
            DeliveryDecision::Permitted(permit) if permit.delivery() == response.delivery() => {}
            DeliveryDecision::Permitted(_) => return Err(RespondError::Denied(DeliveryDenyReason::Uncorrelated)),
            DeliveryDecision::NotPending => return Err(RespondError::NotPending),
            DeliveryDecision::Denied(reason) => return Err(RespondError::Denied(reason)),
        }
        let frame = Checked::<R> {
            json,
            kind: PhantomData,
        };
        self.send(&frame).await.map_err(RespondError::Frame)
    }

    /// The personal build's sink for a socket's write half at `url`: each frame logged to `fence`'s log by its kind
    /// (the URL without its query) before it goes, none sent that couldn't be logged.
    #[must_use]
    pub fn logging(writer: W, fence: Arc<Fence>, url: &str) -> Self {
        Self {
            log: Some((fence, url.to_owned())),
            ..Self::new(writer)
        }
    }

    /// Sends `frame`. An event that asks for an acknowledgement takes the socket's next id, sent or not, so an id is
    /// never used twice.
    ///
    /// # Errors
    /// [`FrameError::Write`] when the socket failed; [`FrameError::LogUnavailable`] when a logging sink couldn't log it
    /// (nothing sent).
    pub async fn send<F: OwnFrame + Sync>(&mut self, frame: &F) -> Result<Sent, FrameError<W::Error>> {
        if let Some((fence, url)) = &self.log {
            fence
                .log()
                .record_frame(&FrameRecord { url, kind: F::KIND })
                .map_err(FrameError::LogUnavailable)?;
        }
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

/// A typed response to a call delivery: the acceptance's acknowledgement ([`AcceptanceAcknowledgement`]) or an offered
/// invitation's receipt ([`InvitationReceipt`]), each built from the delivery it answers in the shape the captures show
/// (`docs/protocol/calling-echo.md`, `docs/protocol/calling.md`). Not a frame by itself: only [`FrameSink::respond`]
/// sends one, after the fence permitted exactly its JSON for the sink's connection. Sealed: no other crate makes
/// response text.
pub trait DeliveryResponse: sealed::Response {
    /// The frame's kind, as a personal sink logs it.
    const KIND: &'static str;
}

/// Why [`FrameSink::respond`] sent nothing.
#[derive(thiserror::Error)]
pub enum RespondError<E: Error + 'static> {
    /// The sink answers no call delivery (no [`FrameSink::with_call_responses`]).
    #[error("this socket answers no call delivery")]
    NoCallResponses,
    /// No delivery of this connection waits for the response (another connection's, one answered already, an id no
    /// delivery had): nothing decided.
    #[error("no delivery of this connection waits for the response")]
    NotPending,
    /// The fence denied it (logged).
    #[error("the fence denied the delivery response: {0}")]
    Denied(DeliveryDenyReason),
    /// It couldn't be logged or sent.
    #[error(transparent)]
    Frame(FrameError<E>),
}

impl<E: Error + 'static> fmt::Debug for RespondError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCallResponses => f.write_str("NoCallResponses"),
            Self::NotPending => f.write_str("NotPending"),
            Self::Denied(reason) => f.debug_tuple("Denied").field(reason).finish(),
            Self::Frame(error) => f.debug_tuple("Frame").field(error).finish(),
        }
    }
}

/// A delivery response the fence permitted, as the sink sends it: never outside [`FrameSink::respond`].
struct Checked<R> {
    json: String,
    kind: PhantomData<fn() -> R>,
}

impl<R> sealed::Sealed for Checked<R> {
    fn event(&self) -> Event {
        Event::Response(self.json.clone())
    }
}

impl<R: DeliveryResponse> OwnFrame for Checked<R> {
    const KIND: &'static str = R::KIND;
}

/// What a call delivery response echoes of its delivery: its id and its `trouter-request`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Answered {
    id: i64,
    trouter_request: String,
}

impl Answered {
    fn of(delivery: &Delivery) -> Option<Self> {
        Some(Self {
            id: delivery.id,
            trouter_request: delivery.trouter_request.clone()?,
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
    pub fn new(delivery: &Delivery, correlation: impl Into<String>, client_delay: u64, links: Links) -> Option<Self> {
        Some(Self {
            answered: Answered::of(delivery)?,
            chain_id: delivery.chain_id.clone()?,
            message_id: delivery.message_id.clone()?,
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
    const KIND: &'static str = "call_acceptance_acknowledgement";
}

/// The response to an offered invitation's delivery, sent at once while it rings: `{"id": <the delivery's>, "status":
/// 200, "headers": {"trouter-request", "trouter-client"}, "body": ""}`, the request id the delivery's own (the captures
/// show it with and without `MS-CV`; this sends none). `Debug` shows the delivery's id only.
#[derive(Clone, PartialEq, Eq)]
pub struct InvitationReceipt {
    answered: Answered,
    client_delay: u64,
}

impl InvitationReceipt {
    /// The receipt of the invitation `delivery`, with the client's delay `client_delay`; `None` when the delivery
    /// carries no `trouter-request`.
    #[must_use]
    pub fn new(delivery: &Delivery, client_delay: u64) -> Option<Self> {
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
    const KIND: &'static str = "call_invitation_receipt";
}
