//! The sign-in window in the UI sandbox, on a local fake sign-in page (`harness = false`: GTK runs on the process's
//! main thread).
//!
//! The parent runs a local HTTP server that records every request it gets, and a TCP listener for `WebSockets` that
//! must never be connected to. It runs itself again inside the sandbox ([`Sandbox::spawn`]: the sandbox's compositor
//! and bus, nothing of the desktop's, a private `HOME`), where the child opens the window ([`window::run`]) on the
//! fake page with a `fence.log` of the test's own, and prints its answer. Two scenarios:
//!
//! - `channels`: the page tries a `fetch` `POST`, a beacon, an image, a frame whose form posts, a worker whose
//!   `fetch` posts, a `WebSocket` and `window.open`, reports what it saw by `GET`, then navigates to the `nativeclient`
//!   redirect with a code and the request's `state`. The window must answer with that redirect; no `POST` and no popup
//!   may reach the server, the `WebSocket` listener must stay unconnected, service workers, `WebSocket` and beacons
//!   must be off, every request the server got must have its `fence.log` line, the code must not be in `fence.log`, and the
//!   private `HOME` must hold no website data.
//! - `gaps`: pins what the guard can't see (the plan's Task 24; `docs/backlog/webkit-sign-in-guard-redirects.md`): a
//!   synchronous `GET` redirected by a 302 and a synchronous `POST` the guard allows (a test-only local own-state rule)
//!   redirected by a 307 reach their targets with no decision and no `fence.log` line; the same `POST` sent
//!   asynchronously has its redirect decided and denied. A kept-alive `GET` redirected after its page is gone is
//!   reported. If `WebKit` ever decides those redirects, this test fails and the backlog item can be revisited.
//! - `remote`: the window stays on loopback for its whole run. The page names a "remote" host: this machine's own
//!   non-loopback address (numeric, so not loopback to the guard, the policy or the proxy bypass), where two counting
//!   listeners stand (HTTP and HTTPS ports), each proven reachable before the run. It sends a synchronous `GET` whose
//!   302 goes there over HTTP and one whose 302 goes there over HTTPS (the gap), a `fetch` and an image there, a
//!   kept-alive `GET` redirected there after its page is gone, a navigation there and one redirected there by a 302.
//!   The listeners must accept no connection, the gap's two redirects must reach the window's refusing proxy (its
//!   `GET http://…` and `CONNECT host:port` denied in `fence.log`), and the rest must be denied in `fence.log`. Without a
//!   non-loopback address the listeners' check is skipped with a note (an error under `GPUI_SMOKE_REQUIRE=1`).
//! - `crash`: once the page is up the window has its app id in niri's window list; then the test kills the child's
//!   web processes, and the window must answer that its page's web process ended.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::net::{IpAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, thread};

use proto_lab::classify::{OwnKind, OwnRule};
use serde_json::Value;
use teams_auth::signin::{WindowAnswer, WindowRequest};
use teams_auth::token::NATIVE_CLIENT_REDIRECT;
use teams_fence::FenceLog;
use teams_signin::window::{self, WindowConfig};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

const MODE_ENV: &str = "TEAMS_SIGNIN_WINDOW_TEST";
const PORT_ENV: &str = "TEAMS_SIGNIN_TEST_PORT";
const LOG_ENV: &str = "TEAMS_SIGNIN_TEST_LOG";
const STATE: &str = "made-up-state-0123";
const CODE: &str = "made-up-code-4567";
/// From the child's start to its answer.
const RUN: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);

