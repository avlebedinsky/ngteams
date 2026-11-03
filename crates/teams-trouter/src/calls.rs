//! The calling stack's socket's side of a call (`docs/protocol/calling.md`): what it hands on and the only way it
//! answers a delivery.
//!
//! - **What it hands on**: every delivery at the calling endpoint (its root, where an incoming call's invitation comes,
//!   and its call agent's callbacks, `/v4/f/<endpoint>/callAgent/…`) is first read by the fence
//!   ([`teams_fence::Fence::observe_delivery`], under the connection's key, the one its responses are decided under): an
//!   acceptance binds its call's links, an end ends it, an invitation to the account from a lab caller is offered. An
//!   invitation the fence doesn't offer (or can't read) is [`crate::PushEvent::Ignored`] (never handed on, nothing sent
//!   for it); every other delivery is a [`CallDelivery`] for the call's consumer, with what the fence made of it. The call
//!   controller's deliveries come gzip'd in base64 (`X-Microsoft-Skype-Content-Encoding: gzip`): the fence reads the
//!   body decoded, as the browser fence does, and one that doesn't decode within [`MAX_DELIVERY`] is
//!   [`crate::Ignored::Undecodable`].
//! - **The only answer**: a typed delivery response ([`CallResponse`]: an acceptance's acknowledgement or an offered
//!   invitation's receipt), handed back through the delivery's own [`CallResponder`] to the connection it came on and
//!   sent there by `teams_proto::FrameSink::send_permitted` once the fence permitted exactly its JSON for that
//!   connection (`teams_proto::ResponseDecider::decide`, `teams_fence::Fence::check_delivery_response`). The fence
//!   decides while the socket reads on (a group call's receipt reads its chat fresh), so a cancellation that comes in
//!   the while is observed before its last look at the ring. A response to a delivery of a connection that has gone down isn't
//!   sent on another ([`RespondFailure::Gone`]), and every other delivery stays unanswered.

use std::fmt;
use std::io::Read as _;
use std::sync::Arc;

use base64::Engine as _;

use teams_fence::{Delivery, DeliveryDenyReason, Fence, MembershipSource, Observed};
use teams_proto::{AcceptanceAcknowledgement, InvitationReceipt};
use tokio::sync::{mpsc, oneshot};

use crate::frames::RawDelivery;

/// The most a call delivery's body may hold once decoded (the socket reads no larger frame either).
pub const MAX_DELIVERY: u64 = crate::socket::MAX_INCOMING as u64;

/// A delivery on the calling stack's connection `connection` as the fence reads it: its id, path, the headers its
/// response echoes and its body, decoded from base64 and gzip when `X-Microsoft-Skype-Content-Encoding` (any case) is
/// `gzip`. `None` when such a body doesn't decode within [`MAX_DELIVERY`].
pub(crate) fn read_delivery(connection: &str, raw: &RawDelivery) -> Option<Delivery> {
    let gzip = raw
        .header("X-Microsoft-Skype-Content-Encoding")
        .is_some_and(|encoding| encoding.eq_ignore_ascii_case("gzip"));
    let body = if gzip {
        gunzip(&raw.body)?
    } else {
        raw.body.clone().into_bytes()
    };
    let header = |name: &str| raw.header(name).map(str::to_owned);
    Some(Delivery {
        connection: connection.to_owned(),
        id: raw.id,
        path: raw.url.clone(),
        trouter_request: header("trouter-request"),
        chain_id: header("X-Microsoft-Skype-Chain-ID"),
        message_id: header("X-Microsoft-Skype-Message-ID"),
        body,
    })
}

/// A gzip'd body in base64, decoded; `None` when it isn't one or holds more than [`MAX_DELIVERY`].
fn gunzip(body: &str) -> Option<Vec<u8>> {
    let compressed = base64::engine::general_purpose::STANDARD.decode(body).ok()?;
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(compressed.as_slice())
        .take(MAX_DELIVERY + 1)
        .read_to_end(&mut decoded)
        .ok()?;
    (decoded.len() as u64 <= MAX_DELIVERY).then_some(decoded)
}

