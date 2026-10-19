//! Arming the lab browser: the fence's HTTP layer on every request and its WebSocket guard in every scope, before any
//! target's code runs.
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
//! on its own session (pages, iframes, shared and service workers, and any type we don't know), `Network.enable`
//! everywhere (the service-worker bypass on pages and iframes needs it, and so does the audit of WebSocket frames,
//! which Chrome reports on the session of the target that sent them), the service-worker bypass, the WebSocket guard
//! ([`crate::fence_ws`]), auto-attach with `waitForDebuggerOnStart` for its own children (iframes, dedicated workers),
//! and only then `Runtime.runIfWaitingForDebugger`. A dedicated worker has no `Fetch` domain of its own: its requests
//! pause on its page's session (and the browser's).
//!
//! The guard goes in through the binding (`Runtime.addBinding`), then: on a page or iframe, with `Runtime.enable` and
//! `Page.enable` on, `Page.addScriptToEvaluateOnNewDocument` with `runImmediately`, so it runs in every document of
//! every frame of the target (same-origin `about:blank` iframes included) before the document's own scripts, and in the
//! contexts there already; on a worker (its only context exists, paused, at attach) `Runtime.evaluate`, which has to
//! report the guard armed. Chrome 155 installs a binding in new contexts only with `Runtime.enable` on, and runs
//! new-document scripts only with `Page.enable` on.
//!
//! A popup (`window.open`, the three-argument `document.open`) is a page target of its own, whose initial
//! `about:blank` document its opener can script at once. Chrome 155 holds the opener inside `open` while the popup,
//! attached and held, is armed: `open('')` returned after about 100 ms, with the guard already in the popup. So the
//! opener never reaches an unguarded `send` through it, and the popup's guard refuses the opener's sockets (a guard
//! queues only its own realm's: see [`crate::fence_ws`]). Checked live by `tests/fence_ws.rs`, which would see the frame
//! reach the server if a later Chrome stopped holding the opener.
//!
//! A target's own auto-attach leaves service workers out: each one attaches once, through the browser, and is armed
//! there. A page in a service worker's scope would attach it too, and on Chrome 155 a service worker attached twice
//! while paused answers no `Network` or `Runtime` command on either session until it runs, so it could never be armed
//! before it runs.
//!
//! If any step fails, or gets no answer within [`STEP_TIMEOUT`], the target is closed and never resumed, and the
//! failure fails the run. The one exception is a session that is gone before its arming finishes (the browser answers
//! "Session with given id not found"): its tab closed, or its worker ended. That session has nothing left to arm or
//! resume, so it is dropped quietly; browser-level `Fetch` stays on regardless.
//!
//! A request therefore pauses on its target's session first and on the browser session after that, and each layer
//! decides it on its own ([`crate::fence_http::HttpFence`]); a deny at either fails it. A WebSocket frame waits in the
//! guard until [`crate::fence_ws::WsFence`] decides it, and every frame the browser reports sent is audited against
//! the allows.
//!
//! The fence holds only while its connection does: a browser that no longer sees the client drops the browser-level
//! `Fetch` and the auto-attach hold, and later requests go out unexamined. So the browser lives and dies with the
//! fence: [`ArmedBrowser`] owns the [`Browser`], dropping it (or a failed [`ArmedBrowser::arm`]) kills the browser
//! before the connection closes, and if the connection's events end while the fence is up (the browser closed the
//! websocket, or the reader stopped on a message it couldn't read) the browser is killed at once and the run fails.
//! [`ArmedBrowser::close`] lets the browser exit on its own only once the browser has confirmed `Browser.close`;
//! without that, it kills the browser too.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;

use crate::browser::{Browser, BrowserError};
use crate::cdp::{Cdp, CdpError, Event, Events, SessionId};
use crate::fence_http::{Denial, HttpFence};
use crate::fence_ws::{FrameDenial, GuardScript, UnsanctionedFrame, WsFence};

/// The origins whose service workers are neutralized at the start of every run.
pub const TEAMS_ORIGINS: [&str; 2] = ["https://teams.microsoft.com", "https://teams.cloud.microsoft"];

/// What every pattern-`*` `Fetch.enable` sends: every request, at the request stage.
fn intercept_all() -> Value {
    json!({"patterns": [{"urlPattern": "*"}]})
}

/// Auto-attach that holds every new target until it is armed: the browser's own.
fn hold_new_targets() -> Value {
    json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true})
}

