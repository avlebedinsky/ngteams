//! The node itself: health, readiness, TLS and configuration.

use diesel_async::{AsyncConnection as _, AsyncPgConnection, SimpleAsyncConnection as _};
use ngteams_server::app::App;
use ngteams_server::config::{Config, ConfigError, TlsConfig};
use ngteams_server::serve::Listener;

use crate::support::{Backend, TestServer, backends};

#[tokio::test(flavor = "multi_thread")]
async fn the_node_answers_live_and_ready_while_its_database_answers() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let http = reqwest::Client::new();
        for path in ["/health/live", "/health/ready"] {
            assert_eq!(http.get(server.url(path)).send().await.unwrap().status(), 204, "{path}");
        }
        assert_eq!(http.get(server.url("/nothing")).send().await.unwrap().status(), 404);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_node_serves_tls_with_its_configured_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let tls = TlsConfig {
        cert: dir.path().join("cert.pem"),
        key: dir.path().join("key.pem"),
    };
    std::fs::write(&tls.cert, certified.cert.pem()).unwrap();
    std::fs::write(&tls.key, certified.signing_key.serialize_pem()).unwrap();
    let listener = Listener::bind(([127, 0, 0, 1], 0).into()).unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Config {
        public_origin: url::Url::parse(&format!("https://localhost:{port}")).unwrap(),
        database_url: format!("sqlite://{}", dir.path().join("db.sqlite").display()),
        tls: Some(tls.clone()),
        ..Config::default()
    };
    let app = App::start(config).await.unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(listener.serve(ngteams_server::app::router(app), Some(tls), async move {
        let _ = stopped.await;
    }));
    let root = reqwest::Certificate::from_pem(certified.cert.pem().as_bytes()).unwrap();
    let http = reqwest::Client::builder()
        .tls_certs_only([root])
        .resolve("localhost", ([127, 0, 0, 1], port).into())
        .build()
        .unwrap();
    let answer = http
        .get(format!("https://localhost:{port}/health/live"))
        .send()
        .await
        .unwrap();
    assert_eq!(answer.status(), 204);
    stop.send(()).unwrap();
    served.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_with_unreadable_tls_files_does_not_serve() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bad.pem"), "not a pem").unwrap();
    for (cert, key) in [("missing.pem", "missing.pem"), ("bad.pem", "bad.pem")] {
        let tls = TlsConfig {
            cert: dir.path().join(cert),
            key: dir.path().join(key),
        };
        let listener = Listener::bind(([127, 0, 0, 1], 0).into()).unwrap();
        let router = axum::Router::new();
        assert!(listener.serve(router, Some(tls), async {}).await.is_err());
    }
}

#[test]
fn the_configuration_layers_a_file_over_the_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("server.toml");
    std::fs::write(
        &file,
        "public_origin = \"https://teams.example.com\"\nregion = \"emea\"\n[tokens]\naccess_lifetime = 600\n",
    )
    .unwrap();
    let config = Config::load(Some(&file)).unwrap();
    assert_eq!(config.origin(), "https://teams.example.com");
    assert_eq!(config.region, "emea");
    assert_eq!(config.tokens.access_lifetime, 600);
    assert_eq!(config.tokens.reuse_grace, 60);
    assert!(Config::load(None).is_ok());
    std::fs::write(&file, "unknown = 1\n").unwrap();
    assert!(matches!(Config::load(Some(&file)), Err(ConfigError::Load(_))));
}

fn with_origin(origin: &str) -> Config {
    Config {
        public_origin: url::Url::parse(origin).unwrap(),
        ..Config::default()
    }
}

#[test]
fn the_configuration_refuses_an_origin_clients_would_not_take() {
    for origin in [
        "http://teams.example.com",
        "https://teams.example.com/path",
        "https://u@teams.example.com",
    ] {
        assert!(
            matches!(with_origin(origin).check(), Err(ConfigError::PublicOrigin)),
            "{origin}"
        );
    }
    for origin in [
        "http://localhost:1",
        "http://[::1]:1",
        "http://127.0.0.1:1",
        "https://teams.example.com",
    ] {
        assert!(with_origin(origin).check().is_ok(), "{origin}");
    }
}