fn main() -> ExitCode {
    let outcome = match std::env::var(MODE_ENV).as_deref() {
        Ok(scenario @ ("channels" | "gaps" | "remote" | "crash")) => child(scenario),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("window test: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn redirect_url() -> String {
    format!("{NATIVE_CLIENT_REDIRECT}?code={CODE}&state={STATE}")
}

// --- the child: the window ---

fn child(scenario: &str) -> SandboxResult {
    let port: u16 = std::env::var(PORT_ENV)?.parse()?;
    let log = FenceLog::open(Path::new(&std::env::var(LOG_ENV)?))?;
    let extension = window::extension_beside(&std::env::current_exe()?).ok_or("no extension module beside the test")?;
    let origin = format!("http://127.0.0.1:{port}");
    let own: Vec<OwnRule> = ["/own-post", "/own-post-async"]
        .iter()
        .map(|path| OwnRule::new(OwnKind::EntraSignIn, "POST", &origin, path))
        .collect();
    let setup: window::GuardSetup = if scenario == "gaps" {
        Box::new(move |guard| own.into_iter().fold(guard, teams_signin::guard::Guard::with_own))
    } else {
        Box::new(|guard| guard)
    };
    let answer = window::run(WindowConfig {
        request: WindowRequest {
            authorize: format!("{origin}/authorize?scenario={scenario}&state={STATE}"),
            redirect_uri: NATIVE_CLIENT_REDIRECT.into(),
        },
        extension,
        log,
        setup,
    });
    match answer {
        WindowAnswer::Redirect(url) => say(&format!("answer redirect {}", url.expose())),
        WindowAnswer::Closed => say("answer closed"),
        WindowAnswer::Failed(reason) => say(&format!("answer failed {reason}")),
    }
    Ok(())
}

// --- the parent: the server, the sandbox and the checks ---

fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip("teams-signin window", &SandboxOptions::default())? else {
        return Ok(());
    };
    let none = Remote::none();
    let channels = run_scenario(&sandbox, "channels", &none, |_, _, _| Ok(()))
        .map_err(|error| format!("channels: {error}{}", sandbox.logs()))?;
    if std::env::var_os("TEAMS_SIGNIN_TEST_DUMP").is_some() {
        say(&format!("{:?}\n{}", channels.requests, channels.log_text));
    }
    check_channels(&channels)?;
    let gaps = run_scenario(&sandbox, "gaps", &none, |_, _, _| Ok(()))
        .map_err(|error| format!("gaps: {error}{}", sandbox.logs()))?;
    if std::env::var_os("TEAMS_SIGNIN_TEST_DUMP").is_some() {
        say(&format!("{:?}\n{}", gaps.requests, gaps.log_text));
    }
    check_gaps(&gaps)?;
    let remote = Remote::start()?;
    let run = run_scenario(&sandbox, "remote", &remote, |_, _, _| Ok(()))
        .map_err(|error| format!("remote: {error}{}", sandbox.logs()))?;
    if std::env::var_os("TEAMS_SIGNIN_TEST_DUMP").is_some() {
        say(&format!("{:?}\n{}", run.requests, run.log_text));
    }
    check_remote(&run, &remote)?;
    let crash = run_scenario(&sandbox, "crash", &remote, kill_web_processes)
        .map_err(|error| format!("crash: {error}{}", sandbox.logs()))?;
    check(
        crash
            .answer
            .starts_with(&format!("answer failed {}", window::WEB_PROCESS_ENDED)),
        "the window answers that its page's web process ended",
        &crash,
    )?;
    say("teams-signin window: ok");
    Ok(())
}

/// What one run left behind.
struct Run {
    answer: String,
    requests: Vec<(String, String)>,
    web_socket_connections: usize,
    log: Vec<Value>,
    log_text: String,
    website_data: Vec<PathBuf>,
}

impl Run {
    fn got(&self, method: &str, path: &str) -> bool {
        self.requests
            .iter()
            .any(|(got_method, got_path)| got_method == method && got_path.split('?').next() == Some(path))
    }

    fn logged(&self, path: &str) -> Vec<&Value> {
        self.log
            .iter()
            .filter(|entry| entry["url"].as_str().is_some_and(|url| url.ends_with(path)))
            .collect()
    }

    fn report(&self, key: &str) -> Option<String> {
        self.requests.iter().find_map(|(_, path)| {
            let query = path.strip_prefix("/report?")?;
            let (name, value) = query.split_once('=')?;
            (name == key).then(|| value.to_owned())
        })
    }
}

/// What the test does while the window runs, before its answer.
type During = fn(&Sandbox, &mut Running, &Server) -> SandboxResult;

fn run_scenario(sandbox: &Sandbox, scenario: &str, remote: &Remote, during: During) -> SandboxResult<Run> {
    let web_sockets = Counting::start(IpAddr::from([127, 0, 0, 1]))?;
    let server = Server::start(Hosts {
        web_socket_port: web_sockets.port,
        remote_http: remote.http(),
        remote_tls: remote.tls(),
    })?;
    remote.reset();
    let dir = tempfile::tempdir()?;
    let log_path = dir.path().join("fence.log");
    let home = dir.path().join("home");
    fs::create_dir(&home)?;
    let port = server.port.to_string();
    let home_text = home.to_string_lossy().into_owned();
    let data = home.join("data").to_string_lossy().into_owned();
    let cache = home.join("cache").to_string_lossy().into_owned();
    let config = home.join("config").to_string_lossy().into_owned();
    let child = sandbox.spawn(
        &std::env::current_exe()?,
        &[],
        &[
            (MODE_ENV, scenario),
            (PORT_ENV, &port),
            (LOG_ENV, &log_path.to_string_lossy()),
            ("TEAMS_SIGNIN_TEST_WS_PORT", &web_sockets.port.to_string()),
            ("HOME", &home_text),
            ("XDG_DATA_HOME", &data),
            ("XDG_CACHE_HOME", &cache),
            ("XDG_CONFIG_HOME", &config),
            ("GTK_A11Y", "none"),
        ],
    )?;
    let mut running = Running::new(child)?;
    if let Err(error) = during(sandbox, &mut running, &server) {
        return Err(running.fail(&error));
    }
    let answer = running
        .line_where(RUN, |line| line.starts_with("answer "))
        .map_err(|error| format!("no answer: {error:?}"))?;
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the child exited {status}: {stderr}").into());
    }
    let log_text = fs::read_to_string(&log_path).unwrap_or_default();
    let log = log_text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Value>, _>>()?;
    Ok(Run {
        answer,
        requests: server.requests(),
        web_socket_connections: web_sockets.connections(),
        log,
        log_text,
        website_data: website_data(&home)?,
    })
}

