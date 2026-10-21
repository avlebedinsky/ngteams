//! The window's network bound while [`crate::cli::NOT_LIVE`] holds: the webview's network session reaches numeric
//! loopback addresses directly and everything else only through [`BlackHole`], a proxy on loopback that refuses every
//! connection it gets and logs a deny for it.
//!
//! This is what keeps the window off the tenant where the guard can't see: `WebKit`'s network process follows a
//! synchronous load's redirects and a kept-alive load's redirects after its page is gone without asking the guard
//! (`docs/backlog/webkit-sign-in-guard-redirects.md`), but it sends every HTTP(S) load through its soup session, whose
//! proxy resolver ([`BYPASS`] direct, the proxy for the rest, no direct fallback) the session's custom proxy settings
//! set. It isn't a proof that nothing else leaves: networking outside the soup session (the `WebKit` features
//! switched off in [`crate::window::FEATURES_OFF`], WebRTC) and DNS lookups are outside it.
//!
//! The black hole never forwards and never opens a tunnel: it reads at most one request line (`CONNECT host:port`
//! for HTTPS, `GET http://host/path` for HTTP) within [`READ_TIMEOUT`], logs it as denied (`fence.log` keeps no
//! query), answers `403` and closes. It serves until the process exits, so a load still running after the window has
//! ended is refused too.

use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::guard::Guard;

/// The hosts the network session reaches directly: numeric loopback only. A name (even `localhost`) goes to the
/// black hole: GIO's ignore-hosts entries for a name match its subdomains too, and a name is resolved by DNS.
pub const BYPASS: [&str; 2] = ["127.0.0.0/8", "::1"];
/// How long the black hole waits for a connection's request line.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest request line it reads.
const LINE_LIMIT: u64 = 8 * 1024;
const REFUSAL: &[u8] = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// The refusing proxy, on `127.0.0.1`.
#[derive(Debug)]
pub struct BlackHole {
    port: u16,
}

impl BlackHole {
    /// Binds a port on `127.0.0.1` and refuses every connection to it, each logged through `guard`, until the
    /// process exits.
    ///
    /// # Errors
    /// The port can't be bound.
    pub fn start(guard: Arc<Guard>) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        thread::Builder::new().name("signin-egress".into()).spawn(move || {
            for stream in listener.incoming().map_while(Result::ok) {
                let guard = Arc::clone(&guard);
                let _ = thread::Builder::new()
                    .name("signin-egress-conn".into())
                    .spawn(move || refuse(&guard, &stream));
            }
        })?;
        Ok(Self { port })
    }

    /// The proxy's URI, for the network session's proxy settings.
    #[must_use]
    pub fn uri(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// Logs what the connection asked for and refuses it; a line that can't be read is logged as `?`.
fn refuse(guard: &Guard, stream: &TcpStream) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let mut line = Vec::new();
    let _ = BufReader::new(stream).take(LINE_LIMIT).read_until(b'\n', &mut line);
    let line = String::from_utf8_lossy(&line);
    let (method, target) = request_line(&line);
    guard.refuse(method, target);
    let mut stream = stream;
    let _ = stream.write_all(REFUSAL);
    let _ = stream.flush();
}

/// The method and target of a proxy request line, `?` for what isn't there.
fn request_line(line: &str) -> (&str, &str) {
    let mut fields = line.split_whitespace();
    (fields.next().unwrap_or("?"), fields.next().unwrap_or("?"))
}
