//! Runs only in the authorized VM console, and reexecutes itself as the normal private-pipe worker.
use std::io::{BufRead as _, BufReader, Read as _};
use std::net::{Ipv4Addr, TcpListener};
use std::process::ExitCode;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use teams_auth::signin::{SignInAnswer, SignInRequest};
use teams_auth::token::NATIVE_CLIENT_REDIRECT;
use teams_signin::{
    classify::{OwnKind, OwnRule},
    window::{self, Bound, EngineSource, WindowConfig, Windows},
};

pub fn main() -> ExitCode {
    if std::env::args_os().nth(1).is_some_and(|a| a == "--guarded-signin") {
        window::worker();
        return ExitCode::SUCCESS;
    }
    if std::env::var_os("NGTEAMS_NATIVE_SIGNIN_REQUIRE").is_none() {
        println!("native sign-in GUI requires the authorized win11 console; set NGTEAMS_NATIVE_SIGNIN_REQUIRE=1 there");
        return ExitCode::SUCCESS;
    }
    match run() {
        Ok(()) => {
            println!("native sign-in guard, channels, redirects and cleanup passed");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("native sign-in contract failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    teams_platform::windows_sandbox::window_allowed_here()?;
    let log = std::path::PathBuf::from(std::env::var_os("UI_SANDBOX_DIR").ok_or("sandbox directory missing")?)
        .join("guard-log");
    teams_platform::private::create_dir_all(&log)?;
    let logger = teams_log::install(&teams_log::Config::from_env(log.clone(), teams_log::Level::Info))?;
    let server = Server::start()?;
    let origin = format!("https://127.0.0.1:{}", server.port);
    let own = OwnRule::new(OwnKind::EntraSignIn, "POST", &origin, "/own");
    let answer = Windows::new("com.nickalie.ngteams.signin.synthetic").ask(WindowConfig {
        engine: EngineSource,
        bound: Bound::Hosts(Arc::new(teams_signin::hosts::SignInHosts::for_tests(
            &["127.0.0.1"],
            server.port,
        ))),
        request: SignInRequest {
            authorize: format!("{origin}/authorize"),
            redirect_uri: NATIVE_CLIENT_REDIRECT.into(),
        },
        extension: std::env::current_exe()?,
        setup: Box::new(move |guard| guard.with_own(own)),
    });
    let snapshot = server
        .requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    std::fs::write(log.join("synthetic-requests.json"), serde_json::to_vec(&snapshot)?)?;
    let _ = logger.flush_written();
    let redirect = match answer {
        SignInAnswer::Redirect(redirect) => redirect,
        SignInAnswer::Failed(reason) => return Err(format!("no synthetic redirect: {reason}").into()),
        _ => return Err("no synthetic redirect returned".into()),
    };
    let profile_prefix = format!("ngteams-signin-{}-", std::process::id());
    if std::fs::read_dir(std::env::temp_dir())?
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with(&profile_prefix))
    {
        return Err("native sign-in left a browser profile behind".into());
    }
    if !redirect.expose().contains("code=synthetic-code") {
        return Err("incorrect synthetic redirect".into());
    }
    if !logger.flush_written() {
        return Err("diagnostic log flush failed".into());
    }
    verify(&server, &log)
}

fn verify(server: &Server, log: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let requests = server
        .requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let saw = |method: &str, path: &str| requests.iter().any(|(m, p)| m == method && p.starts_with(path));
    for (method, path) in [
        ("GET", "/authorize"),
        ("GET", "/image"),
        ("GET", "/empty-phrase"),
        ("GET", "/empty-phrase-report?ok=1"),
        ("GET", "/geometry?full=1"),
        ("GET", "/frame"),
        ("GET", "/frame-report?locked=1"),
        ("GET", "/report?locked=1"),
        ("POST", "/own"),
        ("GET", "/sync"),
    ] {
        if !saw(method, path) {
            return Err(format!("missing synthetic request: {method} {path}; negative main probe: {}; negative frame probe: {}; main reports: {}; frame reports: {}",
                saw("GET", "/report?locked=0"), saw("GET", "/frame-report?locked=0"),
                requests.iter().filter(|(_,p)| p.starts_with("/report")).count(),
                requests.iter().filter(|(_,p)| p.starts_with("/frame-report")).count()).into());
        }
    }
    if requests.iter().any(|(m, p)| m == "POST" && p != "/own") {
        return Err("unknown POST reached synthetic server".into());
    }
    let text = std::fs::read_to_string(log.join(teams_log::FILE_NAME))?;
    if !text.contains("/post-denied") || !text.contains("/sync-next") || text.contains("synthetic-code") {
        return Err("redirect decision logging/redaction contract failed".into());
    }
    let tunnels = text
        .lines()
        .filter(|line| line.contains("CONNECT") && line.contains("\"tunnel\":true"))
        .count();
    if tunnels != server.accepted.load(Ordering::Relaxed) || tunnels == 0 {
        return Err("a synthetic TLS connection bypassed the egress proxy".into());
    }
    for (method, path) in requests {
        let base = path.split('?').next().unwrap_or_default();
        if !text.lines().any(|line| line.contains(base) && line.contains(&method)) {
            return Err(format!("request has no guard decision: {method} {base}").into());
        }
    }
    Ok(())
}

struct Server {
    port: u16,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    stopped: Arc<AtomicBool>,
    accepted: Arc<std::sync::atomic::AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let key = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).map_err(std::io::Error::other)?;
        let tls = tokio_rustls::rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![key.cert.der().clone()],
                tokio_rustls::rustls::pki_types::PrivatePkcs8KeyDer::from(key.signing_key.serialize_der()).into(),
            )
            .map_err(std::io::Error::other)?;
        let tls = Arc::new(tls);
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let accepts = Arc::clone(&accepted);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let (records, stop) = (Arc::clone(&requests), Arc::clone(&stopped));
        let thread = std::thread::spawn(move || accept(&listener, &tls, &records, &stop, &accepts));
        Ok(Self {
            port,
            requests,
            stopped,
            accepted,
            thread: Some(thread),
        })
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn accept(
    listener: &TcpListener,
    tls: &Arc<tokio_rustls::rustls::ServerConfig>,
    records: &Arc<Mutex<Vec<(String, String)>>>,
    stop: &AtomicBool,
    accepts: &std::sync::atomic::AtomicUsize,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                accepts.fetch_add(1, Ordering::Relaxed);
                let records = Arc::clone(records);
                let tls = Arc::clone(tls);
                std::thread::spawn(move || serve_tls(stream, &records, tls));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn serve_tls(
    stream: std::net::TcpStream,
    records: &Mutex<Vec<(String, String)>>,
    tls: Arc<tokio_rustls::rustls::ServerConfig>,
) {
    // Winsock accepts inherit the listener's nonblocking mode. TLS needs blocking I/O here, including when
    // the first ClientHello arrives after accept returned; Unix accept does not inherit that mode.
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    if let Ok(connection) = tokio_rustls::rustls::ServerConnection::new(tls) {
        serve(tokio_rustls::rustls::StreamOwned::new(connection, stream), records);
    }
}

fn serve(mut stream: impl std::io::Read + std::io::Write, requests: &Mutex<Vec<(String, String)>>) {
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => return,
        Ok(_) => {}
    }
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() < 2 {
        return;
    }
    let (method, path) = (fields[0].to_owned(), fields[1].to_owned());
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let _ = reader.take(length).read_to_end(&mut Vec::new());
    requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((method, path.clone()));
    if path.starts_with("/report?locked=1")
        && let Some(root) = std::env::var_os("UI_SANDBOX_DIR")
    {
        let _ = std::fs::write(
            std::path::PathBuf::from(root).join("render-ready"),
            b"synthetic page ready",
        );
    }
    let (status, headers, body) = match path.split('?').next().unwrap_or_default() {
        "/authorize" => ("200 OK", "Content-Type: text/html\r\n", PAGE),
        "/empty-phrase" => ("200 ", "Content-Type: text/plain\r\n", "synthetic"),
        "/frame" => ("200 OK", "Content-Type: text/html\r\n", FRAME),
        "/own" => ("307 Temporary Redirect", "Location: /post-denied\r\n", ""),
        "/sync" => ("302 Found", "Location: /sync-next\r\n", ""),
        _ => ("200 OK", "Content-Type: text/plain\r\n", "synthetic"),
    };
    let body = if std::env::var_os("NGTEAMS_SIGNIN_TEST_HOLD").is_some() {
        body.replace(",3000", ",15000")
    } else {
        body.to_owned()
    };
    let response = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

const FRAME: &str = r"<!doctype html><script>
let locked = ['WebSocket','Worker','SharedWorker','RTCPeerConnection','WebTransport'].every(k=>window[k]===undefined);
new Image().src='/frame-report?locked='+(locked?1:0);
</script>";
const PAGE: &str = r"<!doctype html><html><body><h1>Synthetic sign-in</h1><p>Offline guard contract</p>
<img src='/image'><iframe src='/frame'></iframe><script>
(async()=>{
 const emptyPhrase=await fetch('/empty-phrase').then(r=>r.text()).catch(()=> '');
 new Image().src='/empty-phrase-report?ok='+(emptyPhrase==='synthetic'?1:0);
 const names=['WebSocket','Worker','SharedWorker','RTCPeerConnection','WebTransport'];
 let locked=names.every(k=>window[k]===undefined);
 const blank=document.createElement('iframe'); document.body.append(blank);
 locked=locked&&names.every(k=>blank.contentWindow[k]===undefined);
 blank.remove();
 try { Object.defineProperty(window,'WebSocket',{value:function(){}}); locked=false; } catch(_){}
 locked=locked&&typeof navigator.sendBeacon==='undefined'&&(!navigator.serviceWorker||typeof navigator.serviceWorker.register==='undefined');
 new Image().src='/report?locked='+(locked?1:0);
 new Image().src='/geometry?full='+((innerWidth>=400&&innerHeight>=600)?1:0);
 try { await fetch('/post-denied',{method:'POST',body:'synthetic'}); } catch(_){}
 try { await fetch('/own',{method:'POST',body:'synthetic'}); } catch(_){}
 const xhr=new XMLHttpRequest(); xhr.open('GET','/sync',false); try{xhr.send();}catch(_){}
 new Image().src='http://192.0.2.1/no-egress';
 window.open('/popup');
 setTimeout(()=>location.href='https://login.microsoftonline.com/common/oauth2/nativeclient?code=synthetic-code&state=synthetic-state',3000);
})()
</script></body></html>";
