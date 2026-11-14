//! Pictures, files and link previews: the real own-cloud engine and typed reads against the media routes, with the
//! blobs in the S3 store `NGTEAMS_SERVER_S3_URL` names (else a local directory).

mod files;
mod pictures;
mod previews;
mod storage;

use std::sync::Arc;
use std::time::Duration;

use ngteams_server::config::Config;
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
    let dir = tempfile::tempdir().unwrap();
    let mut storage = Config::default();
    configure_storage(&mut storage, dir.path()).await;
    let server = TestServer::start_with(backend, |config| {
        config.public_origin = url::Url::parse(ORIGIN).unwrap();
        config.partition = "own-01".to_owned();
        config.storage_url = storage.storage_url;
        config.storage_options = storage.storage_options;
        configure(config);
    })
    .await;
    (server, dir)
}

pub fn transport_with(session: Arc<Session<MemorySecretStore>>, server: &TestServer, id: &str) -> Transport {
    HttpTransport::for_cloud_with_mock_origin(
        session,
        &format!("{ORIGIN}/api/chatsvc/own"),
        cloud(),
        &server.origin,
    )
    .unwrap()
    .with_ams(&format!("{ORIGIN}/ams"))
    .unwrap()
    .with_middle_tier(&format!("{ORIGIN}/api/mt/part/own-01"))
    .unwrap()
    .for_account(Mri::new(format!("8:orgid:{id}")))
}

pub async fn client(server: &TestServer, upn: &str, id: &str) -> MediaClient {
    Client::new(transport_with(Arc::new(server.session(upn).await), server, id))
}

pub async fn engine(server: &TestServer, upn: &str, id: &str, dir: &std::path::Path) -> EngineHandle {
    let session = Arc::new(server.session(upn).await);
    let client = Arc::new(Client::new(transport_with(Arc::clone(&session), server, id)));
    let transport = Arc::new(transport_with(session, server, id));
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
        let (server, blobs) = server(backend, configure).await;
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
        image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
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
