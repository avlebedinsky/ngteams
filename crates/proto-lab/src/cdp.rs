//! A thin Chrome `DevTools` Protocol client over one browser-level websocket, in flat session mode.
//!
//! Commands carry an optional `sessionId`, so one connection drives the browser and every attached target (pages,
//! iframes, dedicated, shared and service workers). Responses are matched to their commands by id; events (with the
//! session they came from) go out in arrival order through [`Events`]. Nothing here acts on an event by itself: a
//! target that auto-attaches with `waitForDebuggerOnStart` stays paused until the caller resumes it with
//! `Runtime.runIfWaitingForDebugger`, which is what the browser fence needs to arm a target before its code runs.
//!
//! The connection lives as long as a [`Cdp`] or [`Session`] handle does: dropping the last one closes the websocket, so
//! the browser sees the client detach (and releases whatever it held for it), and [`Events`] then ends.
//!
//! Commands and events are untyped JSON (`serde_json::Value`): the lab uses a handful of methods and needs whatever
//! fields the browser sends, including ones newer than any generated protocol crate.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

/// A flat-mode session id: one attached target.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A protocol event, with the session it came from (`None`: the browser target itself).
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub session_id: Option<SessionId>,
    pub method: String,
    pub params: Value,
}

/// An error the browser returned for a command.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, thiserror::Error)]
#[error("{message} ({code})")]
pub struct RemoteError {
    pub code: i64,
    pub message: String,
}

/// One message from the browser.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Response {
        id: u64,
        outcome: Result<Value, RemoteError>,
    },
    Event(Event),
}

