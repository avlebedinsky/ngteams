//! One socket, from the service's `1::` to its close (see the module docs of [`super`]).

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::watch;
use tokio::time::{Instant, Sleep};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use super::delivery::{self, Kind, Target, Which};
use super::frames::{self, FromClient};
use super::{Control, IC3_AUDIENCE, Stack, control_topic, endpoint_id, push_url, tell};
use crate::app::App;
use crate::auth::tokens::seconds_until;
use crate::db::{DbError, users};
use crate::fanout::journal::EntryId;
use crate::fanout::{FanoutError, Missed, Subscription};

/// How long one frame may take to leave: a client that reads nothing is dropped.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// How long past two ping intervals a silent socket is kept.
const IDLE_SLACK: Duration = Duration::from_secs(20);
/// How long the reconnect parameters of a `trouter.connected` stay valid.
const PARAMS_VALID: Duration = Duration::from_hours(7 * 24);

/// What a connect's query named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Connect {
    pub epid: String,
    pub stack: Stack,
    pub ping: Duration,
}

/// How a socket ends.
#[derive(Debug)]
enum End {
    /// The client left or the socket failed: nothing more is sent.
    Gone,
    /// The service disconnects, with its reason when it has one.
    Tell(Option<&'static str>),
    /// As [`End::Tell`], with a reason another node gave.
    Told(Option<String>),
}

impl From<FanoutError> for End {
    fn from(error: FanoutError) -> Self {
        tracing::warn!(%error, "a push socket lost the fan-out");
        Self::Tell(None)
    }
}

impl From<DbError> for End {
    fn from(error: DbError) -> Self {
        tracing::warn!(%error, "a push socket lost the database");
        Self::Tell(None)
    }
}

/// The socket and the number of the service's last event on it.
struct Link<S> {
    socket: WebSocketStream<S>,
    events: u64,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> Link<S> {
    async fn send(&mut self, text: String) -> Result<(), End> {
        match tokio::time::timeout(SEND_TIMEOUT, self.socket.send(Message::text(text))).await {
            Ok(Ok(())) => Ok(()),
            _ => Err(End::Gone),
        }
    }

    /// The service's next event.
    async fn event(&mut self, name: &str, argument: &Value) -> Result<(), End> {
        self.events += 1;
        self.send(frames::event_out(self.events, name, argument)).await
    }

    /// The client's next text frame; `None` once the socket closed or failed.
    async fn next(&mut self) -> Option<String> {
        loop {
            match self.socket.next().await? {
                Ok(Message::Text(text)) => return Some(text.as_str().to_owned()),
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }
}

/// Serves `socket` until it ends, then closes it.
pub(super) async fn serve<S>(app: Arc<App>, socket: WebSocketStream<S>, connect: Connect)
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let mut link = Link { socket, events: 0 };
    let Err(end) = run(&app, &mut link, &connect).await;
    let reason = match end {
        End::Gone => None,
        End::Tell(reason) => Some(reason.map(str::to_owned)),
        End::Told(reason) => Some(reason),
    };
    if let Some(reason) = reason {
        let _ = link.send(frames::disconnect(reason.as_deref())).await;
    }
    let _ = tokio::time::timeout(SEND_TIMEOUT, link.socket.close(None)).await;
}

/// The account a socket authenticated as.
struct Account {
    oid: String,
    mri: String,
    /// When its token runs out (Unix seconds).
    expires_at: i64,
    /// Whether the client sent back this endpoint's reconnect parameters: it was connected before.
    reconnect: bool,
}

/// Reads `user.authenticate` and checks its token: the account, if it is one of ours and not disabled.
async fn authenticate(app: &App, first: &str, connect: &Connect) -> Option<Account> {
    let FromClient::Event { name, args, .. } = frames::read(first) else {
        return None;
    };
    let argument = args.first().filter(|_| name == "user.authenticate")?;
    let token = argument["headers"]["Authorization"].as_str()?.strip_prefix("Bearer ")?;
    let claims = app.issuer.verify_access(token.trim(), IC3_AUDIENCE).await.ok()?;
    let user = users::by_id(&app.db, &claims.oid).await.ok()??;
    if user.disabled {
        return None;
    }
    let endpoint = endpoint_id(&claims.oid, &connect.epid, connect.stack);
    Some(Account {
        mri: format!("8:orgid:{}", claims.oid),
        reconnect: argument["connectparams"]["sr"].as_str() == Some(endpoint.as_str()),
        oid: claims.oid,
        expires_at: claims.exp,
    })
}

async fn run<S>(app: &Arc<App>, link: &mut Link<S>, connect: &Connect) -> Result<std::convert::Infallible, End>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let mut closing = app.trouter.closing.subscribe();
    if *closing.borrow_and_update() {
        return Err(End::Tell(None));
    }
    link.send(frames::CONNECT.to_owned()).await?;
    let first = match tokio::time::timeout(app.config.trouter.auth_timeout(), link.next()).await {
        Err(_) => return Err(End::Tell(Some("timeout"))),
        Ok(None) => return Err(End::Gone),
        Ok(Some(first)) => first,
    };
    let Some(account) = authenticate(app, &first, connect).await else {
        return Err(End::Tell(Some("unauthorized")));
    };
    let Some(_entered) = app.trouter.enter(&account.mri) else {
        return Err(End::Tell(Some("too_many_connections")));
    };
    let origin = app.config.origin();
    let mut session = Session {
        app,
        link,
        endpoint: endpoint_id(&account.oid, &connect.epid, connect.stack),
        socket: uuid::Uuid::new_v4().simple().to_string(),
        topic: format!("push:{}", account.mri),
        chat_base: format!("{origin}/api/chatsvc/{}", app.config.region),
        host: origin
            .split_once("://")
            .map_or(origin.as_str(), |(_, host)| host)
            .to_owned(),
        origin,
        account,
        connect,
        cursor: None,
        live_from: EntryId { ms: 0, seq: 0 },
        registered_until: 0,
        loss_told: None,
    };
    session.serve(&mut closing).await
}

/// The next payload of a subscription a socket may not have (a calling stack's socket has no chat events).
async fn next_of(subscription: Option<&mut Subscription>) -> Result<Bytes, Missed> {
    match subscription {
        Some(subscription) => subscription.recv().await,
        None => std::future::pending().await,
    }
}

/// What an authenticated socket waits on beside its frames.
struct Waits {
    control: Subscription,
    events: Option<Subscription>,
    /// The token's end.
    expired: Pin<Box<Sleep>>,
    /// How long the client may stay silent.
    quiet: Duration,
    idle: Pin<Box<Sleep>>,
}

/// One turn of an authenticated socket's loop.
enum Step {
    Frame(Option<String>),
    Event(Result<Bytes, Missed>),
    Control(Result<Bytes, Missed>),
    End(End),
}

/// An authenticated socket.
struct Session<'a, S> {
    app: &'a Arc<App>,
    link: &'a mut Link<S>,
    account: Account,
    connect: &'a Connect,
    endpoint: String,
    /// This socket among the endpoint's.
    socket: String,
    /// The account's events' topic, and their journal's.
    topic: String,
    origin: String,
    host: String,
    chat_base: String,
    /// The last journal entry the endpoint was given; `None` on a socket that takes no chat events.
    cursor: Option<EntryId>,
    /// The journal's newest entry when the socket connected: what is replayed up to it is a stored delivery.
    live_from: EntryId,
    /// Until when the endpoint's registration holds (Unix seconds).
    registered_until: i64,
    /// The cursor the client was last told of a loss after: told once.
    loss_told: Option<EntryId>,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> Session<'_, S> {
    async fn serve(&mut self, closing: &mut watch::Receiver<bool>) -> Result<std::convert::Infallible, End> {
        let messaging = self.connect.stack == Stack::Messaging;
        // subscribed before anything is read, so nothing published meanwhile is missed
        let control = self.app.fanout.subscribe(&control_topic(&self.account.mri)).await?;
        let events = if messaging {
            Some(self.app.fanout.subscribe(&self.topic).await?)
        } else {
            None
        };
        let replaced = Control::Replaced {
            endpoint: self.endpoint.clone(),
            socket: self.socket.clone(),
        };
        tell(self.app, &self.account.mri, &replaced).await?;
        self.connected().await?;
        if messaging {
            self.resume().await?;
        }
        self.read_registration().await?;
        self.drain().await?;
        let mut waits = Waits {
            control,
            events,
            expired: Box::pin(tokio::time::sleep(Duration::from_secs(seconds_until(
                self.account.expires_at,
            )))),
            quiet: 2 * self.connect.ping + IDLE_SLACK,
            idle: Box::pin(tokio::time::sleep(2 * self.connect.ping + IDLE_SLACK)),
        };
        loop {
            let step = self.next_step(&mut waits, closing).await;
            self.take(step).await?;
        }
    }

