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
//! Every attached target is then armed by the auto-attach handler (`arm_target`, the only place the steps run) by its
//! type ([`TargetKind::steps`]): `Fetch.enable` with `*` on its own session (pages, iframes, shared and service workers, and any type we don't know), `Network.enable`
//! everywhere (the service-worker bypass on pages and iframes needs it, and so does the audit of WebSocket frames,
//! which Chrome reports on the session of the target that sent them), the service-worker bypass, the WebSocket guard
//! ([`crate::fence_ws`]), auto-attach with `waitForDebuggerOnStart` for its own children (iframes, dedicated workers),
//! and only then `Runtime.runIfWaitingForDebugger`. A dedicated worker has no `Fetch` domain of its own: its requests
//! pause on its page's session (and the browser's). When the run records ([`ArmedBrowser::arm_capturing`]), pages and
//! iframes also get the WebRTC hooks of [`crate::capture`] the same way as the guard, right after it.
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
//! If any step fails, or gets no answer within [`STEP_TIMEOUT`], the target is never resumed: it is closed, and if the
//! close isn't confirmed within [`STEP_TIMEOUT`] (Chrome 155 refuses to close a dedicated worker; closing an iframe
//! closes its whole tab) the browser is killed, its whole process group at once while this connection still holds the
//! target. The failure fails the run's verdict ([`ArmedBrowser::verdict`]) and goes to `fence.log` and the capture
//! ([`teams_fence::ArmingRecord`]). Whether it breached the fence is another matter
//! ([`ArmingFailure::contained_before_it_ran`]). A target held by `waitForDebuggerOnStart` stays held until it is
//! resumed, its parent's auto-attach is turned off, or the connection holding it drops; a refused close, a detach of
//! its session or its parent navigating don't release it (checked on Chrome 155, `tests/browser.rs`). The fence does
//! none of those to a target it gave up on, and a lost connection kills the browser and is a breach of its own. So a
//! held target contained (closed, or killed with the browser) before its resume ran nothing; one whose resume was
//! sent, one that wasn't held when it attached (it was running already), or one that could be neither closed nor
//! killed counts as a breach ([`crate::session::FenceReport::breached`]). The one exception is a session that is gone
//! before its arming finishes (the browser answers "Session with given id not found"): its tab closed, or its worker
//! ended. That session has nothing left to arm or resume, so it is dropped quietly; browser-level `Fetch` stays on
//! regardless.
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
//!
//! Each paused request and each guard request is decided in a task of its own, and a mapped write's decision waits on a
//! live membership read; each target is armed (and, failing that, contained and recorded) in a task of its own too. So
//! once the browser is down, [`ArmedBrowser::close`] waits (up to [`DECISION_GRACE`]) for the decisions and armings
//! still running, and [`ArmedBrowser::decisions`], which outlives the browser, then holds every deny and arming failure
//! of the run; [`Decisions::undecided`] counts any decision or arming that was still running when the wait ended.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use teams_fence::{ArmingCause, ArmingRecord, CloseOutcome, Containment};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;

use crate::browser::{Browser, BrowserError};
use crate::capture::{Capture, RTC_BINDING};
use crate::cdp::{Cdp, CdpError, Event, Events, SessionId};
use crate::fence_http::{Denial, ForcedDeny, Handled, HttpFence};
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
    /// An audio, paint, layout or animation worklet (`worklet`). Its global scope has no network API (no `fetch`,
    /// `XMLHttpRequest`, `WebSocket`, `EventSource` or `sendBeacon`) and its session no `Fetch`, `Network`, `Runtime` or
    /// `Target` domain; the module it runs is fetched by its document, through that document's fence. It is only
    /// resumed. (The web client's calling stack runs its audio in one, seen 2026-10-20.) Worklets that fetch by
    /// themselves have other types (`shared_storage_worklet`, `auction_worklet`) and are [`TargetKind::Other`].
    Worklet,
    /// Any other type (`browser_ui`, `webview`, other worklets, ones newer than this code); armed like a page would be,
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
            "worklet" => Self::Worklet,
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
                Step::CaptureBinding,
                Step::CaptureHooks,
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
            // nothing to arm: no network of its own, and none of the domains the other steps use
            Self::Worklet => &[Step::Resume],
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
    /// `Runtime.addBinding` for the WebRTC hooks' reports; only when the run records ([`crate::capture`]), else
    /// skipped.
    CaptureBinding,
    /// `Page.addScriptToEvaluateOnNewDocument` with `runImmediately`: the WebRTC hooks in every document of every frame
    /// of a page or iframe target, after the guard; only when the run records, else skipped.
    CaptureHooks,
    /// `Target.setAutoAttach` with `waitForDebuggerOnStart` on the target's session, for its children (service workers
    /// left out: they attach through the browser).
    AutoAttach,
    /// `Runtime.runIfWaitingForDebugger`: the target's code starts.
    Resume,
}