/// Why a CDP exchange failed.
#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    #[error("can't connect to the debugging endpoint: {0}")]
    Connect(#[source] Box<tokio_tungstenite::tungstenite::Error>),
    #[error("the debugging connection is closed")]
    Closed,
    #[error("{method} failed: {error}")]
    Remote { method: String, error: RemoteError },
    #[error("a malformed DevTools message: {0}")]
    Malformed(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawIncoming {
    id: Option<u64>,
    result: Option<Value>,
    error: Option<RemoteError>,
    method: Option<String>,
    params: Option<Value>,
    session_id: Option<SessionId>,
}

/// Decodes one message from the browser.
///
/// # Errors
/// [`CdpError::Malformed`] when the text isn't JSON, or is neither a response (`id`) nor an event (`method`).
pub fn parse_incoming(text: &str) -> Result<Incoming, CdpError> {
    let raw: RawIncoming = serde_json::from_str(text).map_err(|error| CdpError::Malformed(error.to_string()))?;
    match (raw.id, raw.method) {
        (Some(id), _) => {
            let outcome = match raw.error {
                Some(error) => Err(error),
                None => Ok(raw.result.unwrap_or(Value::Null)),
            };
            Ok(Incoming::Response { id, outcome })
        }
        (None, Some(method)) => Ok(Incoming::Event(Event {
            session_id: raw.session_id,
            method,
            params: raw.params.unwrap_or(Value::Null),
        })),
        (None, None) => Err(CdpError::Malformed("neither an id nor a method".to_owned())),
    }
}

/// Encodes one command. `params` of `null` is sent as an empty object.
#[must_use]
pub fn encode_command(id: u64, session: Option<&SessionId>, method: &str, params: Value) -> String {
    let params = if params.is_null() { json!({}) } else { params };
    let mut command = json!({ "id": id, "method": method, "params": params });
    if let (Some(session), Some(object)) = (session, command.as_object_mut()) {
        object.insert("sessionId".to_owned(), Value::String(session.0.clone()));
    }
    command.to_string()
}

type Reply = oneshot::Sender<Result<Value, RemoteError>>;

struct Pending {
    replies: HashMap<u64, Reply>,
    /// The connection is gone: no reply will ever come.
    closed: bool,
}

/// The commands waiting for a reply. The reader task holds only this, never the outgoing sender, so the connection
/// closes once every client handle is gone.
struct Replies(Mutex<Pending>);

impl Replies {
    fn pending(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Delivers a response; an id nobody waits for (a dropped caller) is ignored.
    fn resolve(&self, id: u64, outcome: Result<Value, RemoteError>) {
        if let Some(reply) = self.pending().replies.remove(&id) {
            // the caller may have given up; nothing to do then
            let _ = reply.send(outcome);
        }
    }

    /// Marks the connection closed and fails every waiting command.
    fn close(&self) {
        let mut pending = self.pending();
        pending.closed = true;
        pending.replies.clear();
    }
}

/// What the client handles share. Dropping the last handle drops `outgoing`, which ends the writer task and closes the
/// websocket.
struct Inner {
    outgoing: mpsc::UnboundedSender<String>,
    replies: Arc<Replies>,
    next_id: AtomicU64,
}

/// The events of one connection, in arrival order. It ends when the connection closes, which happens once every
/// [`Cdp`] and [`Session`] handle is dropped.
pub struct Events {
    receiver: mpsc::UnboundedReceiver<Event>,
}

impl Events {
    /// The next event, or `None` once the connection is closed and every event was taken.
    pub async fn recv(&mut self) -> Option<Event> {
        self.receiver.recv().await
    }
}

/// A handle to one debugging connection. Clones (and [`Session`]s) share the connection, which closes when the last of
/// them is dropped.
#[derive(Clone)]
pub struct Cdp {
    inner: Arc<Inner>,
}

impl Cdp {
    /// Connects to a browser's debugging websocket (`ws://127.0.0.1:<port>/devtools/browser/<id>`).
    ///
    /// # Errors
    /// [`CdpError::Connect`] when the WebSocket handshake fails.
    pub async fn connect(ws_url: &str) -> Result<(Self, Events), CdpError> {
        let (stream, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|error| CdpError::Connect(Box::new(error)))?;
        let (mut sink, mut source) = stream.split();
        let (outgoing, mut to_send) = mpsc::unbounded_channel::<String>();
        let (events, receiver) = mpsc::unbounded_channel();
        let replies = Arc::new(Replies(Mutex::new(Pending {
            replies: HashMap::new(),
            closed: false,
        })));
        let inner = Arc::new(Inner {
            outgoing,
            replies: Arc::clone(&replies),
            next_id: AtomicU64::new(1),
        });
        // ends when the last handle (the only holder of `outgoing`) is dropped, or when the socket breaks
        tokio::spawn(async move {
            while let Some(text) = to_send.recv().await {
                if sink.send(Message::text(text)).await.is_err() {
                    break;
                }
            }
            // closing is best effort; the browser answers with its own close frame, which ends the reader
            let _ = sink.close().await;
        });
        let reader = replies;
        tokio::spawn(async move {
            while let Some(Ok(message)) = source.next().await {
                if !dispatch(&reader, &events, &message) {
                    break;
                }
            }
            reader.close();
        });
        Ok((Self { inner }, Events { receiver }))
    }

    /// Sends a command to the browser (`session` `None`) or to one attached target, and waits for its result.
    ///
    /// # Errors
    /// [`CdpError::Remote`] when the browser answers with an error, [`CdpError::Closed`] when the connection is gone.
    pub async fn call(&self, session: Option<&SessionId>, method: &str, params: Value) -> Result<Value, CdpError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, response) = oneshot::channel();
        {
            let mut pending = self.inner.replies.pending();
            if pending.closed {
                return Err(CdpError::Closed);
            }
            pending.replies.insert(id, reply);
        }
        if self
            .inner
            .outgoing
            .send(encode_command(id, session, method, params))
            .is_err()
        {
            self.inner.replies.pending().replies.remove(&id);
            return Err(CdpError::Closed);
        }
        match response.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(CdpError::Remote {
                method: method.to_owned(),
                error,
            }),
            Err(_) => Err(CdpError::Closed),
        }
    }

    /// A handle that sends every command to one attached target.
    #[must_use]
    pub fn session(&self, id: SessionId) -> Session {
        Session { cdp: self.clone(), id }
    }
}

/// Routes one WebSocket message; `false` once the connection is over.
fn dispatch(replies: &Replies, events: &mpsc::UnboundedSender<Event>, message: &Message) -> bool {
    let text = match message {
        Message::Text(text) => text.as_str(),
        Message::Close(_) => return false,
        _ => return true,
    };
    match parse_incoming(text) {
        Ok(Incoming::Response { id, outcome }) => replies.resolve(id, outcome),
        Ok(Incoming::Event(event)) => {
            // nobody listens any more; the responses still matter
            let _ = events.send(event);
        }
        // the browser never sends these; one bad message doesn't end the connection
        Err(_) => {}
    }
    true
}

/// A handle to one attached target (flat mode).
#[derive(Clone)]
pub struct Session {
    cdp: Cdp,
    id: SessionId,
}

impl Session {
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// Sends a command to this target.
    ///
    /// # Errors
    /// As [`Cdp::call`].
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        self.cdp.call(Some(&self.id), method, params).await
    }
}
