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
//! A WebSocket frame the audit saw leave the browser without an allow is logged the same way, with the verdict
//! `breach` and the reason `sent_without_allow`.
//!
//! The sign-in window (`teams-signin`) logs every request it decides the same way, its reads too (`"read": true`), and
//! the redirect that carries the authorization code as a deny with the reason `sign_in_redirect` (never its query).
//!
//! The native client's own Trouter frames ([`FrameRecord`]) are logged before each is sent, by the one socket sink
//! that can send them (`teams_proto::FrameSink`): the channel, the socket's URL without its query, fragment or user
//! info, and the frame's kind from the reviewed list, never its text (the authentication frame carries the account's
//! token):
//!
//! ```json
//! {"ts_ms":1760000000000,"verdict":"allow","event":"frame","channel":"web_socket","url":"wss://host/v4/c",
//!  "own":"trouter_ping"}
//! ```
//!
//! The fence's decision on a native read by `POST` (`Fence::check_post_read`) is logged as it is made: the entry's
//! kind, how many MRIs it names (never the MRIs), the verdict and, for a deny, the reason. The native transport then
//! logs the request itself as a [`RequestRecord`] with [`RequestOutcome::PostRead`] right before it sends it:
//!
//! ```json
//! {"ts_ms":1760000000000,"verdict":"allow","event":"post_read_decision","post_read":"short_profile","names":2}
//! ```
//!
//! A mutating request the fence lets through because it reaches nobody (`contacting` called it
//! [`Contact::None`](crate::Contact::None)) is logged as `allowed` ([`AllowedRecord`]), by the browser fence and the
//! native transport alike: its method, its host and its path's shape, every id in the path replaced by its kind
//! ([`path_shape`]: `<chat>`, `<message>`, `<mri>`, `<guid>`, `<id>`), never a query, a header or the body. That is
//! the record that shows a family of requests reaching people is missing from the classifier:
//!
//! ```json
//! {"ts_ms":1760000000000,"verdict":"allow","event":"allowed","method":"PUT","host":"teams.microsoft.com",
//!  "path":"/api/chatsvc/emea/v1/users/ME/conversations/<chat>/properties"}
//! ```
//!
//! So is a target the browser fence couldn't arm ([`ArmingRecord`]): its type, the step that failed and how, whether
//! its resume had been sent, whether it attached held (`waitingForDebugger`), what closing it gave and how it was
//! contained, with the verdict `breach` when it may have run unguarded and `contained` when it can't have. No target
//! or session id, no URL:
//!
//! ```json
//! {"ts_ms":1760000000000,"verdict":"contained","event":"arming_failure","target_type":"worker",
//!  "step":"Network.enable","cause":"no_answer","resume_sent":false,"held":true,"close":"refused",
//!  "containment":"browser_killed","message":"a target not armed in time was contained before it ran"}
//! ```
//!
//! Every entry ends with a newline. After a failed append the log refuses every later entry (so every later allow is
//! denied), because a half-written record would run into the next one. When an existing file doesn't end with a
//! newline, [`FenceLog::open`] terminates that last line first: it stays as one line that isn't JSON (a record whose
//! write failed, reported as failed when it happened), and the entries after it parse on their own.
//!
//! Several processes append to the lab's one `fence.log` at once (each lab account runs a binary of its own). Each
//! append is made under an exclusive advisory lock on the file (`flock`), as is the check for a half-written last line
//! when the log is opened; each append checks for one too, since another process's append may have failed halfway
//! after this log was opened. Within a process, appends are serialized by the log's own lock as well.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use percent_encoding::percent_decode_str;
use serde::Serialize;
use url::Url;

use crate::decision::DenyReason;
use crate::post_read::PostReadDenyReason;
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

#[derive(Serialize)]
struct PostReadDecisionEntry {
    ts_ms: u64,
    verdict: &'static str,
    event: &'static str,
    post_read: &'static str,
    names: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<PostReadDenyReason>,
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
    /// A read sent as a `POST`, on the browser fence's reviewed list or the native client's sealed `PostRead` list (its
    /// kind's name), whose body names only lab accounts ([`crate::Fence::lab_accounts_only`]): let through. Not
    /// own-state and not a write.
    PostRead(&'static str),
    /// Nothing classifies it: denied.
    Unclassified,
    /// It left the browser without the fence's allow: seen only afterwards, by the audit of what was sent (a WebSocket
    /// frame that got past the guard). Logged as a breach; it fails the run.
    SentWithoutAllow,
    /// Denied on purpose, whatever it is: the run forced a deny on it to check that a denied request never leaves the
    /// browser (an enforcement test).
    Forced,
    /// A read (`GET`, `HEAD` or `OPTIONS`), let through. Only the sign-in window logs its reads (`teams-signin`: every
    /// decision is logged before the request goes); the browser fence lets reads through unlogged.
    Read,
    /// The sign-in's redirect to the registered redirect URI, which carries the authorization code: caught and handed
    /// to the parent process, never let through (`teams-signin`). Its query, where the code travels, isn't logged.
    SignInRedirect,
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
    post_read: Option<&'static str>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    read: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'static str>,
}

/// An own frame the native client is about to send on a WebSocket: the socket's URL (logged without its query,
/// fragment or user info) and the frame's kind on the reviewed list. Never the frame's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameRecord<'a> {
    pub url: &'a str,
    pub kind: &'static str,
}