impl Step {
    /// The CDP method the step sends.
    #[must_use]
    pub fn method(self) -> &'static str {
        match self {
            Self::Fetch => "Fetch.enable",
            Self::Network => "Network.enable",
            Self::BypassServiceWorker => "Network.setBypassServiceWorker",
            Self::Runtime => "Runtime.enable",
            Self::Page => "Page.enable",
            Self::Binding | Self::CaptureBinding => "Runtime.addBinding",
            Self::GuardNewDocuments | Self::CaptureHooks => "Page.addScriptToEvaluateOnNewDocument",
            Self::Guard => "Runtime.evaluate",
            Self::AutoAttach => "Target.setAutoAttach",
            Self::Resume => "Runtime.runIfWaitingForDebugger",
        }
    }

    /// The step's parameters, or `None` when the run has nothing for it to do (the capture steps of a run that doesn't
    /// record).
    fn params(self, scripts: &Scripts<'_>) -> Option<Value> {
        let guard = scripts.guard;
        Some(match self {
            Self::Fetch => intercept_all(),
            Self::BypassServiceWorker => json!({"bypass": true}),
            Self::Binding => json!({"name": crate::fence_ws::BINDING}),
            Self::GuardNewDocuments => json!({"source": guard.source(), "runImmediately": true}),
            Self::Guard => json!({"expression": guard.source(), "returnByValue": true}),
            Self::CaptureBinding => {
                scripts.capture?;
                json!({"name": RTC_BINDING})
            }
            Self::CaptureHooks => json!({"source": scripts.capture?.hooks().source(), "runImmediately": true}),
            Self::AutoAttach => hold_children(),
            Self::Network | Self::Runtime | Self::Page | Self::Resume => Value::Null,
        })
    }

    /// Runs this step on a target's session.
    async fn run(self, cdp: &Cdp, session: &SessionId, scripts: &Scripts<'_>) -> Result<(), ArmError> {
        let Some(params) = self.params(scripts) else {
            return Ok(());
        };
        let call = cdp.call(Some(session), self.method(), params);
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

    /// Whether the target's resume was sent: the step that failed is [`Step::Resume`], so its code may be running.
    #[must_use]
    pub fn resume_sent(&self) -> bool {
        matches!(
            self,
            Self::Step { step: Step::Resume, .. } | Self::StepTimeout(Step::Resume)
        )
    }

    /// The arming step that failed, if a step did.
    #[must_use]
    pub fn step(&self) -> Option<Step> {
        match self {
            Self::Step { step, .. } | Self::StepTimeout(step) => Some(*step),
            Self::Guard(_) => Some(Step::Guard),
            _ => None,
        }
    }

    /// How arming failed, as `fence.log` records it. A step that failed because the connection was gone is
    /// [`ArmingCause::ConnectionGone`], not an answer of the browser's.
    #[must_use]
    pub fn cause(&self) -> ArmingCause {
        match self {
            Self::Step {
                source: CdpError::Closed,
                ..
            }
            | Self::Cdp(CdpError::Closed)
            | Self::ConnectionLost => ArmingCause::ConnectionGone,
            Self::StepTimeout(_) => ArmingCause::NoAnswer,
            Self::Guard(_) => ArmingCause::GuardNotArmed,
            Self::Malformed(_) => ArmingCause::Malformed,
            _ => ArmingCause::Error,
        }
    }
}

