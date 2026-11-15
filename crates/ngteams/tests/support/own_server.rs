//! An in-process own server (`ngteams-server`) on loopback for the client's tests: TLS with a certificate a made-up
//! certificate authority signed (the CA's PEM is what an account's `ca_file` names), SQLite in a temporary directory,
//! and made-up users. Nothing leaves loopback.

use std::path::PathBuf;
use std::sync::Arc;

use ngteams_server::admin::{self, Actor, NewAccount};
use ngteams_server::app::{self, App};
use ngteams_server::config::{Config, TlsConfig};
use ngteams_server::serve::Listener;
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};

/// The password of every user [`OwnServer::add_user`] adds.
pub const PASSWORD: &str = "correct horse battery";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A running own server; stopped when dropped.
pub struct OwnServer {
    /// `https://127.0.0.1:<port>`.
    pub origin: String,
    /// The certificate authority that signed its certificate (PEM).
    pub ca_file: PathBuf,
    app: Arc<App>,
    runtime: Option<tokio::runtime::Runtime>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    _dir: tempfile::TempDir,
}

impl OwnServer {
    /// Starts it on a runtime of its own.
    pub fn start() -> TestResult<Self> {
        let dir = tempfile::tempdir()?;
        let ca_key = KeyPair::generate()?;
        let mut ca = CertificateParams::new(Vec::new())?;
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.distinguished_name
            .push(rcgen::DnType::CommonName, "ngteams test certificate authority");
        let ca_cert = ca.self_signed(&ca_key)?;
        let key = KeyPair::generate()?;
        let leaf = CertificateParams::new(vec!["127.0.0.1".to_owned()])?;
        let cert = leaf.signed_by(&key, &Issuer::new(ca, ca_key))?;
        let tls = TlsConfig {
            cert: dir.path().join("cert.pem"),
            key: dir.path().join("key.pem"),
        };
        std::fs::write(&tls.cert, cert.pem())?;
        std::fs::write(&tls.key, key.serialize_pem())?;
        let ca_file = dir.path().join("ca.pem");
        std::fs::write(&ca_file, ca_cert.pem())?;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let database = dir.path().join("server.sqlite");
        let (origin, app) = runtime.block_on(async {
            let listener = Listener::bind(([127, 0, 0, 1], 0).into())?;
            let address = listener.local_addr()?;
            let origin = format!("https://127.0.0.1:{}", address.port());
            let config = Config {
                listen: address,
                public_origin: origin.parse()?,
                database_url: format!("sqlite://{}", database.display()),
                tls: Some(tls.clone()),
                ..Config::default()
            };
            let app = App::start(config).await?;
            let router = app::router(Arc::clone(&app));
            tokio::spawn(listener.serve(router, Some(tls), async move {
                let _ = stopped.await;
            }));
            TestResult::Ok((origin, app))
        })?;
        Ok(Self {
            origin,
            ca_file,
            app,
            runtime: Some(runtime),
            stop: Some(stop),
            _dir: dir,
        })
    }

    /// Adds the user `upn`, named `name`, with [`PASSWORD`]; its object id.
    pub fn add_user(&self, upn: &str, name: &str) -> TestResult<String> {
        let runtime = self.runtime.as_ref().ok_or("the server's runtime")?;
        let new = NewAccount {
            upn,
            display_name: name,
            password: PASSWORD,
            admin: false,
            must_change_password: false,
        };
        Ok(runtime.block_on(admin::create_user(&self.app.db, Actor::CLI, &new))?.id)
    }
}

impl Drop for OwnServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(std::time::Duration::from_secs(5));
        }
    }
}
