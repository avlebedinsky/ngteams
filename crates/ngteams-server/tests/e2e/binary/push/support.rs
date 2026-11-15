use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_core::account::{AMS_GTM, CSA_GTM, MIDDLE_TIER_GTM, PROVISIONING_GTM, USER_PROFILE_GTM};
use teams_core::activity::ActivityFeed;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Event, MessageChange, Parts, Source};
use teams_core::{Outbox, Transient};
use teams_proto::chat::MessageText;
use teams_proto::{Author, Client, Cloud, HttpTransport, Mri};
use teams_store::{OutboxState, Store};
use teams_trouter::{Options, RegistrationReport, RegistrationTimes, Trouter};
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::super::support::{Deployment, Node};
use crate::support::{Backend, CLIENT_ID, PASSWORD, redis_url};

type Transport = HttpTransport<Session<MemorySecretStore>>;

pub const WAIT: Duration = Duration::from_secs(40);
pub const QUIET: Duration = Duration::from_millis(1200);
pub const IC3: &str = "https://ic3.teams.office.com";
pub const SPACES: &str = "https://api.spaces.skype.com";
pub const UNAUTHORIZED: &str = r#"0:::{"reason":"unauthorized"}"#;

/// The client's waits, short enough for a reconnect to take a test moments.
pub fn options() -> Options {
    Options {
        backoff_base: Duration::from_millis(300),
        backoff_cap: Duration::from_secs(2),
        backoff_jitter_percent: 0,
        registration: RegistrationTimes::default(),
        ..Options::default()
    }
}

/// The real server: one node on SQLite, or two nodes on one Postgres and one Redis.
pub struct Rig {
    pub origin: String,
    pub database: String,
    pub first: Node,
    pub second: Option<Node>,
    _deployment: Deployment,
}

impl Rig {
    pub async fn start(backend: Backend) -> Self {
        Self::start_with(backend, "").await
    }

    /// `config`: TOML tables appended to each node's configuration.
    pub async fn start_with(backend: Backend, config: &str) -> Self {
        let redis = (backend == Backend::Postgres).then(redis_url).flatten();
        assert!(
            backend != Backend::Postgres || redis.is_some(),
            "Postgres QA requires Redis"
        );
        Self::start_on(backend, config, redis).await
    }

    /// As [`Rig::start_with`], the nodes on the Redis at `redis`.
    pub async fn start_on(backend: Backend, config: &str, redis: Option<String>) -> Self {
        eprintln!("real-binary push: {backend:?}");
        let mut deployment = Deployment::new(backend, false, redis).await;
        deployment.origin = deployment.origin.replacen("http:", "https:", 1);
        let storage = format!(
            "storage_url = \"file://{}\"\n",
            deployment.dir.path().join("blobs").display()
        );
        let mut first = deployment.node("a");
        prepend(&first.config, &storage);
        append(&first.config, config);
        first.cli(&["migrate"], "");
        for upn in ["a@own.test", "b@own.test", "c@own.test", "d@own.test"] {
            first.add_user(upn, PASSWORD);
        }
        first.start().await;
        let mut second = (backend == Backend::Postgres).then(|| deployment.node("b"));
        if let Some(node) = &mut second {
            prepend(&node.config, &storage);
            append(&node.config, config);
            node.start().await;
        }
        Self {
            origin: deployment.origin.clone(),
            database: deployment.database.clone(),
            first,
            second,
            _deployment: deployment,
        }
    }

    /// The node the second of two clients talks to: the other node when there are two.
    pub fn other(&self) -> &Node {
        self.second.as_ref().unwrap_or(&self.first)
    }

    pub fn two_nodes(&self) -> bool {
        self.second.is_some()
    }

    pub async fn user(&self, node: &Node, upn: &str) -> User {
        User::sign_in(node, &self.origin, upn, options()).await
    }

    /// As [`Rig::user`], its client also reaching the team services authz names.
    pub async fn team_user(&self, node: &Node, upn: &str) -> User {
        let mut user = User::signed_in(node, &self.origin, upn, options()).await;
        user.teams = true;
        user.start();
        user.online(0).await;
        user
    }

    pub async fn stop(mut self) {
        if let Some(node) = &mut self.second {
            node.stop().await;
        }
        self.first.stop().await;
    }

    /// Stops every node, then starts each again: what they serve from here on is what the database kept.
    pub async fn restart(&mut self) {
        if let Some(node) = &mut self.second {
            node.stop().await;
        }
        self.first.stop().await;
        self.first.start().await;
        if let Some(node) = &mut self.second {
            node.start().await;
        }
    }
}

