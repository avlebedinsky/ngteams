//! Pictures, files and link previews: the real own-cloud engine and typed reads against the media routes, with the
//! blobs in the S3 store `NGTEAMS_SERVER_S3_URL` names (else a local directory).

mod access;
mod files;
mod freeing;
mod limits;
mod pictures;
mod previews;
mod storage;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ngteams_server::config::Config;
use ngteams_server::storage::{Blob, Incoming, Storage, StorageError};
use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_core::Outbox;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts};
use teams_proto::{Author, Client, Cloud, HttpTransport, Mri};
use teams_store::{OutboxState, Store};
use tempfile::TempDir;

use crate::support::{Backend, TestServer};

pub const S3_ENV: &str = "NGTEAMS_SERVER_S3_URL";
pub const ORIGIN: &str = "https://own.example.test";
pub const IC3: &str = "https://ic3.teams.office.com";
pub const GRAPH: &str = "https://graph.microsoft.com";

type Transport = HttpTransport<Session<MemorySecretStore>>;
pub type MediaClient = Client<Transport>;

pub fn cloud() -> Cloud {
    Cloud::own(ORIGIN).unwrap()
}

/// The store the media tests keep blobs in: the S3 server at [`S3_ENV`]
/// (`http://<access key>:<secret>@<host>:<port>/<bucket>`) under a prefix of this run's own, else a directory.
pub async fn configure_storage(config: &mut Config, dir: &std::path::Path) {
    let Ok(s3) = std::env::var(S3_ENV) else {
        config.storage_url = format!("file://{}", dir.join("blobs").display());
        return;
    };
    let s3 = url::Url::parse(&s3).unwrap();
    let bucket = s3.path().trim_matches('/').to_owned();
    let endpoint = format!("{}://{}:{}", s3.scheme(), s3.host_str().unwrap(), s3.port().unwrap());
    storage::ensure_bucket(&endpoint, s3.username(), s3.password().unwrap(), &bucket).await;
    config.storage_url = format!("s3://{bucket}/run-{}", uuid::Uuid::new_v4().simple());
    for (key, value) in [
        ("aws_endpoint", endpoint.as_str()),
        ("aws_region", "us-east-1"),
        ("aws_access_key_id", s3.username()),
        ("aws_secret_access_key", s3.password().unwrap()),
        ("aws_allow_http", "true"),
        ("aws_virtual_hosted_style_request", "false"),
    ] {
        config.storage_options.insert(key.to_owned(), value.to_owned());
    }
}

pub async fn server(backend: Backend, configure: impl FnOnce(&mut Config)) -> (TestServer, TempDir) {
    server_storing(backend, configure, |storage| storage).await
}

async fn server_storing(
    backend: Backend,
    configure: impl FnOnce(&mut Config),
    store: impl FnOnce(Arc<dyn Storage>) -> Arc<dyn Storage>,
) -> (TestServer, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut storage = Config::default();
    configure_storage(&mut storage, dir.path()).await;
    let configure = |config: &mut Config| {
        config.public_origin = url::Url::parse(ORIGIN).unwrap();
        config.storage_url = storage.storage_url;
        config.storage_options = storage.storage_options;
        configure(config);
    };
    let server = TestServer::start_storing(backend, configure, store).await;
    (server, dir)
}

/// The server's store with some of its calls held back, and the keys it holds: `held` picks the calls, named
/// `put <key>` and `delete <key>`, that wait for [`Self::open`].
#[derive(Debug)]
pub struct Held {
    inner: Arc<dyn Storage>,
    waits: fn(&str) -> bool,
    gate: tokio::sync::Semaphore,
    waiting: AtomicUsize,
    keys: Mutex<BTreeSet<String>>,
}

impl Held {
    pub fn new(inner: Arc<dyn Storage>, held: fn(&str) -> bool) -> Arc<Self> {
        Arc::new(Self {
            inner,
            waits: held,
            gate: tokio::sync::Semaphore::new(0),
            waiting: AtomicUsize::new(0),
            keys: Mutex::new(BTreeSet::new()),
        })
    }

    /// Lets every held call go on, now and later.
    pub fn open(&self) {
        self.gate.close();
    }

    pub fn keys(&self) -> Vec<String> {
        self.keys.lock().unwrap().iter().cloned().collect()
    }