#[derive(Serialize)]
struct FrameEntry {
    ts_ms: u64,
    verdict: &'static str,
    event: &'static str,
    channel: Channel,
    url: String,
    own: &'static str,
}

/// A mutating request let through because it reaches nobody: its method and URL. Only the URL's host and its path's
/// shape ([`path_shape`]) are logged: never its query, user info or fragment, nor any header or body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllowedRecord<'a> {
    pub method: &'a str,
    pub url: &'a str,
}

#[derive(Serialize)]
struct AllowedEntry<'a> {
    ts_ms: u64,
    verdict: &'static str,
    event: &'static str,
    method: &'a str,
    host: String,
    path: String,
}

/// How an arming step failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmingCause {
    /// The step got no answer in time.
    NoAnswer,
    /// The browser answered the step with an error (or with a message the fence couldn't read).
    Error,
    /// The WebSocket guard didn't report itself armed.
    GuardNotArmed,
    /// The attach event didn't name the target or its session.
    Malformed,
    /// The fence's connection to the browser was gone before the step was answered: the browser never answered it.
    ConnectionGone,
}

/// What `Target.closeTarget` gave for a target that couldn't be armed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseOutcome {
    /// The browser confirmed it.
    Closed,
    /// The browser answered with an error or `success: false` (Chrome 155 refuses to close a dedicated worker).
    Refused,
    /// No answer in time.
    Unanswered,
    /// The fence's connection to the browser was gone before an answer came: the browser never answered.
    ConnectionGone,
    /// There was no target id to close.
    NoTarget,
}

/// How a target that couldn't be armed was kept from running unguarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Containment {
    /// The browser confirmed closing it.
    Closed,
    /// Its close wasn't confirmed, so the whole browser was killed (its process group, while the fence's connection
    /// still held the target as far as the fence could see: its reader hadn't seen the connection end) and reaped.
    BrowserKilled,
    /// Neither: the browser was gone already, or couldn't be killed and reaped.
    Failed,
}

/// A target the browser fence couldn't arm, and how it was contained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArmingRecord<'a> {
    /// `TargetInfo.type`.
    pub target_type: &'a str,
    /// The CDP method of the step that failed; `None` when the attach event itself was malformed.
    pub step: Option<&'a str>,
    pub cause: ArmingCause,
    /// The step that failed was the resume (`Runtime.runIfWaitingForDebugger`): the target's code may have started.
    pub resume_sent: bool,
    /// It attached held, waiting for the debugger. One that didn't was running before its arming began.
    pub held: bool,
    pub close: CloseOutcome,
    pub containment: Containment,
}

impl ArmingRecord<'_> {
    /// Whether the target's code may have run: its resume was sent, or it wasn't held when it attached.
    #[must_use]
    pub fn may_have_run(&self) -> bool {
        self.resume_sent || !self.held
    }

    /// Whether anything of it may have run unguarded: it may have run, or it couldn't be contained. A held target that
    /// was never resumed and was closed, or killed with the browser, is no breach.
    #[must_use]
    pub fn breach(&self) -> bool {
        self.may_have_run() || self.containment == Containment::Failed
    }
}

#[derive(Serialize)]
struct ArmingEntry<'a> {
    ts_ms: u64,
    verdict: &'static str,
    event: &'static str,
    target_type: &'a str,
    step: Option<&'a str>,
    cause: ArmingCause,
    resume_sent: bool,
    held: bool,
    close: CloseOutcome,
    containment: Containment,
    message: &'static str,
}

/// The append-only decision log.
pub struct FenceLog {
    sink: Mutex<Sink>,
}

struct Sink {
    writer: Writer,
    /// An append failed, possibly halfway through a record; nothing more is written.
    broken: bool,
}

