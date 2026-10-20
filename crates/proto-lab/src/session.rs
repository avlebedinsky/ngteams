//! A lab account's web session: the Teams web client opened in the account's persistent profile under the armed
//! fence, and what its events say about the session.
//!
//! [`SessionWatch`] reads the connection's events (from [`crate::arming::ArmedBrowser::watch`]) and tells:
//! - **loaded**: the page's main frame is on Teams and the web client's authz exchange (its access token for a
//!   skypetoken, an own-state request the fence lets through) succeeded in that document, which the client does only
//!   with a working session;
//! - **refused**: the main frame is on Teams, but Teams forbade the exchange (403) and hasn't allowed one since: a
//!   signed-in account Teams won't serve, such as one without a Teams license (the web client then offers a trial). An
//!   expired token (401) isn't a refusal: the client refreshes it and asks again. [`Settling`] gives a refusal
//!   [`REFUSAL_GRACE`] to turn into a session before it is believed ([`LabSession::settle`] and `login` both use it);
//! - **sign-in shown**: the main frame finished loading on a Microsoft sign-in host. The sign-in host also passes a
//!   signed-in browser straight back with a redirect, which never commits a document there, so a page that stays
//!   means the session has expired (or was never there);
//! - **elsewhere**: the main frame finished loading on any other host;
//! - and who the page is signed in as ([`Identity`]): the claims of the bearer tokens its requests carry. Only the
//!   object id, tenant and UPN claims are kept; the token itself never leaves the event.
//!
//! [`LabSession`] runs one: the pinned Chrome on the account's profile, headless unless asked otherwise, one armed tab,
//! and the events watched. By default it is armed fail-closed (`FailClosed`: every write whose target has to be
//! fetched is denied); a run that writes ([`SessionOptions::live_members`]) reads its targets' members live instead
//! (`LiveMembershipSource`, fed the account's chat service credential from the page's own requests). A run can also be
//! recorded as a capture ([`SessionOptions::capture`]).

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use teams_fence::{FailClosed, Fence, LiveMembershipSource, LiveSourceError, MembershipSource, OpenError};
use tokio::sync::mpsc;
use url::Url;

use crate::arming::{ArmError, ArmOptions, ArmedBrowser, ArmingFailure, DECISION_GRACE, Decisions, TEAMS_ORIGINS};
use crate::browser::{Browser, BrowserError, Display, LaunchOptions};
use crate::capture::{Capture, CaptureError, Recorder, RtcHooks};
use crate::cdp::{Cdp, CdpError, Event, SessionId};
use crate::classify::{Classification, Classifier, OwnKind};
use crate::commands::chrome::ChromeBuild;
use crate::config::{Alias, LabPaths};
use crate::fence_http::{Denial, ForcedDeny, HttpFence};
use crate::fence_ws::{FrameClassifier, FrameDenial, GuardScript, WsFence};
use crate::scenario::dsl::Driver;

/// Where the web client starts.
pub const TEAMS_URL: &str = "https://teams.microsoft.com/v2/";

/// The hosts of Microsoft's sign-in pages.
pub const SIGN_IN_HOSTS: [&str; 5] = [
    "login.microsoftonline.com",
    "login.microsoft.com",
    "login.live.com",
    "device.login.microsoftonline.com",
    "autologon.microsoftazuread-sso.com",
];

/// The authz answer that means Teams won't serve the signed-in account.
const FORBIDDEN: u16 = 403;

/// How long a refusal is given to turn into a session (a retry that works) before it is believed.
pub const REFUSAL_GRACE: Duration = Duration::from_secs(5);

/// Where the session is, as far as the events tell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Nothing decided yet.
    Loading,
    /// The web client is on Teams with a working session.
    Loaded,
    /// Signed in, but Teams refused the account: its authz exchange got this status (403).
    Refused(u16),
    /// A Microsoft sign-in page stands in the main frame: the session has expired.
    SignInShown,
    /// The main frame finished loading on this other host.
    Elsewhere(String),
}

/// Who a page is signed in as: claims of a bearer token it sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The Entra object id (`oid`).
    pub oid: String,
    /// `upn`, else `preferred_username`, else `unique_name`.
    pub upn: String,
    /// `tid`.
    pub tenant: String,
}

