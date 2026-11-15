use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use teams_auth::authz::CHAT_BASE_GTM;
use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_core::Availability;
use teams_core::Outbox;
use teams_core::account::{MIDDLE_TIER_GTM, PRESENCE_GTM};
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, OwnStatus, Parts};
use teams_proto::{Author, Client, Cloud, HttpTransport, Mri, PresenceRead, UserPresence};
use teams_store::Store;
use teams_trouter::{RegistrationReport, Trouter};
use tempfile::TempDir;
use tokio::task::JoinHandle;

use super::super::push::support::{brief, options};
use super::super::support::Node;
use crate::support::PASSWORD;

pub type Transport = HttpTransport<Session<MemorySecretStore>>;

pub const WAIT: Duration = Duration::from_secs(40);
/// Longer than any test: what a watcher learns here comes from a push or from the read a newly watched person brings.
const NEVER: Duration = Duration::from_hours(1);
/// How often a running client says it is available, as the app's policy does every two minutes: often, so that the
/// short endpoint lifetimes of these tests never lapse a client that runs.
pub const REFRESH: Duration = Duration::from_millis(900);

/// The node configuration these tests run: endpoints lapse soon after their client stops reporting.
pub const SHORT_PRESENCE: &str = "[presence]\nendpoint_ttl = 5\n";

/// The transport as the account bring-up builds it for an own-server account: every base from authz's GTMs, each
/// request sent to the node at `endpoint`.
pub async fn transport(
    session: &Arc<Session<MemorySecretStore>>,
    endpoint: &str,
    origin: &str,
    mri: &str,
) -> Transport {
    session.skypetoken().await.unwrap();
    let base = |gtm: &str| {
        session
            .service_base(gtm)
            .unwrap_or_else(|| panic!("authz names no {gtm}"))
    };
    HttpTransport::for_cloud_with_mock_origin(
        Arc::clone(session),
        &base(CHAT_BASE_GTM),
        Cloud::own(origin).unwrap(),
        endpoint,
    )
    .unwrap()
    .for_account(Mri::new(mri.to_owned()))
    .with_middle_tier(&base(MIDDLE_TIER_GTM))
    .unwrap()
    .with_presence(&base(PRESENCE_GTM))
    .unwrap()
    .with_loki()
}

/// One running ngteams client of an own-server account: its session, its engine on its store with its push channel,
/// and the app's own-presence policy reduced to its refresh.
pub struct Seat {
    pub upn: String,
    pub mri: String,
    pub epid: String,
    pub log: Vec<Event>,
    session: Arc<Session<MemorySecretStore>>,
    http: String,
    socket: String,
    origin: String,
    engine: Option<EngineHandle>,
    refresh: Option<JoinHandle<()>>,
    dir: TempDir,
}

impl Seat {
    /// Signs `upn` in at `node` with the password, and starts its client: connected and registered.
    pub async fn sign_in(node: &Node, origin: &str, upn: &str) -> Self {
        let session = Arc::new(node.sign_in(upn, PASSWORD).await);
        Self::on(node, origin, upn, session).await
    }

    /// A client of the account `session` is signed in as, at `node`: what a start of the app on a kept sign-in does.
    pub async fn on(node: &Node, origin: &str, upn: &str, session: Arc<Session<MemorySecretStore>>) -> Self {
        let skype = session.skypetoken().await.unwrap();
        let mut seat = Self {
            upn: upn.into(),
            mri: format!("8:{}", skype.skype_id.as_deref().unwrap()),
            epid: String::new(),
            log: Vec::new(),
            session,
            http: node.endpoint.clone(),
            socket: node.endpoint.replacen("http://", "ws://", 1),
            origin: origin.into(),
            engine: None,
            refresh: None,
            dir: tempfile::tempdir().unwrap(),
        };
        seat.start().await;
        seat
    }

    pub fn oid(&self) -> &str {
        self.mri.strip_prefix("8:orgid:").unwrap()
    }

    async fn transport(&self) -> Transport {
        transport(&self.session, &self.http, &self.origin, &self.mri).await
    }

    /// The account's typed client, Loki's region set as the server's configuration names it.
    pub async fn client(&self) -> Client<Transport> {
        let client = Client::new(self.transport().await);
        let answer = client.read(&teams_proto::LokiConfiguration).await.unwrap();
        let body: Value = serde_json::from_slice(answer.body()).unwrap();
        client
            .transport()
            .set_loki_region(body["LokiUrl"].as_str().unwrap())
            .unwrap();
        client
    }

