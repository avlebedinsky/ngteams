//! Arming the lab browser: the fence's HTTP layer on every request, before any target's code runs.
//!
//! [`ArmedBrowser::arm`] takes the browser over and runs on a fresh debugging connection, before any page is opened:
//! 1. `Fetch.enable` with the pattern `*` on the **browser** session. It pauses every request of every target, and it
//!    is the only layer that still sees a `sendBeacon` or `fetch(keepalive)` fired from `pagehide` when the tab closes:
//!    the tab's own session is gone by then, and such a request reaches the network if only per-target `Fetch` is on
//!    (checked on Chrome 155).
//! 2. `Target.setAutoAttach` with `waitForDebuggerOnStart` on the browser session: every new page, shared worker and
//!    service worker attaches paused.
//! 3. Through a blank tab, armed like any other and closed afterwards, the service workers of the given origins are
//!    unregistered and their caches cleared ([`neutralize_service_workers`]), so no Background Sync or queued write
//!    replays. Chrome 155 answers `Storage.clearDataForOrigin` on the browser session with an internal error; on a page
//!    session it clears any origin.
//!
//! Every attached target is then armed ([`arm_target`]) by its type ([`TargetKind::steps`]): `Fetch.enable` with `*`
//! on its own session (pages, iframes, shared and service workers, and any type we don't know), the service-worker
//! bypass on pages and iframes (after `Network.enable`, without which Chrome ignores it), auto-attach with `waitForDebuggerOnStart` for its own children (iframes, dedicated
//! workers), and only then `Runtime.runIfWaitingForDebugger`. A dedicated worker has no `Fetch` domain of its own: its
//! requests pause on its page's session (and the browser's). If any step fails, the target is closed and never resumed,
//! and the failure fails the run. The one exception is a session that is gone before its arming finishes (the browser
//! answers "Session with given id not found"): its tab closed, or the target was attached twice (a service worker
//! attaches through the browser and through every page in its scope) and the other session is armed on its own. That
//! session has nothing left to arm or resume, so it is dropped quietly; browser-level `Fetch` stays on regardless.
//!
//! A request therefore pauses on its target's session first and on the browser session after that, and each layer
//! decides it on its own ([`crate::fence_http::HttpFence`]); a deny at either fails it.
//!
//! The fence holds only while its connection does: a browser that no longer sees the client drops the browser-level
//! `Fetch` and the auto-attach hold, and later requests go out unexamined. So the browser lives and dies with the
//! fence: [`ArmedBrowser`] owns the [`Browser`], dropping it (or a failed [`ArmedBrowser::arm`]) kills the browser
//! before the connection closes, and if the connection's events end while the fence is up (the browser closed the
//! websocket, or the reader stopped on a message it couldn't read) the browser is killed at once and the run fails.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::browser::{Browser, BrowserError};
use crate::cdp::{Cdp, CdpError, Event, Events, SessionId};
use crate::fence_http::{Denial, HttpFence};

/// The origins whose service workers are neutralized at the start of every run.
pub const TEAMS_ORIGINS: [&str; 2] = ["https://teams.microsoft.com", "https://teams.cloud.microsoft"];

/// What every pattern-`*` `Fetch.enable` sends: every request, at the request stage.
fn intercept_all() -> Value {
    json!({"patterns": [{"urlPattern": "*"}]})
}

/// Auto-attach that holds every new target until it is armed.
fn hold_new_targets() -> Value {
    json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true})
}

/// A target's type, as `TargetInfo.type` names it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TargetKind {
    Page,
    /// An out-of-process iframe.
    Iframe,
    /// A dedicated worker.
    Worker,
    SharedWorker,
    ServiceWorker,
    /// Any other type (`browser_ui`, `webview`, worklets, ones newer than this code); armed like a page would be,
    /// minus the page-only steps, and closed if that fails.
    Other(String),
}