/// Every file or directory under `home` whose name says it is `WebKit`'s website data.
fn website_data(home: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut stack = vec![home.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if name.contains("webkit") || name.contains("cookies") || name.contains("localstorage") {
                found.push(path.clone());
            }
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    Ok(found)
}

fn check(condition: bool, what: &str, run: &Run) -> SandboxResult {
    if condition {
        Ok(())
    } else {
        Err(format!(
            "{what}\n  answer: {}\n  requests: {:?}\n  fence.log:\n{}",
            run.answer, run.requests, run.log_text
        )
        .into())
    }
}

fn check_channels(run: &Run) -> SandboxResult {
    check(
        run.answer == format!("answer redirect {}", redirect_url()),
        "the window hands the redirect",
        run,
    )?;
    check(run.got("GET", "/authorize"), "the authorize page loaded", run)?;
    check(run.report("done").is_some(), "the page ran to its end", run)?;
    check(run.got("GET", "/img.png"), "an image is a read and loads", run)?;
    check(run.got("GET", "/frame"), "a frame is a read and loads", run)?;
    check(
        !run.requests.iter().any(|(method, _)| method != "GET"),
        "no POST reached the server",
        run,
    )?;
    check(!run.got("GET", "/popup"), "no new window", run)?;
    check(run.web_socket_connections == 0, "no WebSocket connected", run)?;
    for (key, wanted, what) in [
        ("open", "null", "window.open gives none"),
        ("ws", "undefined", "WebSocket is off"),
        ("sw", "false", "service workers are off"),
        ("beacon", "undefined", "beacons are off"),
        ("fetch", "refused", "the page's fetch POST is refused"),
    ] {
        check(run.report(key).as_deref() == Some(wanted), what, run)?;
    }
    check_channels_log(run)
}

/// The channels run's `fence.log`: every refused request denied, every one the server got allowed first.
fn check_channels_log(run: &Run) -> SandboxResult {
    for denied in ["/fetch-post", "/frame-post"] {
        let lines = run.logged(denied);
        check(
            !lines.is_empty() && lines.iter().all(|line| line["verdict"] == "deny"),
            &format!("{denied} is denied in fence.log"),
            run,
        )?;
    }
    check(
        run.got("GET", "/worker.js"),
        "the worker's script is a read and loads",
        run,
    )?;
    let lines = run.logged("/worker-post");
    check(
        !lines.is_empty() && lines.iter().all(|line| line["verdict"] == "deny"),
        "the worker's POST is denied in fence.log",
        run,
    )?;
    for (method, path) in &run.requests {
        let path = path.split('?').next().unwrap_or_default();
        let logged = run.logged(path);
        check(
            logged
                .iter()
                .any(|line| line["method"] == method.as_str() && line["verdict"] == "allow"),
            &format!("{method} {path} reached the server with its allow in fence.log"),
            run,
        )?;
    }
    check(
        !run.log_text.contains(CODE) && !run.log_text.contains(STATE),
        "no code or state in fence.log",
        run,
    )?;
    check(
        run.log.iter().any(|line| line["reason"] == "sign_in_redirect"),
        "the caught redirect is logged",
        run,
    )?;
    check(
        run.website_data.is_empty(),
        &format!("website data kept: {:?}", run.website_data),
        run,
    )
}

/// The "remote" host: this machine's own non-loopback address, with a counting listener for HTTP and one for HTTPS;
/// or, on a machine without one, a documentation address nothing answers on (192.0.2.1, TEST-NET-1) and no listeners.
struct Remote {
    listeners: Option<(Counting, Counting)>,
}

impl Remote {
    /// Stand-ins for the scenarios that name no remote host.
    const fn none() -> Self {
        Self { listeners: None }
    }

    fn start() -> SandboxResult<Self> {
        let Some(address) = own_address() else {
            if ui_sandbox::required() {
                return Err("no non-loopback address to stand a remote host on, and GPUI_SMOKE_REQUIRE=1".into());
            }
            say("teams-signin window: no non-loopback address here: the remote listeners' check is skipped");
            return Ok(Self::none());
        };
        let listeners = (Counting::start(address)?, Counting::start(address)?);
        for listener in [&listeners.0, &listeners.1] {
            listener.prove_reachable()?;
        }
        Ok(Self {
            listeners: Some(listeners),
        })
    }

    fn http(&self) -> String {
        self.listeners
            .as_ref()
            .map_or_else(|| "192.0.2.1:8080".into(), |(http, _)| http.authority())
    }

    fn tls(&self) -> String {
        self.listeners
            .as_ref()
            .map_or_else(|| "192.0.2.1:8443".into(), |(_, tls)| tls.authority())
    }

    fn reset(&self) {
        if let Some((http, tls)) = &self.listeners {
            http.reset();
            tls.reset();
        }
    }

    /// The connections the listeners accepted, once the ones already made have been accepted; `None` without them.
    fn connections(&self) -> Option<usize> {
        let (http, tls) = self.listeners.as_ref()?;
        thread::sleep(Duration::from_millis(500));
        Some(http.connections() + tls.connections())
    }
}

/// This machine's address on its default route, if it isn't loopback: a UDP socket "connected" to a documentation
/// address learns its source address without sending anything.
fn own_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let address = socket.local_addr().ok()?.ip();
    (!address.is_loopback() && !address.is_unspecified()).then_some(address)
}

