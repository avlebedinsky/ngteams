//! The request guard: decides each request a page of the sign-in window sends, as proto-lab's `sign_in` classifier
//! decides it for Chrome (`GET`, `HEAD` and `OPTIONS` reads; the seeded own-state rules in their exact captured
//! shapes, Entra's sign-in form among them; everything else denied, never by host alone), and appends the decision to
//! `fence.log` before answering. A decision that can't be logged is a deny.
//!
//! On top of the classifier:
//! - a page that hasn't told the guard it is armed gets every request denied;
//! - in loopback-only mode ([`Guard::local_only`], which the window always turns on while [`crate::cli::NOT_LIVE`]
//!   holds) every request that isn't to a page on a loopback address ([`crate::cli::is_local_page`]) is denied, so no
//!   decision of the guard lets the window off this machine;
//! - the redirect to the registered redirect URI (it carries the authorization code) is never let through: it is
//!   logged without its query ([`RequestOutcome::SignInRedirect`]) and handed to the window ([`GuardEvent::Redirect`]),
//!   which hands it to the parent.
//!
//! [`GuardServer`] serves the guard on a Unix socket, one thread per web process connection
//! (`teams_signin_extension::protocol`).

use std::collections::BTreeSet;
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;

use proto_lab::classify::{Classification, Classifier};
use teams_auth::redirect::RedirectUri;
use teams_auth::secrets::Secret;
use teams_fence::{Channel, FenceLog, RequestOutcome, RequestRecord};
use teams_signin_extension::protocol::{ALLOW, DENY, LINE_LIMIT, OK, Query};

use crate::cli::is_local_page;

/// What the guard tells the window.
#[derive(Debug)]
pub enum GuardEvent {
    /// A page's `send-request` handler is connected. Arming is per page: the process id the extension tells is not
    /// used, and only the view's first arming is waited for (`docs/backlog/webkit-sign-in-guard-redirects.md`).
    Armed { page: u64 },
    /// The redirect to the registered redirect URI, caught (it carries the code).
    Redirect(Secret),
}

/// One request's fate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// The guard's decisions, shared by every connection.
pub struct Guard {
    classifier: Classifier,
    log: FenceLog,
    redirect: RedirectUri,
    /// Every request off a loopback address denied ([`Guard::local_only`]).
    local_only: bool,
    /// Armed pages.
    armed: Mutex<BTreeSet<u64>>,
    events: Mutex<mpsc::Sender<GuardEvent>>,
}

impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Guard")
            .field("redirect", &self.redirect)
            .finish_non_exhaustive()
    }
}

impl Guard {
    /// A guard with proto-lab's `sign_in` classifier, logging to `log`, catching the redirect to `redirect`, telling
    /// its events on `events`.
    #[must_use]
    pub fn new(log: FenceLog, redirect: RedirectUri, events: mpsc::Sender<GuardEvent>) -> Self {
        Self {
            classifier: Classifier::sign_in(),
            log,
            redirect,
            local_only: false,
            armed: Mutex::new(BTreeSet::new()),
            events: Mutex::new(events),
        }
    }

    /// The same guard denying every request that isn't to a page on a loopback address (after catching the redirect,
    /// which is still caught and logged as such): the window's mode while [`crate::cli::NOT_LIVE`] holds.
    #[must_use]
    pub const fn local_only(mut self) -> Self {
        self.local_only = true;
        self
    }

