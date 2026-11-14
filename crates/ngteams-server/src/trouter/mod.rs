//! The push channel: Trouter (`docs/protocol/trouter.md`), socket.io 0.9 over a WebSocket at `/trouter/v4/c`.
//!
//! A socket authenticates in its first frame with the account's `ic3` token and learns its endpoint
//! (`trouter.connected`). The endpoint's id is derived from the account, the client's `epid` and its stack, so the
//! same client gets the same endpoint on every connect, on any node. Once the registrar has the endpoint's
//! registration for the messaging stack ([`crate::registrar`]), the account's chat events (`push:<mri>` on the
//! fan-out) are delivered to it: those the journal keeps from the endpoint's cursor on, so what it missed while it
//! was away is replayed (as stored deliveries) and what the journal no longer holds is reported
//! (`trouter.message_loss`); typing only to the sockets open now.
//!
//! Nodes tell an account's sockets apart from its events on the control topic `trouter:<mri>`: a registration the
//! registrar took, a newer socket of the same endpoint (the older one closes), an event the journal couldn't keep
//! (the clients are told of a loss), a disconnect.
//!
//! A socket is its account's only while the account may sign in with the password its token was issued under (the
//! token's `pwd_ver`): one that is disabled or given a new password is disconnected as a refused socket is
//! ([`revoke`]), and a token from before the new password opens none. The word alone would not do: a node without
//! Redis isn't reached by a command run beside it, and a word can pass a socket that is just connecting. So a socket
//! reads its account once it listens, before each delivery, and, where there is no Redis, every second, a replay
//! under way or not.

mod delivery;
mod frames;
mod socket;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::http::header::{CONNECTION, ORIGIN, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_VERSION, UPGRADE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine as _;
use bytes::Bytes;
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::sync::watch;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

pub use delivery::keep;

use crate::app::App;
use crate::fanout::{Fanout, FanoutError};

/// The connect path.
pub const CONNECT_PATH: &str = "/trouter/v4/c";
/// The sockets one account may hold open on a node.
pub const MAX_SOCKETS: usize = 32;
/// The reason a socket is disconnected with when its token or its account doesn't hold.
pub const UNAUTHORIZED: &str = "unauthorized";
/// The largest frame a socket reads.
const MAX_INCOMING: usize = 256 * 1024;
/// The ping interval a connect may name, in seconds.
const PING_SECONDS: std::ops::RangeInclusive<u64> = 5..=300;

/// Which of the client's two stacks an endpoint belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stack {
    /// `TeamsCDL`, registered as `TeamsCDLWebWorker`: chat deliveries.
    Messaging,
    /// `SkypeSpaces`, registered as `SkypeSpacesWeb`: call deliveries.
    Calling,
}

impl Stack {
    /// The app the stack registers its endpoint as.
    #[must_use]
    pub fn app_id(self) -> &'static str {
        match self {
            Self::Messaging => "TeamsCDLWebWorker",
            Self::Calling => "SkypeSpacesWeb",
        }
    }

    /// The stack that registers as `app_id`.
    #[must_use]
    pub fn of_app(app_id: &str) -> Option<Self> {
        [Self::Messaging, Self::Calling]
            .into_iter()
            .find(|stack| stack.app_id() == app_id)
    }
}

/// The id of the endpoint `epid` of the account `oid` on `stack`: 22 base64url characters, as Teams' are.
#[must_use]
pub fn endpoint_id(oid: &str, epid: &str, stack: Stack) -> String {
    let mut hash = Sha256::new();
    for part in ["trouter-endpoint", oid, epid, stack.app_id()] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&hash.finalize()[..16])
}

/// The push URL of `endpoint` (`surl`): what its registration names.
#[must_use]
pub fn push_url(origin: &str, endpoint: &str) -> String {
    format!("{origin}/trouter/v4/f/{endpoint}/")
}

