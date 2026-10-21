//! The mock's Trouter (M1 Task 16): a WebSocket server on loopback speaking the push channel as M1 Task 14's
//! captures recorded it (`docs/protocol/trouter.md`, the synthetic `tests/fixtures/trouter/` fixtures), and the
//! registrar's registration of an endpoint, on the chat service mock's own origin.
//!
//! - **Connect**: `GET /v4/c?…` (the query kept for the test to read); the mock answers `1::` and waits
//!   [`TrouterMock::set_auth_timeout`] for `user.authenticate` as the first frame. Late, it closes with
//!   `0:::{"reason":"timeout"}` (seen live); with headers other than the messaging stack's three, or a token it didn't
//!   hand out, with `0:::{"reason":"unauthorized"}` (the bundle's reason).
//! - **`trouter.connected`**, shaped like `session.json`'s: the endpoint id (the one the `connectparams`' `sr` names
//!   when the account had it before, else a new one), push URLs and the `reconnectUrl` on a made-up regional host
//!   (`mock-zz-01`), and `connectparams`. A connect of an endpoint seen before also gets the fixture's
//!   `trouter.message_loss` report (as on reconnects live).
//! - **Keep-alives**: `5:<n>+::{"name":"ping"}` is answered `6:::<n>+["pong"]` (unless [`TrouterMock::set_pongs`]
//!   turned that off), any other acknowledged event `6:::<n>+[]`; `0::` closes the socket.
//! - **Deliveries**: once a registration names the socket's endpoint, the chat service mock's queued events for the
//!   account are taken and delivered as `3:::{…}` frames shaped like `deliveries.json`'s: a message as `NewMessage`,
//!   typing as `NewMessage` of `Control/Typing` (not stored), an edit, a delete or a reaction as `MessageUpdate`, the
//!   message as a read gives it. Read receipts and chat creation stay queued
//!   ([`crate::ChatServiceMock::take_events`]): no capture showed what another member is told of them. Every delivered
//!   event but typing is **stored**: each later connect of the account replays them (`"mstore":true`, the same `id`),
//!   as the service replays unanswered deliveries; the client never answers, so they never leave the store.
//! - **Registrar**: `POST /registrar/prod/V2/registrations` with the account's token, `X-MS-Migration: True` and the
//!   messaging stack's registration (`registration.json`'s shape) is answered `202` with no body and recorded; another
//!   body is a `400`, a missing credential a `401`.
//!
//! Test hooks: every socket's record ([`TrouterMock::sockets`]: the account, the endpoint, the query, every frame the
//! client sent), dropping an account's sockets, a service disconnect with a reason, an incoming call's delivery.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Map, Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request as Handshake, Response as HandshakeAnswer,
};
use wiremock::{Request, ResponseTemplate};

use crate::world::{State, iso_time};
use crate::{ChatEvent, EventKind};

const SESSION: &str = include_str!("../../../tests/fixtures/trouter/session.json");
const DELIVERIES: &str = include_str!("../../../tests/fixtures/trouter/deliveries.json");

/// The made-up regional name of the mock's Trouter hosts.
pub const REGIONAL_NAME: &str = "mock-zz-01";

/// An endpoint registered with the registrar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    /// The account the registration's token is.
    pub account: String,
    /// The `registrationId` (the client's `epid`).
    pub registration_id: String,
    /// The push URL it names.
    pub path: String,
    pub ttl: u64,
}

/// What the mock knows of one socket.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SocketRecord {
    /// The connect's query, as it came.
    pub query: String,
    /// The account its `user.authenticate` named, once it did.
    pub account: Option<String>,
    /// The endpoint id the mock handed over.
    pub endpoint: Option<String>,
    /// Whether its `user.authenticate` carried `connectparams` (a reconnect).
    pub reconnect: bool,
    /// Every text frame the client sent on it, in order.
    pub frames: Vec<String>,
    /// Still open.
    pub open: bool,
}

/// A command a test gives an account's open sockets.
#[derive(Clone, Debug)]
enum Command {
    /// Close the TCP connection without a word.
    Drop,
    /// `0:::{"reason":…}`, then close.
    Disconnect(String),
    /// A delivery of this body at this path under the endpoint's push URL.
    Deliver { path: String, body: String },
}

/// One delivery the account's endpoints get again on every connect.
#[derive(Clone, Debug)]
struct Stored {
    id: u64,
    frame_body: Value,
    headers: Map<String, Value>,
}