/// A target's auto-attach for its children: held like the browser's, but without service workers, which attach through
/// the browser only (see the module docs). The rest of the filter is Chrome's default.
fn hold_children() -> Value {
    json!({
        "autoAttach": true,
        "waitForDebuggerOnStart": true,
        "flatten": true,
        "filter": [
            {"type": "service_worker", "exclude": true},
            {"type": "browser", "exclude": true},
            {"type": "tab", "exclude": true},
            {},
        ],
    })
}

/// How long one arming step may go unanswered before the target is given up on (closed, and the run failed).
pub const STEP_TIMEOUT: Duration = Duration::from_secs(10);

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
                Step::Runtime,
                Step::Page,
                Step::Binding,
                Step::GuardNewDocuments,
                Step::AutoAttach,
                Step::Resume,
            ],
            // no Fetch domain: the page's session and the browser's pause its requests
            Self::Worker => &[
                Step::Network,
                Step::Binding,
                Step::Guard,
                Step::AutoAttach,
                Step::Resume,
            ],
            Self::SharedWorker | Self::ServiceWorker => &[
                Step::Fetch,
                Step::Network,
                Step::Binding,
                Step::Guard,
                Step::AutoAttach,
                Step::Resume,
            ],
            // the browser's own pages (`browser_ui`, `background_page`) need `Runtime.enable` for the binding to be there
            // when the guard runs (checked on Chrome 155)
            Self::Other(_) => &[
                Step::Fetch,
                Step::Network,
                Step::Runtime,
                Step::Binding,
                Step::Guard,
                Step::AutoAttach,
                Step::Resume,
            ],
        }
    }
}