/// A GUID as the client writes its `epid`: `8-4-4-4-12` lowercase hex digits.
#[must_use]
pub fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups.iter().all(|group| {
            group
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

/// The topic an account's sockets are told on, apart from its events.
#[must_use]
pub fn control_topic(mri: &str) -> String {
    format!("trouter:{mri}")
}

/// What an account's sockets are told on their control topic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Control {
    /// The registrar took `endpoint`'s registration, good until `until` (Unix seconds).
    Registered { endpoint: String, until: i64 },
    /// The socket `socket` serves `endpoint` since `at` (Unix milliseconds): the endpoint's sockets from before it
    /// close, and one from after it says so in turn.
    Replaced { endpoint: String, socket: String, at: u64 },
    /// An event of the account wasn't kept in its journal: its messaging sockets tell their clients of a loss.
    Lost,
    /// Every socket of the account closes, telling the client `reason` when there is one.
    Disconnect { reason: Option<String> },
}

/// Tells `mri`'s sockets on every node.
///
/// # Errors
/// The fan-out is unavailable.
pub async fn tell(app: &App, mri: &str, control: &Control) -> Result<(), FanoutError> {
    let payload = serde_json::to_vec(control).unwrap_or_default();
    app.fanout.publish(&control_topic(mri), payload.into()).await
}

/// The fan-out's word ([`crate::fanout::journal::Unkept`]) for an account's event its journal didn't keep.
#[must_use]
pub fn unkept(topic: &str) -> Option<(String, Bytes)> {
    let mri = topic.strip_prefix("push:")?;
    let lost = serde_json::to_vec(&Control::Lost).ok()?;
    Some((control_topic(mri), lost.into()))
}

/// Disconnects `mri`'s sockets on every node, with `reason` as the client reads it: `unauthorized` makes it come
/// back at once with a new token and `dup` at once with the one it has, `skypetoken-deprecated` stops it for good,
/// and after any other it waits out its backoff.
///
/// # Errors
/// The fan-out is unavailable.
pub async fn disconnect(app: &App, mri: &str, reason: Option<&str>) -> Result<(), FanoutError> {
    let reason = reason.map(str::to_owned);
    tell(app, mri, &Control::Disconnect { reason }).await
}

/// Disconnects the sockets of the account `oid` on every node `fanout` reaches, as a socket whose token is refused
/// is: the client comes back with a new token, which a disabled account or one with a new password doesn't get.
///
/// # Errors
/// The fan-out is unavailable.
pub async fn revoke(fanout: &dyn Fanout, oid: &str) -> Result<(), FanoutError> {
    let control = Control::Disconnect {
        reason: Some(UNAUTHORIZED.to_owned()),
    };
    let payload = serde_json::to_vec(&control).unwrap_or_default();
    fanout
        .publish(&control_topic(&format!("8:orgid:{oid}")), payload.into())
        .await
}

/// A node's open sockets: how many each account holds, and the word to close them all.
#[derive(Debug)]
pub struct Hub {
    sockets: Mutex<HashMap<String, usize>>,
    closing: watch::Sender<bool>,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            sockets: Mutex::default(),
            closing: watch::Sender::new(false),
        }
    }
}

impl Hub {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Closes every socket of the node (`0::`), and each one opened from now on: the node is shutting down.
    pub fn close_all(&self) {
        self.closing.send_replace(true);
    }

    /// The sockets open on the node now.
    #[must_use]
    pub fn open(&self) -> usize {
        self.sockets().values().sum()
    }

    fn sockets(&self) -> std::sync::MutexGuard<'_, HashMap<String, usize>> {
        self.sockets.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Counts a socket of `mri` in until the guard is dropped; `None` when the account holds [`MAX_SOCKETS`].
    fn enter(self: &Arc<Self>, mri: &str) -> Option<Entered> {
        let mut sockets = self.sockets();
        let count = sockets.entry(mri.to_owned()).or_default();
        if *count >= MAX_SOCKETS {
            return None;
        }
        *count += 1;
        Some(Entered {
            hub: Arc::clone(self),
            mri: mri.to_owned(),
        })
    }
}

/// A socket counted in a [`Hub`].
struct Entered {
    hub: Arc<Hub>,
    mri: String,
}

impl Drop for Entered {
    fn drop(&mut self) {
        let mut sockets = self.hub.sockets();
        if let Some(count) = sockets.get_mut(&self.mri) {
            *count -= 1;
            if *count == 0 {
                sockets.remove(&self.mri);
            }
        }
    }
}

pub(crate) fn router() -> Router<Arc<App>> {
    Router::new().route(CONNECT_PATH, get(connect))
}

/// What a connect's query says: the client's stack (`tc`'s `ua`), its endpoint (`epid`) and its ping interval.
fn connect_of(query: &HashMap<String, String>) -> Option<socket::Connect> {
    let epid = query.get("epid").filter(|epid| is_guid(epid))?.clone();
    let calling = query
        .get("tc")
        .and_then(|tc| serde_json::from_str::<serde_json::Value>(tc).ok())
        .is_some_and(|tc| tc["ua"] == "SkypeSpaces");
    let ping = query
        .get("timeout")
        .and_then(|timeout| timeout.parse::<u64>().ok())
        .filter(|seconds| PING_SECONDS.contains(seconds))
        .unwrap_or(40);
    Some(socket::Connect {
        epid,
        stack: if calling { Stack::Calling } else { Stack::Messaging },
        ping: Duration::from_secs(ping),
    })
}