/// What the calling stack's socket decides with ([`crate::Trouter::for_calls`]): the fence that reads its deliveries
/// and decides its responses (the account's transport's own, whose call registry its call writes feed), and the source
/// that fence reads an offered group call's chat from.
#[derive(Clone)]
pub struct CallStack {
    pub(crate) fence: Arc<Fence>,
    pub(crate) source: Arc<dyn MembershipSource + Send>,
    pub(crate) lab: Option<Arc<dyn teams_proto::lab_calls::CallObserver>>,
}

impl CallStack {
    #[must_use]
    pub fn new(fence: Arc<Fence>, source: Arc<dyn MembershipSource + Send>) -> Self {
        Self {
            fence,
            source,
            lab: None,
        }
    }

    /// Uses the write transport's independent account/call evidence for lab delivery handling.
    #[must_use]
    pub fn with_lab_calls(mut self, observer: Arc<dyn teams_proto::lab_calls::CallObserver>) -> Self {
        if observer.call_bindings().is_some() {
            self.lab = Some(observer);
        }
        self
    }
}

impl fmt::Debug for CallStack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallStack").finish_non_exhaustive()
    }
}

/// A delivery at the calling endpoint, read by the fence first (see the module docs). `Debug` shows no body.
#[derive(Debug)]
pub struct CallDelivery {
    /// The delivery as the fence read it: the connection it came on, its id, path, request, chain and message ids
    /// and its body (a response is built from it: [`AcceptanceAcknowledgement::new`], [`InvitationReceipt::new`]).
    pub delivery: Delivery,
    /// What the fence made of it.
    pub observed: Observed,
    responder: CallResponder,
}

impl CallDelivery {
    pub(crate) fn new(delivery: Delivery, observed: Observed, responder: CallResponder) -> Self {
        Self {
            delivery,
            observed,
            responder,
        }
    }

    /// Answers the delivery on the connection it came on (see [`CallResponder::respond`]).
    ///
    /// # Errors
    /// As [`CallResponder::respond`]'s.
    pub async fn respond(&self, response: impl Into<CallResponse>) -> Result<(), RespondFailure> {
        self.responder.respond(response).await
    }

    /// The way to answer on the delivery's connection later.
    #[must_use]
    pub fn responder(&self) -> CallResponder {
        self.responder.clone()
    }
}

/// A typed call delivery response (see the module docs).
#[derive(Clone, Debug)]
pub enum CallResponse {
    /// The acknowledgement of a registered call's acceptance.
    Acknowledgement(AcceptanceAcknowledgement),
    /// An offered invitation's receipt.
    Receipt(InvitationReceipt),
}

impl From<AcceptanceAcknowledgement> for CallResponse {
    fn from(acknowledgement: AcceptanceAcknowledgement) -> Self {
        Self::Acknowledgement(acknowledgement)
    }
}

impl From<InvitationReceipt> for CallResponse {
    fn from(receipt: InvitationReceipt) -> Self {
        Self::Receipt(receipt)
    }
}

/// Why a call delivery response wasn't sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RespondFailure {
    /// The connection the delivery came on is down (or the client stopped): nothing is sent on another.
    #[error("the delivery's connection is gone")]
    Gone,
    /// No delivery of the connection waits for this response (one no fence observed as answerable, answered already,
    /// another connection's).
    #[error("no delivery of the connection waits for the response")]
    NotPending,
    /// The fence denied it (logged).
    #[error("the fence denied the delivery response: {0}")]
    Denied(DeliveryDenyReason),
    /// It couldn't be logged or written; the connection ends.
    #[error("the response couldn't be logged or written")]
    Failed,
}

/// A response on its way to its connection, and where its outcome goes.
pub(crate) struct ResponseRequest {
    pub(crate) response: CallResponse,
    pub(crate) done: oneshot::Sender<Result<(), RespondFailure>>,
}