/// An object id that isn't a GUID, so no MRI can be made of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the object id isn't a GUID")]
pub struct NotAGuid;

impl Identity {
    /// The account's MRI as Teams writes it: `8:orgid:` and the object id, lowercase.
    ///
    /// # Errors
    /// [`NotAGuid`] when the object id isn't shaped like a GUID.
    pub fn mri(&self) -> Result<String, NotAGuid> {
        let oid = self.oid.to_ascii_lowercase();
        let groups: Vec<&str> = oid.split('-').collect();
        let shaped = groups.len() == 5
            && groups
                .iter()
                .zip([8, 4, 4, 4, 12])
                .all(|(group, length)| group.len() == length && group.bytes().all(|byte| byte.is_ascii_hexdigit()));
        if shaped {
            Ok(format!("8:orgid:{oid}"))
        } else {
            Err(NotAGuid)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Place {
    /// `about:blank`, or no main-frame document yet.
    Blank,
    Teams,
    SignIn,
    Other(String),
}

impl Place {
    fn of(url: &str) -> Self {
        let Ok(url) = Url::parse(url) else {
            return Self::Blank;
        };
        let origin = url.origin().ascii_serialization();
        match url.host_str() {
            None => Self::Blank,
            Some(_) if TEAMS_ORIGINS.contains(&origin.as_str()) => Self::Teams,
            Some(host) if url.scheme() == "https" && SIGN_IN_HOSTS.contains(&host) => Self::SignIn,
            Some(host) => Self::Other(host.to_owned()),
        }
    }
}

/// Tells from a page's events where its session is (see the module docs).
#[derive(Debug)]
pub struct SessionWatch {
    page: SessionId,
    main_frame: Option<String>,
    place: Place,
    /// The main frame finished loading since its last navigation.
    stopped: bool,
    /// An authz exchange succeeded since the main frame's last navigation.
    authz_ok: bool,
    /// Teams forbade an authz exchange since the main frame's last navigation.
    authz_forbidden: bool,
    /// The status of the last answer to an authz exchange.
    last_authz: Option<u16>,
    /// The method of every authz request seen since the main frame's last navigation, by session and request id. An
    /// answer to a request from before it belongs to the document that went away, and doesn't count.
    authz_requests: HashMap<(Option<SessionId>, String), String>,
    /// How many documents the main frame has committed.
    documents: u64,
    identities: Vec<Identity>,
}

impl SessionWatch {
    /// A watch on the tab whose session is `page`.
    #[must_use]
    pub fn new(page: SessionId) -> Self {
        Self {
            page,
            main_frame: None,
            place: Place::Blank,
            stopped: false,
            authz_ok: false,
            authz_forbidden: false,
            last_authz: None,
            authz_requests: HashMap::new(),
            documents: 0,
            identities: Vec::new(),
        }
    }

    /// Takes one event into account.
    pub fn observe(&mut self, event: &Event) {
        match event.method.as_str() {
            "Page.frameNavigated" if self.on_page(event) => self.navigated(&event.params["frame"]),
            "Page.frameStoppedLoading" if self.on_page(event) => {
                if self
                    .main_frame
                    .as_deref()
                    .is_some_and(|main| event.params["frameId"] == main)
                {
                    self.stopped = true;
                }
            }
            "Network.requestWillBeSent" | "Fetch.requestPaused" => self.request(event),
            "Network.responseReceived" => self.response(event),
            _ => {}
        }
    }

    fn on_page(&self, event: &Event) -> bool {
        event.session_id.as_ref() == Some(&self.page)
    }

    fn navigated(&mut self, frame: &Value) {
        if frame.get("parentId").is_some() {
            return;
        }
        self.main_frame = frame["id"].as_str().map(str::to_owned);
        self.place = Place::of(frame["url"].as_str().unwrap_or_default());
        self.stopped = false;
        self.authz_ok = false;
        self.authz_forbidden = false;
        self.authz_requests.clear();
        self.documents += 1;
    }

    fn request(&mut self, event: &Event) {
        let request = &event.params["request"];
        if let Some(identity) = identity_in(&request["headers"])
            && !self.identities.contains(&identity)
        {
            self.identities.push(identity);
        }
        // `Fetch.requestPaused` ids are the fence's, not the network's; the responses carry the network's
        if event.method == "Network.requestWillBeSent"
            && let (Some(id), Some(url), Some(method)) = (
                event.params["requestId"].as_str(),
                request["url"].as_str(),
                request["method"].as_str(),
            )
            && is_authz(url)
        {
            let key = (event.session_id.clone(), id.to_owned());
            self.authz_requests.insert(key, method.to_ascii_uppercase());
        }
    }

    fn response(&mut self, event: &Event) {
        let Some(id) = event.params["requestId"].as_str() else {
            return;
        };
        let key = (event.session_id.clone(), id.to_owned());
        if self.authz_requests.get(&key).is_none_or(|method| method != "POST") {
            return;
        }
        let Some(Ok(status)) = event.params["response"]["status"].as_u64().map(u16::try_from) else {
            return;
        };
        self.last_authz = Some(status);
        match status {
            200..=299 => self.authz_ok = true,
            FORBIDDEN => self.authz_forbidden = true,
            _ => {}
        }
    }

    /// Where the session is now.
    #[must_use]
    pub fn state(&self) -> SessionState {
        match &self.place {
            Place::Teams if self.authz_ok => SessionState::Loaded,
            Place::Teams if self.authz_forbidden => SessionState::Refused(FORBIDDEN),
            Place::SignIn if self.stopped => SessionState::SignInShown,
            Place::Other(host) if self.stopped => SessionState::Elsewhere(host.clone()),
            _ => SessionState::Loading,
        }
    }

    /// Which document the main frame is on: it counts the main frame's navigations (`0` before the first), and goes up
    /// whenever a new document commits there, a reload of the same URL included. Changes within a document (a history
    /// push, the page changing its content) leave it alone.
    #[must_use]
    pub fn document(&self) -> u64 {
        self.documents
    }

    /// The status of the last answer to the web client's authz exchange, for a report on a session that didn't load.
    #[must_use]
    pub fn last_authz_status(&self) -> Option<u16> {
        self.last_authz
    }

    /// The identity the page has shown for `upn` (case aside). `None` until a request carried it, and when tokens for
    /// that UPN named more than one object id or tenant.
    #[must_use]
    pub fn identity_for(&self, upn: &str) -> Option<&Identity> {
        let mut matching = self
            .identities
            .iter()
            .filter(|identity| identity.upn.eq_ignore_ascii_case(upn));
        let first = matching.next()?;
        matching
            .all(|other| other.oid == first.oid && other.tenant == first.tenant)
            .then_some(first)
    }
}

/// Holds a refusal back until it has stood for [`REFUSAL_GRACE`]: until then the client may still retry its way into a
/// session.
#[derive(Clone, Copy, Debug, Default)]
pub struct Settling {
    refused_since: Option<tokio::time::Instant>,
}

impl Settling {
    /// What `state`, seen at `now`, counts as: [`SessionState::Loading`] for a refusal that hasn't stood unbroken for
    /// [`REFUSAL_GRACE`] since it was first seen, `state` itself otherwise. Any other state ends the refusal's stand.
    pub fn judge(&mut self, state: SessionState, now: tokio::time::Instant) -> SessionState {
        let SessionState::Refused(_) = state else {
            self.refused_since = None;
            return state;
        };
        let since = *self.refused_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= REFUSAL_GRACE {
            state
        } else {
            SessionState::Loading
        }
    }
}

/// Whether `url` is one of Teams' authz endpoints.
fn is_authz(url: &str) -> bool {
    Classifier::seeded().classify("POST", url, None) == Classification::Own(OwnKind::Authz)
}

/// The identity in a request's `Authorization: Bearer` token, if it is a JWT with the claims.
fn identity_in(headers: &Value) -> Option<Identity> {
    let value = headers
        .as_object()?
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))?
        .1
        .as_str()?;
    let (scheme, token) = value.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let mut parts = token.trim().split('.');
    let (_, payload, _, None) = (parts.next()?, parts.next()?, parts.next()?, parts.next()) else {
        return None;
    };
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    let text = |name: &str| claims[name].as_str().map(str::to_owned);
    Some(Identity {
        oid: text("oid")?,
        upn: text("upn")
            .or_else(|| text("preferred_username"))
            .or_else(|| text("unique_name"))?,
        tenant: text("tid")?,
    })
}

/// What the fence did during a run. Denies are expected while it runs fail-closed; a breach is not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FenceReport {
    /// Denied requests by host.
    pub denied_requests: BTreeMap<String, usize>,
    pub denied_frames: usize,
    /// Frames the browser sent without an allow.
    pub unsanctioned_frames: usize,
    /// Targets that couldn't be armed (and were closed).
    pub unarmed: Vec<ArmingFailure>,
    /// The fence lost its connection to the browser.
    pub connection_lost: bool,
}

