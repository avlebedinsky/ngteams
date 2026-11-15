use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, Scope, TokenClient};
use tempfile::TempDir;

use crate::support::{Backend, CLIENT_ID, database_url};

pub const TLS_DIR: &str = "NGTS_QA_TLS_DIR";
pub const LOGIN_WINDOW_SECONDS: u64 = 30;

pub struct Deployment {
    pub dir: TempDir,
    pub database: String,
    pub origin: String,
    pub tls: bool,
    pub redis: Option<String>,
    _port: Port,
}

impl Deployment {
    pub async fn new(backend: Backend, tls: bool, redis: Option<String>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let database = database_url(backend, &dir).await;
        let port = Port::take();
        let origin = format!("{}://127.0.0.1:{}", if tls { "https" } else { "http" }, port.number);
        Self {
            dir,
            database,
            origin,
            tls,
            redis,
            _port: port,
        }
    }

    pub fn node(&self, name: &str) -> Node {
        let own = (name != "a").then(Port::take);
        let port = own.as_ref().map_or_else(
            || url::Url::parse(&self.origin).unwrap().port().unwrap(),
            |port| port.number,
        );
        let endpoint = format!("{}://127.0.0.1:{port}", if self.tls { "https" } else { "http" });
        let config = self.dir.path().join(format!("{name}.toml"));
        let mut text = format!(
            "listen = \"127.0.0.1:{port}\"\npublic_origin = {:?}\ndatabase_url = {:?}\n",
            self.origin, self.database
        );
        if let Some(redis) = &self.redis {
            writeln!(text, "redis_url = {redis:?}").unwrap();
        }
        writeln!(text, "[login]\nmax_failures = 3\nwindow = {LOGIN_WINDOW_SECONDS}").unwrap();
        if self.tls {
            let certs = PathBuf::from(std::env::var_os(TLS_DIR).unwrap());
            writeln!(
                text,
                "[tls]\ncert = {:?}\nkey = {:?}",
                certs.join("leaf.pem"),
                certs.join("key.pem")
            )
            .unwrap();
        }
        std::fs::write(&config, text).unwrap();
        Node {
            config,
            endpoint,
            binary: PathBuf::from(env!("CARGO_BIN_EXE_ngteams-server")),
            process: None,
            http: http_client(),
            _port: own,
        }
    }
}

pub struct Node {
    pub config: PathBuf,
    pub endpoint: String,
    /// The server binary the node and its commands run: this build's, unless a test runs another version.
    pub binary: PathBuf,
    process: Option<Child>,
    pub http: reqwest::Client,
    _port: Option<Port>,
}