/// The Trouter mock's own state, beside the chat world's.
#[derive(Default)]
struct Sockets {
    records: Vec<SocketRecord>,
    commands: Vec<(usize, String, mpsc::UnboundedSender<Command>)>,
    /// Endpoint ids handed out, per account.
    endpoints: BTreeMap<String, Vec<String>>,
    stored: BTreeMap<String, Vec<Stored>>,
    next_endpoint: u64,
    next_delivery: u64,
    no_pongs: bool,
    auth_timeout: Option<Duration>,
}

struct Shared {
    world: Arc<Mutex<State>>,
    sockets: Mutex<Sockets>,
    templates: Templates,
}

impl Shared {
    fn sockets(&self) -> MutexGuard<'_, Sockets> {
        self.sockets.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn world(&self) -> MutexGuard<'_, State> {
        self.world.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The frames the mock's answers are shaped from.
struct Templates {
    connected: Value,
    message_loss: String,
    headers: Map<String, Value>,
}

impl Templates {
    fn load() -> std::io::Result<Self> {
        let invalid =
            |what: &str| std::io::Error::other(format!("a Trouter fixture isn't as the mock expects: {what}"));
        let session: Value = serde_json::from_str(SESSION).map_err(|_| invalid("session.json"))?;
        let frame = |name: &str| {
            session["frames"]
                .as_array()
                .and_then(|frames| frames.iter().find(|frame| frame["name"] == name))
                .and_then(|frame| frame["frame"].as_str())
                .map(str::to_owned)
        };
        let connected = frame("the endpoint and its push URLs")
            .and_then(|text| serde_json::from_str::<Value>(text.split_once("::")?.1).ok())
            .ok_or_else(|| invalid("session.json: trouter.connected"))?;
        let message_loss = frame("the service's report of what the endpoint missed while away (sent more than once)")
            .ok_or_else(|| invalid("session.json: trouter.message_loss"))?;
        let deliveries: Value = serde_json::from_str(DELIVERIES).map_err(|_| invalid("deliveries.json"))?;
        let headers = deliveries["deliveries"][1]["frame"]
            .as_str()
            .and_then(|text| text.strip_prefix("3:::"))
            .and_then(|json| serde_json::from_str::<Value>(json).ok())
            .and_then(|delivery| delivery["headers"].as_object().cloned())
            .ok_or_else(|| invalid("deliveries.json: a message's headers"))?;
        Ok(Self {
            connected,
            message_loss,
            headers,
        })
    }
}

/// The running Trouter mock: a WebSocket server on loopback on its own thread. Dropping it stops serving.
pub struct TrouterMock {
    origin: String,
    shared: Arc<Shared>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for TrouterMock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrouterMock")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl Drop for TrouterMock {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl TrouterMock {
    /// Starts the mock's Trouter over the chat world `world`, on a free loopback port.
    pub(crate) fn start(world: Arc<Mutex<State>>) -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let origin = format!("ws://{}", listener.local_addr()?);
        let shared = Arc::new(Shared {
            world,
            sockets: Mutex::default(),
            templates: Templates::load()?,
        });
        let (shutdown, stopped) = oneshot::channel();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let serving = Arc::clone(&shared);
        let thread = std::thread::spawn(move || {
            runtime.block_on(async move {
                let Ok(listener) = TcpListener::from_std(listener) else {
                    return;
                };
                tokio::select! {
                    () = accept(listener, serving) => {}
                    _ = stopped => {}
                }
            });
        });
        Ok(Self {
            origin,
            shared,
            shutdown: Some(shutdown),
            thread: Some(thread),
        })
    }

    /// The origin clients connect to (`ws://127.0.0.1:<port>`), for the `test-support` mock origin.
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Every socket so far, in the order they connected.
    #[must_use]
    pub fn sockets(&self) -> Vec<SocketRecord> {
        self.shared.sockets().records.clone()
    }

    /// The endpoints registered so far, in order.
    #[must_use]
    pub fn registrations(&self) -> Vec<Registration> {
        self.shared.world().registrations.clone()
    }

    /// Whether pings are answered (they are unless this turned it off).
    pub fn set_pongs(&self, answer: bool) {
        self.shared.sockets().no_pongs = !answer;
    }

    /// How long the mock waits for `user.authenticate` after its `1::` (2 s unless this changed it).
    pub fn set_auth_timeout(&self, timeout: Duration) {
        self.shared.sockets().auth_timeout = Some(timeout);
    }

    /// Drops `account`'s open sockets without a word (the network failing).
    pub fn drop_sockets(&self, account: &str) {
        self.command(account, &Command::Drop);
    }