    /// What the socket has to do next.
    async fn next_step(&mut self, waits: &mut Waits, closing: &mut watch::Receiver<bool>) -> Step {
        tokio::select! {
            frame = self.link.next() => {
                waits.idle.as_mut().reset(Instant::now() + waits.quiet);
                Step::Frame(frame)
            }
            event = next_of(waits.events.as_mut()) => Step::Event(event),
            told = waits.control.recv() => Step::Control(told),
            // an open socket is never authenticated again: the client comes back with a new token
            () = &mut waits.expired => Step::End(End::Tell(Some("unauthorized"))),
            () = &mut waits.idle => Step::End(End::Gone),
            _ = closing.changed() => Step::End(End::Tell(None)),
        }
    }

    async fn take(&mut self, step: Step) -> Result<(), End> {
        match step {
            Step::Frame(Some(text)) => self.on_frame(&text).await,
            Step::Frame(None) => Err(End::Gone),
            Step::Event(Ok(payload)) => self.on_event(&payload).await,
            Step::Event(Err(Missed::Lagged(_))) => self.drain().await,
            Step::Control(Ok(payload)) => self.on_control(&payload).await,
            Step::Control(Err(Missed::Lagged(_))) => {
                self.read_registration().await?;
                self.drain().await
            }
            Step::Event(Err(Missed::Closed)) | Step::Control(Err(Missed::Closed)) => Err(End::Tell(None)),
            Step::End(end) => Err(end),
        }
    }

