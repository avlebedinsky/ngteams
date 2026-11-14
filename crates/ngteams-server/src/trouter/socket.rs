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
use super::{Control, Stack, control_topic, endpoint_id, push_url, tell};
use crate::app::App;
use crate::auth::extract::{Audience as _, Ic3};
use crate::auth::tokens::seconds_until;
use crate::db::{DbError, users};
use crate::fanout::journal::{Entry, EntryId, Kept};
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
    let claims = app.issuer.verify_access(token.trim(), Ic3::AUDIENCE).await.ok()?;
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
        claim: Arc::new(Claim {
            mri: account.mri.clone(),
            endpoint: endpoint_id(&account.oid, &connect.epid, connect.stack),
            socket: uuid::Uuid::new_v4().simple().to_string(),
            at: 0,
        }),
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
        kept: None,
        live_from: EntryId { ms: 0, seq: 0 },
        registered_until: 0,
        loss_told: None,
        caught_up: false,
        gap: false,
    };
    let end = session.serve(&mut closing).await;
    // what was written to the socket is not replayed, however it ended
    let _ = session.keep_cursor().await;
    end
}

/// The next payload of a subscription a socket may not have (a calling stack's socket has no chat events).
async fn next_of(subscription: Option<&mut Subscription>) -> Result<Bytes, Missed> {
    match subscription {
        Some(subscription) => subscription.recv().await,
        None => std::future::pending().await,
    }
}

fn unix_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default()
}

/// A socket's claim to its endpoint. Of an endpoint's sockets the one that claimed it last stays.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Claim {
    mri: String,
    endpoint: String,
    /// This socket among the endpoint's.
    socket: String,
    /// When it claimed the endpoint (Unix milliseconds), taken once it listens to the control topic: a socket that
    /// can't have heard this claim made its own after it.
    at: u64,
}

/// What a word on the control topic means to a socket.
#[derive(Debug)]
enum Told {
    End(End),
    /// The endpoint's registration holds until then.
    Registered(i64),
    /// An event of the account passed its journal by.
    Lost,
    /// A socket this one replaced claims the endpoint: it has to hear this one's claim.
    Replaces,
    Nothing,
}

impl Claim {
    fn told(&self, payload: &[u8]) -> Told {
        match serde_json::from_slice::<Control>(payload) {
            Ok(Control::Registered { endpoint, until }) if endpoint == self.endpoint => Told::Registered(until),
            Ok(Control::Replaced { endpoint, socket, at }) if endpoint == self.endpoint && socket != self.socket => {
                if (at, socket.as_str()) > (self.at, self.socket.as_str()) {
                    Told::End(End::Tell(None))
                } else {
                    Told::Replaces
                }
            }
            Ok(Control::Lost) => Told::Lost,
            Ok(Control::Disconnect { reason }) => Told::End(End::Told(reason)),
            _ => Told::Nothing,
        }
    }

    /// Tells the endpoint's other sockets of this one.
    async fn announce(&self, app: &App) -> Result<(), FanoutError> {
        let replaced = Control::Replaced {
            endpoint: self.endpoint.clone(),
            socket: self.socket.clone(),
            at: self.at,
        };
        tell(app, &self.mri, &replaced).await
    }
}

/// What the control topic said that doesn't end the socket, for it to act on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Noted {
    registered: Option<i64>,
    lost: bool,
    /// A socket this one replaced claims the endpoint.
    replaces: bool,
    /// Words were missed: any of them may have been said.
    lagged: bool,
}

impl Noted {
    fn of(told: Told) -> Result<Self, End> {
        let mut noted = Self::default();
        match told {
            Told::End(end) => return Err(end),
            Told::Registered(until) => noted.registered = Some(until),
            Told::Lost => noted.lost = true,
            Told::Replaces => noted.replaces = true,
            Told::Nothing => {}
        }
        Ok(noted)
    }

    fn and(self, other: Self) -> Self {
        Self {
            registered: other.registered.or(self.registered),
            lost: self.lost || other.lost,
            replaces: self.replaces || other.replaces,
            lagged: self.lagged || other.lagged,
        }
    }
}