    /// Disconnects `account`'s open sockets with `reason` (`0:::{"reason":…}`), then closes them.
    pub fn disconnect(&self, account: &str, reason: &str) {
        self.command(account, &Command::Disconnect(reason.to_owned()));
    }

    /// Delivers an incoming call to `account`'s open sockets: a body with a `callNotification` (the bundle's sign of
    /// one) at a call agent's path under the push URL. The client must never answer it.
    pub fn deliver_incoming_call(&self, account: &str) {
        let body = json!({"callNotification": {"from": "8:orgid:mock-caller", "callId": "mock-call-1"}});
        self.command(
            account,
            &Command::Deliver {
                path: "callAgent/mock-agent/1/incomingCall".to_owned(),
                body: body.to_string(),
            },
        );
    }

    fn command(&self, account: &str, command: &Command) {
        let sockets = self.shared.sockets();
        for (_, owner, commands) in &sockets.commands {
            if owner == account {
                let _ = commands.send(command.clone());
            }
        }
    }
}

async fn accept(listener: TcpListener, shared: Arc<Shared>) {
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(serve(stream, Arc::clone(&shared)));
    }
}

/// One socket, from its handshake to its close.
async fn serve(stream: TcpStream, shared: Arc<Shared>) {
    let mut query = String::new();
    let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(stream, QueryOf(&mut query)).await else {
        return;
    };
    let index = {
        let mut sockets = shared.sockets();
        sockets.records.push(SocketRecord {
            query,
            open: true,
            ..SocketRecord::default()
        });
        sockets.records.len() - 1
    };
    if socket.send(Message::text("1::")).await.is_ok()
        && let Some(account) = authenticate(&mut socket, &shared, index).await
    {
        connected(socket, &shared, index, &account).await;
    }
    let mut sockets = shared.sockets();
    if let Some(record) = sockets.records.get_mut(index) {
        record.open = false;
    }
    sockets.commands.retain(|(socket, ..)| *socket != index);
}

type Socket = WebSocketStream<TcpStream>;

/// Keeps the connect's query, from the handshake.
struct QueryOf<'a>(&'a mut String);

impl Callback for QueryOf<'_> {
    fn on_request(self, request: &Handshake, answer: HandshakeAnswer) -> Result<HandshakeAnswer, ErrorResponse> {
        request.uri().query().unwrap_or_default().clone_into(self.0);
        Ok(answer)
    }
}

/// The next text frame from the client, recorded; `None` once the socket closed.
async fn next_text(socket: &mut Socket, shared: &Shared, index: usize) -> Option<String> {
    while let Some(Ok(message)) = socket.next().await {
        match message {
            Message::Text(text) => {
                let text = text.as_str().to_owned();
                if let Some(record) = shared.sockets().records.get_mut(index) {
                    record.frames.push(text.clone());
                }
                return Some(text);
            }
            Message::Close(_) => return None,
            _ => {}
        }
    }
    None
}

