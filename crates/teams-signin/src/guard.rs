//! The sign-in request guard: each armed page is restricted to its sign-in hosts and captured own-state rules.
//! Decisions go to the app's diagnostic log, without query values, bodies or headers. The registered redirect is
//! caught and handed directly to the in-process window, never loaded. Diagnostic logging does not govern permission.

use std::collections::BTreeSet;
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;

use crate::diagnostics::{self, Outcome as RequestOutcome};
use teams_auth::redirect::RedirectUri;
use teams_auth::secrets::Secret;
use teams_signin_protocol::protocol::{ALLOW, DENY, LINE_LIMIT, OK, Query};

use crate::classify::{Classification, Classifier};
#[cfg(feature = "test-support")]
use crate::window::is_local_page;

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
    redirect: RedirectUri,
    /// Every request off a loopback address denied ([`Guard::local_only`]).
    #[cfg(feature = "test-support")]
    local_only: bool,
    /// The sign-in host bound; absent only in offline test constructors.
    hosts: Option<Arc<crate::hosts::SignInHosts>>,
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
    /// A guard bound to `hosts`, catching `redirect` and telling its events on `events`.
    #[must_use]
    pub fn new(hosts: Arc<crate::hosts::SignInHosts>, redirect: RedirectUri, events: mpsc::Sender<GuardEvent>) -> Self {
        Self {
            classifier: Classifier::personal(&hosts),
            redirect,
            #[cfg(feature = "test-support")]
            local_only: false,
            hosts: Some(hosts),
            armed: Mutex::new(BTreeSet::new()),
            events: Mutex::new(events),
        }
    }

    /// An unrestricted host set for offline classifier/socket tests only. A production guard always takes hosts.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn for_tests(redirect: RedirectUri, events: mpsc::Sender<GuardEvent>) -> Self {
        Self {
            classifier: Classifier::sign_in(),
            redirect,
            local_only: false,
            hosts: None,
            armed: Mutex::new(BTreeSet::new()),
            events: Mutex::new(events),
        }
    }

    /// Restricts the offline test window to numeric loopback.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub const fn local_only(mut self) -> Self {
        self.local_only = true;
        self
    }

    /// The same guard with one more own-state rule: tests stand a local endpoint in for one of Entra's. Only in a
    /// `test-support` build, which production ngteams refuses to run.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn with_own(mut self, rule: crate::classify::OwnRule) -> Self {
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

    /// Whether `page` may send `method` `url`; the decision is submitted to the diagnostic log before this returns.
    pub fn decide(&self, page: u64, method: &str, url: &str) -> Decision {
        let armed = self
            .armed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&page);
        if !armed {
            diagnostics::record(method, url, RequestOutcome::Unclassified);
            return Decision::Deny;
        }
        if self.redirect.matches(url) {
            diagnostics::record(method, url, RequestOutcome::SignInRedirect);
            self.tell(GuardEvent::Redirect(Secret::new(url)));
            return Decision::Deny;
        }
        #[cfg(feature = "test-support")]
        if self.local_only && !is_local_page(url) {
            self.refuse(method, url);
            return Decision::Deny;
        }
        if self.hosts.as_ref().is_some_and(|hosts| !hosts.admits_url(url)) {
            self.refuse(method, url);
            return Decision::Deny;
        }
        let outcome = match self.classifier.classify(method, url) {
            Classification::Read => RequestOutcome::Read,
            Classification::Own(kind) => RequestOutcome::Own(kind.as_str()),
            Classification::Unknown => RequestOutcome::Unclassified,
        };
        let allow = matches!(outcome, RequestOutcome::Read | RequestOutcome::Own(_));
        diagnostics::record(method, url, outcome);
        if allow { Decision::Allow } else { Decision::Deny }
    }

    /// Logs the redirect caught by the window's navigation policy before it became a request (a page's own navigation
    /// to it); the window hands it on itself.
    pub fn caught_navigation(&self, url: &str) {
        diagnostics::record("GET", url, RequestOutcome::SignInRedirect);
    }

    /// Logs a deny for `method` `url` that never became a decided request: a navigation the window's policy refused,
    /// or a connection the window's egress proxy refused ([`crate::egress`]).
    pub fn refuse(&self, method: &str, url: &str) {
        diagnostics::record(method, url, RequestOutcome::Unclassified);
    }

    /// Logs a tunnel the sign-in host gate opens. No request headers or body are included.
    pub fn tunnel(&self, host: &str, port: u16) {
        diagnostics::record("CONNECT", &format!("https://{host}:{port}/"), RequestOutcome::Tunnel);
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