/// What arming installs in a target: the WebSocket guard, and the WebRTC hooks when the run records.
struct Scripts<'a> {
    guard: &'a GuardScript,
    capture: Option<&'a Capture>,
}

/// Runs a target's arming steps on its session, resuming it last. Stops at the first step that fails, so a target that
/// isn't fully armed is never resumed.
///
/// # Errors
/// [`ArmError::Step`] naming the step that failed, [`ArmError::StepTimeout`] naming one that got no answer, or
/// [`ArmError::Guard`] when the guard didn't report itself armed.
async fn arm_target(cdp: &Cdp, session: &SessionId, kind: &TargetKind, scripts: &Scripts<'_>) -> Result<(), ArmError> {
    for &step in kind.steps() {
        step.run(cdp, session, scripts).await?;
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

/// How long [`ArmedBrowser::close`] waits, once the browser is down, for the decisions and armings still running: a live
/// membership read takes up to [`teams_fence::FETCH_TIMEOUT`], and the rest of a decision is quick; a failed arming's
/// close waits up to [`STEP_TIMEOUT`] (and the steps of an arming fail at once on a connection that is gone).
pub const DECISION_GRACE: Duration = Duration::from_secs(teams_fence::FETCH_TIMEOUT.as_secs() + 5);

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

/// A target that couldn't be armed, and how it was contained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArmingFailure {
    /// Missing when the attach event didn't name one.
    pub target_id: Option<String>,
    /// `TargetInfo.type`.
    pub kind: String,
    pub error: String,
    /// The step that failed; `None` when the attach event was malformed.
    pub step: Option<Step>,
    pub cause: ArmingCause,
    /// The step that failed was the resume itself (`Runtime.runIfWaitingForDebugger`, sent and refused or unanswered),
    /// so the target's code may have started. Any other failure stops before the resume.
    pub resume_sent: bool,
    /// The attach event said `waitingForDebugger`: the target was held. One that wasn't was running already.
    pub held: bool,
    /// What `Target.closeTarget` gave.
    pub close: CloseOutcome,
    /// How it was kept from running: closed, or, when its close wasn't confirmed, killed with the whole browser.
    pub containment: Containment,
}

impl ArmingFailure {
    /// The failure as `fence.log` records it.
    #[must_use]
    pub fn record(&self) -> ArmingRecord<'_> {
        ArmingRecord {
            target_type: &self.kind,
            step: self.step.map(Step::method),
            cause: self.cause,
            resume_sent: self.resume_sent,
            held: self.held,
            close: self.close,
            containment: self.containment,
        }
    }

    /// Whether the target's code may have started: its resume was sent, or it wasn't held when it attached.
    #[must_use]
    pub fn may_have_run(&self) -> bool {
        self.record().may_have_run()
    }

    /// Whether the target was contained before any of its code ran: it attached held, arming gave up on it before the
    /// resume, and it was closed, or killed with the browser. A held target stays held until it is resumed, its
    /// parent's auto-attach is turned off or the connection holding it drops (checked on Chrome 155: not when its
    /// close is refused, its session detached or its parent navigates), and the fence does none of these to it; so
    /// nothing of it ran unguarded, and it is no breach. What it would have done (an iframe's content, a worker's job)
    /// just doesn't happen, and a run that needed it fails on its own steps.
    #[must_use]
    pub fn contained_before_it_ran(&self) -> bool {
        !self.record().breach()
    }

    /// Whether the browser was killed to contain it.
    #[must_use]
    pub fn browser_killed(&self) -> bool {
        self.containment == Containment::BrowserKilled
    }
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
    /// What the run records, if it does.
    capture: Option<Capture>,
    targets: Mutex<Targets>,
    changed: Notify,
    /// The browser, until the fence is closed or dropped, or its connection is lost.
    browser: Mutex<Option<Browser>>,
    /// The connection's events ended, for whatever reason.
    lost: AtomicBool,
    /// The connection's events ended while the fence was up, before [`ArmedBrowser::close`] began: nothing was decided
    /// after that. (A connection that ends while closing is the close's business: it fails unless the browser
    /// confirmed.)
    broken: AtomicBool,
    /// [`ArmedBrowser::close`] has asked the browser to close, and decides from its answer whether to kill it.
    closing: AtomicBool,
    /// The browser was sent `SIGKILL` to contain a target that couldn't be armed or closed ([`Shared::contain`]), and
    /// the connection's reader hadn't seen the connection end by then: its end after that is expected. Set and read
    /// under the [`Shared::browser`] lock. (A socket that dropped just before the signal, its end not read yet, still
    /// counts as after: an accepted risk, `docs/backlog/containment-kill-cannot-order-an-unread-eof.md`.)
    contained: AtomicBool,
    /// How that containment ended, once it has; every target that needs it waits for the one kill.
    containment: tokio::sync::OnceCell<Containment>,
    /// How many decisions (of a paused request or a guard request) and target armings are still running, each in a
    /// task of its own.
    deciding: AtomicUsize,
    /// Notified when the last decision running finishes.
    settled: Notify,
    /// Where a copy of every event goes ([`ArmedBrowser::watch`]).
    taps: Mutex<Vec<mpsc::UnboundedSender<Event>>>,
}

