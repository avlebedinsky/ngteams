//! The credential broker, `teams-app secrets --lab <alias> --fd <n>`: its command line, the socket it takes over, the
//! account it serves, and the binary itself run as a process.
//!
//! The process runs with a cleared environment (`ui_sandbox::clean_command`) and only its socket passed through
//! (`ui_sandbox::spawn::pass_only`); its `lab.toml` is a made-up one in a temp dir, and its session bus, Wayland display
//! and niri socket are listening canaries the test checks stay untouched. The requests it gets are only ones it must
//! refuse or frames it must close the socket on, so it never connects the Secret Service: nothing here reaches the
//! user's bus or keyring. A served `get`, `put` and `delete` is tested on a `MemorySecretStore` (`run`, in process).

use std::fs;
use std::io::{ErrorKind, Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, IntoRawFd as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use clap::Parser as _;
use teams_app::cli::{Cli, Command};
use teams_app::secrets::{AdoptError, LazyKeyring, adopt_socket, lab_scope, run};
use teams_auth::broker::{BrokerError, BrokerSecretStore, MAX_FRAME};
use teams_auth::secrets::{MemorySecretStore, Secret, SecretError, SecretKey, SecretStore};
use teams_fence::lab::{Alias, LabConfig};
use ui_sandbox::{LineError, Running, clean_command, spawn::pass_only};

const UPN_A: &str = "bot-a@lab.example.test";
const UPN_B: &str = "bot-b@lab.example.test";
const SPACES: &str = "https://api.spaces.skype.com";
const PLANTED: &str = "planted-secret-value-0123456789";
const EXIT_DEADLINE: Duration = Duration::from_secs(20);

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const LAB_TOML: &str = "\
[[accounts]]
alias = \"a\"
upn = \"bot-a@lab.example.test\"

[[accounts]]
alias = \"b\"
upn = \"bot-b@lab.example.test\"

[[accounts]]
alias = \"c\"
upn = \"bot-c@lab.example.test\"
";

fn key(account: &str, audience: &str) -> SecretKey {
    SecretKey::new(account, audience)
}

fn refused<T: std::fmt::Debug>(result: &Result<T, SecretError>) -> bool {
    matches!(result, Err(SecretError::Broker(BrokerError::Refused)))
}

#[test]
fn the_command_line_takes_the_lab_account_and_the_socket() -> TestResult {
    let cli = Cli::try_parse_from(["teams-app", "secrets", "--lab", "b", "--fd", "5"])?;
    let Command::Secrets { alias, fd } = cli.command;
    assert_eq!((alias, fd), (Alias::B, 5));

    for refused in [
        &["teams-app", "secrets", "--lab", "b"][..],
        &["teams-app", "secrets", "--fd", "5"],
        &["teams-app", "secrets", "--lab", "d", "--fd", "5"],
        &["teams-app", "secrets", "--lab", "b", "--fd", "five"],
        &["teams-app", "--lab", "b", "--fd", "5"],
    ] {
        assert!(Cli::try_parse_from(refused).is_err(), "{refused:?}");
    }
    Ok(())
}

#[test]
fn the_scope_is_the_aliased_accounts_own_lab_items() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = LabConfig::parse(LAB_TOML, dir.path())?;

    let scope = lab_scope(&config, Alias::B);

    assert_eq!(scope.application(), "teams-lab");
    assert_eq!(scope.account(), UPN_B);
    assert_eq!(lab_scope(&config, Alias::A).account(), UPN_A);
    Ok(())
}