fn check_remote(run: &Run, remote: &Remote) -> SandboxResult {
    check(
        run.answer == format!("answer redirect {}", redirect_url()),
        "the window hands the redirect",
        run,
    )?;
    check(run.report("fired").is_some(), "the page sent everything", run)?;
    for first in ["/gap-http", "/gap-https", "/nav-redirect"] {
        check(run.got("GET", first), &format!("{first} (loopback) loaded"), run)?;
    }
    let (http, tls) = (remote.http(), remote.tls());
    let denied = |method: &str, url: &str| {
        run.log
            .iter()
            .any(|line| line["method"] == method && line["url"] == url && line["verdict"] == "deny")
    };
    check(
        denied("GET", &format!("http://{http}/gap-http-target")),
        "a synchronous GET's 302 off loopback (the gap) reaches the refusing proxy over HTTP, denied",
        run,
    )?;
    check(
        denied("CONNECT", &tls),
        "a synchronous GET's 302 off loopback (the gap) reaches the refusing proxy over HTTPS, denied",
        run,
    )?;
    for path in ["/fetch", "/img.png", "/nav", "/nav-target"] {
        check(
            denied("GET", &format!("http://{http}{path}")),
            &format!("{path} off loopback is denied in fence.log"),
            run,
        )?;
    }
    check(
        !run.log
            .iter()
            .any(|line| line["verdict"] == "allow" && line["url"].as_str().is_some_and(|url| url.contains(&http))),
        "nothing off loopback is allowed",
        run,
    )?;
    let keepalive = format!("http://{http}/keepalive-target");
    say(&format!(
        "teams-signin window: a kept-alive GET redirected off loopback after its page is gone: {}",
        if denied("GET", &keepalive) {
            "refused by the proxy"
        } else {
            "not followed"
        }
    ));
    match remote.connections() {
        Some(connections) => check(
            connections == 0,
            &format!("no connection reached the remote host's listeners ({connections} did)"),
            run,
        ),
        None => Ok(()),
    }
}