impl Shared {
    fn targets(&self) -> MutexGuard<'_, Targets> {
        self.targets.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn browser(&self) -> MutexGuard<'_, Option<Browser>> {
        self.browser.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn take_browser(&self) -> Option<Browser> {
        self.browser().take()
    }

    /// Kills the browser to contain a target that couldn't be armed and whose close wasn't confirmed, once for the
    /// run: every caller gets the outcome of the one kill.
    ///
    /// The whole process group is sent `SIGKILL` while the fence's connection is still open as far as the fence knows,
    /// so the browser shouldn't see that connection drop (which would release every target it holds) while it can
    /// still act. What is checked is that the connection's reader hadn't seen the connection end
    /// ([`Cdp::is_closed`]) when the group was signalled; only then is the connection's end marked as expected, and
    /// the browser reaped then makes it [`Containment::BrowserKilled`]. A browser already gone (the fence is closing,
    /// or lost its connection), a connection already seen closed (the browser is still killed, and the loss stays a
    /// breach of its own), a group that couldn't be signalled or a browser that couldn't be reaped is
    /// [`Containment::Failed`].
    ///
    /// The check can't order an end the reader hasn't read yet against the signal: a socket Chrome dropped (or that
    /// broke) just before the kill, its EOF still unread, is classified as contained, though the drop came first and
    /// could have released a held target. Once the kill is sent the connection always ends, so nothing in userspace
    /// tells the two apart; this is an accepted risk (`docs/backlog/containment-kill-cannot-order-an-unread-eof.md`).
    async fn contain(&self, cdp: &Cdp) -> Containment {
        *self.containment.get_or_init(|| self.kill_to_contain(cdp)).await
    }

    async fn kill_to_contain(&self, cdp: &Cdp) -> Containment {
        let browser = {
            let mut slot = self.browser();
            let Some(browser) = slot.take() else {
                return Containment::Failed;
            };
            // read before the kill: a connection seen ending first may have let the held targets go (an end not read
            // yet can't be seen here; see the doc above)
            let lost_first = cdp.is_closed();
            if browser.start_kill().is_err() || lost_first {
                return Containment::Failed;
            }
            self.contained.store(true, Ordering::SeqCst);
            browser
        };
        match browser.reap().await {
            Ok(_) => Containment::BrowserKilled,
            Err(_) => Containment::Failed,
        }
    }

    /// Kills the browser and every process it started, unless it was taken already.
    fn kill_browser(&self) {
        drop(self.take_browser());
    }