    /// Starts the client on its store, as a start of ngteams does, and waits until its push channel registered.
    pub async fn start(&mut self) {
        assert!(self.engine.is_none());
        let writes = Arc::new(self.transport().await);
        let trouter = Trouter::with_mock_origin(Arc::clone(&writes), Arc::clone(&self.session), &self.socket)
            .unwrap()
            .with_options(options());
        trouter.endpoint_id().clone_into(&mut self.epid);
        let engine = engine::start(Parts {
            store: Store::open(&self.dir.path().join("account.sqlite")).unwrap(),
            client: Arc::new(Client::new(self.transport().await)),
            outbox: Outbox::new(writes, Author::new(Mri::new(self.mri.clone()), &self.upn)),
            calls: None,
            push: Some(engine::Push::new(trouter)),
            options: EngineOptions {
                tick: Duration::from_millis(20),
                presence_poll: NEVER,
                presence_poll_subscribed: NEVER,
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        self.engine = Some(engine);
        let mark = self.mark();
        self.online(mark).await;
    }

    /// The push channel connected and registered its endpoint since `mark`.
    pub async fn online(&mut self, mark: usize) {
        let connected = self
            .until(mark, "connect", |event| matches!(event, Event::Connected { .. }))
            .await;
        self.until(connected, "registration", |event| {
            matches!(event, Event::Registration(RegistrationReport::Registered))
        })
        .await;
    }

    pub fn engine(&self) -> &EngineHandle {
        self.engine.as_ref().unwrap()
    }

    pub fn mark(&self) -> usize {
        self.log.len()
    }

    async fn pump(&mut self, wait: Duration) -> bool {
        let engine = self.engine.as_mut().unwrap();
        match tokio::time::timeout(wait, engine.next_event()).await {
            Ok(Some(event)) => {
                self.log.push(event);
                true
            }
            Ok(None) | Err(_) => false,
        }
    }

    /// Waits for an event since `mark` that `pick` takes; its place in the log.
    pub async fn until(&mut self, mark: usize, what: &str, pick: impl Fn(&Event) -> bool) -> usize {
        let deadline = tokio::time::Instant::now() + WAIT;
        let mut seen = mark;
        loop {
            if let Some(found) = self.log[seen..].iter().position(&pick) {
                return seen + found;
            }
            seen = self.log.len();
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !left.is_zero() && self.pump(left).await,
                "{}: no {what} in time; since the mark: {:?}",
                self.upn,
                self.log[mark..].iter().map(brief).collect::<Vec<_>>()
            );
        }
    }

    /// Takes the events of the next `wait`.
    pub async fn idle_for(&mut self, wait: Duration) {
        let until = tokio::time::Instant::now() + wait;
        loop {
            let left = until.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return;
            }
            self.pump(left).await;
        }
    }

    /// Who this client was told shows as what since `mark`, in order.
    pub fn told(&self, mark: usize, of: &str) -> Vec<Availability> {
        self.log[mark..]
            .iter()
            .filter_map(|event| match event {
                Event::Presence { mri, availability } if mri == of => Some(availability.clone()),
                _ => None,
            })
            .collect()
    }

    /// Starts watching `mri` as a view showing them does; what the engine knew of them then.
    pub async fn watch(&self, mri: &str) -> Availability {
        self.engine().presence(mri.to_owned()).await.unwrap()
    }

    /// Waits until the engine tells, after `mark`, that `mri` shows as `wanted`.
    pub async fn sees(&mut self, mark: usize, mri: &str, wanted: &Availability) -> usize {
        let what = format!("{mri} as {wanted:?}");
        self.until(
            mark,
            &what,
            |event| matches!(event, Event::Presence { mri: of, availability } if of == mri && availability == wanted),
        )
        .await
    }

    /// Reports the account's own status from this client, once its endpoint is registered.
    pub async fn report(&self, status: OwnStatus) {
        let until = tokio::time::Instant::now() + WAIT;
        loop {
            match self.engine().own_availability(status).await {
                Ok(_) => return,
                Err(error) => assert!(
                    tokio::time::Instant::now() < until,
                    "{}: {status:?} wasn't sent: {error:?}",
                    self.upn
                ),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The app's policy while its window is used: Available now and again every [`REFRESH`].
    pub async fn in_use(&mut self) {
        self.report(OwnStatus::Available).await;
        if self.refresh.is_some() {
            return;
        }
        let requests = (**self.engine()).clone();
        self.refresh = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(REFRESH).await;
                if requests.own_availability(OwnStatus::Available).await.is_err() {
                    // a reconnect re-registers the endpoint; the next refresh reports it
                }
            }
        }));
    }