#[test]
fn only_an_open_unix_stream_socket_is_taken_over() -> TestResult {
    // a socket pair's end: taken, and usable
    let (ours, theirs) = UnixStream::pair()?;
    let mut adopted = adopt_socket(theirs.into_raw_fd())?;
    adopted.write_all(b"ping")?;
    let mut read = [0; 4];
    (&ours).read_exact(&mut read)?;
    assert_eq!(&read, b"ping");

    for fd in [-1, 0, 1, 2] {
        assert!(matches!(adopt_socket(fd), Err(AdoptError::Reserved(_))), "{fd}");
    }
    assert!(matches!(adopt_socket(1_000_000), Err(AdoptError::NotOpen { .. })));

    let file = fs::File::open("/dev/null")?;
    assert!(matches!(
        adopt_socket(file.into_raw_fd()),
        Err(AdoptError::NotAUnixStream { .. })
    ));
    let (datagram, _other) = UnixDatagram::pair()?;
    assert!(matches!(
        adopt_socket(datagram.into_raw_fd()),
        Err(AdoptError::NotAUnixStream { .. })
    ));
    let tcp = std::net::TcpListener::bind("127.0.0.1:0")?;
    assert!(matches!(
        adopt_socket(tcp.into_raw_fd()),
        Err(AdoptError::NotAUnixStream { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn run_serves_the_scopes_own_items_and_refuses_the_rest() -> TestResult {
    let dir = tempfile::tempdir()?;
    let scope = lab_scope(&LabConfig::parse(LAB_TOML, dir.path())?, Alias::B);
    let (server, client) = UnixStream::pair()?;
    let serving = tokio::spawn(async move {
        let store = MemorySecretStore::new();
        let mut log = Vec::new();
        let end = run(server, &scope, &store, &mut log).await;
        (end, store, String::from_utf8_lossy(&log).into_owned())
    });
    let client = BrokerSecretStore::new(client, "teams-lab");

    client.put(&key(UPN_B, SPACES), &Secret::new(PLANTED)).await?;
    let got = client.get(&key(UPN_B, SPACES)).await?.ok_or("stored")?;
    assert_eq!(got.expose(), PLANTED);
    assert!(refused(&client.get(&key(UPN_A, SPACES)).await));
    assert!(refused(&client.put(&key(UPN_A, SPACES), &Secret::new(PLANTED)).await));
    client.delete(&key(UPN_B, "skypetoken")).await?;
    drop(client);

    let (end, store, log) = serving.await?;
    end?;
    assert_eq!(
        store
            .get(&key(UPN_B, SPACES))
            .await?
            .map(|secret| secret.expose().to_owned()),
        Some(PLANTED.to_owned())
    );
    assert_eq!(store.get(&key(UPN_A, SPACES)).await?.map(|_| ()), None);
    // only the refusals are logged, by operation, account and audience
    assert_eq!(log.lines().count(), 2, "{log}");
    assert!(
        log.lines().all(|line| line.contains("refused") && line.contains(UPN_A)),
        "{log}"
    );
    assert!(!log.contains(PLANTED), "{log}");
    Ok(())
}

#[test]
fn the_lazy_keyring_connects_nothing_until_asked() {
    let keyring = LazyKeyring::new("teams-lab");
    assert!(!keyring.connected());
    assert!(format!("{keyring:?}").contains("teams-lab"));
}

/// A listening Unix socket that counts as touched once anything connects to it.
struct Canary {
    name: &'static str,
    listener: UnixListener,
}

impl Canary {
    fn new(name: &'static str, path: &Path) -> TestResult<Self> {
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        Ok(Self { name, listener })
    }

    fn untouched(&self) -> TestResult {
        match self.listener.accept() {
            Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(()),
            other => Err(format!("the {} canary was reached: {other:?}", self.name).into()),
        }
    }
}

/// The broker as a process, for lab account b, on a socket pair; its session bus, Wayland display and niri socket
/// canaries.
struct Broker {
    _dir: tempfile::TempDir,
    canaries: Vec<Canary>,
    running: Running,
    socket: UnixStream,
}

impl Broker {
    fn start() -> TestResult<Self> {
        let dir = tempfile::tempdir()?;
        let config_home = dir.path().join("config");
        fs::create_dir_all(config_home.join("teams-lab"))?;
        fs::write(config_home.join("teams-lab/lab.toml"), LAB_TOML)?;
        let runtime_dir = dir.path().join("run");
        fs::create_dir(&runtime_dir)?;
        fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700))?;
        let bus = dir.path().join("bus");
        let niri = dir.path().join("niri.sock");
        let canaries = vec![
            Canary::new("session bus", &bus)?,
            Canary::new("Wayland display", &runtime_dir.join("wayland-0"))?,
            Canary::new("niri", &niri)?,
        ];

        let (socket, theirs) = UnixStream::pair()?;
        let mut command = clean_command(env!("CARGO_BIN_EXE_teams-app"));
        command
            .args(["secrets", "--lab", "b", "--fd", &theirs.as_raw_fd().to_string()])
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_DATA_HOME", dir.path().join("data"))
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}", bus.display()))
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("NIRI_SOCKET", &niri)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        pass_only(&mut command, &[theirs.as_fd()]);
        let child = command.spawn()?;
        // the broker's end lives only in the broker: its closing is what the test sees
        drop(theirs);
        Ok(Self {
            _dir: dir,
            canaries,
            running: Running::new(child)?,
            socket,
        })
    }

    /// Waits for the exit (the test's end closed first); checks nothing came on stdout, no canary was reached and no
    /// value is in stderr; gives whether it succeeded and its stderr.
    fn finish(self) -> TestResult<(bool, String)> {
        drop(self.socket);
        assert_eq!(
            self.running.line(EXIT_DEADLINE),
            Err(LineError::Closed),
            "stdout stays empty"
        );
        let (status, stderr) = self.running.finish(EXIT_DEADLINE)?;
        for canary in &self.canaries {
            canary.untouched()?;
        }
        assert!(!stderr.contains(PLANTED), "{stderr}");
        Ok((status.success(), stderr))
    }
}

#[tokio::test]
async fn the_broker_refuses_another_account_or_application_and_exits_when_the_socket_closes() -> TestResult {
    let broker = Broker::start()?;
    let lab = BrokerSecretStore::new(broker.socket.try_clone()?, "teams-lab");
    let personal = BrokerSecretStore::new(broker.socket.try_clone()?, "teams-personal");

    assert!(refused(&lab.get(&key(UPN_A, SPACES)).await));
    assert!(refused(&lab.put(&key(UPN_A, SPACES), &Secret::new(PLANTED)).await));
    assert!(refused(&lab.delete(&key(UPN_A, SPACES)).await));
    assert!(refused(&lab.get(&key(UPN_B, "")).await));
    assert!(refused(&personal.get(&key(UPN_B, SPACES)).await));
    assert!(refused(&personal.put(&key(UPN_B, SPACES), &Secret::new(PLANTED)).await));
    drop((lab, personal));

    let (success, stderr) = broker.finish()?;
    assert!(success, "{stderr}");
    assert_eq!(stderr.lines().count(), 6, "{stderr}");
    assert!(stderr.lines().all(|line| line.contains("refused")), "{stderr}");
    Ok(())
}

/// Sends `bytes` raw to a broker process and reads until it closes the socket; what came back, and how it exited.
fn send_raw(bytes: &[u8]) -> TestResult<(Vec<u8>, bool, String)> {
    let broker = Broker::start()?;
    let mut socket = broker.socket.try_clone()?;
    socket.set_read_timeout(Some(EXIT_DEADLINE))?;
    socket.write_all(bytes)?;
    // a close with bytes of ours still unread shows as a reset rather than an end
    let mut back = Vec::new();
    match socket.read_to_end(&mut back) {
        Err(error) if error.kind() == ErrorKind::ConnectionReset => {}
        read => {
            read?;
        }
    }
    drop(socket);
    let (success, stderr) = broker.finish()?;
    Ok((back, success, stderr))
}

#[test]
fn a_malformed_frame_closes_the_socket_and_ends_the_broker() -> TestResult {
    let mut body = vec![9];
    body.extend_from_slice(PLANTED.as_bytes());
    let mut frame = u32::try_from(body.len())?.to_be_bytes().to_vec();
    frame.extend(body);

    let (back, success, stderr) = send_raw(&frame)?;

    assert!(back.is_empty(), "answered {back:?}");
    assert!(!success, "{stderr}");
    assert!(stderr.contains("malformed"), "{stderr}");
    Ok(())
}

#[test]
fn an_oversized_frame_closes_the_socket_and_ends_the_broker() -> TestResult {
    let (back, success, stderr) = send_raw(&u32::try_from(MAX_FRAME + 1)?.to_be_bytes())?;

    assert!(back.is_empty(), "answered {back:?}");
    assert!(!success, "{stderr}");
    assert!(stderr.contains(&(MAX_FRAME + 1).to_string()), "{stderr}");
    Ok(())
}

#[test]
fn a_descriptor_that_isnt_a_socket_stops_the_broker_before_it_reads_anything() -> TestResult {
    let dir = tempfile::tempdir()?;
    let output = ui_sandbox::spawn::output(
        clean_command(env!("CARGO_BIN_EXE_teams-app"))
            .args(["secrets", "--lab", "b", "--fd", "1000000"])
            // no lab.toml there: the descriptor is checked first
            .env("XDG_CONFIG_HOME", dir.path())
            .stdin(Stdio::null()),
    )?;

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("isn't open"), "{stderr}");
    Ok(())
}
