//! An own-server account's engine whose first write needs a token: the refresh and the write go over TLS from the
//! engine's own thread, which a debug build once overflowed. The engine runs with half of a thread's default stack,
//! so the write keeps twice the depth it needs.

use std::process::{Command, Stdio};

use ngteams_server::admin::{self, Actor, NewAccount};
use ngteams_server::app::{self, App};
use ngteams_server::config::{Config, TlsConfig};
use ngteams_server::serve::Listener;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::cloud::Cloud;
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_auth::session::Session;
use teams_auth::token::{Scope, TokenClient};
use teams_core::account::{self, Account};
use teams_core::engine::Action;
use teams_proto::Mri;
use teams_store::OutboxState;

use crate::support::{CLIENT_ID, PASSWORD};

/// Set in the child that runs the engine: a stack overflow aborts that process only, and the parent says so.
const CHILD: &str = "NGTEAMS_FIRST_WRITE_CHILD";
/// The child's threads' stack: half of Rust's default 2 MiB.
const HALF_STACK: &str = "1048576";
const ADA: &str = "ada@own.test";
const BOB: &str = "bob@own.test";

#[test]
fn an_engines_first_write_refreshes_its_token_and_goes_out_over_tls() {
    if std::env::var_os(CHILD).is_some() {
        first_write();
        return;
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "first_write::an_engines_first_write_refreshes_its_token_and_goes_out_over_tls",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("RUST_MIN_STACK", HALF_STACK)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the engine's process ended with {}\n--- stdout\n{stdout}\n--- stderr\n{stderr}",
        output.status
    );
    assert!(stdout.contains("1 passed"), "the child ran no test\n{stdout}\n{stderr}");
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn first_write() {
    let dir = tempfile::tempdir().unwrap();
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let pem = certified.cert.pem();
    let tls = TlsConfig {
        cert: dir.path().join("cert.pem"),
        key: dir.path().join("key.pem"),
    };
    std::fs::write(&tls.cert, &pem).unwrap();
    std::fs::write(&tls.key, certified.signing_key.serialize_pem()).unwrap();
    let listener = Listener::bind(([127, 0, 0, 1], 0).into()).unwrap();
    let port = listener.local_addr().unwrap().port();
    let origin = format!("https://localhost:{port}");
    let config = Config {
        public_origin: url::Url::parse(&origin).unwrap(),
        database_url: format!("sqlite://{}", dir.path().join("db.sqlite").display()),
        tls: Some(tls.clone()),
        ..Config::default()
    };
    let app = App::start(config).await.unwrap();
    for (upn, name) in [(ADA, "Ada"), (BOB, "Bob")] {
        let new = NewAccount {
            upn,
            display_name: name,
            password: PASSWORD,
            admin: false,
            must_change_password: false,
        };
        admin::create_user(&app.db, Actor::CLI, &new).await.unwrap();
    }
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(listener.serve(app::router(app), Some(tls), async move {
        let _ = stopped.await;
    }));
    let cloud = Cloud::own(&origin).unwrap().trusting(pem.as_bytes()).unwrap();
    let grant = Grant::new(&origin, &pem);

    let (ada, region) = grant.mri_and_region(ADA, &cloud).await;
    let (bob, _) = grant.mri_and_region(BOB, &cloud).await;

    // only a refresh token, as after a sign-in: the write's token is the engine's first refresh
    let started = account::start_for_cloud(
        Account {
            upn: ADA.into(),
            mri: ada.clone(),
            display_name: "Ada".into(),
            store_file: dir.path().join("account.sqlite"),
            region,
            secrets: grant.signed_in(ADA).await,
            push: false,
            known_names: vec![(ada, "Ada".into())],
            calls: None,
            read_only: false,
        },
        teams_core::CallFloor::new(),
        &cloud,
    )
    .unwrap();
    let row = started
        .handle
        .write(Action::CreateChat {
            others: vec![Mri::new(&bob)],
            topic: None,
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent);
    started.handle.shutdown();
    stop.send(()).unwrap();
    served.await.unwrap().unwrap();
}

/// The password grant over TLS, as the client's sign-in form posts it.
struct Grant {
    endpoint: String,
    http: reqwest::Client,
}

impl Grant {
    fn new(origin: &str, pem: &str) -> Self {
        let root = reqwest::Certificate::from_pem(pem.as_bytes()).unwrap();
        Self {
            endpoint: format!("{origin}/oauth2/v2.0/token"),
            http: reqwest::Client::builder().tls_certs_only([root]).build().unwrap(),
        }
    }

    /// `upn`'s MRI and region, from a session of its own.
    async fn mri_and_region(&self, upn: &str, cloud: &Cloud) -> (String, Region) {
        let session = Session::new(
            upn,
            self.signed_in(upn).await,
            TokenClient::for_cloud(cloud).unwrap(),
            AuthzClient::for_cloud(cloud).unwrap(),
            Region::default(),
        );
        let mri = format!("8:{}", session.skypetoken().await.unwrap().skype_id.unwrap());
        (mri, session.region())
    }

    /// A store holding only `upn`'s refresh token from a fresh password grant.
    async fn signed_in(&self, upn: &str) -> MemorySecretStore {
        let answer: serde_json::Value = self
            .http
            .post(&self.endpoint)
            .form(&[
                ("client_id", CLIENT_ID),
                ("grant_type", "password"),
                ("username", upn),
                ("password", PASSWORD),
                ("scope", Scope::spaces().as_str()),
            ])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let store = MemorySecretStore::new();
        let refresh = answer["refresh_token"].as_str().unwrap();
        store.put(&refresh_token_key(upn), &Secret::new(refresh)).await.unwrap();
        store
    }
}
