//! Calling deliveries decoded for the call tracker and answered by typed replies on their originating socket.

use std::fmt;
use std::io::Read as _;
use std::sync::Arc;

use base64::Engine as _;

use teams_call::{Delivery, Observed, Tracker};
use teams_proto::{AcceptanceAcknowledgement, InvitationReceipt};
use tokio::sync::{mpsc, oneshot};

use crate::frames::RawDelivery;

/// The most a call delivery's body may hold once decoded (the socket reads no larger frame either).
pub const MAX_DELIVERY: u64 = crate::socket::MAX_INCOMING as u64;

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

/// The calling socket's tracker, optionally coordinated with the app's other accounts.
#[derive(Clone)]
pub struct CallStack {
    observe: Arc<dyn Fn(&Delivery) -> Observed + Send + Sync>,
}

impl CallStack {
    /// Observes deliveries through this account's call tracker.
    #[must_use]
    pub fn new(tracker: Arc<Tracker>) -> Self {
        Self {
            observe: Arc::new(move |delivery| tracker.observe_delivery(delivery)),
        }
    }
    /// Installs the app's cross-account invitation coordinator.
    #[must_use]
    pub fn observing(mut self, observe: impl Fn(&Delivery) -> Observed + Send + Sync + 'static) -> Self {
        self.observe = Arc::new(observe);
        self
    }
    pub(crate) fn observed(&self, delivery: &Delivery) -> Observed {
        (self.observe)(delivery)
    }
}

impl fmt::Debug for CallStack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallStack").finish_non_exhaustive()
    }
}

/// A decoded calling delivery with its tracker result and a responder for its originating socket.
#[derive(Debug)]
pub struct CallDelivery {
    /// The decoded body and correlation fields from the originating connection.
    pub delivery: Delivery,
    /// What the call tracker learned or offered from this delivery.
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
    #[error("no delivery of the connection waits for the response")]
    NotPending,
    #[error("the delivery belongs to another connection")]
    OtherConnection,
    /// The socket couldn't write it; the connection ends.
    #[error("the response couldn't be written")]
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

    /// Sends the typed response on this responder's connection.
    ///
    /// # Errors
    /// Returns Gone when disconnected, or a connection/write failure.
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