/// The `crash` scenario's step: once the page is up, its window is in niri's list under the window's app id, and
/// every `WebKit` web process under the child is killed.
fn kill_web_processes(sandbox: &Sandbox, running: &mut Running, server: &Server) -> SandboxResult {
    if wait_until(RUN, || {
        server
            .requests()
            .iter()
            .any(|(_, path)| path == "/report?ready=1")
            .then_some(())
    })
    .is_none()
    {
        return Err("the page never came up".into());
    }
    let windows = sandbox.niri_msg(&["windows"])?;
    if !windows.contains(window::APP_ID) {
        return Err(format!("no window with the app id {}: {windows}", window::APP_ID).into());
    }
    let web_processes: Vec<u32> = descendants(running.pid())
        .into_iter()
        .filter(|(_, name)| name.starts_with("WebKitWebProces"))
        .map(|(pid, _)| pid)
        .collect();
    if web_processes.is_empty() {
        return Err("no web process under the child".into());
    }
    for pid in web_processes {
        let status = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()?;
        if !status.success() {
            return Err(format!("web process {pid} couldn't be killed").into());
        }
    }
    Ok(())
}

/// Every process under `root`, with its name (`/proc`).
fn descendants(root: u32) -> Vec<(u32, String)> {
    let processes: Vec<(u32, u32, String)> = fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let stat = fs::read_to_string(entry.path().join("stat")).ok()?;
            let (head, rest) = stat.rsplit_once(") ")?;
            let name = head.split_once(" (")?.1.to_owned();
            let parent: u32 = rest.split_whitespace().nth(1)?.parse().ok()?;
            Some((pid, parent, name))
        })
        .collect();
    let mut found = Vec::new();
    let mut parents = vec![root];
    while let Some(parent) = parents.pop() {
        for (pid, _, name) in processes.iter().filter(|(_, of, _)| *of == parent) {
            found.push((*pid, name.clone()));
            parents.push(*pid);
        }
    }
    found
}

fn check_gaps(run: &Run) -> SandboxResult {
    check(
        run.answer == format!("answer redirect {}", redirect_url()),
        "the window hands the redirect",
        run,
    )?;
    // the first hops are decided
    check(
        !run.logged("/sync-redirect").is_empty(),
        "the synchronous GET is decided",
        run,
    )?;
    check(
        run.logged("/own-post")
            .iter()
            .any(|line| line["own"] == "entra_sign_in"),
        "the synchronous POST is allowed as own-state",
        run,
    )?;
    // the pinned gaps: their redirect targets are reached with no decision
    check(
        run.got("GET", "/sync-target") && run.logged("/sync-target").is_empty(),
        "GAP PINNED (a change would close it): a synchronous GET's 302 is followed with no decision",
        run,
    )?;
    check(
        run.got("POST", "/escaped-post") && run.logged("/escaped-post").is_empty(),
        "GAP PINNED (a change would close it): an allowed synchronous POST's 307 reaches an unclassified endpoint",
        run,
    )?;
    // asynchronously, the redirect comes back to the page and is decided
    check(run.got("POST", "/own-post-async"), "the asynchronous POST went", run)?;
    check(
        !run.got("POST", "/async-escape") && run.logged("/async-escape").iter().any(|line| line["verdict"] == "deny"),
        "an asynchronous POST's 307 is decided and denied",
        run,
    )?;
    let keepalive = run.got("GET", "/keepalive-target");
    say(&format!(
        "teams-signin window: a kept-alive GET redirected after its page is gone: {}",
        match (keepalive, run.logged("/keepalive-target").is_empty()) {
            (true, true) => "followed with no decision (the gap)",
            (true, false) => "decided",
            (false, _) => "not followed",
        }
    ));
    Ok(())
}

// --- the local server ---

/// The other hosts a page names: the `WebSocket` listener's port and the remote host's two authorities.
struct Hosts {
    web_socket_port: u16,
    remote_http: String,
    remote_tls: String,
}

/// A tiny HTTP/1.1 server: one request per connection, every one recorded.
struct Server {
    port: u16,
    requests: Arc<Mutex<Vec<(String, String)>>>,
}