fn refused(change: fn(&mut Config)) -> ConfigError {
    let mut config = Config::default();
    change(&mut config);
    config.check().unwrap_err()
}

#[test]
fn a_redis_namespace_is_a_short_plain_name() {
    let named = |namespace: &str| Config {
        redis_namespace: Some(namespace.to_owned()),
        ..Config::default()
    };
    for namespace in ["", "two words", "a:b", "{a}"] {
        assert!(
            matches!(named(namespace).check(), Err(ConfigError::Namespace)),
            "{namespace}"
        );
    }
    named("eu-1.staging_2").check().unwrap();
}

#[test]
fn the_configuration_refuses_bad_names_databases_and_lifetimes() {
    assert!(matches!(
        refused(|c| c.region = "Bad Region".into()),
        ConfigError::Region
    ));
    assert!(matches!(refused(|c| c.partition = String::new()), ConfigError::Region));
    assert!(matches!(
        refused(|c| c.database_url = "mysql://x".into()),
        ConfigError::Database
    ));
    assert!(matches!(
        refused(|c| c.tokens.access_lifetime = 0),
        ConfigError::Lifetime
    ));
    assert!(matches!(refused(|c| c.login.window = 0), ConfigError::Lifetime));
    assert!(matches!(refused(|c| c.trouter.auth_timeout = 0), ConfigError::Lifetime));
}

#[test]
fn the_configuration_refuses_media_sizes_and_times_of_zero_but_takes_no_quota() {
    for change in [
        (|c| c.media.max_picture_bytes = 0) as fn(&mut Config),
        |c| c.media.max_file_bytes = 0,
        |c| c.media.link_lifetime = 0,
        |c| c.media.preview_timeout = 0,
        |c| c.media.preview_max_page_bytes = 0,
        |c| c.media.preview_max_picture_bytes = 0,
    ] {
        assert!(matches!(refused(change), ConfigError::Media));
    }
    let mut unlimited = Config::default();
    unlimited.media.user_quota_bytes = 0;
    unlimited.check().unwrap();
}

#[test]
fn the_configuration_refuses_a_store_wait_of_zero_but_takes_no_retries() {
    assert!(matches!(
        refused(|c| c.storage.connect_timeout = 0),
        ConfigError::Storage
    ));
    assert!(matches!(
        refused(|c| c.storage.request_timeout = 0),
        ConfigError::Storage
    ));
    let mut single = Config::default();
    single.storage.max_retries = 0;
    single.storage.retry_timeout = 0;
    single.check().unwrap();
}

#[test]
fn the_configuration_names_the_calling_keys_it_refuses() {
    for change in [
        (|c| c.calling.ring_timeout = 0) as fn(&mut Config),
        |c| c.calling.turn.ttl = 0,
        |c| c.calling.turn.username = Some("relay".into()),
        |c| c.calling.turn.password = Some("synthetic".into()),
        |c| {
            c.calling.turn.username = Some(String::new());
            c.calling.turn.password = Some("synthetic".into());
        },
        |c| {
            c.calling.turn.urls = vec!["turn:turn.own.example.test".to_owned()];
            c.calling.turn.username = Some("relay".into());
            c.calling.turn.password = Some(String::new());
        },
    ] {
        let mut config = Config::default();
        change(&mut config);
        let error = config.check().unwrap_err();
        assert!(matches!(error, ConfigError::Calling), "{error}");
        assert!(error.to_string().contains("calling.turn.username"));
    }
}