    /// `trouter.connected`: the endpoint, its push URL and where and with what to reconnect.
    async fn connected(&mut self) -> Result<(), End> {
        let origin = &self.origin;
        let push = push_url(origin, &self.endpoint);
        let now = chrono::Utc::now().timestamp_millis();
        let valid = i64::try_from(PARAMS_VALID.as_millis()).unwrap_or(i64::MAX);
        let info = json!({
            "id": self.endpoint,
            "ccid": self.endpoint[..self.endpoint.len().min(14)],
            "url": push,
            "surl": push,
            "curlb": origin,
            "healthUrl": format!("{origin}/trouter/v4/h"),
            "reconnectUrl": format!("{}{}", origin.replacen("http", "ws", 1), super::CONNECT_PATH),
            "socketio": format!("{origin}/trouter/"),
            "ttl": PARAMS_VALID.as_secs().to_string(),
            "dur": "1",
            "connectparams": {
                "issuer": "", "scae": "1", "sig": "", "sr": self.endpoint, "sp": self.app.config.region,
                "se": (now + valid).to_string(), "st": now.to_string(),
            },
        });
        self.link.event("trouter.connected", &info).await
    }

    /// Finds where the endpoint's deliveries go on: at its kept cursor, or for a new endpoint at the journal's end.
    /// An endpoint that was connected before and may have missed what the journal no longer holds is told.
    async fn resume(&mut self) -> Result<(), End> {
        let journal = &self.app.journal;
        self.live_from = journal.tail(&self.topic).await?;
        let (cursor, lost) = if let Some(cursor) = journal.cursor(&self.endpoint).await? {
            (cursor, !journal.after(&self.topic, cursor).await?.complete)
        } else {
            journal.set_cursor(&self.endpoint, self.live_from).await?;
            (self.live_from, self.account.reconnect)
        };
        self.cursor = Some(cursor);
        if lost {
            self.message_loss(cursor).await?;
        }
        Ok(())
    }