    fn lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    fn taps(&self) -> MutexGuard<'_, Vec<mpsc::UnboundedSender<Event>>> {
        self.taps.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hands a copy of `event` to every watcher still listening.
    fn tap(&self, event: &Event) {
        self.taps().retain(|tap| tap.send(event.clone()).is_ok());
    }

    /// Marks the connection lost (and broken, unless the fence is closing or killed the browser to contain a target)
    /// and ends every watch: their senders go, under the same lock [`ArmedBrowser::watch`] checks the mark with, so no
    /// watch outlives the connection.
    fn mark_lost(&self) {
        let mut taps = self.taps();
        self.lost.store(true, Ordering::SeqCst);
        // under the browser's lock: a containment kill signals and sets its mark under it, so an end its own signal
        // caused is read only after the mark
        let expected = {
            let _browser = self.browser();
            self.closing.load(Ordering::SeqCst) || self.contained.load(Ordering::SeqCst)
        };
        if !expected {
            self.broken.store(true, Ordering::SeqCst);
        }
        taps.clear();
    }

    fn deciding(&self) -> usize {
        self.deciding.load(Ordering::SeqCst)
    }

    /// Waits up to `timeout` for every decision still running to finish.
    async fn settle(&self, timeout: Duration) {
        let wait = async {
            loop {
                let settled = self.settled.notified();
                if self.deciding() == 0 {
                    return;
                }
                settled.await;
            }
        };
        // what is still running after that is counted by `Decisions::undecided`
        let _ = tokio::time::timeout(timeout, wait).await;
    }

    fn verdict(&self) -> Result<(), RunFailed> {
        let failed = RunFailed {
            denials: self.http.denials(),
            frame_denials: self.ws.denials(),
            unsanctioned: self.ws.unsanctioned(),
            failures: self.targets().failures.clone(),
            connection_lost: self.broken.load(Ordering::SeqCst),
        };
        let clean = failed.denials.is_empty()
            && failed.frame_denials.is_empty()
            && failed.unsanctioned.is_empty()
            && failed.failures.is_empty()
            && !failed.connection_lost;
        if clean { Ok(()) } else { Err(failed) }
    }
}

/// One decision running, counted in [`Shared::deciding`] and in [`RUNNING_ANYWHERE`] from before its task is spawned
/// until the task ends (or is dropped unfinished).
struct Deciding(Arc<Shared>);

impl Deciding {
    fn start(shared: &Arc<Shared>) -> Self {
        RUNNING_ANYWHERE.fetch_add(1, Ordering::SeqCst);
        shared.deciding.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(shared))
    }
}

impl Drop for Deciding {
    fn drop(&mut self) {
        if self.0.deciding.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.settled.notify_waiters();
        }
        if RUNNING_ANYWHERE.fetch_sub(1, Ordering::SeqCst) == 1 {
            SETTLED_ANYWHERE.notify_waiters();
        }
    }
}

/// How many decisions and target armings are running in every fence of the process, dropped ones' included.
static RUNNING_ANYWHERE: AtomicUsize = AtomicUsize::new(0);

/// Notified when the last decision running anywhere finishes.
static SETTLED_ANYWHERE: Notify = Notify::const_new();

/// How many decisions and target armings are running in every fence of the process, dropped ones' included.
#[must_use]
pub fn running_anywhere() -> usize {
    RUNNING_ANYWHERE.load(Ordering::SeqCst)
}

/// Waits up to `timeout` for the decisions and target armings still running in every fence of the process, and gives
/// how many still were when the wait ended: what [`ArmedBrowser::close`] waits for, for the fences that were dropped
/// instead of closed (`proto-lab` stopped by a signal, [`crate::stop`]).
///
/// A dropped fence's browser was killed first, so a decision that finishes after it answers nobody: it is only
/// recorded (`fence.log`, the capture, [`Decisions`]). An arming that finishes after it can't confirm the kill
/// contained its target and is recorded as [`Containment::Failed`], as one cut off by [`ArmedBrowser::close`] is.
/// The runtime's other tasks have to run meanwhile, so this is awaited on the runtime the fences ran on.
pub async fn settle_every_fence(timeout: Duration) -> usize {
    let wait = async {
        loop {
            let settled = SETTLED_ANYWHERE.notified();
            if running_anywhere() == 0 {
                return;
            }
            settled.await;
        }
    };
    let _ = tokio::time::timeout(timeout, wait).await;
    running_anywhere()
}