/// One step of arming a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// `Fetch.enable` with the pattern `*` on the target's session.
    Fetch,
    /// `Network.enable`: the service-worker bypass needs it to take effect, and the audit of WebSocket frames reads its
    /// events.
    Network,
    /// `Network.setBypassServiceWorker(true)`: the page's requests go to the network (and through `Fetch`), never to a
    /// service worker's fetch handler.
    BypassServiceWorker,
    /// `Runtime.enable`, without which a binding isn't installed in the target's new contexts. Not on workers: a worker
    /// has its one context at attach already.
    Runtime,
    /// `Page.enable`, without which new-document scripts don't run.
    Page,
    /// `Runtime.addBinding`: the guard's channel for verdicts.
    Binding,
    /// `Page.addScriptToEvaluateOnNewDocument` with `runImmediately`: the guard in every document of every frame of a
    /// page or iframe target, and in its contexts already there.
    GuardNewDocuments,
    /// `Runtime.evaluate` of the guard in a worker's context; the guard has to report itself armed.
    Guard,
    /// `Target.setAutoAttach` with `waitForDebuggerOnStart` on the target's session, for its children (service workers
    /// left out: they attach through the browser).
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
            Self::Runtime => "Runtime.enable",
            Self::Page => "Page.enable",
            Self::Binding => "Runtime.addBinding",
            Self::GuardNewDocuments => "Page.addScriptToEvaluateOnNewDocument",
            Self::Guard => "Runtime.evaluate",
            Self::AutoAttach => "Target.setAutoAttach",
            Self::Resume => "Runtime.runIfWaitingForDebugger",
        }
    }

    fn params(self, guard: &GuardScript) -> Value {
        match self {
            Self::Fetch => intercept_all(),
            Self::BypassServiceWorker => json!({"bypass": true}),
            Self::Binding => json!({"name": crate::fence_ws::BINDING}),
            Self::GuardNewDocuments => json!({"source": guard.source(), "runImmediately": true}),
            Self::Guard => json!({"expression": guard.source(), "returnByValue": true}),
            Self::AutoAttach => hold_children(),
            Self::Network | Self::Runtime | Self::Page | Self::Resume => Value::Null,
        }
    }

    /// Runs this step on a target's session.
    async fn run(self, cdp: &Cdp, session: &SessionId, guard: &GuardScript) -> Result<(), ArmError> {
        let call = cdp.call(Some(session), self.method(), self.params(guard));
        let result = tokio::time::timeout(STEP_TIMEOUT, call)
            .await
            .map_err(|_| ArmError::StepTimeout(self))?
            .map_err(|source| ArmError::Step { step: self, source })?;
        if self == Self::Guard && result["result"]["value"] != "armed" {
            let outcome = match &result["exceptionDetails"]["exception"]["description"] {
                Value::String(description) => description.clone(),
                _ => result["result"]["value"].to_string(),
            };
            return Err(ArmError::Guard(outcome));
        }
        Ok(())
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
    #[error("arming step {0:?} got no answer within {STEP_TIMEOUT:?}")]
    StepTimeout(Step),
    #[error("the WebSocket guard isn't armed: {0}")]
    Guard(String),
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
/// [`ArmError::Step`] naming the step that failed, [`ArmError::StepTimeout`] naming one that got no answer, or
/// [`ArmError::Guard`] when the guard didn't report itself armed.
pub async fn arm_target(
    cdp: &Cdp,
    session: &SessionId,
    kind: &TargetKind,
    guard: &GuardScript,
) -> Result<(), ArmError> {
    for &step in kind.steps() {
        step.run(cdp, session, guard).await?;
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

/// Why a run failed: a denied request or frame, a frame sent without an allow, a target that couldn't be armed, or a
/// fence that lost its connection.
#[derive(Debug, thiserror::Error)]
#[error(
    "the run failed: {} request(s) denied, {} frame(s) denied, {} frame(s) sent without an allow, {} target(s) not \
     armed{}",
    denials.len(),
    frame_denials.len(),
    unsanctioned.len(),
    failures.len(),
    if *connection_lost { ", and the fence lost its connection to the browser" } else { "" }
)]
pub struct RunFailed {
    pub denials: Vec<Denial>,
    pub frame_denials: Vec<FrameDenial>,
    /// Frames the browser reported sent that no allow covered: they got out.
    pub unsanctioned: Vec<UnsanctionedFrame>,
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
    ws: WsFence,
    targets: Mutex<Targets>,
    changed: Notify,
    /// The browser, until the fence is closed or dropped, or its connection is lost.
    browser: Mutex<Option<Browser>>,
    /// The connection's events ended while the fence was up.
    lost: AtomicBool,
    /// [`ArmedBrowser::close`] has asked the browser to close, and decides from its answer whether to kill it.
    closing: AtomicBool,
    /// Where a copy of every event goes ([`ArmedBrowser::watch`]).
    taps: Mutex<Vec<mpsc::UnboundedSender<Event>>>,
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

    /// Hands a copy of `event` to every watcher still listening.
    fn tap(&self, event: &Event) {
        let mut taps = self.taps.lock().unwrap_or_else(PoisonError::into_inner);
        taps.retain(|tap| tap.send(event.clone()).is_ok());
    }
}

/// A browser with the fence armed: its HTTP layer and its WebSocket guard. It owns the browser, and handles the
/// connection's events until it is closed or dropped.
///
/// Dropping it kills the browser (with every process the browser started) before the connection closes, so the browser
/// never runs without the fence; [`Self::close`] lets the browser exit on its own once it has confirmed closing.
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
    /// from then on: every paused request goes through `http`, every WebSocket frame through `ws`, and every attached
    /// target is armed before it runs.
    ///
    /// # Errors
    /// When the connection can't be opened, the browser refuses browser-level `Fetch` or auto-attach, or the
    /// service-worker cleanup fails. The browser is killed then: it never runs half armed.
    pub async fn arm(browser: Browser, http: HttpFence, ws: WsFence, options: &ArmOptions) -> Result<Self, ArmError> {
        let (cdp, events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
        cdp.call(None, "Fetch.enable", intercept_all()).await?;
        let shared = Arc::new(Shared {
            http,
            ws,
            targets: Mutex::new(Targets::default()),
            changed: Notify::new(),
            browser: Mutex::new(Some(browser)),
            lost: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            taps: Mutex::new(Vec::new()),
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

    /// Asks the browser to close (`Browser.close`). Once the browser has confirmed it, waits up to `grace` for it to
    /// exit and stops it the way [`Browser::wait_or_kill`] does if it doesn't.
    ///
    /// The connection closes as the browser exits, so while closing, the end of the connection doesn't kill the browser
    /// (it would cut a profile's shutdown short); this decides instead, from the browser's answer. Without the browser's
    /// confirmation that it is shutting down (the connection is gone, the browser refused, or no answer came within
    /// `grace`) the browser may still be running and no longer see the fence, so it is killed at once; a lost
    /// connection fails the waiting call at once, so that takes no longer than the loss.
    ///
    /// # Errors
    /// [`BrowserError::CloseUnconfirmed`] when the browser was killed instead (or had been already, its connection
    /// lost), and [`BrowserError::Stop`] when the browser process can't be reaped.
    pub async fn close(self, grace: Duration) -> Result<(), BrowserError> {
        self.shared.closing.store(true, Ordering::SeqCst);
        let closing = tokio::time::timeout(grace, self.cdp.call(None, "Browser.close", Value::Null)).await;
        let Some(browser) = self.shared.take_browser() else {
            return Err(BrowserError::CloseUnconfirmed(
                "the fence had lost its connection".to_owned(),
            ));
        };
        let unconfirmed = match closing {
            Ok(Ok(_)) => return browser.wait_or_kill(grace).await.map(drop),
            Ok(Err(error)) => error.to_string(),
            Err(_) => format!("no answer within {grace:?}"),
        };
        browser.kill().await?;
        Err(BrowserError::CloseUnconfirmed(unconfirmed))
    }

    /// The connection, for driving the armed targets.
    #[must_use]
    pub fn cdp(&self) -> &Cdp {
        &self.cdp
    }

    /// A copy of every event of the connection from now on, in arrival order, for watching what the targets do. The
    /// fence handles the events whether or not anyone watches; a watcher only observes (answering a paused request or a
    /// held target is the fence's job), and the channel ends with the connection.
    #[must_use]
    pub fn watch(&self) -> mpsc::UnboundedReceiver<Event> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.shared
            .taps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sender);
        receiver
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

    /// Every WebSocket frame denied so far.
    #[must_use]
    pub fn frame_denials(&self) -> Vec<FrameDenial> {
        self.shared.ws.denials()
    }

    /// Every WebSocket frame the browser sent without an allow so far.
    #[must_use]
    pub fn unsanctioned_frames(&self) -> Vec<UnsanctionedFrame> {
        self.shared.ws.unsanctioned()
    }

    /// How many allowed WebSocket frames the browser hasn't reported sent yet ([`WsFence::allows_not_seen_sent`]).
    #[must_use]
    pub fn allows_not_seen_sent(&self) -> usize {
        self.shared.ws.allows_not_seen_sent()
    }

    /// The run's verdict so far: any denied request or frame, frame sent without an allow or unarmed target fails it,
    /// and so does a lost connection.
    ///
    /// # Errors
    /// [`RunFailed`] listing them.
    pub fn verdict(&self) -> Result<(), RunFailed> {
        let failed = RunFailed {
            denials: self.denials(),
            frame_denials: self.frame_denials(),
            unsanctioned: self.unsanctioned_frames(),
            failures: self.failures(),
            connection_lost: self.shared.lost(),
        };
        let clean = failed.denials.is_empty()
            && failed.frame_denials.is_empty()
            && failed.unsanctioned.is_empty()
            && failed.failures.is_empty()
            && !failed.connection_lost;
        if clean { Ok(()) } else { Err(failed) }
    }
}

/// Handles the connection's events until they end, which happens only when the connection does: the browser dropped
/// it, or the reader stopped on a message it couldn't read. Either way nothing is decided any more, so the run fails
/// and the browser is killed: a browser that no longer sees the client releases what the fence held, and one that
/// still does keeps its paused requests and held targets waiting forever.
async fn handle_events(cdp: Cdp, events: Events, shared: Arc<Shared>) {
    dispatch_events(cdp, events, &shared).await;
    shared.lost.store(true, Ordering::SeqCst);
    // while closing, `ArmedBrowser::close` kills it unless the browser confirmed it is shutting down
    if !shared.closing.load(Ordering::SeqCst) {
        shared.kill_browser();
    }
    shared.changed.notify_waiters();
}

async fn dispatch_events(cdp: Cdp, mut events: Events, shared: &Arc<Shared>) {
    while let Some(event) = events.recv().await {
        dispatch(&cdp, shared, event);
    }
}

/// Handles one event. The audit's events are handled here, in arrival order: an allow is recorded before the guard
/// hears of it, so the frame it lets out is always reported after it.
fn dispatch(cdp: &Cdp, shared: &Arc<Shared>, event: Event) {
    shared.tap(&event);
    match event.method.as_str() {
        "Fetch.requestPaused" => {
            let (cdp, shared) = (cdp.clone(), Arc::clone(shared));
            // a request whose target is gone can't be answered; it was never sent either
            tokio::spawn(async move {
                let _ = shared.http.handle(&cdp, &event).await;
            });
        }
        "Runtime.bindingCalled" => {
            let (cdp, shared) = (cdp.clone(), Arc::clone(shared));
            // a frame whose context is gone can't be answered; it was never sent either
            tokio::spawn(async move {
                let _ = shared.ws.handle_binding(&cdp, &event).await;
            });
        }
        "Network.webSocketCreated" | "Network.webSocketClosed" | "Network.webSocketFrameSent" => {
            shared.ws.observe(&event);
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
                drop(targets);
                shared.ws.forget(&session);
            }
        }
        _ => {}
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
            arm_target(&cdp, session, &kind, shared.ws.script())
                .await
                .map(|()| ArmedTarget {
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