/// Listens to the control topic while the socket is busy: resolves when the socket has to end, and notes the rest.
async fn overtaken(control: &mut Subscription, claim: &Claim, noted: &mut Noted) -> End {
    loop {
        let heard = match control.recv().await {
            Ok(payload) => Noted::of(claim.told(&payload)),
            Err(Missed::Lagged(_)) => Ok(Noted {
                lagged: true,
                ..Noted::default()
            }),
            Err(Missed::Closed) => Err(End::Tell(None)),
        };
        match heard {
            Ok(heard) => *noted = noted.and(heard),
            Err(end) => return end,
        }
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

/// What a socket does in one go, unless it has to end first.
enum Work {
    /// `trouter.connected`, and what the endpoint missed.
    Begin,
    Step(Step),
    Noted(Noted),
}

/// Whether a reader at the cursor `kept` was read after may have lost entries. One that had read everything and
/// listened since can only have lost them to an overflow: its cursor grows old while the account is quiet, and the
/// journal empties, without anything passing it by. A socket never listens across a break of its node's
/// subscription: the break closes it ([`Missed::Closed`]).
fn lost(kept: &Kept, caught_up: bool) -> bool {
    if caught_up { kept.overflowed } else { !kept.complete }
}

/// An authenticated socket.
struct Session<'a, S> {
    app: &'a Arc<App>,
    link: &'a mut Link<S>,
    account: Account,
    connect: &'a Connect,
    claim: Arc<Claim>,
    /// The account's events' topic, and their journal's.
    topic: String,
    origin: String,
    host: String,
    chat_base: String,
    /// The last journal entry written to the endpoint's socket; `None` on a socket that takes no chat events.
    cursor: Option<EntryId>,
    /// The cursor the journal keeps for the endpoint.
    kept: Option<EntryId>,
    /// The journal's newest entry when the socket connected: what is replayed up to it is a stored delivery.
    live_from: EntryId,
    /// Until when the endpoint's registration holds (Unix seconds).
    registered_until: i64,
    /// The cursor the client was last told of a loss after: told once.
    loss_told: Option<EntryId>,
    /// Whether the socket delivered all the journal held and has heard of every kept event since.
    caught_up: bool,
    /// Whether an event passed the journal by, and the client isn't told yet.
    gap: bool,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> Session<'_, S> {
    async fn serve(&mut self, closing: &mut watch::Receiver<bool>) -> Result<std::convert::Infallible, End> {
        // subscribed before anything is read, so nothing published meanwhile is missed
        let control = self.app.fanout.subscribe(&control_topic(&self.account.mri)).await?;
        let events = if self.connect.stack == Stack::Messaging {
            Some(self.app.fanout.subscribe(&self.topic).await?)
        } else {
            None
        };
        Arc::make_mut(&mut self.claim).at = unix_ms();
        self.claim.announce(self.app).await?;
        let mut waits = Waits {
            control,
            events,
            expired: Box::pin(tokio::time::sleep(Duration::from_secs(seconds_until(
                self.account.expires_at,
            )))),
            quiet: 2 * self.connect.ping + IDLE_SLACK,
            idle: Box::pin(tokio::time::sleep(2 * self.connect.ping + IDLE_SLACK)),
        };
        self.unless_ended(Work::Begin, &mut waits, closing).await?;
        loop {
            let step = self.next_step(&mut waits, closing).await;
            self.unless_ended(Work::Step(step), &mut waits, closing).await?;
        }
    }

    /// Does `work`, unless the socket has to end first: its token runs out, the node stops or the control topic
    /// says so, however long a slow reader makes a replay take. What else the control topic says meanwhile is acted
    /// on after.
    async fn unless_ended(
        &mut self,
        mut work: Work,
        waits: &mut Waits,
        closing: &mut watch::Receiver<bool>,
    ) -> Result<(), End> {
        loop {
            let claim = Arc::clone(&self.claim);
            let mut noted = Noted::default();
            tokio::select! {
                biased;
                () = &mut waits.expired => return Err(End::Tell(Some("unauthorized"))),
                _ = closing.changed() => return Err(End::Tell(None)),
                end = overtaken(&mut waits.control, &claim, &mut noted) => return Err(end),
                done = self.work(work) => done?,
            }
            if noted == Noted::default() {
                return Ok(());
            }
            work = Work::Noted(noted);
        }
    }

    async fn work(&mut self, work: Work) -> Result<(), End> {
        match work {
            Work::Begin => self.begin().await,
            Work::Step(step) => self.take(step).await,
            Work::Noted(noted) => self.noted(noted).await,
        }
    }

    async fn begin(&mut self) -> Result<(), End> {
        self.connected().await?;
        self.read_registration().await?;
        if self.connect.stack == Stack::Messaging {
            self.resume().await?;
        }
        self.drain().await
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
            Step::Event(Err(Missed::Lagged(_))) => {
                self.caught_up = false;
                self.drain().await
            }
            Step::Control(Ok(payload)) => self.on_control(&payload).await,
            Step::Control(Err(Missed::Lagged(_))) => {
                let lagged = Noted {
                    lagged: true,
                    ..Noted::default()
                };
                self.noted(lagged).await
            }
            Step::Event(Err(Missed::Closed)) | Step::Control(Err(Missed::Closed)) => Err(End::Tell(None)),
            Step::End(end) => Err(end),
        }
    }

    /// `trouter.connected`: the endpoint, its push URL and where and with what to reconnect.
    async fn connected(&mut self) -> Result<(), End> {
        let origin = &self.origin;
        let endpoint = &self.claim.endpoint;
        let push = push_url(origin, endpoint);
        let now = chrono::Utc::now().timestamp_millis();
        let valid = i64::try_from(PARAMS_VALID.as_millis()).unwrap_or(i64::MAX);
        let info = json!({
            "id": endpoint,
            "ccid": endpoint[..endpoint.len().min(14)],
            "url": push,
            "surl": push,
            "curlb": origin,
            "healthUrl": format!("{origin}/trouter/v4/h"),
            "reconnectUrl": format!("{}{}", origin.replacen("http", "ws", 1), super::CONNECT_PATH),
            "socketio": format!("{origin}/trouter/"),
            "ttl": PARAMS_VALID.as_secs().to_string(),
            "dur": "1",
            "connectparams": {
                "issuer": "", "scae": "1", "sig": "", "sr": endpoint, "sp": self.app.config.region,
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
        self.kept = journal.cursor(&self.topic, &self.claim.endpoint).await?;
        let (cursor, lost) = match self.kept {
            Some(cursor) => (cursor, !journal.after(&self.topic, cursor).await?.complete),
            None => (self.live_from, self.account.reconnect),
        };
        self.cursor = Some(cursor);
        if lost {
            self.message_loss(cursor).await?;
        }
        Ok(())
    }

    /// Has the journal keep the endpoint's cursor where its socket is. Only a registered endpoint's is kept, so an
    /// account's kept cursors are bounded as its registrations are, not by how often it connects.
    async fn keep_cursor(&mut self) -> Result<(), End> {
        if let Some(cursor) = self
            .cursor
            .filter(|cursor| self.registered() && self.kept != Some(*cursor))
        {
            let journal = &self.app.journal;
            journal.set_cursor(&self.topic, &self.claim.endpoint, cursor).await?;
            self.kept = Some(cursor);
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
            endpoint: &self.claim.endpoint,
            chat_base: &self.chat_base,
            host: &self.host,
        }
    }

    /// Delivers what the journal holds after the cursor, to a registered messaging endpoint.
    async fn drain(&mut self) -> Result<(), End> {
        let Some(mut cursor) = self.cursor.filter(|_| self.registered()) else {
            return Ok(());
        };
        if std::mem::take(&mut self.gap) {
            self.message_loss(cursor).await?;
        }
        loop {
            let kept = self.app.journal.after(&self.topic, cursor).await?;
            let lost = lost(&kept, self.caught_up);
            if lost && self.loss_told != Some(cursor) {
                self.message_loss(cursor).await?;
            }
            if lost && kept.entries.is_empty() {
                // nothing left of what was lost: go on from where this read stood, not from a later look at the
                // journal's end, which would pass by an entry appended in between
                self.cursor = Some(cursor.max(kept.read_at));
            }
            self.deliver(&kept.entries).await?;
            self.keep_cursor().await?;
            if !kept.more {
                self.caught_up = true;
                return Ok(());
            }
            cursor = self.cursor.unwrap_or(cursor);
        }
    }

    /// Writes `entries` to the socket, the cursor following each one written.
    async fn deliver(&mut self, entries: &[Entry]) -> Result<(), End> {
        for entry in entries {
            if let Ok(envelope) = serde_json::from_slice::<Value>(&entry.payload) {
                let which = Which::Entry {
                    id: entry.id,
                    stored: entry.id <= self.live_from,
                };
                let delivery = delivery::delivery(&self.target(), &envelope, &which);
                self.link.send(frames::delivery(&delivery)).await?;
            }
            self.cursor = Some(entry.id);
        }
        Ok(())
    }

    /// An event of the account, as the fan-out carried it: a kept one is read from the journal (in order, with what
    /// came before it), a transient one delivered as it is.
    async fn on_event(&mut self, payload: &[u8]) -> Result<(), End> {
        let Ok(envelope) = serde_json::from_slice::<Value>(payload) else {
            return Ok(());
        };
        match delivery::kind(&envelope) {
            Kind::Kept => {
                // one that can't be delivered now may be gone from the journal by the time it can
                self.caught_up &= self.registered();
                self.drain().await
            }
            Kind::Transient if self.registered() => {
                let delivery = delivery::delivery(&self.target(), &envelope, &Which::Once);
                self.link.send(frames::delivery(&delivery)).await
            }
            Kind::Transient | Kind::Internal => Ok(()),
        }
    }

    async fn on_control(&mut self, payload: &[u8]) -> Result<(), End> {
        let noted = Noted::of(self.claim.told(payload))?;
        if noted == Noted::default() {
            return Ok(());
        }
        self.noted(noted).await
    }

    /// Acts on what the control topic said. After missed words the socket assumes each of them: it reads its
    /// registration, tells of a loss and claims its endpoint again, so a newer socket answers.
    async fn noted(&mut self, noted: Noted) -> Result<(), End> {
        if noted.replaces || noted.lagged {
            self.claim.announce(self.app).await?;
        }
        if noted.lagged {
            self.read_registration().await?;
        } else if let Some(until) = noted.registered {
            self.registered_until = until;
        }
        self.gap |= noted.lost || noted.lagged;
        self.drain().await
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

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(socket: &str, at: u64) -> Claim {
        Claim {
            mri: "8:orgid:a".to_owned(),
            endpoint: "Endpoint".to_owned(),
            socket: socket.to_owned(),
            at,
        }
    }

    fn word(control: &Control) -> Vec<u8> {
        serde_json::to_vec(control).unwrap()
    }

    fn replaced(endpoint: &str, socket: &str, at: u64) -> Vec<u8> {
        word(&Control::Replaced {
            endpoint: endpoint.to_owned(),
            socket: socket.to_owned(),
            at,
        })
    }

    #[test]
    fn of_an_endpoints_sockets_only_the_one_that_claimed_it_last_stays() {
        let me = claim("m", 100);
        // a later claim ends this socket; an earlier one is answered, so that socket ends
        assert!(matches!(
            me.told(&replaced("Endpoint", "x", 101)),
            Told::End(End::Tell(None))
        ));
        assert!(matches!(me.told(&replaced("Endpoint", "x", 99)), Told::Replaces));
        // two claims of the same moment: both sockets agree on the one that stays
        assert!(matches!(me.told(&replaced("Endpoint", "n", 100)), Told::End(_)));
        assert!(matches!(
            claim("n", 100).told(&replaced("Endpoint", "m", 100)),
            Told::Replaces
        ));
        // its own claim, and another endpoint's
        assert!(matches!(me.told(&replaced("Endpoint", "m", 100)), Told::Nothing));
        assert!(matches!(me.told(&replaced("Other", "x", 101)), Told::Nothing));
    }

    #[test]
    fn a_socket_reads_the_control_topics_other_words() {
        let me = claim("m", 100);
        let registered = |endpoint: &str| {
            word(&Control::Registered {
                endpoint: endpoint.to_owned(),
                until: 7,
            })
        };
        assert!(matches!(me.told(&registered("Endpoint")), Told::Registered(7)));
        assert!(matches!(me.told(&registered("Other")), Told::Nothing));
        assert!(matches!(me.told(&word(&Control::Lost)), Told::Lost));
        let disconnect = word(&Control::Disconnect {
            reason: Some("dup".to_owned()),
        });
        assert!(matches!(me.told(&disconnect), Told::End(End::Told(Some(reason))) if reason == "dup"));
        assert!(matches!(me.told(b"not json"), Told::Nothing));
    }

    #[test]
    fn what_the_control_topic_says_while_a_socket_is_busy_adds_up() {
        let noted = Noted::of(Told::Registered(7))
            .unwrap()
            .and(Noted::of(Told::Lost).unwrap())
            .and(Noted::of(Told::Replaces).unwrap())
            .and(Noted::of(Told::Nothing).unwrap());
        let all = Noted {
            registered: Some(7),
            lost: true,
            replaces: true,
            lagged: false,
        };
        assert_eq!(noted, all);
        assert_eq!(noted.and(Noted::of(Told::Registered(9)).unwrap()).registered, Some(9));
        assert!(matches!(Noted::of(Told::End(End::Gone)), Err(End::Gone)));
    }

    #[test]
    fn a_socket_that_followed_its_journal_lost_entries_only_to_an_overflow() {
        // a quiet account: the cursor is older than the journal keeps, and nothing passed the socket by
        let aged = Kept {
            complete: false,
            ..Kept::default()
        };
        assert!(!lost(&aged, true));
        assert!(lost(&aged, false));
        let overflowed = Kept {
            complete: false,
            overflowed: true,
            ..Kept::default()
        };
        assert!(lost(&overflowed, true));
        assert!(lost(&overflowed, false));
        let whole = Kept {
            complete: true,
            ..Kept::default()
        };
        assert!(!lost(&whole, true));
        assert!(!lost(&whole, false));
    }
}