/// What the fence decided in a run, read live: it outlives the [`ArmedBrowser`], so a run's denials can be read after
/// [`ArmedBrowser::close`] has waited for the decisions still running.
#[derive(Clone)]
pub struct Decisions(Arc<Shared>);

impl Decisions {
    /// Every request denied so far.
    #[must_use]
    pub fn denials(&self) -> Vec<Denial> {
        self.0.http.denials()
    }

    /// Every WebSocket frame denied so far.
    #[must_use]
    pub fn frame_denials(&self) -> Vec<FrameDenial> {
        self.0.ws.denials()
    }

    /// Every WebSocket frame the browser sent without an allow so far.
    #[must_use]
    pub fn unsanctioned_frames(&self) -> Vec<UnsanctionedFrame> {
        self.0.ws.unsanctioned()
    }

    /// Every target that couldn't be armed.
    #[must_use]
    pub fn failures(&self) -> Vec<ArmingFailure> {
        self.0.targets().failures.clone()
    }

    /// How many decisions and target armings are still running: after [`ArmedBrowser::close`], the ones its wait gave up
    /// on, whose denies or failures may be missing from [`Self::denials`], [`Self::frame_denials`] and
    /// [`Self::failures`].
    #[must_use]
    pub fn undecided(&self) -> usize {
        self.0.deciding()
    }

    /// The run's verdict so far, as [`ArmedBrowser::verdict`] gives it; a connection that ends during
    /// [`ArmedBrowser::close`] doesn't fail it.
    ///
    /// # Errors
    /// [`RunFailed`] listing what failed it.
    pub fn verdict(&self) -> Result<(), RunFailed> {
        self.0.verdict()
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
        Self::arm_inner(browser, http, ws, options, None).await
    }

    /// [`Self::arm`], recording the run into `capture` (see [`crate::capture`]): every page and iframe target also
    /// gets the WebRTC hooks before it runs, and the capture sees the connection's `Network` and target events, the
    /// hooks' reports and every verdict of the fence.
    ///
    /// # Errors
    /// As [`Self::arm`].
    pub async fn arm_capturing(
        browser: Browser,
        http: HttpFence,
        ws: WsFence,
        options: &ArmOptions,
        capture: Capture,
    ) -> Result<Self, ArmError> {
        Self::arm_inner(browser, http, ws, options, Some(capture)).await
    }

    async fn arm_inner(
        browser: Browser,
        http: HttpFence,
        ws: WsFence,
        options: &ArmOptions,
        capture: Option<Capture>,
    ) -> Result<Self, ArmError> {
        let (cdp, events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
        // every request; with a redirect catch, also the login origin's responses (`crate::fence_http`)
        cdp.call(None, "Fetch.enable", http.fetch_patterns()).await?;
        let shared = Arc::new(Shared {
            http,
            ws,
            capture,
            targets: Mutex::new(Targets::default()),
            changed: Notify::new(),
            browser: Mutex::new(Some(browser)),
            lost: AtomicBool::new(false),
            broken: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            contained: AtomicBool::new(false),
            containment: tokio::sync::OnceCell::new(),
            deciding: AtomicUsize::new(0),
            settled: Notify::new(),
            taps: Mutex::new(Vec::new()),
        });
        let task = tokio::spawn(handle_events(cdp.clone(), events, Arc::clone(&shared)));
        // from here on, an early return drops `armed`, and that kills the browser
        let armed = Self { cdp, shared, task };
        armed.cdp.call(None, "Target.setAutoAttach", hold_new_targets()).await?;
        let blank = armed.open_page(SETUP_TIMEOUT).await?;
        neutralize_service_workers(&armed.cdp, &blank.session, &options.service_worker_origins).await?;
        if close_target(&armed.cdp, &blank.target_id).await != CloseOutcome::Closed {
            return Err(ArmError::SetupNotClosed);
        }
        Ok(armed)
    }

    /// Asks the browser to close (`Browser.close`). Once the browser has confirmed it, waits up to `grace` for it to
    /// exit and stops it the way [`Browser::wait_or_kill`] does if it doesn't. Then, with the browser down either way,
    /// waits up to [`DECISION_GRACE`] for the decisions still running, so that [`Self::decisions`] holds them.
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
        let closed = self.shut(grace).await;
        self.shared.settle(DECISION_GRACE).await;
        closed
    }