fn header<'a>(headers: &'a HeaderMap, name: &axum::http::HeaderName) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

/// `GET /trouter/v4/c`: the WebSocket handshake. It carries no credential; a browser page of another origin is
/// refused all the same.
async fn connect(
    State(app): State<Arc<App>>,
    Query(query): Query<HashMap<String, String>>,
    mut request: Request,
) -> Response {
    let headers = request.headers();
    let upgrade = header(headers, &UPGRADE).eq_ignore_ascii_case("websocket")
        && header(headers, &SEC_WEBSOCKET_VERSION) == "13"
        && !header(headers, &SEC_WEBSOCKET_KEY).is_empty();
    if !upgrade {
        return StatusCode::UPGRADE_REQUIRED.into_response();
    }
    let origin = header(headers, &ORIGIN);
    if !origin.is_empty() && origin != app.config.origin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(connect) = connect_of(&query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let accept = derive_accept_key(header(headers, &SEC_WEBSOCKET_KEY).as_bytes());
    let Some(upgrading) = request.extensions_mut().remove::<hyper::upgrade::OnUpgrade>() else {
        return StatusCode::UPGRADE_REQUIRED.into_response();
    };
    tokio::spawn(async move {
        let Ok(upgraded) = upgrading.await else { return };
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_INCOMING))
            .max_frame_size(Some(MAX_INCOMING));
        let socket = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config)).await;
        socket::serve(app, socket, connect).await;
    });
    (
        StatusCode::SWITCHING_PROTOCOLS,
        [
            (UPGRADE, "websocket".to_owned()),
            (CONNECTION, "upgrade".to_owned()),
            (SEC_WEBSOCKET_ACCEPT, accept),
        ],
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_endpoint_is_the_accounts_epid_on_its_stack() {
        let epid = "00000000-0000-4000-8000-000000000001";
        let id = endpoint_id("a", epid, Stack::Messaging);
        assert_eq!(id.len(), 22);
        assert_eq!(id, endpoint_id("a", epid, Stack::Messaging));
        assert_ne!(id, endpoint_id("a", epid, Stack::Calling));
        assert_ne!(id, endpoint_id("b", epid, Stack::Messaging));
        assert_ne!(
            endpoint_id("ab", "c", Stack::Messaging),
            endpoint_id("a", "bc", Stack::Messaging)
        );
        assert_eq!(Stack::of_app("SkypeSpacesWeb"), Some(Stack::Calling));
        assert_eq!(Stack::of_app("TeamsCDLWebWorker"), Some(Stack::Messaging));
        assert_eq!(Stack::of_app("Other"), None);
    }

    #[test]
    fn only_an_accounts_events_have_a_word_for_one_that_was_not_kept() {
        let (topic, word) = unkept("push:8:orgid:a").unwrap();
        assert_eq!(topic, "trouter:8:orgid:a");
        assert_eq!(serde_json::from_slice::<Control>(&word).unwrap(), Control::Lost);
        assert!(unkept("trouter:8:orgid:a").is_none());
    }

    #[test]
    fn a_connect_names_its_endpoint_stack_and_ping() {
        let query = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect::<HashMap<_, _>>()
        };
        let epid = "00000000-0000-4000-8000-00000000000a";
        let messaging = connect_of(&query(&[
            ("epid", epid),
            ("tc", r#"{"ua":"TeamsCDL"}"#),
            ("timeout", "7"),
        ]))
        .unwrap();
        assert_eq!(messaging.stack, Stack::Messaging);
        assert_eq!(messaging.ping, Duration::from_secs(7));
        let calling = connect_of(&query(&[
            ("epid", epid),
            ("tc", r#"{"ua":"SkypeSpaces"}"#),
            ("timeout", "1"),
        ]))
        .unwrap();
        assert_eq!(calling.stack, Stack::Calling);
        assert_eq!(calling.ping, Duration::from_secs(40));
        assert!(connect_of(&query(&[("epid", "00000000-0000-4000-8000-00000000000A")])).is_none());
        assert!(connect_of(&query(&[])).is_none());
    }

    #[test]
    fn a_hub_counts_an_accounts_sockets_up_to_the_cap() {
        let hub = Arc::new(Hub::new());
        let held: Vec<_> = (0..MAX_SOCKETS).map(|_| hub.enter("a").unwrap()).collect();
        assert!(hub.enter("a").is_none());
        let other = hub.enter("b").unwrap();
        assert_eq!(hub.open(), MAX_SOCKETS + 1);
        drop(held);
        drop(other);
        assert_eq!(hub.open(), 0);
        assert!(hub.enter("a").is_some());
    }
}