/// Waits for `user.authenticate` as the first frame and checks it: the account whose token it carries, with its
/// endpoint id; `None` (the socket told why and closed) otherwise.
async fn authenticate(socket: &mut Socket, shared: &Shared, index: usize) -> Option<String> {
    let timeout = shared.sockets().auth_timeout.unwrap_or(Duration::from_secs(2));
    let first = tokio::time::timeout(timeout, next_text(socket, shared, index)).await;
    let Ok(Some(first)) = first else {
        let _ = socket.send(Message::text(r#"0:::{"reason":"timeout"}"#)).await;
        return None;
    };
    let Some((account, endpoint, reconnect)) = authenticated(&first, shared) else {
        let _ = socket.send(Message::text(r#"0:::{"reason":"unauthorized"}"#)).await;
        return None;
    };
    if let Some(record) = shared.sockets().records.get_mut(index) {
        record.account = Some(account.clone());
        record.endpoint = Some(endpoint.clone());
        record.reconnect = reconnect;
    }
    let seen_before = send_connected(socket, shared, &account, &endpoint).await?;
    if seen_before {
        let report = shared.templates.message_loss.clone();
        socket.send(Message::text(report)).await.ok()?;
    }
    Some(account)
}

/// The account, the endpoint id and whether the frame carried `connectparams`, if `frame` is the messaging stack's
/// `user.authenticate` with a token the mock handed out.
fn authenticated(frame: &str, shared: &Shared) -> Option<(String, String, bool)> {
    let event: Value = serde_json::from_str(frame.strip_prefix("5:::")?).ok()?;
    let argument = event["args"].as_array().filter(|args| args.len() == 1)?.first()?;
    let headers = argument["headers"].as_object()?;
    let token = headers.get("Authorization")?.as_str()?.strip_prefix("Bearer ")?;
    let messaging_shape = event["name"] == "user.authenticate"
        && headers.len() == 3
        && headers.get("X-Ms-Test-User") == Some(&json!("False"))
        && headers.get("X-MS-Migration") == Some(&json!("True"));
    if !messaging_shape {
        return None;
    }
    let account = shared.world().account_by_token(token)?.mri.clone();
    let sr = argument["connectparams"]["sr"].as_str();
    let mut sockets = shared.sockets();
    let known = sockets.endpoints.entry(account.clone()).or_default().clone();
    let endpoint = if let Some(sr) = sr.filter(|sr| known.iter().any(|known| known == sr)) {
        sr.to_owned()
    } else {
        sockets.next_endpoint += 1;
        let endpoint = format!("MockEndpoint{:06}", sockets.next_endpoint);
        sockets
            .endpoints
            .entry(account.clone())
            .or_default()
            .push(endpoint.clone());
        endpoint
    };
    Some((account, endpoint, !argument["connectparams"].is_null()))
}

/// Sends `trouter.connected` for `endpoint`: whether `account` had connected with it before.
async fn send_connected(socket: &mut Socket, shared: &Shared, account: &str, endpoint: &str) -> Option<bool> {
    let seen_before = shared
        .sockets()
        .records
        .iter()
        .filter(|record| record.account.as_deref() == Some(account) && record.endpoint.as_deref() == Some(endpoint))
        .count()
        > 1;
    let mut event = shared.templates.connected.clone();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let host = |kind: &str| format!("{REGIONAL_NAME}-{kind}.trouter.teams.microsoft.com");
    let info = &mut event["args"][0];
    info["id"] = json!(endpoint);
    info["ccid"] = json!(endpoint.trim_start_matches("Mock"));
    info["url"] = json!(format!("https://{}:8443/v4/f/{endpoint}/", host("f")));
    info["surl"] = json!(format!("https://{}:3443/v4/f/{endpoint}/", host("f")));
    info["curlb"] = json!(format!("https://{}:443", host("t")));
    info["healthUrl"] = json!(format!("https://{}:443/v4/h", host("t")));
    info["reconnectUrl"] = json!(format!("wss://{}:443/v4/c", host("t")));
    info["socketio"] = json!(format!("https://{}:443/", host("t")));
    info["connectparams"]["sr"] = json!(endpoint);
    info["connectparams"]["sp"] = json!(REGIONAL_NAME);
    info["connectparams"]["st"] = json!((now * 1000).to_string());
    info["connectparams"]["se"] = json!(((now + 7 * 86_400) * 1000).to_string());
    socket
        .send(Message::text(format!("5:1::{event}")))
        .await
        .ok()
        .map(|()| seen_before)
}

/// An authenticated socket: keep-alives answered, deliveries sent once the endpoint is registered, commands carried
/// out, until the client or a command closes it.
async fn connected(mut socket: Socket, shared: &Shared, index: usize, account: &str) {
    let (commands, mut commanded) = mpsc::unbounded_channel();
    shared.sockets().commands.push((index, account.to_owned(), commands));
    let mut changes = shared.world().changes.subscribe();
    let endpoint = shared
        .sockets()
        .records
        .get(index)
        .and_then(|record| record.endpoint.clone())
        .unwrap_or_default();
    let mut replayed = false;
    loop {
        if !replayed && registered(shared, account, &endpoint) {
            replayed = true;
            if replay(&mut socket, shared, account, &endpoint).await.is_none() {
                return;
            }
        }
        if replayed && deliver_queued(&mut socket, shared, account, &endpoint).await.is_none() {
            return;
        }
        tokio::select! {
            text = next_text(&mut socket, shared, index) => {
                let Some(text) = text else { return };
                if answer(&mut socket, shared, &text).await.is_none() {
                    return;
                }
            }
            command = commanded.recv() => {
                let Some(command) = command else { return };
                if carry_out(&mut socket, &endpoint, command).await.is_none() {
                    return;
                }
            }
            woke = changes.changed() => {
                if woke.is_err() {
                    return;
                }
            }
        }
    }
}

/// Answers a client frame: a ping's pong, any other acknowledged event's empty acknowledgement; `None` when the
/// client disconnected (`0::`) or the socket failed.
async fn answer(socket: &mut Socket, shared: &Shared, text: &str) -> Option<()> {
    if text == "0::" {
        return None;
    }
    let Some((id, event)) = text.strip_prefix("5:").and_then(|rest| rest.split_once("+::")) else {
        return Some(());
    };
    let ping = event == r#"{"name":"ping"}"#;
    if ping && shared.sockets().no_pongs {
        return Some(());
    }
    let ack = if ping {
        format!(r#"6:::{id}+["pong"]"#)
    } else {
        format!("6:::{id}+[]")
    };
    socket.send(Message::text(ack)).await.ok()
}

async fn carry_out(socket: &mut Socket, endpoint: &str, command: Command) -> Option<()> {
    match command {
        Command::Drop => None,
        Command::Disconnect(reason) => {
            let _ = socket
                .send(Message::text(format!("0:::{}", json!({"reason": reason}))))
                .await;
            None
        }
        Command::Deliver { path, body } => {
            let delivery = json!({
                "id": 9_000_001,
                "method": "POST",
                "url": format!("/v4/f/{endpoint}/{path}"),
                "headers": {"Content-Type": "application/json"},
                "body": body,
            });
            socket.send(Message::text(format!("3:::{delivery}"))).await.ok()
        }
    }
}

/// Whether a registration of `account` names `endpoint`'s push URL.
fn registered(shared: &Shared, account: &str, endpoint: &str) -> bool {
    let suffix = format!("/v4/f/{endpoint}/");
    shared
        .world()
        .registrations
        .iter()
        .any(|registration| registration.account == account && registration.path.ends_with(&suffix))
}

/// Replays `account`'s stored deliveries, as stored ones.
async fn replay(socket: &mut Socket, shared: &Shared, account: &str, endpoint: &str) -> Option<()> {
    let stored = shared.sockets().stored.get(account).cloned().unwrap_or_default();
    for delivery in stored {
        let mut headers = delivery.headers.clone();
        let request = json!({"id": format!("00000000-0000-4000-8000-{:012x}", delivery.id), "src": "192.0.2.1",
            "port": 3443, "mstore": true, "flow": "messaging", "seq": delivery.id});
        headers.insert("trouter-request".to_owned(), json!(request.to_string()));
        headers.insert("Trouter-Timeout".to_owned(), json!("7500"));
        headers.insert("Connection".to_owned(), json!("keep-alive"));
        send_delivery(socket, endpoint, delivery.id, headers, &delivery.frame_body).await?;
    }
    Some(())
}

/// Takes `account`'s queued events that have a captured wire form and delivers them.
async fn deliver_queued(socket: &mut Socket, shared: &Shared, account: &str, endpoint: &str) -> Option<()> {
    let events = {
        let mut world = shared.world();
        let queued = world.events.remove(account).unwrap_or_default();
        let (deliverable, kept): (Vec<ChatEvent>, Vec<ChatEvent>) = queued
            .into_iter()
            .partition(|event| resource_type(event.kind).is_some());
        if !kept.is_empty() {
            world.events.insert(account.to_owned(), kept);
        }
        deliverable
    };
    for event in events {
        let typing = event.kind == EventKind::Typing;
        let (id, body) = {
            let mut sockets = shared.sockets();
            sockets.next_delivery += 1;
            let id = 1_000_000 + sockets.next_delivery;
            let body = event_message(&event, id);
            if !typing {
                sockets.stored.entry(account.to_owned()).or_default().push(Stored {
                    id,
                    frame_body: body.clone(),
                    headers: live_headers(shared, id, false),
                });
            }
            (id, body)
        };
        send_delivery(socket, endpoint, id, live_headers(shared, id, typing), &body).await?;
    }
    Some(())
}

/// The headers of a live delivery: `deliveries.json`'s, with this delivery's ids (typing is never stored).
fn live_headers(shared: &Shared, id: u64, typing: bool) -> Map<String, Value> {
    let mut headers = shared.templates.headers.clone();
    let guid = |kind: u64| format!("00000000-0000-4000-{kind:04x}-{id:012x}");
    headers.insert("X-Microsoft-Skype-Message-ID".to_owned(), json!(guid(0x8001)));
    headers.insert("__RequestTunnelId__".to_owned(), json!(guid(0x8003)));
    let request = json!({"id": guid(0x8002), "src": "192.0.2.1", "port": 3443});
    headers.insert("trouter-request".to_owned(), json!(request.to_string()));
    if typing {
        headers.insert(
            "X-Trouter-Delivery-Control".to_owned(),
            json!("async; ttl=900; flow=messaging; prio=normal; resilience=nostorenoloss"),
        );
    }
    headers
}

async fn send_delivery(
    socket: &mut Socket,
    endpoint: &str,
    id: u64,
    mut headers: Map<String, Value>,
    body: &Value,
) -> Option<()> {
    let body = body.to_string();
    headers.insert("Content-Length".to_owned(), json!(body.len().to_string()));
    let delivery = json!({
        "id": id,
        "method": "POST",
        "url": format!("/v4/f/{endpoint}/messaging"),
        "headers": headers,
        "body": body,
    });
    socket.send(Message::text(format!("3:::{delivery}"))).await.ok()
}

/// The `resourceType` an event is delivered as; `None` for one no capture showed delivered to another member.
fn resource_type(kind: EventKind) -> Option<&'static str> {
    match kind {
        EventKind::Message | EventKind::Typing => Some("NewMessage"),
        EventKind::Edit | EventKind::Delete | EventKind::Reaction => Some("MessageUpdate"),
        EventKind::ReadReceipt | EventKind::ChatCreated => None,
    }
}

/// The delivery's body (an `EventMessage`), `deliveries.json`'s shape around the event's message.
fn event_message(event: &ChatEvent, id: u64) -> Value {
    const NOTIFICATIONS: &str = "https://notifications.skype.net/v1/users/ME";
    let chat = &event.conversation;
    let conversation_link = format!("{NOTIFICATIONS}/conversations/{chat}");
    let mut resource = if event.kind == EventKind::Typing {
        json!({
            "clientmessageid": id.to_string(), "content": "", "id": id.to_string(), "version": id.to_string(),
            "messagetype": event.resource["messagetype"], "contenttype": "Application/Message",
            "from": format!("{NOTIFICATIONS}/contacts/{}", event.from), "imdisplayname": "",
            "properties": {"importance": "", "subject": ""}, "type": "Message",
        })
    } else {
        event.resource.clone()
    };
    if let Value::Object(fields) = &mut resource {
        fields.insert("conversationLink".to_owned(), json!(conversation_link));
        fields.insert("to".to_owned(), json!(chat));
        fields.insert("threadtype".to_owned(), json!("chat"));
    }
    let message_id = resource["id"].as_str().unwrap_or_default().to_owned();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or_default());
    json!({
        "time": iso_time(now, 7),
        "type": "EventMessage",
        "resourceLink": format!("{conversation_link}/messages/{message_id}"),
        "resourceType": resource_type(event.kind).unwrap_or_default(),
        "resource": resource,
        "isactive": true,
    })
}

/// The registrar: `POST /registrar/prod/V2/registrations` (see the module docs); `None` for any other path.
pub(crate) fn registrar(state: &mut State, request: &Request) -> Option<ResponseTemplate> {
    if request.url.path() != "/registrar/prod/V2/registrations" {
        return None;
    }
    if request.method.as_str() != "POST" {
        return Some(ResponseTemplate::new(404));
    }
    let header = |name: &str| request.headers.get(name).and_then(|value| value.to_str().ok());
    let account = header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|token| state.account_by_token(token))
        .map(|account| account.mri.clone())
        .filter(|_| header("x-ms-migration") == Some("True"));
    let Some(account) = account else {
        return Some(ResponseTemplate::new(401));
    };
    let Some(registration) = messaging_registration(&request.body, account) else {
        return Some(ResponseTemplate::new(400));
    };
    state.registrations.push(registration);
    state.changed();
    Some(ResponseTemplate::new(202))
}

/// The messaging stack's registration of `registration.json`'s shape, by `account`.
fn messaging_registration(body: &[u8], account: String) -> Option<Registration> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let description = &body["clientDescription"];
    let template = description["templateKey"].as_str()?;
    let transport = body["transports"]["TROUTER"]
        .as_array()
        .filter(|list| list.len() == 1)?
        .first()?;
    let path = transport["path"].as_str()?;
    let messaging = description["appId"] == "TeamsCDLWebWorker"
        && template
            .strip_prefix("TeamsCDLWebWorker_")
            .is_some_and(|version| !version.is_empty())
        && path.starts_with("https://")
        && path.contains("/v4/f/");
    messaging.then(|| Registration {
        account,
        registration_id: body["registrationId"].as_str().unwrap_or_default().to_owned(),
        path: path.to_owned(),
        ttl: transport["ttl"].as_u64().unwrap_or_default(),
    })
}