    /// The window wasn't used for the idle time: the policy reports nothing more.
    pub fn idle(&mut self) {
        if let Some(refresh) = self.refresh.take() {
            refresh.abort();
        }
    }

    /// Quits the client: its policy stops and its engine shuts down, as a quit does (nothing is reported).
    pub fn close(&mut self) {
        self.idle();
        if let Some(engine) = self.engine.take() {
            engine.shutdown();
        }
    }

    /// The presence of `mris` as this account's client reads it on demand (UPS's read).
    pub async fn read(&self, mris: &[&str]) -> Vec<UserPresence> {
        let client = Client::new(self.transport().await);
        let read = PresenceRead::new(mris.iter().map(|mri| Mri::new((*mri).to_owned()))).unwrap();
        let answer = client.post_read(read.clone()).await.unwrap();
        read.read_answer(answer.body()).unwrap()
    }

    /// The availability this account reads for `mri` now.
    pub async fn reads(&self, mri: &str) -> Option<String> {
        self.read(&[mri]).await.remove(0).availability
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        self.close();
    }
}

/// Waits until the account `mri` reads as `wanted` to `reader` (an on-demand read, as a card's).
pub async fn reads_as(reader: &Seat, mri: &str, wanted: &str) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let read = reader.reads(mri).await;
        if read.as_deref() == Some(wanted) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{mri} still reads as {read:?}, not {wanted}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// An access token of `upn` for `audience`, as the client's session gets it after a sign-in at `node`.
pub async fn bearer(node: &Node, upn: &str, audience: &str) -> String {
    let session = node.sign_in(upn, PASSWORD).await;
    session.access_token(audience).await.unwrap().token.expose().to_owned()
}

/// The keyspace of `node`'s deployment in Redis: its tenant's, as `upn`'s access token names it.
pub async fn keyspace(node: &Node, upn: &str) -> ngteams_server::fanout::keyspace::Keyspace {
    use base64::Engine as _;
    let answer: Value = node.password(upn, PASSWORD).await.json().await.unwrap();
    let claims = answer["access_token"].as_str().unwrap().split('.').nth(1).unwrap();
    let claims: Value =
        serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap();
    ngteams_server::fanout::keyspace::Keyspace::new(claims["tid"].as_str().unwrap())
}

diesel::table! {
    /// The server's table of the statuses set by hand (`migrations/*/..._presence_manual`).
    presence_manual (user_id) {
        user_id -> Text,
        availability -> Text,
        expires_at -> BigInt,
    }
}

/// The status set by hand that the database of `database` keeps for the account `oid`, and when it expires (Unix
/// seconds).
pub async fn manual_row(database: &str, oid: &str) -> Option<(String, i64)> {
    use diesel::{OptionalExtension as _, QueryDsl as _};
    use diesel_async::RunQueryDsl as _;
    let db = ngteams_server::db::Db::connect(database, 1).await.unwrap();
    let query = presence_manual::table
        .find(oid.to_owned())
        .select((presence_manual::availability, presence_manual::expires_at));
    let row: Result<_, ngteams_server::db::DbError> =
        async { ngteams_server::with_conn!(&db, |conn| Ok(query.first(conn).await.optional()?)) }.await;
    row.unwrap()
}

/// Makes the status set by hand that the database of `database` keeps for the account `oid` expire at `at` (Unix
/// seconds), as if it had been set that long ago.
pub async fn manual_expires(database: &str, oid: &str, at: i64) {
    use diesel::{ExpressionMethods as _, QueryDsl as _};
    use diesel_async::RunQueryDsl as _;
    let db = ngteams_server::db::Db::connect(database, 1).await.unwrap();
    let query = diesel::update(presence_manual::table.find(oid.to_owned())).set(presence_manual::expires_at.eq(at));
    let changed: Result<usize, ngteams_server::db::DbError> =
        async { ngteams_server::with_conn!(&db, |conn| Ok(query.execute(conn).await?)) }.await;
    assert_eq!(changed.unwrap(), 1, "{oid} has a status set by hand");
}