    /// `trouter.message_loss`: chat events since `since` may be missing; the client catches up and acknowledges.
    async fn message_loss(&mut self, since: EntryId) -> Result<(), End> {
        let time = i64::try_from(since.ms)
            .ok()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .unwrap_or_default();
        let etag = delivery::iso(time);
        let report = json!({"droppedIndicators": [{"tag": "messaging", "etag": etag}]});
        self.loss_told = Some(since);
        self.link.event("trouter.message_loss", &report).await
    }

    async fn read_registration(&mut self) -> Result<(), End> {
        let app_id = self.connect.stack.app_id();
        let until = crate::registrar::until(&self.app.db, &self.account.oid, &self.connect.epid, app_id).await?;
        self.registered_until = until.unwrap_or(0);
        Ok(())
    }

    fn registered(&self) -> bool {
        self.registered_until > crate::db::now()
    }

    fn target(&self) -> Target<'_> {
        Target {
            endpoint: &self.endpoint,
            chat_base: &self.chat_base,
            host: &self.host,
        }
    }

    /// Delivers what the journal holds after the cursor, to a registered messaging endpoint.
    async fn drain(&mut self) -> Result<(), End> {
        let Some(mut cursor) = self.cursor.filter(|_| self.registered()) else {
            return Ok(());
        };
        loop {
            let kept = self.app.journal.after(&self.topic, cursor).await?;
            if !kept.complete && self.loss_told != Some(cursor) {
                self.message_loss(cursor).await?;
            }
            let reached = match kept.entries.last() {
                Some(last) => last.id,
                None if kept.complete => return Ok(()),
                // nothing left of what was lost: go on from the journal's end
                None => self.app.journal.tail(&self.topic).await?,
            };
            for entry in &kept.entries {
                let Ok(envelope) = serde_json::from_slice::<Value>(&entry.payload) else {
                    continue;
                };
                let which = Which::Entry {
                    id: entry.id,
                    stored: entry.id <= self.live_from,
                };
                let delivery = delivery::delivery(&self.target(), &envelope, &which);
                self.link.send(frames::delivery(&delivery)).await?;
            }
            cursor = reached;
            self.cursor = Some(cursor);
            self.app.journal.set_cursor(&self.endpoint, cursor).await?;
            if !kept.more {
                return Ok(());
            }
        }
    }

    /// An event of the account, as the fan-out carried it: a kept one is read from the journal (in order, with what
    /// came before it), a transient one delivered as it is.
    async fn on_event(&mut self, payload: &[u8]) -> Result<(), End> {
        let Ok(envelope) = serde_json::from_slice::<Value>(payload) else {
            return Ok(());
        };
        match delivery::kind(&envelope) {
            Kind::Kept => self.drain().await,
            Kind::Transient if self.registered() => {
                let delivery = delivery::delivery(&self.target(), &envelope, &Which::Once);
                self.link.send(frames::delivery(&delivery)).await
            }
            Kind::Transient | Kind::Internal => Ok(()),
        }
    }

    async fn on_control(&mut self, payload: &[u8]) -> Result<(), End> {
        match serde_json::from_slice::<Control>(payload) {
            Ok(Control::Registered { endpoint, until }) if endpoint == self.endpoint => {
                self.registered_until = until;
                self.drain().await
            }
            Ok(Control::Replaced { endpoint, socket }) if endpoint == self.endpoint && socket != self.socket => {
                Err(End::Tell(None))
            }
            Ok(Control::Disconnect { reason }) => Err(End::Told(reason)),
            _ => Ok(()),
        }
    }

    /// A frame of the client: its keep-alive and its other acknowledged events are acknowledged, its answers to
    /// deliveries taken, its disconnect followed.
    async fn on_frame(&mut self, text: &str) -> Result<(), End> {
        match frames::read(text) {
            FromClient::Disconnect => Err(End::Gone),
            FromClient::Event {
                ack: Some(id), name, ..
            } => {
                let args = if name == "ping" { json!(["pong"]) } else { json!([]) };
                self.link.send(frames::ack(id, &args)).await
            }
            FromClient::Response { id, status } => {
                tracing::debug!(id, status, "a delivery was answered");
                Ok(())
            }
            FromClient::Event { .. } | FromClient::Other => Ok(()),
        }
    }
}