impl TargetKind {
    #[must_use]
    pub fn from_type(name: &str) -> Self {
        match name {
            "page" => Self::Page,
            "iframe" => Self::Iframe,
            "worker" => Self::Worker,
            "shared_worker" => Self::SharedWorker,
            "service_worker" => Self::ServiceWorker,
            other => Self::Other(other.to_owned()),
        }
    }

    /// The arming steps for this kind of target, in order; [`Step::Resume`] is always last.
    #[must_use]
    pub fn steps(&self) -> &'static [Step] {
        match self {
            Self::Page | Self::Iframe => &[
                Step::Fetch,
                Step::Network,
                Step::BypassServiceWorker,
                Step::AutoAttach,
                Step::Resume,
            ],
            // no Fetch domain: the page's session and the browser's pause its requests
            Self::Worker => &[Step::AutoAttach, Step::Resume],
            Self::SharedWorker | Self::ServiceWorker | Self::Other(_) => &[Step::Fetch, Step::AutoAttach, Step::Resume],
        }
    }
}

/// One step of arming a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// `Fetch.enable` with the pattern `*` on the target's session.
    Fetch,
    /// `Network.enable`, which the service-worker bypass needs to take effect.
    Network,
    /// `Network.setBypassServiceWorker(true)`: the page's requests go to the network (and through `Fetch`), never to a
    /// service worker's fetch handler.
    BypassServiceWorker,
    /// `Target.setAutoAttach` with `waitForDebuggerOnStart` on the target's session, for its children.
    AutoAttach,
    /// `Runtime.runIfWaitingForDebugger`: the target's code starts.
    Resume,
}

impl Step {
    fn method(self) -> &'static str {
        match self {
            Self::Fetch => "Fetch.enable",
            Self::Network => "Network.enable",
            Self::BypassServiceWorker => "Network.setBypassServiceWorker",
            Self::AutoAttach => "Target.setAutoAttach",
            Self::Resume => "Runtime.runIfWaitingForDebugger",
        }
    }

    fn params(self) -> Value {
        match self {
            Self::Fetch => intercept_all(),
            Self::BypassServiceWorker => json!({"bypass": true}),
            Self::AutoAttach => hold_new_targets(),
            Self::Network | Self::Resume => Value::Null,
        }
    }
}

/// Why a target couldn't be armed.
#[derive(Debug, thiserror::Error)]
pub enum ArmError {
    #[error("arming step {step:?} failed: {source}")]
    Step {
        step: Step,
        #[source]
        source: CdpError,
    },
    #[error("an attach event without {0}")]
    Malformed(&'static str),
    #[error("no armed page within {0:?}")]
    Timeout(Duration),
    #[error("the blank tab for the service-worker cleanup couldn't be closed")]
    SetupNotClosed,
    #[error("the fence lost its connection to the browser; the browser was killed")]
    ConnectionLost,
    #[error(transparent)]
    Cdp(#[from] CdpError),
}

/// The CDP error code for a session that doesn't exist (any more).
const SESSION_NOT_FOUND: i64 = -32001;

impl ArmError {
    /// Whether the step failed only because the target's session is gone (detached, or its target closed).
    #[must_use]
    pub fn session_gone(&self) -> bool {
        matches!(
            self,
            Self::Step {
                source: CdpError::Remote { error, .. },
                ..
            } if error.code == SESSION_NOT_FOUND
        )
    }
}

/// Runs a target's arming steps on its session, resuming it last. Stops at the first step that fails, so a target that
/// isn't fully armed is never resumed.
///
/// # Errors
/// [`ArmError::Step`] naming the step that failed.
pub async fn arm_target(cdp: &Cdp, session: &SessionId, kind: &TargetKind) -> Result<(), ArmError> {
    for &step in kind.steps() {
        cdp.call(Some(session), step.method(), step.params())
            .await
            .map_err(|source| ArmError::Step { step, source })?;
    }
    Ok(())
}

/// Unregisters the service workers of `origins` and clears their cache storage, through a page's session (any page:
/// the origins needn't be its own).
///
/// # Errors
/// When the browser refuses to clear an origin.
pub async fn neutralize_service_workers(cdp: &Cdp, page: &SessionId, origins: &[String]) -> Result<(), CdpError> {
    for origin in origins {
        let params = json!({"origin": origin, "storageTypes": "service_workers,cache_storage"});
        cdp.call(Some(page), "Storage.clearDataForOrigin", params).await?;
    }
    Ok(())
}

/// How long arming waits for the blank tab it neutralizes service workers through.
const SETUP_TIMEOUT: Duration = Duration::from_secs(15);

/// How to arm a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArmOptions {
    /// The origins whose service workers are neutralized before anything loads.
    pub service_worker_origins: Vec<String>,
}

impl Default for ArmOptions {
    fn default() -> Self {
        Self {
            service_worker_origins: TEAMS_ORIGINS.iter().map(|&origin| origin.to_owned()).collect(),
        }
    }
}

/// An armed target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArmedTarget {
    pub target_id: String,
    pub session: SessionId,
    pub kind: TargetKind,
    /// The session it attached through; `None` for the browser's own auto-attach.
    pub parent: Option<SessionId>,
}