impl FenceReport {
    /// What `decisions` holds so far.
    #[must_use]
    pub fn of(decisions: &Decisions) -> Self {
        let mut denied_requests = BTreeMap::new();
        for denial in decisions.denials() {
            let host = Url::parse(&denial.url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_else(|| "?".to_owned());
            *denied_requests.entry(host).or_default() += 1;
        }
        let failed = decisions.verdict().err();
        Self {
            denied_requests,
            denied_frames: decisions.frame_denials().len(),
            unsanctioned_frames: decisions.unsanctioned_frames().len(),
            unarmed: decisions.failures(),
            connection_lost: failed.is_some_and(|failed| failed.connection_lost),
        }
    }

    /// How many requests were denied.
    #[must_use]
    pub fn denied(&self) -> usize {
        self.denied_requests.values().sum()
    }

    /// Whether anything got past the fence or escaped it: a frame sent without an allow, a target that couldn't be
    /// armed, a lost connection. Denies are not breaches.
    #[must_use]
    pub fn breached(&self) -> bool {
        self.unsanctioned_frames > 0 || !self.unarmed.is_empty() || self.connection_lost
    }

    /// One line for the operator: the denies by host, then anything worse.
    #[must_use]
    pub fn summary(&self) -> String {
        let hosts: Vec<String> = self
            .denied_requests
            .iter()
            .map(|(host, count)| format!("{host} {count}"))
            .collect();
        let mut line = format!(
            "fence denied {} request(s) [{}] and {} frame(s)",
            self.denied(),
            hosts.join(", "),
            self.denied_frames
        );
        if self.unsanctioned_frames > 0 {
            let _ = write!(line, "; {} frame(s) sent without an allow", self.unsanctioned_frames);
        }
        for failure in &self.unarmed {
            let _ = write!(line, "; a {} target not armed ({})", failure.kind, failure.error);
        }
        if self.connection_lost {
            line.push_str("; the fence lost its connection");
        }
        line
    }
}

/// Why a lab session couldn't run.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("no Chrome for Testing at {} (run `proto-lab chrome install`)", .0.display())]
    NoChrome(std::path::PathBuf),
    #[error("can't open the lab's fence: {0}")]
    Fence(#[from] OpenError),
    #[error("can't make the WebSocket guard: {0}")]
    Guard(#[source] std::io::Error),
    #[error(transparent)]
    Browser(#[from] BrowserError),
    #[error("can't arm the browser: {0}")]
    Arm(#[from] ArmError),
    #[error(transparent)]
    Cdp(#[from] CdpError),
    #[error("can't record the run: {0}")]
    Capture(#[from] CaptureError),
    #[error(
        "{0} fence decision(s) still undecided {DECISION_GRACE:?} after the browser closed; their denies may be missing"
    )]
    Undecided(usize),
    #[error(transparent)]
    LiveSource(#[from] LiveSourceError),
}

/// How a lab session runs.
#[derive(Clone, Debug)]
pub struct SessionOptions {
    /// What the fence makes of each request.
    pub classifier: Classifier,
    /// Read write targets' members live ([`LiveMembershipSource`]), for a run that writes; otherwise every write whose
    /// target has to be fetched is denied ([`FailClosed`]).
    pub live_members: bool,
    /// Record the run as a capture of this scenario (`captures/<scenario>/<ts>/` in the lab's data dir).
    pub capture: Option<String>,
    pub display: Display,
}

impl SessionOptions {
    /// Fail-closed, headless, not recorded.
    #[must_use]
    pub fn fail_closed(classifier: Classifier) -> Self {
        Self {
            classifier,
            live_members: false,
            capture: None,
            display: Display::Headless,
        }
    }
}

/// How long the browser gets to put its debugging endpoint up.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the first tab gets to be armed.
const PAGE_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the browser gets to exit after closing.
const CLOSE_GRACE: Duration = Duration::from_secs(10);

/// One lab account's browser, armed, with one tab and its events watched.
pub struct LabSession {
    armed: ArmedBrowser,
    page: SessionId,
    events: mpsc::UnboundedReceiver<Event>,
    watch: SessionWatch,
    /// The run's capture, if it records.
    recorder: Option<Arc<Recorder>>,
}

impl LabSession {
    /// [`Self::open_with`] fail-closed, headless and not recorded ([`SessionOptions::fail_closed`]).
    ///
    /// # Errors
    /// As [`Self::open_with`].
    pub async fn open(paths: &LabPaths, alias: Alias, classifier: Classifier) -> Result<Self, SessionError> {
        Self::open_with(paths, alias, SessionOptions::fail_closed(classifier)).await
    }

    /// Launches the pinned Chrome on `alias`'s profile and arms it before any page loads: the fence from `lab.toml`
    /// ([`Fence::open_lab`]), the membership source and request classifier `options` name, and the seeded frame list;
    /// recording into a new capture when asked. Then opens one armed tab on `about:blank`.
    ///
    /// # Errors
    /// When Chrome isn't installed, the fence, the live source or the capture can't be made, or the browser can't be
    /// launched or armed (it is killed then).
    pub async fn open_with(paths: &LabPaths, alias: Alias, options: SessionOptions) -> Result<Self, SessionError> {
        let chrome = ChromeBuild::pinned().executable(&paths.chrome_dir());
        if !chrome.is_file() {
            return Err(SessionError::NoChrome(chrome));
        }
        let fence = Arc::new(Fence::open_lab()?);
        let (members, http): (Arc<dyn MembershipSource + Send>, _) = if options.live_members {
            let live = Arc::new(LiveMembershipSource::new()?);
            let http =
                HttpFence::new(options.classifier, Arc::clone(&fence), live.clone()).with_credentials(live.clone());
            (live, http)
        } else {
            let members: Arc<dyn MembershipSource + Send> = Arc::new(FailClosed);
            let http = HttpFence::new(options.classifier, Arc::clone(&fence), Arc::clone(&members));
            (members, http)
        };
        let guard = GuardScript::generate().map_err(SessionError::Guard)?;
        let ws = WsFence::new(FrameClassifier::seeded(), fence, members, guard);
        let recorder = match &options.capture {
            Some(scenario) => Some(Arc::new(Recorder::create(&paths.captures_dir(), scenario)?)),
            None => None,
        };
        let launch = LaunchOptions {
            display: options.display,
            ..LaunchOptions::for_profile(paths, alias)
        };
        let browser = Browser::launch(&chrome, &launch, LAUNCH_TIMEOUT).await?;
        let armed = match &recorder {
            Some(recorder) => {
                let capture = Capture::new(Arc::clone(recorder), RtcHooks::default());
                ArmedBrowser::arm_capturing(browser, http, ws, &ArmOptions::default(), capture).await?
            }
            None => ArmedBrowser::arm(browser, http, ws, &ArmOptions::default()).await?,
        };
        let events = armed.watch();
        let page = armed.open_page(PAGE_TIMEOUT).await?.session;
        Ok(Self {
            watch: SessionWatch::new(page.clone()),
            armed,
            page,
            events,
            recorder,
        })
    }

    /// A scenario driver on the tab, with its own watch of the events from now on.
    #[must_use]
    pub fn driver(&self) -> Driver {
        Driver::new(self.armed.cdp().clone(), self.page.clone(), self.armed.watch())
    }

    /// Where the run is recorded, if it is.
    #[must_use]
    pub fn capture_dir(&self) -> Option<&Path> {
        self.recorder.as_deref().map(Recorder::dir)
    }

    /// Denies every request `rule` matches from now on (an enforcement test).
    pub fn force_deny(&self, rule: ForcedDeny) {
        self.armed.force_deny(rule);
    }

    /// Stops every forced deny.
    pub fn clear_forced(&self) {
        self.armed.clear_forced();
    }

    /// Every request denied so far.
    #[must_use]
    pub fn denials(&self) -> Vec<Denial> {
        self.armed.denials()
    }

    /// Every WebSocket frame denied so far.
    #[must_use]
    pub fn frame_denials(&self) -> Vec<FrameDenial> {
        self.armed.frame_denials()
    }

    /// Navigates the tab.
    ///
    /// # Errors
    /// When the browser refuses the navigation.
    pub async fn navigate(&self, url: &str) -> Result<(), SessionError> {
        self.armed
            .cdp()
            .call(Some(&self.page), "Page.navigate", json!({"url": url}))
            .await?;
        Ok(())
    }

    /// Feeds the watch every event that has come in, waiting up to `wait` for one if none has. `false` once the
    /// connection is gone.
    pub async fn pump(&mut self, wait: Duration) -> bool {
        match tokio::time::timeout(wait, self.events.recv()).await {
            Ok(Some(event)) => self.watch.observe(&event),
            Ok(None) => return false,
            Err(_) => return true,
        }
        loop {
            match self.events.try_recv() {
                Ok(event) => self.watch.observe(&event),
                Err(mpsc::error::TryRecvError::Empty) => return true,
                Err(mpsc::error::TryRecvError::Disconnected) => return false,
            }
        }
    }

    /// Pumps until the session is anything but [`SessionState::Loading`] ([`Settling`]: a refusal counts once it has
    /// stood for [`REFUSAL_GRACE`]). `Loading` when `timeout` passes first, or the connection is lost, a refusal that
    /// hasn't stood its grace by then included.
    pub async fn settle(&mut self, timeout: Duration) -> SessionState {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut settling = Settling::default();
        loop {
            let state = settling.judge(self.watch.state(), tokio::time::Instant::now());
            if state != SessionState::Loading || tokio::time::Instant::now() >= deadline {
                return state;
            }
            if !self.pump(Duration::from_millis(500)).await {
                return SessionState::Loading;
            }
        }
    }

    #[must_use]
    pub fn watch(&self) -> &SessionWatch {
        &self.watch
    }

    #[must_use]
    pub fn cdp(&self) -> &Cdp {
        self.armed.cdp()
    }

    /// The tab's session.
    #[must_use]
    pub fn page(&self) -> &SessionId {
        &self.page
    }

    /// What the fence did so far.
    #[must_use]
    pub fn report(&self) -> FenceReport {
        FenceReport::of(&self.armed.decisions())
    }

    /// What the fence decides in this session, readable after [`Self::close`] too: the run's final denials are read
    /// from it once the close has waited for the decisions still running.
    #[must_use]
    pub fn decisions(&self) -> Decisions {
        self.armed.decisions()
    }

    /// Closes the browser (killing it unless it confirms), waits for the fence's decisions still running, finishes the
    /// capture, and says what the fence did, every one of those decisions included.
    ///
    /// # Errors
    /// When the browser had to be killed or couldn't be reaped, the capture lost events, or a decision was still
    /// running when the wait for them ended ([`SessionError::Undecided`]).
    pub async fn close(self) -> Result<FenceReport, SessionError> {
        let decisions = self.armed.decisions();
        let closed = self.armed.close(CLOSE_GRACE).await;
        if let Some(recorder) = &self.recorder {
            recorder.finish()?;
        }
        closed?;
        match decisions.undecided() {
            0 => Ok(FenceReport::of(&decisions)),
            undecided => Err(SessionError::Undecided(undecided)),
        }
    }

    /// [`Self::close`], also giving the capture's directory.
    ///
    /// # Errors
    /// As [`Self::close`].
    pub async fn close_recorded(self) -> Result<(FenceReport, Option<PathBuf>), SessionError> {
        let dir = self.capture_dir().map(Path::to_path_buf);
        Ok((self.close().await?, dir))
    }
}