impl Server {
    fn start(hosts: Hosts) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let hosts = Arc::new(hosts);
        thread::spawn(move || {
            for stream in listener.incoming().map_while(Result::ok) {
                let (recorded, hosts) = (Arc::clone(&recorded), Arc::clone(&hosts));
                thread::spawn(move || {
                    let _ = handle(stream, port, &recorded, &hosts);
                });
            }
        });
        Ok(Self { port, requests })
    }

    fn requests(&self) -> Vec<(String, String)> {
        self.requests.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

fn handle(stream: TcpStream, port: u16, recorded: &Mutex<Vec<(String, String)>>, hosts: &Hosts) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Ok(());
    };
    let (method, path) = (method.to_owned(), path.to_owned());
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim_end().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    recorded
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((method.clone(), path.clone()));
    respond(stream, &route(&path, port, hosts))
}

/// A response: status line, extra headers, content type and body.
struct Response {
    status: &'static str,
    headers: Vec<(&'static str, String)>,
    content_type: &'static str,
    body: Vec<u8>,
}

fn page(body: &str) -> Response {
    Response {
        status: "200 OK",
        headers: Vec::new(),
        content_type: "text/html; charset=utf-8",
        body: format!("<!doctype html><html><head><meta charset=\"utf-8\"></head><body>{body}</body></html>")
            .into_bytes(),
    }
}

fn redirect(status: &'static str, location: &str) -> Response {
    Response {
        status,
        headers: vec![("Location", location.to_owned())],
        content_type: "text/plain",
        body: b"moved".to_vec(),
    }
}

fn text(body: &str) -> Response {
    Response {
        status: "200 OK",
        headers: Vec::new(),
        content_type: "text/plain",
        body: body.as_bytes().to_vec(),
    }
}

/// A 1x1 GIF.
const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\
    \x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

fn route(path: &str, port: u16, hosts: &Hosts) -> Response {
    let redirect_to = redirect_url();
    let (http, tls) = (&hosts.remote_http, &hosts.remote_tls);
    match path.split('?').next().unwrap_or_default() {
        "/authorize" if path.contains("scenario=channels") => page(&channels_page(hosts.web_socket_port, &redirect_to)),
        "/authorize" if path.contains("scenario=remote") => page(&remote_page(http)),
        "/authorize" if path.contains("scenario=crash") => {
            page("<script>fetch('/report?ready=1').catch(() => {});</script>")
        }
        "/authorize" => page(&gaps_page()),
        "/gap-http" => redirect("302 Found", &format!("http://{http}/gap-http-target")),
        "/gap-https" => redirect("302 Found", &format!("https://{tls}/gap-https-target")),
        "/nav-redirect" => redirect("302 Found", &format!("http://{http}/nav-target")),
        "/keepalive-remote" => {
            thread::sleep(Duration::from_millis(1500));
            redirect("302 Found", &format!("http://{http}/keepalive-target"))
        }
        "/img.png" => Response {
            status: "200 OK",
            headers: Vec::new(),
            content_type: "image/gif",
            body: GIF.to_vec(),
        },
        "/frame" => page(
            "<form id=f method=post action=/frame-post><input name=a value=b></form>\
             <script>document.getElementById('f').submit();</script>",
        ),
        "/worker.js" => Response {
            status: "200 OK",
            headers: Vec::new(),
            content_type: "text/javascript",
            body: b"fetch('/worker-post', {method: 'POST', body: 'x'})\
                .catch(() => {}).finally(() => postMessage('done'));"
                .to_vec(),
        },
        "/sync-redirect" => redirect("302 Found", "/sync-target"),
        "/own-post" => redirect("307 Temporary Redirect", "/escaped-post"),
        "/own-post-async" => redirect("307 Temporary Redirect", "/async-escape"),
        "/keepalive-redirect" => {
            thread::sleep(Duration::from_millis(1500));
            redirect("302 Found", &format!("http://127.0.0.1:{port}/keepalive-target"))
        }
        "/next" => page(&format!(
            "<script>setTimeout(() => {{ location.href = '{redirect_to}'; }}, 3000);</script>"
        )),
        "/report" | "/sync-target" | "/escaped-post" | "/async-escape" | "/keepalive-target" => text("ok"),
        _ => Response {
            status: "404 Not Found",
            headers: Vec::new(),
            content_type: "text/plain",
            body: b"no".to_vec(),
        },
    }
}

fn channels_page(ws_port: u16, redirect_to: &str) -> String {
    format!(
        "<img src=/img.png><iframe src=/frame></iframe><script>
const report = (k, v) => fetch('/report?' + k + '=' + encodeURIComponent(String(v))).catch(() => {{}});
(async () => {{
  await report('sw', 'serviceWorker' in navigator);
  await report('ws', typeof WebSocket);
  await report('beacon', typeof navigator.sendBeacon);
  try {{ await fetch('/fetch-post', {{method: 'POST', body: 'x'}}); await report('fetch', 'sent'); }}
  catch (e) {{ await report('fetch', 'refused'); }}
  try {{ if (navigator.sendBeacon) navigator.sendBeacon('/beacon', 'x'); }} catch (e) {{}}
  try {{ new WebSocket('ws://127.0.0.1:{ws_port}/'); await report('wsopen', 'made'); }}
  catch (e) {{ await report('wsopen', 'threw'); }}
  try {{
    const worker = new Worker('/worker.js');
    await new Promise((done) => {{ worker.onmessage = done; worker.onerror = done; setTimeout(done, 3000); }});
  }} catch (e) {{}}
  const popup = window.open('/popup');
  await report('open', popup === null ? 'null' : 'window');
  await new Promise((done) => setTimeout(done, 1500));
  await report('done', 1);
  location.href = '{redirect_to}';
}})();
</script>"
    )
}

/// The `remote` scenario's page: every way it has to reach `http` (the remote host's HTTP authority) or, through
/// `/gap-https`, its HTTPS one; then it ends on `/next`, which navigates to the redirect.
fn remote_page(http: &str) -> String {
    format!(
        "<script>
const report = (k, v) => fetch('/report?' + k + '=' + encodeURIComponent(String(v))).catch(() => {{}});
const sync = (url) => {{ const x = new XMLHttpRequest(); x.open('GET', url, false); try {{ x.send(); }} catch (e) {{}} }};
(async () => {{
  sync('/gap-http');
  sync('/gap-https');
  try {{ await fetch('http://{http}/fetch'); }} catch (e) {{}}
  await new Promise((done) => {{
    const img = new Image(); img.onload = done; img.onerror = done; img.src = 'http://{http}/img.png';
  }});
  fetch('/keepalive-remote', {{keepalive: true}}).catch(() => {{}});
  await report('fired', 1);
  location.href = 'http://{http}/nav';
  setTimeout(() => {{ location.href = '/nav-redirect'; }}, 400);
  setTimeout(() => {{ location.href = '/next'; }}, 900);
}})();
</script>"
    )
}

fn gaps_page() -> String {
    "<script>
(async () => {
  const sync = new XMLHttpRequest();
  sync.open('GET', '/sync-redirect', false);
  try { sync.send(); } catch (e) {}
  const own = new XMLHttpRequest();
  own.open('POST', '/own-post', false);
  try { own.send('x'); } catch (e) {}
  try { await fetch('/own-post-async', {method: 'POST', body: 'x'}); } catch (e) {}
  fetch('/keepalive-redirect', {keepalive: true}).catch(() => {});
  setTimeout(() => { location.href = '/next'; }, 200);
})();
</script>"
        .into()
}

fn respond(mut stream: TcpStream, response: &Response) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
    for (name, value) in &response.headers {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

/// A TCP listener nothing may connect to (a `WebSocket`'s, the remote host's); it counts connections and serves none.
struct Counting {
    address: IpAddr,
    port: u16,
    connections: Arc<Mutex<usize>>,
}

impl Counting {
    fn start(address: IpAddr) -> io::Result<Self> {
        let listener = TcpListener::bind((address, 0))?;
        let port = listener.local_addr()?.port();
        let connections = Arc::new(Mutex::new(0));
        let counted = Arc::clone(&connections);
        thread::spawn(move || {
            for _ in listener.incoming() {
                *counted.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            }
        });
        Ok(Self {
            address,
            port,
            connections,
        })
    }

    fn authority(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }

    /// Connects once and waits for it to be counted, so a count of nothing later means something.
    fn prove_reachable(&self) -> SandboxResult {
        let _probe = TcpStream::connect((self.address, self.port))?;
        wait_until(WAIT, || (self.connections() > 0).then_some(()))
            .ok_or_else(|| format!("the listener on {} never counted a connection", self.authority()))?;
        self.reset();
        Ok(())
    }

    fn reset(&self) {
        *self.connections.lock().unwrap_or_else(PoisonError::into_inner) = 0;
    }

    fn connections(&self) -> usize {
        *self.connections.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