/// A target that couldn't be armed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArmingFailure {
    /// Missing when the attach event didn't name one.
    pub target_id: Option<String>,
    /// `TargetInfo.type`.
    pub kind: String,
    pub error: String,
    /// Whether `Target.closeTarget` closed it. A target that couldn't be closed was never resumed either.
    pub closed: bool,
}

/// Why a run failed: a denied request, a target that couldn't be armed, or a fence that lost its connection.
#[derive(Debug, thiserror::Error)]
#[error(
    "the run failed: {} request(s) denied, {} target(s) not armed{}",
    denials.len(),
    failures.len(),
    if *connection_lost { ", and the fence lost its connection to the browser" } else { "" }
)]
pub struct RunFailed {
    pub denials: Vec<Denial>,
    pub failures: Vec<ArmingFailure>,
    /// The connection's events ended while the fence was up: nothing was decided after that, and the browser was
    /// killed.
    pub connection_lost: bool,
}

#[derive(Default)]
struct Targets {
    /// Every session attached and not detached since, armed or not. A target's arming records it only if its session
    /// is still here, so one that detaches mid-arming never shows up as armed.
    attached: HashSet<SessionId>,
    armed: HashMap<SessionId, ArmedTarget>,
    failures: Vec<ArmingFailure>,
}

struct Shared {
    http: HttpFence,
    targets: Mutex<Targets>,
    changed: Notify,
    /// The browser, until the fence is closed or dropped, or its connection is lost.
    browser: Mutex<Option<Browser>>,
    /// The connection's events ended while the fence was up.
    lost: AtomicBool,
}

impl Shared {
    fn targets(&self) -> MutexGuard<'_, Targets> {
        self.targets.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn take_browser(&self) -> Option<Browser> {
        self.browser.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    /// Kills the browser and every process it started, unless it was taken already.
    fn kill_browser(&self) {
        drop(self.take_browser());
    }

    fn lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }
}