    /// The same guard with one more own-state rule: tests stand a local endpoint in for one of Entra's. Only in a
    /// `test-support` build, which the `teams-signin` binary refuses to run.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn with_own(mut self, rule: proto_lab::classify::OwnRule) -> Self {
        self.classifier = self.classifier.with_own(rule);
        self
    }

    /// The answer to one protocol line.
    pub fn answer(&self, line: &str) -> &'static str {
        match Query::parse(line) {
            // the process id is on the wire but unused: arming is per page
            Some(Query::Armed { page, .. }) => {
                self.armed.lock().unwrap_or_else(PoisonError::into_inner).insert(page);
                self.tell(GuardEvent::Armed { page });
                OK
            }
            Some(Query::Request { page, method, url }) => match self.decide(page, &method, &url) {
                Decision::Allow => ALLOW,
                Decision::Deny => DENY,
            },
            None => DENY,
        }
    }

    /// Whether `page` may send `method` `url`; the decision is in `fence.log` before this returns.
    pub fn decide(&self, page: u64, method: &str, url: &str) -> Decision {
        let armed = self
            .armed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&page);
        if !armed {
            self.record(method, url, RequestOutcome::Unclassified);
            return Decision::Deny;
        }
        if self.redirect.matches(url) {
            self.record(method, url, RequestOutcome::SignInRedirect);
            self.tell(GuardEvent::Redirect(Secret::new(url)));
            return Decision::Deny;
        }
        if self.local_only && !is_local_page(url) {
            self.refuse(method, url);
            return Decision::Deny;
        }
        let outcome = match self.classifier.classify(method, url, None) {
            Classification::Read => RequestOutcome::Read,
            Classification::Own(kind) => RequestOutcome::Own(kind.as_str()),
            // the sign-in classifier maps no writes, no read by POST and no own availability
            Classification::PostRead { .. }
            | Classification::OwnAvailability { .. }
            | Classification::Write(_)
            | Classification::Unknown => RequestOutcome::Unclassified,
        };
        let allow = matches!(outcome, RequestOutcome::Read | RequestOutcome::Own(_));
        if self.record(method, url, outcome) && allow {
            Decision::Allow
        } else {
            Decision::Deny
        }
    }

    /// Logs the redirect caught by the window's navigation policy before it became a request (a page's own navigation
    /// to it); the window hands it on itself.
    pub fn caught_navigation(&self, url: &str) {
        self.record("GET", url, RequestOutcome::SignInRedirect);
    }

    /// Logs a deny for `method` `url` that never became a decided request: a navigation the window's policy refused,
    /// or a connection the window's egress proxy refused ([`crate::egress`]).
    pub fn refuse(&self, method: &str, url: &str) {
        self.record(method, url, RequestOutcome::Unclassified);
    }

    /// Logs one decision; `false` when it couldn't be.
    fn record(&self, method: &str, url: &str, outcome: RequestOutcome) -> bool {
        let record = RequestRecord {
            channel: Channel::Http,
            method,
            url,
            outcome,
        };
        self.log.record_request(&record).is_ok()
    }

    fn tell(&self, event: GuardEvent) {
        let _ = self.events.lock().unwrap_or_else(PoisonError::into_inner).send(event);
    }
}

/// The guard on a Unix socket: one thread accepting, one per connection answering line by line. A line longer than
/// [`LINE_LIMIT`], or one that isn't UTF-8, is answered with a deny and ends the connection (so the extension denies
/// everything after it).
#[derive(Debug)]
pub struct GuardServer {
    path: PathBuf,
}

impl GuardServer {
    /// Binds `path` (a socket in a private directory) and serves `guard` there until the process exits.
    ///
    /// # Errors
    /// The socket can't be bound.
    pub fn start(path: &Path, guard: Arc<Guard>) -> io::Result<Self> {
        let listener = UnixListener::bind(path)?;
        thread::Builder::new().name("signin-guard".into()).spawn(move || {
            for stream in listener.incoming().map_while(Result::ok) {
                let guard = Arc::clone(&guard);
                let _ = thread::Builder::new()
                    .name("signin-guard-conn".into())
                    .spawn(move || serve(&guard, stream));
            }
        })?;
        Ok(Self { path: path.to_owned() })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn serve(guard: &Guard, stream: UnixStream) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let limit = u64::try_from(LINE_LIMIT).unwrap_or(u64::MAX) + 1;
    let mut reader = BufReader::new(stream);
    loop {
        let mut bytes = Vec::new();
        match reader.by_ref().take(limit).read_until(b'\n', &mut bytes) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let complete = bytes.pop() == Some(b'\n');
        let answer = match std::str::from_utf8(&bytes) {
            Ok(line) if complete => guard.answer(line),
            _ => {
                let _ = writer.write_all(format!("{DENY}\n").as_bytes());
                return;
            }
        };
        if writer.write_all(format!("{answer}\n").as_bytes()).is_err() {
            return;
        }
    }
}