/// Root keys can't follow the configuration's tables, so they go before them.
fn prepend(config: &Path, keys: &str) {
    let rest = std::fs::read_to_string(config).unwrap();
    std::fs::write(config, format!("{keys}{rest}")).unwrap();
}

fn append(config: &Path, tables: &str) {
    let mut file = std::fs::OpenOptions::new().append(true).open(config).unwrap();
    writeln!(file, "{tables}").unwrap();
}

/// One running ngteams client of an account: its session, its engine on its store, its push channel.
pub struct User {
    pub upn: String,
    pub mri: Mri,
    /// The endpoint id of the running push channel (the registration's id).
    pub epid: String,
    pub log: Vec<Event>,
    /// Whether the client's requests reach the team services too.
    teams: bool,
    engine: Option<EngineHandle>,
    session: Arc<Session<MemorySecretStore>>,
    origin: String,
    http: String,
    socket: String,
    options: Options,
    dir: TempDir,
}

impl User {
    /// Signs `upn` in at `node` and starts its client, connected and registered.
    pub async fn sign_in(node: &Node, origin: &str, upn: &str, options: Options) -> Self {
        let mut user = Self::signed_in(node, origin, upn, options).await;
        user.start();
        user.online(0).await;
        user
    }

    /// Signed in, its client not yet started.
    pub async fn signed_in(node: &Node, origin: &str, upn: &str, options: Options) -> Self {
        let session = Arc::new(node.sign_in(upn, PASSWORD).await);
        let skype = session.skypetoken().await.unwrap();
        Self {
            upn: upn.into(),
            mri: Mri::new(format!("8:{}", skype.skype_id.as_deref().unwrap())),
            epid: String::new(),
            log: Vec::new(),
            teams: false,
            engine: None,
            session,
            origin: origin.into(),
            http: node.endpoint.clone(),
            socket: node.endpoint.replacen("http://", "ws://", 1),
            options,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// Signs the account in again on this client, with `password`, and starts it on its store: connected and
    /// registered. What the client was before is closed.
    pub async fn sign_in_again(&mut self, node: &Node, password: &str) {
        self.close();
        self.session = Arc::new(node.sign_in(&self.upn, password).await);
        let mark = self.mark();
        self.start();
        self.online(mark).await;
    }

    pub fn store_path(&self) -> std::path::PathBuf {
        self.dir.path().join("account.sqlite")
    }

    /// Waits until the client's session is signed out for good: its tokens can't be renewed.
    pub async fn until_signed_out(&mut self) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while self.session.signed_out().is_none() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{}: still signed in; events: {:?}",
                self.upn,
                self.log.iter().map(brief).collect::<Vec<_>>()
            );
            self.pump(Duration::from_millis(100)).await;
        }
    }

    /// The account's object id.
    pub fn oid(&self) -> &str {
        self.mri.as_str().strip_prefix("8:orgid:").unwrap()
    }

    /// The client's push socket goes through `link` from now on (its next start).
    pub fn through(&mut self, link: &Link) {
        self.socket = format!("ws://127.0.0.1:{}", link.port);
    }

    fn transport(&self) -> Transport {
        let chats = HttpTransport::for_cloud_with_mock_origin(
            Arc::clone(&self.session),
            &format!("{}/api/chatsvc/own", self.origin),
            Cloud::own(&self.origin).unwrap(),
            &self.http,
        )
        .unwrap()
        .for_account(self.mri.clone());
        let base = |gtm: &str| {
            self.session
                .service_base(gtm)
                .unwrap_or_else(|| panic!("authz names no {gtm}"))
        };
        let chats = match self.session.service_base(AMS_GTM) {
            Some(ams) => chats.with_ams(&ams).unwrap(),
            None => chats,
        };
        if !self.teams {
            return chats;
        }
        chats
            .with_middle_tier(&base(MIDDLE_TIER_GTM))
            .unwrap()
            .with_csa(&base(CSA_GTM))
            .unwrap()
            .with_provisioning(&base(PROVISIONING_GTM))
            .unwrap()
            .with_user_profile(&base(USER_PROFILE_GTM))
            .unwrap()
    }

    /// A typed client of the account, for a request no engine action makes.
    pub fn client(&self) -> Client<Transport> {
        Client::new(self.transport())
    }

    /// Starts the client on its store: a new engine with a new push channel, as a start of ngteams makes them.
    pub fn start(&mut self) {
        assert!(self.engine.is_none());
        let trouter = Trouter::with_mock_origin(Arc::new(self.transport()), Arc::clone(&self.session), &self.socket)
            .unwrap()
            .with_options(self.options);
        trouter.endpoint_id().clone_into(&mut self.epid);
        let engine = engine::start(Parts {
            store: Store::open(&self.store_path()).unwrap(),
            client: Arc::new(Client::new(self.transport())),
            outbox: Outbox::new(Arc::new(self.transport()), Author::new(self.mri.clone(), "QA Operator")),
            calls: None,
            push: Some(engine::Push::new(trouter)),
            options: EngineOptions {
                tick: Duration::from_millis(20),
                catch_up_retry: Duration::from_secs(2),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        self.engine = Some(engine);
    }

    /// Closes the client: its engine is shut down, as a quit or a sign-out does it.
    pub fn close(&mut self) {
        if let Some(engine) = self.engine.take() {
            engine.shutdown();
        }
    }

    /// Quits the client and starts it again on its store, as a quit and a start of ngteams: connected, registered
    /// and caught up.
    pub async fn restart(&mut self) {
        self.close();
        let mark = self.mark();
        self.start();
        self.online(mark).await;
        self.caught_up(mark).await;
    }

    /// Waits for the end of a catch-up the client itself started since `mark`.
    pub async fn caught_up(&mut self, mark: usize) {
        self.since(mark, "catch-up", |event| matches!(event, Event::CaughtUp { .. }))
            .await;
    }

    /// The client's activity feed once `pick` takes it; nothing asks the service for it here.
    pub async fn feed(&mut self, what: &str, pick: impl Fn(&ActivityFeed) -> bool) -> ActivityFeed {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let feed = self.engine().activity().await.unwrap();
            if pick(&feed) {
                return feed;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{}: the feed never showed {what}: {feed:#?}",
                self.upn
            );
            self.pump(Duration::from_millis(100)).await;
        }
    }

    pub fn engine(&self) -> &EngineHandle {
        self.engine.as_ref().unwrap()
    }

    /// Where the log stands: events from here on are the ones after this moment.
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
    pub async fn since(&mut self, mark: usize, what: &str, pick: impl Fn(&Event) -> bool) -> usize {
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

    /// Takes the events of a quiet stretch.
    pub async fn settle(&mut self) {
        let until = tokio::time::Instant::now() + QUIET;
        loop {
            let left = until.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() || !self.pump(left).await {
                return;
            }
        }
    }

    pub fn count(&self, mark: usize, pick: impl Fn(&Event) -> bool) -> usize {
        self.log[mark..].iter().filter(|event| pick(event)).count()
    }

    /// The push channel connected and registered its endpoint since `mark`: deliveries come from here on.
    pub async fn online(&mut self, mark: usize) {
        let connected = self
            .since(mark, "connect", |event| matches!(event, Event::Connected { .. }))
            .await;
        self.since(connected, "registration", |event| {
            matches!(event, Event::Registration(RegistrationReport::Registered))
        })
        .await;
    }

    pub async fn write(&self, action: Action) -> teams_store::OutboxEntry {
        let row = self.engine().write(action).await.unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        row
    }

    pub async fn send(&self, chat: &str, text: &str) -> String {
        self.write(Action::Send {
            conversation: chat.into(),
            text: MessageText::plain(text).unwrap(),
        })
        .await
        .message_id
        .unwrap()
    }

    pub async fn create(&self, others: &[&Self], topic: Option<&str>) -> String {
        self.write(Action::CreateChat {
            others: others.iter().map(|user| user.mri.clone()).collect(),
            topic: topic.map(str::to_owned),
        })
        .await
        .message_id
        .unwrap()
    }

    pub async fn typing(&self, chat: &str) {
        self.engine()
            .transient(Transient::Typing {
                conversation: chat.into(),
            })
            .await
            .unwrap();
    }

    /// Waits until the push channel delivered message `id` as `change` (live or replayed) since `mark`.
    pub async fn pushed(&mut self, mark: usize, chat: &str, id: &str, change: MessageChange) {
        self.since(mark, &format!("pushed {change:?} of {id}"), |event| {
            is_pushed(event, chat, id, change)
        })
        .await;
    }

    /// How often the push channel delivered message `id` as `change` since `mark`.
    pub fn pushes(&self, mark: usize, chat: &str, id: &str, change: MessageChange) -> usize {
        self.count(mark, |event| is_pushed(event, chat, id, change))
    }

    /// The stored message's text holds `text`.
    pub async fn shows(&self, chat: &str, id: &str, text: &str) {
        let message = self.engine().message(chat, id).await.unwrap();
        let message = message.unwrap_or_else(|| panic!("{}: message {id} is not stored", self.upn));
        assert!(message.content.contains(text), "{}: {id} lacks {text:?}", self.upn);
    }

    /// The ids of the chat's stored messages whose text holds `marker`, the oldest first.
    pub async fn stored(&self, chat: &str, marker: &str) -> Vec<String> {
        let page = self.engine().messages(chat, None, 1000).await.unwrap();
        let mut ids: Vec<String> = page
            .messages
            .into_iter()
            .filter(|message| message.content.contains(marker))
            .map(|message| message.id)
            .collect();
        ids.reverse();
        ids
    }

    /// The message once the store holds it; nothing asks the service for it here.
    pub async fn eventually_stored(&mut self, chat: &str, id: &str) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while self.engine().message(chat, id).await.unwrap().is_none() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{}: message {id} never reached the store; events: {:?}",
                self.upn,
                self.log.iter().map(brief).collect::<Vec<_>>()
            );
            self.pump(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for User {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn is_pushed(event: &Event, chat: &str, message: &str, wanted: MessageChange) -> bool {
    matches!(event, Event::Message { conversation, id, change, source: Source::Delivery | Source::Replay, .. }
        if conversation == chat && id == message && *change == wanted)
}

/// An event's kind and what it is about, without its payload.
pub fn brief(event: &Event) -> String {
    match event {
        Event::Message {
            id,
            change,
            applied,
            source,
            ..
        } => format!("Message({id} {change:?} {applied:?} {source:?})"),
        Event::Connected { .. } => "Connected".into(),
        Event::Registration(report) => format!("Registration({report:?})"),
        Event::Disconnected { down, .. } => format!("Disconnected({down:?})"),
        Event::PushStopped(stop) => format!("PushStopped({stop:?})"),
        Event::CaughtUp { cause, .. } => format!("CaughtUp({cause:?})"),
        Event::CatchUpFailed { cause, .. } => format!("CatchUpFailed({cause:?})"),
        Event::Typing { typing, .. } => format!("Typing({typing})"),
        Event::ReadHorizon { .. } => "ReadHorizon".into(),
        Event::ConversationChanged { .. } => "ConversationChanged".into(),
        Event::Failed { during, .. } => format!("Failed({during})"),
        other => {
            let text = format!("{other:?}");
            text.split(['(', '{', ' ']).next().unwrap_or_default().to_owned()
        }
    }
}

/// The network between a client's push socket and its node: a loopback relay that can be cut and restored.
pub struct Link {
    pub port: u16,
    down: Arc<AtomicBool>,
    cut: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

/// Carries one connection to `target` until either side ends it or the link is cut.
async fn relay(mut client: TcpStream, target: String, cut: Arc<Notify>) {
    let Ok(mut server) = TcpStream::connect(&target).await else {
        return;
    };
    tokio::select! {
        _ = tokio::io::copy_bidirectional(&mut client, &mut server) => {}
        () = cut.notified() => {}
    }
}

impl Link {
    pub async fn to(node: &Node) -> Self {
        let target = node.endpoint.strip_prefix("http://").unwrap().to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (down, cut) = (Arc::new(AtomicBool::new(false)), Arc::new(Notify::new()));
        let (is_down, on_cut) = (Arc::clone(&down), Arc::clone(&cut));
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                if !is_down.load(Ordering::SeqCst) {
                    tokio::spawn(relay(client, target.clone(), Arc::clone(&on_cut)));
                }
            }
        });
        Self { port, down, cut, task }
    }

    /// Drops every connection and takes no new one.
    pub fn cut(&self) {
        self.down.store(true, Ordering::SeqCst);
        self.cut.notify_waiters();
    }

    pub fn restore(&self) {
        self.down.store(false, Ordering::SeqCst);
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The run's Redis that a test may stop: a podman container of its own, which no other test's nodes use.
pub struct OwnRedis {
    container: String,
    pub url: String,
}

/// Names the container and its URL, as `<container>=<url>`.
pub const OWN_REDIS_ENV: &str = "NGTEAMS_SERVER_STOP_REDIS";

impl OwnRedis {
    pub fn from_env() -> Option<Self> {
        let named = std::env::var(OWN_REDIS_ENV).ok()?;
        let (container, url) = named.split_once('=')?;
        Some(Self {
            container: container.into(),
            url: url.into(),
        })
    }

    fn podman(&self, args: &[&str]) {
        let done = std::process::Command::new("podman")
            .args(args)
            .arg(&self.container)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(done.success(), "podman {args:?} {}", self.container);
    }

    async fn answers(&self) -> bool {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let address = self.url.strip_prefix("redis://").unwrap();
        let Ok(mut stream) = TcpStream::connect(address).await else {
            return false;
        };
        let mut answer = [0_u8; 5];
        stream.write_all(b"PING\r\n").await.is_ok()
            && matches!(
                tokio::time::timeout(Duration::from_secs(1), stream.read_exact(&mut answer)).await,
                Ok(Ok(_))
            )
            && answer == *b"+PONG"
    }

    /// Stops Redis: nothing answers at its URL from here on.
    pub async fn stop(&self) {
        self.podman(&["stop", "-t", "2"]);
        assert!(!self.answers().await, "Redis still answers");
    }

    /// Starts Redis (again) and waits until it answers.
    pub async fn start(&self) {
        self.podman(&["start"]);
        let deadline = tokio::time::Instant::now() + WAIT;
        while !self.answers().await {
            assert!(tokio::time::Instant::now() < deadline, "Redis never answered");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// An access token of `upn` for `resource`, from the password grant at `node`.
pub async fn token(node: &Node, upn: &str, resource: &str) -> String {
    let answer: Value = node
        .http
        .post(format!("{}/oauth2/v2.0/token", node.endpoint))
        .form(&[
            ("client_id", CLIENT_ID),
            ("grant_type", "password"),
            ("username", upn),
            ("password", PASSWORD),
            ("scope", &format!("{resource}/.default openid profile offline_access")),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    answer["access_token"].as_str().unwrap().to_owned()
}

/// A registration as the client's messaging stack posts it; the registrar's status.
pub async fn register(node: &Node, token: &str, epid: &str, path: &str) -> u16 {
    let body = json!({
        "clientDescription": {"appId": "TeamsCDLWebWorker", "aesKey": "", "languageId": "en-US", "platform": "chrome",
            "templateKey": "TeamsCDLWebWorker_2.6", "platformUIVersion": "1/1"},
        "registrationId": epid, "nodeId": "",
        "transports": {"TROUTER": [{"context": "", "path": path, "ttl": 3600}]},
    });
    node.http
        .post(format!("{}/registrar/prod/V2/registrations", node.endpoint))
        .header("X-MS-Migration", "True")
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// A bare push socket, for what the client's own session never sends.
pub struct Raw(WebSocketStream<MaybeTlsStream<TcpStream>>);

impl Raw {
    pub async fn open(node: &Node, origin: &str, epid: &str) -> Self {
        let tc = "%7B%22cv%22:%221%22,%22ua%22:%22TeamsCDL%22,%22hr%22:%22%22,%22v%22:%221%22%7D";
        let url = format!(
            "{}/trouter/v4/c?tc={tc}&timeout=40&epid={epid}&cor_id=c&con_num=1_1",
            node.endpoint.replacen("http://", "ws://", 1)
        );
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert("origin", origin.parse().unwrap());
        Self(tokio_tungstenite::connect_async(request).await.unwrap().0)
    }

    pub async fn send(&mut self, text: &str) {
        self.0.send(Message::text(text.to_owned())).await.unwrap();
    }

    /// The next text frame within `wait`; `Ok(None)` once the socket closed.
    pub async fn next_within(&mut self, wait: Duration) -> Result<Option<String>, tokio::time::error::Elapsed> {
        tokio::time::timeout(wait, async {
            loop {
                match self.0.next().await {
                    Some(Ok(Message::Text(text))) => return Some(text.as_str().to_owned()),
                    Some(Ok(Message::Close(_)) | Err(_)) | None => return None,
                    Some(Ok(_)) => {}
                }
            }
        })
        .await
    }

    pub async fn next(&mut self) -> Option<String> {
        self.next_within(Duration::from_secs(15))
            .await
            .expect("no frame in time")
    }

    pub async fn authenticate(&mut self, token: &str) {
        assert_eq!(self.next().await.as_deref(), Some("1::"));
        let argument = json!({"headers": {
            "X-Ms-Test-User": "False", "Authorization": format!("Bearer {token}"), "X-MS-Migration": "True",
        }});
        self.send(&format!(
            "5:::{}",
            json!({"name": "user.authenticate", "args": [argument]})
        ))
        .await;
    }

    /// Authenticates and reads `trouter.connected`'s argument.
    pub async fn connect(&mut self, token: &str) -> Value {
        self.authenticate(token).await;
        let frame = self.next().await.unwrap();
        let event: Value = serde_json::from_str(frame.strip_prefix("5:1::").unwrap()).unwrap();
        assert_eq!(event["name"], "trouter.connected");
        event["args"][0].clone()
    }

    /// Whether the socket ends with this disconnect.
    pub async fn ends_with(&mut self, frame: &str) -> bool {
        self.next().await.as_deref() == Some(frame) && self.next().await.is_none()
    }
}