#[test]
fn the_configuration_takes_a_relays_urls_in_the_turn_form_only_beside_its_credential() {
    let relay = |urls: &[&str], secret: Option<&str>| {
        let mut config = Config::default();
        config.calling.turn.urls = urls.iter().map(|url| (*url).to_owned()).collect();
        config.calling.turn.secret = secret.map(str::to_owned);
        config.check()
    };
    let secret = Some("shared with coturn");
    for urls in [
        &["stun:turn.own.example.test:3478"][..],
        &["https://turn.own.example.test"],
        &["turn:"],
        &["turn:?transport=udp"],
        &["turns:turn.own.example.test:5349", "turn: turn.own.example.test"],
        &["turn:turn.own.example.test:3478?transport=sctp"],
        &["turn:turn.own.example.test:port"],
    ] {
        let error = relay(urls, secret).unwrap_err();
        assert!(matches!(error, ConfigError::Relay), "{urls:?}: {error}");
        assert!(error.to_string().contains("calling.turn.urls"), "{error}");
    }
    // a credential without a relay to use it at, and a relay with no credential for it
    assert!(matches!(relay(&[], secret), Err(ConfigError::Relay)));
    assert!(matches!(
        relay(&["turn:turn.own.example.test:3478?transport=udp"], None),
        Err(ConfigError::Relay)
    ));
    relay(
        &[
            "turn:turn.own.example.test:3478?transport=udp",
            "turn:turn.own.example.test:3478?transport=tcp",
            "turns:turn.own.example.test:5349",
            "turn:[2001:db8::7]",
            "turn:192.0.2.7:3478",
        ],
        secret,
    )
    .unwrap();
    let mut user = Config::default();
    user.calling.turn.urls = vec!["turn:turn.own.example.test".to_owned()];
    user.calling.turn.username = Some("relay".into());
    user.calling.turn.password = Some("synthetic".into());
    user.check().unwrap();
    Config::default().check().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn housekeeping_rotates_a_due_key_and_the_earlier_tokens_still_verify() {
    let server = TestServer::start_with(crate::support::Backend::Sqlite, |config| config.tokens.key_rotation = 1).await;
    server.add_user("ada@own.test", "Ada").await;
    let session = server.session("ada@own.test").await;
    let before = session.access_token(teams_auth::token::SPACES_RESOURCE).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    ngteams_server::app::tidy(&server.app).await;
    let kid = |token: &teams_auth::secrets::Secret| jsonwebtoken::decode_header(token.expose()).unwrap().kid.unwrap();
    let after = server.session("ada@own.test").await;
    let fresh = after.access_token(teams_auth::token::SPACES_RESOURCE).await.unwrap();
    assert_ne!(kid(&before.token), kid(&fresh.token));
    server
        .authz_client()
        .exchange(&before.token, teams_auth::authz::AuthzKind::TokenRefresh)
        .await
        .unwrap();
    assert!(format!("{:?}", server.app.issuer.keys()).contains("KeyRing"));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_whose_database_is_gone_is_not_ready() {
    if !backends().contains(&crate::support::Backend::Postgres) {
        return;
    }
    let server = TestServer::start(crate::support::Backend::Postgres).await;
    let url = url::Url::parse(&server.app.config.database_url).unwrap();
    let name = url.path().trim_start_matches('/').to_owned();
    let mut admin = AsyncPgConnection::establish(&std::env::var(crate::support::PG_ENV).unwrap())
        .await
        .unwrap();
    admin
        .batch_execute(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .await
        .unwrap();
    let ready = reqwest::get(server.url("/health/ready")).await.unwrap();
    assert_eq!(ready.status(), 503);
    server.stop().await;
}

/// The binary itself: `migrate`, then `serve` until SIGTERM. Linux only, as the binary's harness (`crate::binary`).
#[cfg(target_os = "linux")]
#[test]
fn the_binary_migrates_and_serves_until_terminated() {
    let dir = tempfile::tempdir().unwrap();
    // Locked until the child is gone: `serve` binds it only after `migrate`, while other tests run.
    let taken = crate::binary::support::Port::take();
    let port = taken.number;
    let config = dir.path().join("server.toml");
    std::fs::write(
        &config,
        format!(
            "listen = \"127.0.0.1:{port}\"\npublic_origin = \"http://127.0.0.1:{port}\"\ndatabase_url = \"sqlite://{}\"\n",
            dir.path().join("db.sqlite").display()
        ),
    )
    .unwrap();
    let binary = env!("CARGO_BIN_EXE_ngteams-server");
    let migrated = std::process::Command::new(binary)
        .args(["--config", config.to_str().unwrap(), "migrate"])
        .status()
        .unwrap();
    assert!(migrated.success());
    let failed = std::process::Command::new(binary)
        .args(["--config", dir.path().join("missing.toml").to_str().unwrap(), "migrate"])
        .status()
        .unwrap();
    assert!(!failed.success());
    let mut serving = std::process::Command::new(binary)
        .args(["--config", config.to_str().unwrap(), "serve"])
        .env("NGTEAMS_SERVER_LOG", "warn")
        .spawn()
        .unwrap();
    let live = format!("http://127.0.0.1:{port}/health/live");
    let up = (0..100).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        binary_is_live(port)
    });
    assert!(up, "{live} never answered");
    let terminated = std::process::Command::new("kill")
        .args(["-TERM", &serving.id().to_string()])
        .status()
        .unwrap();
    assert!(terminated.success());
    assert!(serving.wait().unwrap().success());
    drop(taken);
}

#[cfg(target_os = "linux")]
fn binary_is_live(port: u16) -> bool {
    use std::io::{Read as _, Write as _};
    let wait = std::time::Duration::from_millis(500);
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), wait) else {
        return false;
    };
    stream.set_read_timeout(Some(wait)).unwrap();
    stream.set_write_timeout(Some(wait)).unwrap();
    if stream
        .write_all(b"GET /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut response = String::new();
    stream.read_to_string(&mut response).is_ok() && response.starts_with("HTTP/1.1 204 ")
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_on_the_command_line_stops_when_told() {
    use clap::Parser as _;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("server.toml");
    std::fs::write(
        &config,
        format!(
            "listen = \"127.0.0.1:0\"\ndatabase_url = \"sqlite://{}\"\n",
            dir.path().join("db.sqlite").display()
        ),
    )
    .unwrap();
    let cli = ngteams_server::cli::Cli::parse_from(["ngteams-server", "--config", config.to_str().unwrap(), "serve"]);
    ngteams_server::cli::run(cli, &mut "".as_bytes(), &mut Vec::new(), &mut Vec::new(), async {})
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_rotations_leave_a_signer_on_every_node() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let second = TestServer::start_with(backend, |config| {
            config.database_url.clone_from(&server.app.config.database_url);
            config.public_origin.clone_from(&server.app.config.public_origin);
        })
        .await;
        server.add_user("ada@own.test", "Ada").await;
        for _ in 0..12 {
            let (a, b) = tokio::join!(server.app.issuer.keys().rotate(), second.app.issuer.keys().rotate());
            a.unwrap();
            b.unwrap();
            server.app.issuer.keys().reload().await.unwrap();
            second.app.issuer.keys().reload().await.unwrap();
            let rows = ngteams_server::db::keys::all(&server.app.db).await.unwrap();
            assert_eq!(rows.iter().filter(|key| key.retired_at.is_none()).count(), 1);
            for node in [&server, &second] {
                node.session("ada@own.test").await.skypetoken().await.unwrap();
            }
        }
        second.stop().await;
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn retired_keys_are_removed_after_access_lifetime_and_reload_slack() {
    use diesel::{ExpressionMethods as _, QueryDsl as _};
    use diesel_async::RunQueryDsl as _;
    use ngteams_server::db::{keys, now, schema::signing_keys, with_conn};
    for backend in backends() {
        let server = TestServer::start(backend).await;
        server.add_user("ada@own.test", "Ada").await;
        let stale = TestServer::start_with(backend, |config| {
            config.database_url.clone_from(&server.app.config.database_url);
            config.public_origin.clone_from(&server.app.config.public_origin);
        })
        .await;
        let previous = keys::all(&server.app.db).await.unwrap()[0].kid.clone();
        server.app.issuer.keys().rotate().await.unwrap();
        for (slack, kept) in [(3601, true), (7261, true), (10801, true), (10861, false)] {
            let retired_at = now() - i64::try_from(server.app.config.tokens.access_lifetime).unwrap() - slack;
            async {
                with_conn!(&server.app.db, |conn| diesel::update(
                    signing_keys::table.find(&previous)
                )
                .set(signing_keys::retired_at.eq(Some(retired_at)))
                .execute(conn)
                .await?);
                Ok::<_, ngteams_server::db::DbError>(())
            }
            .await
            .unwrap();
            let session = stale.session("ada@own.test").await;
            let token = session.access_token(teams_auth::token::SPACES_RESOURCE).await.unwrap();
            assert_eq!(
                jsonwebtoken::decode_header(token.token.expose())
                    .unwrap()
                    .kid
                    .as_deref(),
                Some(previous.as_str())
            );
            ngteams_server::app::tidy(&server.app).await;
            let present = keys::all(&server.app.db)
                .await
                .unwrap()
                .iter()
                .any(|key| key.kid == previous);
            assert_eq!(present, kept, "{backend:?}: reload slack {slack}");
            let exchange = server
                .authz_client()
                .exchange(&token.token, teams_auth::authz::AuthzKind::TokenRefresh)
                .await;
            assert_eq!(exchange.is_ok(), kept, "{backend:?}: token at reload slack {slack}");
        }
        stale.stop().await;
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn debug_logging_never_formats_postgres_signing_keys() {
    if !backends().contains(&crate::support::Backend::Postgres) {
        return;
    }
    let server = TestServer::start(crate::support::Backend::Postgres).await;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("server.toml");
    std::fs::write(
        &config,
        format!("database_url = {:?}\n", server.app.config.database_url),
    )
    .unwrap();
    for filter in ["debug", "trace,tokio_postgres=trace"] {
        let answer = std::process::Command::new(env!("CARGO_BIN_EXE_ngteams-server"))
            .args(["--config", config.to_str().unwrap(), "keys", "rotate"])
            .env("NGTEAMS_SERVER_LOG", filter)
            .output()
            .unwrap();
        assert!(answer.status.success());
        let stderr = String::from_utf8(answer.stderr).unwrap();
        for key in ngteams_server::db::keys::all(&server.app.db).await.unwrap() {
            assert!(
                !stderr.contains(&format!("{:?}", key.private_der)),
                "private key reached stderr"
            );
        }
        assert!(!stderr.contains("ToSqlHelper"), "SQL bind helper reached stderr");
    }
    server.stop().await;
}

#[test]
fn debug_omits_config_credentials_and_minted_tokens() {
    let mut config = Config {
        database_url: "postgres://user:synthetic-database-secret@localhost/db".into(),
        redis_url: Some("redis://:synthetic-redis-secret@localhost/".into()),
        storage_url: "s3://synthetic-storage-url-secret".into(),
        ..Config::default()
    };
    config
        .storage_options
        .insert("access_key_id".into(), "synthetic-storage-key-secret".into());
    let debug = format!("{config:?}");
    for secret in [
        "synthetic-database-secret",
        "synthetic-redis-secret",
        "synthetic-storage-url-secret",
        "synthetic-storage-key-secret",
    ] {
        assert!(!debug.contains(secret));
    }
    let minted = ngteams_server::auth::tokens::Minted {
        token: "synthetic-minted-token-secret".into(),
        expires_at: 42,
    };
    let debug = format!("{minted:?}");
    assert!(!debug.contains("synthetic-minted-token-secret"));
    assert!(debug.contains("42"));
}

/// The head of the answer to `request` (sent with `body`, then the connection left open), as the server writes it.
async fn answer_head(server: &TestServer, request: &str, body: &[u8]) -> String {
    use tokio::io::AsyncWriteExt as _;
    let address = server.origin.trim_start_matches("http://");
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    read_head(&mut stream).await
}

/// The next answer head on `stream`, in lower case.
async fn read_head(stream: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt as _;
    let mut head = Vec::new();
    let mut byte = [0; 1];
    while !head.ends_with(b"\r\n\r\n") {
        let read = tokio::time::timeout(std::time::Duration::from_secs(20), stream.read(&mut byte))
            .await
            .expect("the head came in time")
            .unwrap();
        assert_eq!(
            read,
            1,
            "the connection ended before the head: {}",
            String::from_utf8_lossy(&head)
        );
        head.push(byte[0]);
    }
    String::from_utf8(head).unwrap().to_ascii_lowercase()
}

/// Reads and drops on `stream` the body of the answer that has `head`, which must give its `content-length`.
async fn skip_body(stream: &mut tokio::net::TcpStream, head: &str) {
    use tokio::io::AsyncReadExt as _;
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .unwrap_or_else(|| panic!("no content-length: {head}"))
        .trim()
        .parse()
        .unwrap();
    let mut body = vec![0; length];
    tokio::time::timeout(std::time::Duration::from_secs(20), stream.read_exact(&mut body))
        .await
        .expect("the body came in time")
        .unwrap();
}

/// A body refused before it was read to its end is read and dropped before the answer, so the connection carries the
/// next request. A body too big for that closes the connection, and the answer says so.
#[tokio::test(flavor = "multi_thread")]
async fn a_body_the_answer_left_unread_is_read_to_its_end_or_closes_the_connection() {
    let server = TestServer::start(Backend::Sqlite).await;
    let too_big = "POST /oauth2/v2.0/token HTTP/1.1\r\nhost: x\r\ncontent-type: application/x-www-form-urlencoded\r\n\
                   content-length: 100000000\r\n\r\n";
    let refused = answer_head(&server, too_big, &vec![b'a'; 256 * 1024]).await;
    assert!(refused.starts_with("http/1.1 413"), "{refused}");
    assert!(refused.contains("\r\nconnection: close\r\n"), "{refused}");
    let small = "POST /oauth2/v2.0/token HTTP/1.1\r\nhost: x\r\ncontent-type: application/x-www-form-urlencoded\r\n\
                 content-length: 4\r\n\r\n";
    let read = answer_head(&server, small, b"a=bc").await;
    assert!(
        read.starts_with("http/1.1 4") && !read.starts_with("http/1.1 413"),
        "{read}"
    );
    assert!(!read.contains("connection: close"), "{read}");
    // a body over the limit but small enough to drain: the next request on the same connection gets its answer
    {
        use tokio::io::AsyncWriteExt as _;
        let over = "POST /oauth2/v2.0/token HTTP/1.1\r\nhost: x\r\ncontent-type: application/x-www-form-urlencoded\r\n\
                    content-length: 2097152\r\n\r\n";
        let address = server.origin.trim_start_matches("http://");
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream.write_all(over.as_bytes()).await.unwrap();
        stream.write_all(&vec![b'a'; 2 * 1024 * 1024]).await.unwrap();
        stream
            .write_all(b"GET /health/live HTTP/1.1\r\nhost: x\r\n\r\n")
            .await
            .unwrap();
        let drained = read_head(&mut stream).await;
        assert!(drained.starts_with("http/1.1 413"), "{drained}");
        assert!(!drained.contains("connection: close"), "{drained}");
        skip_body(&mut stream, &drained).await;
        let next = read_head(&mut stream).await;
        assert!(next.starts_with("http/1.1 204"), "{next}");
    }
    // a client that sends its next request at once on the same connection never loses it
    let http = reqwest::Client::new();
    for _ in 0..50 {
        let refused = http
            .post(server.token_endpoint())
            .header("content-type", "application/x-www-form-urlencoded")
            .body(vec![b'a'; 2 * 1024 * 1024])
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), 413);
        let live = http.get(server.url("/health/live")).send().await.unwrap();
        assert_eq!(live.status(), 204);
    }
    server.stop().await;
}