    async fn shut(&self, grace: Duration) -> Result<(), BrowserError> {
        self.shared.closing.store(true, Ordering::SeqCst);
        let closing = tokio::time::timeout(grace, self.cdp.call(None, "Browser.close", Value::Null)).await;
        let Some(browser) = self.shared.take_browser() else {
            let why = if self.shared.contained.load(Ordering::SeqCst) {
                "the browser had been killed to contain a target not armed in time"
            } else {
                "the fence had lost its connection"
            };
            return Err(BrowserError::CloseUnconfirmed(why.to_owned()));
        };
        let unconfirmed = match closing {
            Ok(Ok(_)) => return browser.wait_or_kill(grace).await.map(drop),
            Ok(Err(error)) => error.to_string(),
            Err(_) => format!("no answer within {grace:?}"),
        };
        browser.kill().await?;
        Err(BrowserError::CloseUnconfirmed(unconfirmed))
    }

    /// What the fence decides in this run, readable after [`Self::close`] too.
    #[must_use]
    pub fn decisions(&self) -> Decisions {
        Decisions(Arc::clone(&self.shared))
    }

    /// The connection, for driving the armed targets.
    #[must_use]
    pub fn cdp(&self) -> &Cdp {
        &self.cdp
    }

    /// A copy of every event of the connection from now on, in arrival order, for watching what the targets do. The
    /// fence handles the events whether or not anyone watches; a watcher only observes (answering a paused request or a
    /// held target is the fence's job), and the channel ends with the connection: once it is gone, the receiver reports
    /// the channel closed after the last event, and a watch asked for after that is closed from the start.
    #[must_use]
    pub fn watch(&self) -> mpsc::UnboundedReceiver<Event> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut taps = self.shared.taps();
        if !self.shared.lost() {
            taps.push(sender);
        }
        receiver
    }

    /// What the run records, if it does.
    #[must_use]
    pub fn capture(&self) -> Option<&Capture> {
        self.shared.capture.as_ref()
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

    /// Denies every request `rule` matches from now on ([`HttpFence::force_deny`]).
    pub fn force_deny(&self, rule: ForcedDeny) {
        self.shared.http.force_deny(rule);
    }

    /// Stops every forced deny ([`HttpFence::clear_forced`]).
    pub fn clear_forced(&self) {
        self.shared.http.clear_forced();
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
        self.shared.verdict()
    }
}