/// A browser with the fence's HTTP layer armed. It owns the browser, and handles the connection's events until it is
/// closed or dropped.
///
/// Dropping it kills the browser (with every process the browser started) before the connection closes, so the browser
/// never runs without the fence; [`Self::close`] lets the browser exit on its own first.
pub struct ArmedBrowser {
    cdp: Cdp,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl Drop for ArmedBrowser {
    fn drop(&mut self) {
        // this body runs before the fields (the connection among them) are dropped
        self.shared.kill_browser();
        self.task.abort();
    }
}

impl ArmedBrowser {
    /// Takes `browser` over, arms it on a fresh connection (see the module docs) and handles that connection's events
    /// from then on: every paused request goes through `http`, every attached target is armed before it runs.
    ///
    /// # Errors
    /// When the connection can't be opened, the browser refuses browser-level `Fetch` or auto-attach, or the
    /// service-worker cleanup fails. The browser is killed then: it never runs half armed.
    pub async fn arm(browser: Browser, http: HttpFence, options: &ArmOptions) -> Result<Self, ArmError> {
        let (cdp, events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
        cdp.call(None, "Fetch.enable", intercept_all()).await?;
        let shared = Arc::new(Shared {
            http,
            targets: Mutex::new(Targets::default()),
            changed: Notify::new(),
            browser: Mutex::new(Some(browser)),
            lost: AtomicBool::new(false),
        });
        let task = tokio::spawn(handle_events(cdp.clone(), events, Arc::clone(&shared)));
        // from here on, an early return drops `armed`, and that kills the browser
        let armed = Self { cdp, shared, task };
        armed.cdp.call(None, "Target.setAutoAttach", hold_new_targets()).await?;
        let blank = armed.open_page(SETUP_TIMEOUT).await?;
        neutralize_service_workers(&armed.cdp, &blank.session, &options.service_worker_origins).await?;
        if !close_target(&armed.cdp, &blank.target_id).await {
            return Err(ArmError::SetupNotClosed);
        }
        Ok(armed)
    }

    /// Asks the browser to close (`Browser.close`), waits up to `grace` for it to exit, and stops it the way
    /// [`Browser::wait_or_kill`] does if it doesn't. A browser killed already (its connection was lost) needs nothing
    /// more.
    ///
    /// # Errors
    /// [`BrowserError::Stop`] when the browser process can't be reaped.
    pub async fn close(self, grace: Duration) -> Result<(), BrowserError> {
        // taken first, so the connection closing as the browser exits isn't taken for a lost one
        let Some(browser) = self.shared.take_browser() else {
            return Ok(());
        };
        // best effort: the connection may drop before the reply, or never answer if its reader is gone
        let _ = tokio::time::timeout(grace, self.cdp.call(None, "Browser.close", Value::Null)).await;
        browser.wait_or_kill(grace).await?;
        Ok(())
    }

    /// The connection, for driving the armed targets.
    #[must_use]
    pub fn cdp(&self) -> &Cdp {
        &self.cdp
    }

    /// Opens a new tab on `about:blank` and waits up to `timeout` for it to be armed.
    ///
    /// # Errors
    /// When the tab can't be created, or isn't armed in time.
    pub async fn open_page(&self, timeout: Duration) -> Result<ArmedTarget, ArmError> {
        let created = self
            .cdp
            .call(None, "Target.createTarget", json!({"url": "about:blank"}))
            .await?;
        let target_id = created["targetId"].as_str().ok_or(ArmError::Malformed("targetId"))?;
        let wait = async {
            loop {
                let changed = self.shared.changed.notified();
                if let Some(page) = self.armed_page(target_id) {
                    return Ok(page);
                }
                if self.shared.lost() {
                    return Err(ArmError::ConnectionLost);
                }
                changed.await;
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| ArmError::Timeout(timeout))?
    }

    fn armed_page(&self, target_id: &str) -> Option<ArmedTarget> {
        self.shared
            .targets()
            .armed
            .values()
            .find(|target| target.target_id == target_id && target.parent.is_none())
            .cloned()
    }

    /// Every target armed so far and still attached.
    #[must_use]
    pub fn targets(&self) -> Vec<ArmedTarget> {
        self.shared.targets().armed.values().cloned().collect()
    }

    /// Every target that couldn't be armed.
    #[must_use]
    pub fn failures(&self) -> Vec<ArmingFailure> {
        self.shared.targets().failures.clone()
    }

    /// Every request denied so far.
    #[must_use]
    pub fn denials(&self) -> Vec<Denial> {
        self.shared.http.denials()
    }

    /// The run's verdict so far: any denied request or unarmed target fails it, and so does a lost connection.
    ///
    /// # Errors
    /// [`RunFailed`] listing them.
    pub fn verdict(&self) -> Result<(), RunFailed> {
        let denials = self.denials();
        let failures = self.failures();
        let connection_lost = self.shared.lost();
        if denials.is_empty() && failures.is_empty() && !connection_lost {
            return Ok(());
        }
        Err(RunFailed {
            denials,
            failures,
            connection_lost,
        })
    }
}

/// Handles the connection's events until they end, which happens only when the connection does: the browser dropped
/// it, or the reader stopped on a message it couldn't read. Either way nothing is decided any more, so the run fails
/// and the browser is killed: a browser that no longer sees the client releases what the fence held, and one that
/// still does keeps its paused requests and held targets waiting forever.
async fn handle_events(cdp: Cdp, events: Events, shared: Arc<Shared>) {
    dispatch_events(cdp, events, &shared).await;
    shared.lost.store(true, Ordering::SeqCst);
    shared.kill_browser();
    shared.changed.notify_waiters();
}

async fn dispatch_events(cdp: Cdp, mut events: Events, shared: &Arc<Shared>) {
    while let Some(event) = events.recv().await {
        match event.method.as_str() {
            "Fetch.requestPaused" => {
                let (cdp, shared) = (cdp.clone(), Arc::clone(shared));
                // a request whose target is gone can't be answered; it was never sent either
                tokio::spawn(async move {
                    let _ = shared.http.handle(&cdp, &event).await;
                });
            }
            "Target.attachedToTarget" => {
                // recorded here, in event order, so the arming sees a detach that comes before it finishes
                if let Some(session) = event.params["sessionId"].as_str() {
                    shared.targets().attached.insert(SessionId::new(session));
                }
                tokio::spawn(on_attached(cdp.clone(), Arc::clone(shared), event));
            }
            "Target.detachedFromTarget" => {
                if let Some(session) = event.params["sessionId"].as_str() {
                    let session = SessionId::new(session);
                    let mut targets = shared.targets();
                    targets.attached.remove(&session);
                    targets.armed.remove(&session);
                }
            }
            _ => {}
        }
    }
}

async fn on_attached(cdp: Cdp, shared: Arc<Shared>, event: Event) {
    let info = &event.params["targetInfo"];
    let kind_name = info["type"].as_str().unwrap_or("?").to_owned();
    let target_id = info["targetId"].as_str().map(str::to_owned);
    let session = event.params["sessionId"].as_str().map(SessionId::new);
    let outcome = match (&target_id, &session) {
        (Some(target_id), Some(session)) => {
            let kind = TargetKind::from_type(&kind_name);
            arm_target(&cdp, session, &kind).await.map(|()| ArmedTarget {
                target_id: target_id.clone(),
                session: session.clone(),
                kind,
                parent: event.session_id.clone(),
            })
        }
        (None, _) => Err(ArmError::Malformed("targetInfo.targetId")),
        (_, None) => Err(ArmError::Malformed("sessionId")),
    };
    match outcome {
        Ok(armed) => {
            let mut targets = shared.targets();
            // one that detached while it was being armed is gone: listing it would hand out a dead session
            if targets.attached.contains(&armed.session) {
                targets.armed.insert(armed.session.clone(), armed);
            }
        }
        // the session is gone; nothing left to arm, resume or close through it
        Err(error) if error.session_gone() => forget(&shared, session.as_ref()),
        Err(error) => {
            forget(&shared, session.as_ref());
            let closed = match &target_id {
                Some(id) => close_target(&cdp, id).await,
                None => false,
            };
            shared.targets().failures.push(ArmingFailure {
                target_id,
                kind: kind_name,
                error: error.to_string(),
                closed,
            });
        }
    }
    shared.changed.notify_waiters();
}

/// Drops a session that won't be armed from the attached set: if its target can't be closed, no detach may come.
fn forget(shared: &Shared, session: Option<&SessionId>) {
    if let Some(session) = session {
        shared.targets().attached.remove(session);
    }
}

async fn close_target(cdp: &Cdp, target_id: &str) -> bool {
    cdp.call(None, "Target.closeTarget", json!({"targetId": target_id}))
        .await
        .is_ok_and(|result| result["success"] != json!(false))
}