/// Where the entries go.
enum Writer {
    /// A log file other processes may append to too: every append under the file's advisory lock.
    File(File),
    /// Any other writer (tests only: [`FenceLog::from_writer`]).
    #[cfg(feature = "test-support")]
    Other(Box<dyn Write + Send>),
}

impl Writer {
    /// Writes `line` whole and flushes it; a file under its lock, after ending a half line another writer left.
    fn append(&mut self, line: &[u8]) -> io::Result<()> {
        match self {
            Self::File(file) => locked(file, |file| {
                terminate_last_line(file)?;
                file.write_all(line)?;
                file.flush()
            }),
            #[cfg(feature = "test-support")]
            Self::Other(writer) => {
                writer.write_all(line)?;
                writer.flush()
            }
        }
    }
}

/// Runs `step` holding the file's exclusive advisory lock (`flock`), waiting for any other holder; the lock is let go
/// afterwards whatever `step` gave.
fn locked(file: &mut File, step: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    file.lock()?;
    let done = step(file);
    let unlocked = file.unlock();
    done.and(unlocked)
}

impl fmt::Debug for FenceLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FenceLog")
    }
}

impl FenceLog {
    /// Opens `path` for appending, creating it readable by the owner only (0600). When the file's last line has no
    /// newline (an append that failed halfway), the line is terminated first, under the file's lock, so the next entry
    /// starts a line of its own.
    ///
    /// # Errors
    /// When the file can't be opened, created, locked, read or terminated.
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        locked(&mut file, terminate_last_line)?;
        Ok(Self::with_writer(Writer::File(file)))
    }

    /// Logs into any writer. Tests only (`test-support`): a lab binary's log is the lab's `fence.log`.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn from_writer(writer: impl Write + Send + 'static) -> Self {
        Self::with_writer(Writer::Other(Box::new(writer)))
    }

    fn with_writer(writer: Writer) -> Self {
        Self {
            sink: Mutex::new(Sink { writer, broken: false }),
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

    /// Appends the fence's decision on one read by `POST` (`kind`, naming `names` MRIs; `None` allows) as a single line,
    /// and flushes it. Once an append has failed, every later one fails too.
    pub(crate) fn record_post_read(
        &self,
        kind: &'static str,
        names: usize,
        verdict: Option<PostReadDenyReason>,
    ) -> io::Result<()> {
        self.append(&PostReadDecisionEntry {
            ts_ms: now_ms(),
            verdict: if verdict.is_some() { "deny" } else { "allow" },
            event: "post_read_decision",
            post_read: kind,
            names,
            reason: verdict,
            message: verdict.map(|reason| reason.to_string()),
        })
    }

    /// Appends one browser request decided without a membership check, as a single line, and flushes it. Once an
    /// append has failed, every later one fails too.
    ///
    /// # Errors
    /// When the entry can't be written, or an earlier append failed.
    pub fn record_request(&self, request: &RequestRecord<'_>) -> io::Result<()> {
        let (verdict, reason, message) = match request.outcome {
            RequestOutcome::Own(_) | RequestOutcome::PostRead(_) | RequestOutcome::Read => ("allow", None, None),
            RequestOutcome::Unclassified => ("deny", Some("unclassified"), Some("no rule classifies this request")),
            RequestOutcome::SentWithoutAllow => (
                "breach",
                Some("sent_without_allow"),
                Some("this left the browser without the fence's allow"),
            ),
            RequestOutcome::Forced => (
                "deny",
                Some("forced"),
                Some("denied on purpose by the run (an enforcement test)"),
            ),
            RequestOutcome::SignInRedirect => (
                "deny",
                Some("sign_in_redirect"),
                Some("the sign-in's redirect, caught and handed to the parent; it never loads"),
            ),
        };
        self.append(&RequestEntry {
            ts_ms: now_ms(),
            verdict,
            channel: request.channel,
            method: request.method,
            url: redact_url(request.url),
            own: match request.outcome {
                RequestOutcome::Own(kind) => Some(kind),
                _ => None,
            },
            post_read: match request.outcome {
                RequestOutcome::PostRead(kind) => Some(kind),
                _ => None,
            },
            read: request.outcome == RequestOutcome::Read,
            reason,
            message,
        })
    }

    /// Appends one own frame the native client is about to send, as a single line, and flushes it: the sink calls it
    /// before the frame goes out and sends nothing it couldn't log. Once an append has failed, every later one fails
    /// too.
    ///
    /// # Errors
    /// When the entry can't be written, or an earlier append failed.
    pub fn record_frame(&self, frame: &FrameRecord<'_>) -> io::Result<()> {
        self.append(&FrameEntry {
            ts_ms: now_ms(),
            verdict: "allow",
            event: "frame",
            channel: Channel::WebSocket,
            url: redact_url(frame.url),
            own: frame.kind,
        })
    }

    /// Appends one mutating request let through because it reaches nobody, as a single line, and flushes it: its
    /// method, host (lowercase, no port) and path shape ([`path_shape`]). A URL that doesn't parse is logged with an
    /// empty host and the path `<unparsable>`. Once an append has failed, every later one fails too.
    ///
    /// # Errors
    /// When the entry can't be written, or an earlier append failed.
    pub fn record_allowed(&self, request: &AllowedRecord<'_>) -> io::Result<()> {
        let (host, path) = Url::parse(request.url).map_or_else(
            |_| (String::new(), "<unparsable>".to_owned()),
            |url| {
                let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
                (host, path_shape(url.path()))
            },
        );
        self.append(&AllowedEntry {
            ts_ms: now_ms(),
            verdict: "allow",
            event: "allowed",
            method: request.method,
            host,
            path,
        })
    }

    /// Appends one target the browser fence couldn't arm, as a single line, and flushes it. Once an append has failed,
    /// every later one fails too.
    ///
    /// # Errors
    /// When the entry can't be written, or an earlier append failed.
    pub fn record_arming(&self, record: &ArmingRecord<'_>) -> io::Result<()> {
        let (verdict, message) = if record.breach() {
            ("breach", "a target not armed may have run unguarded")
        } else {
            ("contained", "a target not armed in time was contained before it ran")
        };
        self.append(&ArmingEntry {
            ts_ms: now_ms(),
            verdict,
            event: "arming_failure",
            target_type: record.target_type,
            step: record.step,
            cause: record.cause,
            resume_sent: record.resume_sent,
            held: record.held,
            close: record.close,
            containment: record.containment,
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
        let appended = sink.writer.append(&line);
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

/// The longest path segment [`path_shape`] keeps as it is.
const MAX_WORD: usize = 32;

/// The shape of a URL path (as [`Url::path`] gives it, percent-encoded): each segment percent-decoded and, when it is
/// an id, replaced by its kind, so that the shape names an endpoint and not whom it concerns:
/// - `<chat>`: a conversation id (`19:…`, a chat's, a channel's or a meeting's);
/// - `<mri>`: any other MRI (`8:orgid:…`, `28:…`), but a named `48:` stream (`48:notifications`) stays as it is;
/// - `<guid>`: a GUID;
/// - `<message>`: a number of 6 digits or more (a message id, a timestamp);
/// - `<id>`: anything else that isn't a short word: longer than 32 characters, holding a character other than ASCII
///   letters, digits and `. _ ~ $ -`, or a run of 6 digits or more (an object id, an endpoint id, a token).
///
/// Every other segment is a word of the API and stays.
#[must_use]
pub fn path_shape(path: &str) -> String {
    path.split('/')
        .map(|segment| shape_segment(&percent_decode_str(segment).decode_utf8_lossy()))
        .collect::<Vec<_>>()
        .join("/")
}

fn shape_segment(segment: &str) -> String {
    if segment.is_empty() {
        return String::new();
    }
    let kind = if segment.starts_with("19:") {
        "<chat>"
    } else if let Some((prefix, rest)) = segment.split_once(':')
        && !prefix.is_empty()
        && prefix.bytes().all(|byte| byte.is_ascii_digit())
    {
        if prefix == "48" && !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_lowercase()) {
            return segment.to_owned();
        }
        "<mri>"
    } else if is_guid_text(segment) {
        "<guid>"
    } else if segment.len() >= 6 && segment.bytes().all(|byte| byte.is_ascii_digit()) {
        "<message>"
    } else if is_word(segment) {
        return segment.to_owned();
    } else {
        "<id>"
    };
    kind.to_owned()
}

/// A short word of an API: at most [`MAX_WORD`] ASCII letters, digits and `. _ ~ $ -`, without a run of 6 digits.
fn is_word(segment: &str) -> bool {
    segment.len() <= MAX_WORD
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'$' | b'-'))
        && !segment
            .as_bytes()
            .windows(6)
            .any(|run| run.iter().all(u8::is_ascii_digit))
}

/// A GUID, `8-4-4-4-12` hex digits, in braces or not.
fn is_guid_text(segment: &str) -> bool {
    let bare = segment
        .strip_prefix('{')
        .and_then(|inner| inner.strip_suffix('}'))
        .unwrap_or(segment);
    let groups: Vec<&str> = bare.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|byte| byte.is_ascii_hexdigit()))
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