/// Handles the connection's events until they end, which happens only when the connection does: the browser dropped
/// it, or the reader stopped on a message it couldn't read. Either way nothing is decided any more, so the run fails
/// and the browser is killed: a browser that no longer sees the client releases what the fence held, and one that
/// still does keeps its paused requests and held targets waiting forever.
async fn handle_events(cdp: Cdp, events: Events, shared: Arc<Shared>) {
    dispatch_events(cdp, events, &shared).await;
    shared.mark_lost();
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
    if let Some(capture) = &shared.capture {
        capture.observe(cdp, &event);
    }
    match event.method.as_str() {
        "Fetch.requestPaused" => {
            let (cdp, deciding) = (cdp.clone(), Deciding::start(shared));
            // a request whose target is gone can't be answered; it was never sent either
            tokio::spawn(async move {
                let shared = &deciding.0;
                let handled = shared.http.handle(&cdp, &event).await;
                // only a decided request is a verdict; a caught redirect or a continued login response isn't recorded
                if let (Some(capture), Ok(Handled::Decided(verdict))) = (&shared.capture, handled) {
                    capture.http_verdict(&event, verdict);
                }
            });
        }
        "Runtime.bindingCalled" => {
            let (cdp, deciding) = (cdp.clone(), Deciding::start(shared));
            // a frame whose context is gone can't be answered; it was never sent either
            tokio::spawn(async move {
                let shared = &deciding.0;
                let verdict = shared.ws.handle_binding(&cdp, &event).await;
                if let (Some(capture), Ok(Some(verdict))) = (&shared.capture, verdict) {
                    capture.ws_verdict(&event, shared.ws.script(), verdict);
                }
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
            // counted like a decision, so that closing waits for a failure's containment and its record
            let (cdp, deciding) = (cdp.clone(), Deciding::start(shared));
            tokio::spawn(async move {
                let shared = Arc::clone(&deciding.0);
                on_attached(cdp, shared, event).await;
                drop(deciding);
            });
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
    let held = event.params["waitingForDebugger"] == json!(true);
    let outcome = match (&target_id, &session) {
        (Some(target_id), Some(session)) => {
            let kind = TargetKind::from_type(&kind_name);
            let scripts = Scripts {
                guard: shared.ws.script(),
                capture: shared.capture.as_ref(),
            };
            arm_target(&cdp, session, &kind, &scripts).await.map(|()| ArmedTarget {
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
            let failure = contain_unarmed(&cdp, &shared, target_id, kind_name, held, &error).await;
            record_failure(&shared, session.as_ref(), failure);
        }
    }
    shared.changed.notify_waiters();
}

/// Closes a target that couldn't be armed, and kills the browser when the close isn't confirmed (a refused, failed or
/// unanswered close, or no target id): a target that can't be closed is contained only by ending the browser it is held
/// in ([`Shared::contain`]). Chrome 155 refuses to close a dedicated worker, and closing an iframe closes its whole tab.
async fn contain_unarmed(
    cdp: &Cdp,
    shared: &Shared,
    target_id: Option<String>,
    kind: String,
    held: bool,
    error: &ArmError,
) -> ArmingFailure {
    let close = match &target_id {
        Some(id) => close_target(cdp, id).await,
        None => CloseOutcome::NoTarget,
    };
    let containment = if close == CloseOutcome::Closed {
        Containment::Closed
    } else {
        shared.contain(cdp).await
    };
    ArmingFailure {
        target_id,
        kind,
        error: error.to_string(),
        step: error.step(),
        cause: error.cause(),
        resume_sent: error.resume_sent(),
        held,
        close,
        containment,
    }
}

/// Records a target that couldn't be armed in `fence.log` and the capture, then among the run's failures.
fn record_failure(shared: &Shared, session: Option<&SessionId>, failure: ArmingFailure) {
    // the failure fails the run's verdict whether or not the log takes it
    let _ = shared.http.log().record_arming(&failure.record());
    if let Some(capture) = &shared.capture {
        capture.arming_failure(session, &failure);
    }
    shared.targets().failures.push(failure);
}

/// Drops a session that won't be armed from the attached set: if its target can't be closed, no detach may come.
fn forget(shared: &Shared, session: Option<&SessionId>) {
    if let Some(session) = session {
        shared.targets().attached.remove(session);
    }
}

/// Asks the browser to close a target, waiting up to [`STEP_TIMEOUT`] for the answer. A connection gone before the
/// answer is [`CloseOutcome::ConnectionGone`]: the browser never answered.
async fn close_target(cdp: &Cdp, target_id: &str) -> CloseOutcome {
    let call = cdp.call(None, "Target.closeTarget", json!({"targetId": target_id}));
    match tokio::time::timeout(STEP_TIMEOUT, call).await {
        Err(_) => CloseOutcome::Unanswered,
        Ok(Err(CdpError::Closed)) => CloseOutcome::ConnectionGone,
        Ok(Ok(result)) if result["success"] != json!(false) => CloseOutcome::Closed,
        Ok(_) => CloseOutcome::Refused,
    }
}
