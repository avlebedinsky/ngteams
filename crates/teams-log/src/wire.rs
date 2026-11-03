//! Typed records of what goes over the wire, for the transport and the socket to hand the log at `debug`: a request,
//! its answer, a Trouter frame. They hold everything as it was sent or read (the token included: redaction is the
//! logger's, never the call site's); their `Display` is what the log writes, redacted ([`crate::redact`]), and every
//! line then passes the logger's last pass too.
//!
//! ```text
//! log::debug!("{}", teams_log::wire::Request { method: "GET", url, headers: &headers, body: None });
//! ```

use std::fmt;
use std::time::Duration;

use crate::redact;

mod known;

pub use known::WithSecrets;

/// The target the transport logs its [`Request`]s and [`Response`]s under, included in the app's debug log.
pub const HTTP_TARGET: &str = "teams_wire::http";

/// The target the push channel logs its [`Frame`]s under (as [`HTTP_TARGET`]).
pub const FRAME_TARGET: &str = "teams_wire::trouter";

/// The value of a header named `name` (in any case) among `headers`.
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// `{name: value, …}` with every credential's value redacted.
struct Headers<'a>(&'a [(String, String)]);

impl fmt::Display for Headers<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;
        for (at, (name, value)) in self.0.iter().enumerate() {
            if at > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{name}: {}", redact::header_value(name, value))?;
        }
        f.write_str("}")
    }
}

/// An HTTP request as it was sent: its method, its whole URL (query included), every header (the credential's too)
/// and its body, if it has one.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [(String, String)],
    pub body: Option<&'a [u8]>,
}

impl fmt::Display for Request<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "request {} {} headers {}",
            self.method,
            redact::url(self.url),
            Headers(self.headers)
        )?;
        match self.body {
            Some(body) => write!(f, " body {}", redact::body(header(self.headers, "content-type"), body)),
            None => Ok(()),
        }
    }
}

/// An HTTP answer as it was read: the request it answers (method and URL), its status, every header, its body, and how
/// long the exchange took.
#[derive(Clone, Copy, Debug)]
pub struct Response<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub status: u16,
    pub headers: &'a [(String, String)],
    pub body: &'a [u8],
    pub elapsed: Duration,
}

impl fmt::Display for Response<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "response {} {} -> {} in {} ms headers {} body {}",
            self.method,
            redact::url(self.url),
            self.status,
            self.elapsed.as_millis(),
            Headers(self.headers),
            redact::body(header(self.headers, "content-type"), self.body)
        )
    }
}

/// Which way a frame went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Sent,
    Received,
}

/// A Trouter (socket.io 0.9) text frame as it was sent or read, `user.authenticate` included.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    pub direction: Direction,
    pub text: &'a str,
}

impl fmt::Display for Frame<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let way = match self.direction {
            Direction::Sent => "frame sent",
            Direction::Received => "frame received",
        };
        write!(f, "{way} {}", redact::frame(self.text))
    }
}