impl Node {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("NGTEAMS_SERVER_") {
                command.env_remove(key);
            }
        }
        command.args(["--config", self.config.to_str().unwrap()]);
        command
            .env("NGTEAMS_SERVER_LOG", "warn")
            .env("TOKIO_WORKER_THREADS", "2");
        command
    }

    pub fn cli(&self, args: &[&str], input: &str) -> String {
        let output = self.cli_output(args, input);
        assert!(output.status.success(), "CLI {args:?} failed (output withheld)");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("PRIVATE KEY"));
        stdout
    }

    pub fn cli_output(&self, args: &[&str], input: &str) -> std::process::Output {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    /// A command that may fail: whether it succeeded, and what it wrote to its error output.
    pub fn try_cli(&self, args: &[&str], input: &str) -> (bool, String) {
        let output = self.cli_output(args, input);
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        )
    }

    pub fn add_user(&self, upn: &str, password: &str) {
        let added = self.cli(&["user", "add", upn, "--name", "QA Operator"], &format!("{password}\n"));
        assert!(added.starts_with(&format!("added {upn} (")));
        assert!(!added.contains(password));
    }

    pub async fn start(&mut self) {
        self.start_with_log(Stdio::null()).await;
    }

    /// Starts it with its log (standard error) written to `log`.
    pub async fn start_logged(&mut self, log: &Path) {
        self.start_with_log(std::fs::File::create(log).unwrap().into()).await;
    }

    async fn start_with_log(&mut self, log: Stdio) {
        assert!(self.process.is_none());
        self.process = Some(
            self.command()
                .arg("serve")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while tokio::time::Instant::now() < deadline {
            assert!(
                self.process.as_mut().unwrap().try_wait().unwrap().is_none(),
                "binary exited before ready"
            );
            if self.status("/health/ready").await == Some(204) {
                // a node that couldn't take its port exits; the answer was then another's
                tokio::time::sleep(Duration::from_millis(100)).await;
                assert!(
                    self.process.as_mut().unwrap().try_wait().unwrap().is_none(),
                    "binary exited once ready"
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("binary never became ready");
    }

    pub async fn stop(&mut self) {
        let Some(mut process) = self.process.take() else { return };
        assert!(
            Command::new("kill")
                .args(["-TERM", &process.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            if let Some(status) = process.try_wait().unwrap() {
                assert!(status.success(), "SIGTERM failed");
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let _ = process.kill();
        let _ = process.wait();
        panic!("binary did not stop after SIGTERM");
    }

    pub async fn status(&self, path: &str) -> Option<u16> {
        self.http
            .get(format!("{}{path}", self.endpoint))
            .send()
            .await
            .ok()
            .map(|r| r.status().as_u16())
    }

    pub async fn password(&self, upn: &str, password: &str) -> reqwest::Response {
        self.http
            .post(format!("{}/oauth2/v2.0/token", self.endpoint))
            .form(&[
                ("client_id", CLIENT_ID),
                ("grant_type", "password"),
                ("username", upn),
                ("password", password),
                ("scope", Scope::spaces().as_str()),
            ])
            .send()
            .await
            .unwrap()
    }

    pub async fn sign_in(&self, upn: &str, password: &str) -> Session<MemorySecretStore> {
        let response = self.password(upn, password).await;
        assert_eq!(response.status(), 200);
        let answer: Value = response.json().await.unwrap();
        let store = MemorySecretStore::new();
        store
            .put(
                &refresh_token_key(upn),
                &Secret::new(answer["refresh_token"].as_str().unwrap()),
            )
            .await
            .unwrap();
        self.session(upn, store)
    }

    pub fn session(&self, upn: &str, store: MemorySecretStore) -> Session<MemorySecretStore> {
        Session::new(
            upn,
            store,
            TokenClient::new(EntraConfig::teams_native().with_authority(&self.endpoint)).unwrap(),
            self.authz(),
            Region::default(),
        )
    }

    pub fn authz(&self) -> AuthzClient {
        AuthzClient::with_endpoint(&format!("{}/api/authsvc/v1.0/authz", self.endpoint)).unwrap()
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.kill();
            let _ = process.wait();
        }
    }
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(12))
        .build()
        .unwrap()
}

/// A loopback port only its holder uses, from when it is picked until its node is gone: a node binds it long after
/// (the command line's migration and accounts come first), and a port the system hands out could meanwhile go to
/// another test's node or to an outgoing connection. So the ports are below the system's range for those, and each
/// is locked by a file among every test process of the machine.
pub struct Port {
    pub number: u16,
    _lock: std::fs::File,
}

impl Port {
    const RANGE: std::ops::Range<u16> = 20_000..32_000;

    pub fn take() -> Self {
        let locks = std::env::temp_dir().join("ngteams-server-e2e-ports");
        std::fs::create_dir_all(&locks).unwrap();
        let span = u128::from(Self::RANGE.end - Self::RANGE.start);
        let start = u16::try_from(uuid::Uuid::new_v4().as_u128() % span).unwrap();
        (0..Self::RANGE.end - Self::RANGE.start)
            .map(|step| Self::RANGE.start + (start + step) % (Self::RANGE.end - Self::RANGE.start))
            .find_map(|number| {
                let lock = std::fs::File::create(locks.join(number.to_string())).ok()?;
                lock.try_lock().ok()?;
                std::net::TcpListener::bind(("127.0.0.1", number)).ok()?;
                Some(Self { number, _lock: lock })
            })
            .expect("a free loopback port")
    }
}

// Re-exec isolates the TLS trust environment from parallel tests and the user's trust store.
pub fn tls_parent(test: &str) -> bool {
    if std::env::var_os(TLS_DIR).is_some() {
        return false;
    }
    let dir = tempfile::tempdir().unwrap();
    write_test_ca(dir.path());
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(TLS_DIR, dir.path())
        .env("SSL_CERT_FILE", dir.path().join("ca.pem"))
        .env("SSL_CERT_DIR", dir.path().join("empty"))
        .status()
        .unwrap();
    assert!(status.success(), "TLS child failed");
    true
}

fn write_test_ca(dir: &Path) {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_key = KeyPair::generate().unwrap();
    let ca = params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(params, ca_key);
    let key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["127.0.0.1".into()])
        .unwrap()
        .signed_by(&key, &issuer)
        .unwrap();
    std::fs::write(dir.join("ca.pem"), ca.pem()).unwrap();
    std::fs::write(dir.join("leaf.pem"), leaf.pem()).unwrap();
    std::fs::write(dir.join("key.pem"), key.serialize_pem()).unwrap();
    std::fs::create_dir(dir.join("empty")).unwrap();
}
