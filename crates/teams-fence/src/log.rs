//! `fence.log`: one JSON line per decision.
//!
//! ```json
//! {"ts_ms":1760000000000,"verdict":"deny","kind":"message","scope":{"conversation":{"id":"19:…"}},"added":0,
//!  "reason":{"outsiders":{"count":1}},"message":"1 participant(s) outside the lab"}
//! ```
//!
//! An entry holds the write kind, the scope ids, how many participants the write adds and the reason: never a token,
//! and never the MRIs of the participants. The scope ids are logged as they are, though, and a 1:1 chat id
//! (`19:<oid>_<oid>@unq.gbl.spaces`) holds both members' directory object ids, so `fence.log` is not sanitized
//! material: it stays local (0600) and goes through the fixture guard and a manual review before any of it is copied
//! into the repo. Allows are logged too (without a reason), so the log is the full audit of a run.
//!
//! The browser fence logs the requests it decides without a membership check ([`RequestRecord`]): an own-state request
//! let through, or one that nothing classifies, denied. Such an entry holds the channel, the method and the URL without
//! its query, fragment or user info (where tokens travel), never a body:
//!
//! ```json
//! {"ts_ms":1760000000000,"verdict":"deny","channel":"http","method":"POST","url":"https://host/path",
//!  "reason":"unclassified","message":"no rule classifies this request"}
//! ```
//!
//! Every entry ends with a newline. After a failed append the log refuses every later entry (so every later allow is
//! denied), because a half-written record would run into the next one. When an existing file doesn't end with a
//! newline, [`FenceLog::open`] terminates that last line first: it stays as one line that isn't JSON (a record whose
//! write failed, reported as failed when it happened), and the entries after it parse on their own.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::decision::DenyReason;
use crate::target::{Scope, Target, WriteKind};

/// The outcome of one decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Allow,
    Deny(DenyReason),
}

#[derive(Serialize)]
struct Entry<'a> {
    ts_ms: u64,
    verdict: &'static str,
    kind: WriteKind,
    scope: &'a Scope,
    added: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<DenyReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

/// The channel a browser request went over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Http,
    WebSocket,
}

/// How the browser fence decided a request it didn't need a membership check for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestOutcome {
    /// An own-state request on the reviewed list (its kind's name), let through.
    Own(&'static str),
    /// Nothing classifies it: denied.
    Unclassified,
}

/// A browser request the fence decided without a membership check. The URL is logged without its query, fragment or
/// user info.
#[derive(Clone, Copy, Debug)]
pub struct RequestRecord<'a> {
    pub channel: Channel,
    pub method: &'a str,
    pub url: &'a str,
    pub outcome: RequestOutcome,
}

#[derive(Serialize)]
struct RequestEntry<'a> {
    ts_ms: u64,
    verdict: &'static str,
    channel: Channel,
    method: &'a str,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    own: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'static str>,
}

/// The append-only decision log.
pub struct FenceLog {
    sink: Mutex<Sink>,
}

struct Sink {
    writer: Box<dyn Write + Send>,
    /// An append failed, possibly halfway through a record; nothing more is written.
    broken: bool,
}

impl fmt::Debug for FenceLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FenceLog")
    }
}

impl FenceLog {
    /// Opens `path` for appending, creating it readable by the owner only (0600). When the file's last line has no
    /// newline (an append that failed halfway), the line is terminated first, so the next entry starts a line of its
    /// own.
    ///
    /// # Errors
    /// When the file can't be opened, created, read or terminated.
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        terminate_last_line(&mut file)?;
        Ok(Self::from_writer(file))
    }

    /// Logs into any writer.
    #[must_use]
    pub fn from_writer(writer: impl Write + Send + 'static) -> Self {
        Self {
            sink: Mutex::new(Sink {
                writer: Box::new(writer),
                broken: false,
            }),
        }
    }

    /// Appends one decision as a single line and flushes it. Once an append has failed, every later one fails too.
    pub(crate) fn record(&self, target: &Target, verdict: Verdict) -> io::Result<()> {
        let reason = match verdict {
            Verdict::Allow => None,
            Verdict::Deny(reason) => Some(reason),
        };
        let entry = Entry {
            ts_ms: now_ms(),
            verdict: if reason.is_some() { "deny" } else { "allow" },
            kind: target.kind(),
            scope: target.scope(),
            added: target.added().len(),
            reason,
            message: reason.map(|reason| reason.to_string()),
        };
        self.append(&entry)
    }

    /// Appends one browser request decided without a membership check, as a single line, and flushes it. Once an
    /// append has failed, every later one fails too.
    ///
    /// # Errors
    /// When the entry can't be written, or an earlier append failed.
    pub fn record_request(&self, request: &RequestRecord<'_>) -> io::Result<()> {
        let (verdict, own, reason, message) = match request.outcome {
            RequestOutcome::Own(kind) => ("allow", Some(kind), None, None),
            RequestOutcome::Unclassified => (
                "deny",
                None,
                Some("unclassified"),
                Some("no rule classifies this request"),
            ),
        };
        self.append(&RequestEntry {
            ts_ms: now_ms(),
            verdict,
            channel: request.channel,
            method: request.method,
            url: redact_url(request.url),
            own,
            reason,
            message,
        })
    }

    fn append(&self, entry: &impl Serialize) -> io::Result<()> {
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        if sink.broken {
            return Err(io::Error::other("an earlier fence.log append failed"));
        }
        let appended = sink.writer.write_all(&line).and_then(|()| sink.writer.flush());
        if appended.is_err() {
            sink.broken = true;
        }
        appended
    }
}

/// The URL without its query, fragment and user info: what's left names the endpoint, not the credentials sent to it.
fn redact_url(url: &str) -> String {
    let end = url.find(['?', '#']).unwrap_or(url.len());
    let url = &url[..end];
    let Some(scheme_end) = url.find("://").map(|index| index + 3) else {
        return url.to_owned();
    };
    let rest = &url[scheme_end..];
    let authority = &rest[..rest.find('/').unwrap_or(rest.len())];
    match authority.rfind('@') {
        Some(at) => format!("{}{}", &url[..scheme_end], &rest[at + 1..]),
        None => url.to_owned(),
    }
}

/// Ends the file's last line with a newline when it has none.
fn terminate_last_line(file: &mut File) -> io::Result<()> {
    let len = file.metadata()?.len();
    let Some(last) = len.checked_sub(1) else {
        return Ok(());
    };
    let mut byte = [0_u8];
    file.read_exact_at(&mut byte, last)?;
    if byte != *b"\n" {
        file.write_all(b"\n")?;
        file.flush()?;
    }
    Ok(())
}

fn now_ms() -> u64 {
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
}