    /// Waits until a call is held.
    pub async fn waited_on(&self) {
        while self.waiting.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn pass(&self, call: &str, key: &str) {
        if (self.waits)(&format!("{call} {key}")) {
            self.waiting.fetch_add(1, Ordering::SeqCst);
            let _ = self.gate.acquire().await;
        }
    }

    fn keep(&self, key: &str) {
        self.keys.lock().unwrap().insert(key.to_owned());
    }
}

#[async_trait::async_trait]
impl Storage for Held {
    async fn put(&self, key: &str, bytes: bytes::Bytes) -> Result<(), StorageError> {
        self.pass("put", key).await;
        self.inner.put(key, bytes).await?;
        self.keep(key);
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<bytes::Bytes>, StorageError> {
        self.inner.get(key).await
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.pass("delete", key).await;
        self.inner.delete(key).await?;
        self.keys.lock().unwrap().remove(key);
        Ok(())
    }

    async fn put_stream(&self, key: &str, body: Incoming, limit: u64) -> Result<u64, StorageError> {
        let size = self.inner.put_stream(key, body, limit).await?;
        self.keep(key);
        Ok(size)
    }

    async fn open(&self, key: &str, range: Option<std::ops::Range<u64>>) -> Result<Option<Blob>, StorageError> {
        self.inner.open(key, range).await
    }
}

/// The transport as the account bring-up builds it: every base from authz's GTMs.
pub async fn transport_with(session: Arc<Session<MemorySecretStore>>, server: &TestServer, id: &str) -> Transport {
    session.skypetoken().await.unwrap();
    let base = |gtm: &str| {
        session
            .service_base(gtm)
            .unwrap_or_else(|| panic!("authz names no {gtm}"))
    };
    let (chat, ams, middle_tier) = (
        base(teams_auth::authz::CHAT_BASE_GTM),
        base(teams_core::account::AMS_GTM),
        base(teams_core::account::MIDDLE_TIER_GTM),
    );
    HttpTransport::for_cloud_with_mock_origin(session, &chat, cloud(), &server.origin)
        .unwrap()
        .with_ams(&ams)
        .unwrap()
        .with_middle_tier(&middle_tier)
        .unwrap()
        .for_account(Mri::new(format!("8:orgid:{id}")))
}

pub async fn client(server: &TestServer, upn: &str, id: &str) -> MediaClient {
    Client::new(transport_with(Arc::new(server.session(upn).await), server, id).await)
}

pub async fn engine(server: &TestServer, upn: &str, id: &str, dir: &std::path::Path) -> EngineHandle {
    let session = Arc::new(server.session(upn).await);
    let client = Arc::new(Client::new(transport_with(Arc::clone(&session), server, id).await));
    let transport = Arc::new(transport_with(session, server, id).await);
    engine::start(Parts {
        store: Store::open(&dir.join("account.sqlite")).unwrap(),
        client,
        outbox: Outbox::new(transport, Author::new(Mri::new(format!("8:orgid:{id}")), upn)),
        calls: None,
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(20),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap()
}

/// a and b in a chat, c outside it.
pub struct Trio {
    pub server: TestServer,
    pub ids: [String; 3],
    pub chat: String,
    pub a: EngineHandle,
    pub b: EngineHandle,
    _dirs: [TempDir; 3],
}

impl Trio {
    pub async fn start(backend: Backend) -> Self {
        Self::start_with(backend, |_| {}).await
    }

    pub async fn start_with(backend: Backend, configure: impl FnOnce(&mut Config)) -> Self {
        Self::start_storing(backend, configure, |storage| storage).await
    }

    /// The trio on a store whose `held` calls wait ([`Held`]), and that store.
    pub async fn start_held(
        backend: Backend,
        configure: impl FnOnce(&mut Config),
        held: fn(&str) -> bool,
    ) -> (Self, Arc<Held>) {
        let mut store = None;
        let trio = Self::start_storing(backend, configure, |inner| {
            let held = Held::new(inner, held);
            store = Some(Arc::clone(&held));
            held
        })
        .await;
        (trio, store.unwrap())
    }

    async fn start_storing(
        backend: Backend,
        configure: impl FnOnce(&mut Config),
        store: impl FnOnce(Arc<dyn Storage>) -> Arc<dyn Storage>,
    ) -> Self {
        let (server, blobs) = server_storing(backend, configure, store).await;
        let ids = [
            server.add_user("a@own.test", "Ada").await,
            server.add_user("b@own.test", "Bert").await,
            server.add_user("c@own.test", "Cora").await,
        ];
        let dirs = [blobs, tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let a = engine(&server, "a@own.test", &ids[0], dirs[1].path()).await;
        let b = engine(&server, "b@own.test", &ids[1], dirs[2].path()).await;
        let row = a
            .write(Action::CreateChat {
                others: vec![Mri::new(format!("8:orgid:{}", ids[1]))],
                topic: None,
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent);
        let chat = row.message_id.unwrap();
        assert!(b.sync().await.unwrap().failed.is_empty());
        Self {
            server,
            ids,
            chat,
            a,
            b,
            _dirs: dirs,
        }
    }

    pub async fn outsider(&self) -> MediaClient {
        client(&self.server, "c@own.test", &self.ids[2]).await
    }

    pub async fn stop(self) {
        self.a.shutdown();
        self.b.shutdown();
        self.server.stop().await;
    }
}

/// A PNG of `width` by `height`, each pixel its own colour.
pub fn png(width: u32, height: u32) -> Vec<u8> {
    let picture = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([x.to_le_bytes()[0], y.to_le_bytes()[0], (x + y).to_le_bytes()[0]])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

/// The HTTP status a typed read was refused with.
pub fn status(error: &teams_proto::HttpError) -> Option<u16> {
    match error {
        teams_proto::HttpError::Status(answer) => Some(answer.status()),
        _ => None,
    }
}

/// `upn`'s access token for `audience`, as its session gets one.
pub async fn token(server: &TestServer, upn: &str, audience: &str) -> String {
    let session = server.session(upn).await;
    let token = session.access_token(audience).await.unwrap();
    token.token.expose().to_owned()
}

/// A raw request to the server with `token`; the answer's status, headers and body.
pub async fn raw(
    server: &TestServer,
    method: reqwest::Method,
    path: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> (u16, reqwest::header::HeaderMap, Vec<u8>) {
    let mut request = reqwest::Client::new().request(method, server.url(path));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    let answer = request.send().await.unwrap();
    let status = answer.status().as_u16();
    let headers = answer.headers().clone();
    (status, headers, answer.bytes().await.unwrap().to_vec())
}