/// The way to answer call deliveries on the one connection they came on. Cloned freely; once that connection is down
/// every response fails with [`RespondFailure::Gone`].
#[derive(Clone)]
pub struct CallResponder(mpsc::UnboundedSender<ResponseRequest>);

impl fmt::Debug for CallResponder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallResponder")
            .field("open", &!self.0.is_closed())
            .finish()
    }
}

impl CallResponder {
    /// A responder and the receiving end its connection reads.
    pub(crate) fn channel() -> (Self, mpsc::UnboundedReceiver<ResponseRequest>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self(sender), receiver)
    }

    /// Sends `response` on this responder's connection, once the fence permitted exactly its JSON for that connection
    /// (`teams_proto::ResponseDecider::decide`, then `teams_proto::FrameSink::send_permitted`), and waits for the
    /// outcome.
    ///
    /// # Errors
    /// [`RespondFailure`]; nothing is sent but on success.
    pub async fn respond(&self, response: impl Into<CallResponse>) -> Result<(), RespondFailure> {
        let (done, outcome) = oneshot::channel();
        let request = ResponseRequest {
            response: response.into(),
            done,
        };
        self.0.send(request).map_err(|_| RespondFailure::Gone)?;
        outcome.await.unwrap_or(Err(RespondFailure::Gone))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use base64::Engine as _;
    use serde_json::{Map, Value, json};

    use super::{MAX_DELIVERY, read_delivery};
    use crate::frames::RawDelivery;

    fn gzip(bytes: &[u8]) -> String {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        base64::engine::general_purpose::STANDARD.encode(encoder.finish().unwrap())
    }

    fn raw(headers: &Value, body: String) -> RawDelivery {
        RawDelivery {
            id: -7,
            method: "POST".to_owned(),
            url: "/v4/f/Endpoint0000000000/callAgent/agent/1/call/acceptance/".to_owned(),
            headers: headers.as_object().cloned().unwrap_or_else(Map::new),
            body,
        }
    }

    #[test]
    fn a_gzip_body_is_read_decoded_and_the_echoed_headers_kept() {
        let headers = json!({
            "x-microsoft-skype-content-encoding": "GZIP",
            "trouter-request": r#"{"id":"r"}"#,
            "X-Microsoft-Skype-Chain-ID": "chain",
            "X-Microsoft-Skype-Message-ID": "message",
        });
        let delivery = read_delivery("b/1", &raw(&headers, gzip(br#"{"callAcceptance":{}}"#))).unwrap();
        assert_eq!(delivery.body, br#"{"callAcceptance":{}}"#);
        assert_eq!((delivery.connection.as_str(), delivery.id), ("b/1", -7));
        assert_eq!(delivery.trouter_request.as_deref(), Some(r#"{"id":"r"}"#));
        assert_eq!(delivery.chain_id.as_deref(), Some("chain"));
        assert_eq!(delivery.message_id.as_deref(), Some("message"));
    }

    #[test]
    fn a_plain_body_is_read_as_it_came() {
        let delivery = read_delivery("b/1", &raw(&json!({}), r#"{"evt":107}"#.to_owned())).unwrap();
        assert_eq!(delivery.body, br#"{"evt":107}"#);
        assert_eq!(delivery.trouter_request, None);
    }

    #[test]
    fn a_gzip_body_that_does_not_decode_or_is_too_large_is_no_delivery() {
        let headers = json!({"X-Microsoft-Skype-Content-Encoding": "gzip"});
        assert!(read_delivery("b/1", &raw(&headers, "not base64!".to_owned())).is_none());
        let not_gzip = base64::engine::general_purpose::STANDARD.encode(b"{}");
        assert!(read_delivery("b/1", &raw(&headers, not_gzip)).is_none());
        let limit = usize::try_from(MAX_DELIVERY).unwrap();
        assert!(read_delivery("b/1", &raw(&headers, gzip(&vec![b' '; limit + 1]))).is_none());
        assert!(read_delivery("b/1", &raw(&headers, gzip(&vec![b' '; limit]))).is_some());
    }
}
