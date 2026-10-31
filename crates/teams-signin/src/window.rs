//! The window: Microsoft's sign-in page (or, offline, a local fake of it) in a WPE `WebKit` view behind the guard,
//! drawn in a GPUI window ([`crate::view`]).
//!
//! [`Page::start`] sets the page up before anything loads:
//! - the web process extension (`teams-signin-extension`) copied alone into a private directory `WebKit` loads it from,
//!   given the guard's socket; the guard ([`crate::guard`]) serving on that socket;
//! - the engine ([`teams_wpe::Host::start_worker`]): this executable run again as a worker process of its own (so
//!   the window opens again in the same process), rendering on the CPU into shared memory, every web process it starts
//!   loading the extension;
//! - an ephemeral network session (no website data kept), Intelligent Tracking Prevention off, downloads cancelled,
//!   and its proxy set by the window's [`Bound`]: with [`Bound::Loopback`] it reaches numeric loopback addresses
//!   directly and anything else only through [`crate::egress::BlackHole`], which refuses it; with `Bound::Hosts` (the
//!   personal sign-in window, `personal` feature) nothing goes direct and every connection goes through
//!   `crate::egress::HostGate`, which lets out only the sign-in hosts;
//! - the guard in the bound's mode: loopback-only ([`Guard::local_only`]) or bound to the sign-in hosts
//!   (`Guard::hosts_only`);
//! - the `WebKit` features the guard can't see or that would send on their own switched off ([`FEATURES_OFF`]), WebRTC,
//!   developer extras, the page cache and script-opened windows off (no new window is ever made: the engine leaves
//!   `create` unanswered);
//! - the policy questions ([`Page::navigation`], [`Page::response`]): no new window, no response `WebKit` can't show (a
//!   download), no navigation before the page is armed, none off the bound (refused and logged: anything but a page on
//!   a loopback address or `about:blank` for the loopback window, anything `SignInHosts::admits_url` doesn't take for
//!   the personal one), and the redirect to the registered redirect URI caught before it loads.
//!
//! Then it loads `about:blank`, which makes `WebKit` create the page, and opens the authorize URL only once that page
//! (by the id the engine reports for it) has told the guard it is armed. The redirect, from the policy or from the
//! guard, whichever comes first, ends the run with [`WindowAnswer::Redirect`]; closing the window with
//! [`WindowAnswer::Closed`], or with [`WindowAnswer::Refused`] naming the last host a refused navigation went to that
//! the user could add to the account's `idp_hosts` (the personal window: `SignInHosts::idp_candidate`, the host alone);
//! the page's web process ending (a crash: a process `WebKit` started again might not be armed) with
//! [`WEB_PROCESS_ENDED`]. However the run ends, the engine is shut down (every web process ended, the worker reaped)
//! and the window closed (the black hole, or the host gate, keeps deciding whatever might still reach it).
//!
//! Not a complete fence: a synchronous load's redirects and a kept-alive load's redirects after its page is gone never
//! reach the guard (only the egress proxy gets them), and only the view's first arming is waited for: a later web
//! process whose extension failed to load would be unguarded, and it is not detected
//! (`docs/backlog/webkit-sign-in-guard-redirects.md`).

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use async_channel::Receiver;
use gpui::{App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, WindowingModes, px, size};
use teams_auth::redirect::RedirectUri;
use teams_auth::secrets::Secret;
use teams_auth::signin::{WindowAnswer, WindowRequest};
use teams_fence::FenceLog;
use teams_signin_extension::ENTRY_POINT;
use teams_wpe::{
    Event, FrameMode, Geometry, Host, HostConfig, Load, SessionSpec, Settings, ViewId, ViewSpec, WebProcessExtensions,
};

use crate::cli::is_local_page;
use crate::egress::{BYPASS, BlackHole};
use crate::guard::{Guard, GuardEvent, GuardServer};
use crate::view::SignInView;

/// The lab window's Wayland app id (the personal build opens its window under the app's own).
pub const APP_ID: &str = "com.nickalie.ngteams.signin";
/// The window's title.
pub const TITLE: &str = "Sign in to Microsoft Teams";
/// The window's size, in logical pixels.
pub const SIZE: (u16, u16) = (520, 720);
/// The extension module's file name, as cargo builds it.
pub const EXTENSION_FILE: &str = "libteams_signin_extension.so";
/// `WebKit` features switched off before anything loads: what `send-request` can't see (`WebSocket`), what sends on its
/// own or outside a page (beacons, service and shared workers, push, notifications, WebTransport, preconnects and
/// prefetches, Private Click Measurement's reports, background fetch), and the process swap on a cross-site navigation
/// (fewer processes to arm; not a single-process guarantee). A feature missing from `WebKit`'s list stops the window.
pub const FEATURES_OFF: [&str; 14] = [
    "WebSocket",
    "BeaconAPI",
    "ServiceWorkers",
    "SharedWorker",
    "PushAPI",
    "Notifications",
    "WebTransport",
    "LinkPreconnect",
    "LinkPreconnectEarlyHints",
    "LinkPrefetch",
    "SpeculationRulesPrefetch",
    "PrivateClickMeasurement",
    "BackgroundFetchAPI",
    "ProcessSwapOnCrossSiteNavigation",
];
/// How long the page has to report armed.
pub const ARM_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the guard's events are read on the window's thread.
pub(crate) const PUMP: Duration = Duration::from_millis(20);
/// Why the run failed when the page's web process ended.
pub const WEB_PROCESS_ENDED: &str = "the page's web process ended";

/// The guard's own setup: a hook for tests that add a local own-state rule (`Guard::with_own`).
pub type GuardSetup = Box<dyn FnOnce(Guard) -> Guard + Send>;

/// Where the window may go.
#[derive(Clone, Debug)]
pub enum Bound {
    /// Loopback only (the lab's window while [`crate::cli::NOT_LIVE`] holds): the guard in loopback-only mode, the
    /// network session's way out the black hole ([`BlackHole`]).
    Loopback,
    /// The personal sign-in window's hosts (`personal`): the guard bound to them ([`Guard::hosts_only`]), the network
    /// session's only way out the host gate ([`crate::egress::HostGate`]).
    #[cfg(feature = "personal")]
    Hosts(Arc<crate::hosts::SignInHosts>),
}

impl Bound {
    /// Whether a navigation to `uri` stays on the bound once the page is armed.
    fn admits(&self, uri: &str) -> bool {
        match self {
            Self::Loopback => is_local_page(uri),
            #[cfg(feature = "personal")]
            Self::Hosts(hosts) => hosts.admits_url(uri),
        }
    }

    /// The host of a refused navigation to `uri` the user could add to the account's `idp_hosts`: only the personal
    /// window's bound names one (`SignInHosts::idp_candidate`).
    fn idp_candidate(&self, uri: &str) -> Option<String> {
        match self {
            Self::Loopback => {
                // the loopback window has no identity provider: a page off loopback is never one to name
                let _ = uri;
                None
            }
            #[cfg(feature = "personal")]
            Self::Hosts(hosts) => hosts.idp_candidate(uri),
        }
    }
}

/// What the window needs.
pub struct WindowConfig {
    pub request: WindowRequest,
    /// Where it may go.
    pub bound: Bound,
    /// The extension module (`libteams_signin_extension.so`).
    pub extension: PathBuf,
    /// Where the guard's decisions go (the lab's `fence.log`, or the personal build's for the personal window).
    pub log: FenceLog,
    pub setup: GuardSetup,
}

/// Where the extension module is for the binary at `exe`: next to it, else in its `deps/` (where a `cargo build`
/// leaves it, as a dependency's).
#[must_use]
pub fn extension_beside(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    [dir.join(EXTENSION_FILE), dir.join("deps").join(EXTENSION_FILE)]
        .into_iter()
        .find(|path| path.is_file())
}

/// Runs the window as an app of its own until the redirect is caught, the window is closed, or it fails: the lab's
/// `teams-signin --lab` and the window test. Must run on the main thread, in a program that serves the engine's
/// worker first thing in its `main` (`teams_wpe::worker::serve_if_asked`).
#[must_use]
pub fn run(config: WindowConfig) -> WindowAnswer {
    let answer = Arc::new(std::sync::Mutex::new(None));
    let kept = Arc::clone(&answer);
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        open(config, APP_ID, cx, move |given, cx| {
            *kept.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(given);
            cx.quit();
        });
    });
    let taken = answer.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    taken.unwrap_or_else(|| WindowAnswer::Failed("the window ended without an answer".into()))
}

/// What a sign-in is told once its window answered.
pub type Done = Box<dyn FnOnce(WindowAnswer, &mut App)>;

/// Opens the sign-in window for `config` in the running app, under `app_id`; `done` gets its answer once (a page that
/// can't be set up answers at once, with no window).
pub fn open(config: WindowConfig, app_id: &str, cx: &mut App, done: impl FnOnce(WindowAnswer, &mut App) + 'static) {
    let page = match Page::start(config) {
        Ok(page) => page,
        Err(reason) => {
            done(WindowAnswer::Failed(reason), cx);
            return;
        }
    };
    let bounds = Bounds::centered(None, size(px(f32::from(SIZE.0)), px(f32::from(SIZE.1))), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(TITLE.into()),
            ..TitlebarOptions::default()
        }),
        app_id: Some(app_id.to_owned()),
        ..WindowOptions::default()
    };
    let done: Done = Box::new(done);
    let slot = std::rc::Rc::new(std::cell::RefCell::new(Some((page, done))));
    let taken = std::rc::Rc::clone(&slot);
    let opened = cx.open_window(options, move |window, cx| {
        let run = taken.borrow_mut().take();
        cx.new(|cx| SignInView::new(run, window, cx))
    });
    if let Err(error) = opened {
        // the view never took them: answer here
        if let Some((page, done)) = slot.borrow_mut().take() {
            page.shut_down(cx);
            done(WindowAnswer::Failed(format!("the window can't be opened: {error}")), cx);
        }
    }
}

/// Opens sign-in windows on GPUI's foreground for code that runs elsewhere: the personal build's sign-in, which
/// runs on a background thread and blocks on the window's answer ([`Windows::ask`]).
#[derive(Clone)]
pub struct Windows {
    asks: async_channel::Sender<(WindowConfig, mpsc::Sender<WindowAnswer>)>,
}

impl Windows {
    /// Serves the asks in the running app, each a window under `app_id`, until every [`Windows`] is gone.
    #[must_use]
    pub fn serve(app_id: &str, cx: &mut App) -> Self {
        let (asks, waiting) = async_channel::unbounded::<(WindowConfig, mpsc::Sender<WindowAnswer>)>();
        let app_id = app_id.to_owned();
        cx.spawn(async move |cx| {
            while let Ok((config, reply)) = waiting.recv().await {
                let app_id = app_id.clone();
                cx.update(|cx| {
                    open(config, &app_id, cx, move |answer, _| {
                        let _ = reply.send(answer);
                    });
                });
            }
        })
        .detach();
        Self { asks }
    }

    /// Opens a window for `config` and waits for its answer (blocking: never call it on GPUI's foreground).
    #[must_use]
    pub fn ask(&self, config: WindowConfig) -> WindowAnswer {
        let (reply, answer) = mpsc::channel();
        if self.asks.send_blocking((config, reply)).is_err() {
            return WindowAnswer::Failed("the app has no window for the sign-in any more".into());
        }
        answer
            .recv()
            .unwrap_or_else(|_| WindowAnswer::Failed("the sign-in window went without an answer".into()))
    }
}

/// Where the run is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// `about:blank` loaded; waiting for the page to be armed.
    Arming,
    /// The authorize URL is open.
    Open,
}

/// One sign-in's page: the engine and its view, the guard behind it and what it was asked to open.
pub(crate) struct Page {
    pub(crate) host: Arc<Host>,
    pub(crate) view: ViewId,
    pub(crate) events: Receiver<Event>,
    pub(crate) guard_events: mpsc::Receiver<GuardEvent>,
    guard: Arc<Guard>,
    redirect: RedirectUri,
    bound: Bound,
    authorize: String,
    stage: Stage,
    /// The view's page id, once the engine reported it.
    page_id: Option<u64>,
    /// The pages the guard was told are armed (arming is per page: `GuardEvent::Armed`).
    armed: Vec<u64>,
    deadline: Instant,
    /// The last refused navigation's host the user could add to `idp_hosts`.
    refused: Option<String>,
    // kept alive until the run ends: the guard's socket and the extension live there
    _dir: tempfile::TempDir,
    _server: GuardServer,
}

/// What the window does after a policy question or a guard event.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// Nothing more.
    Continue,
    /// The run ends with this answer.
    Finish(WindowAnswer),
}

impl Page {
    /// Sets everything up (module docs) and loads `about:blank`.
    pub(crate) fn start(config: WindowConfig) -> Result<Self, String> {
        let redirect =
            RedirectUri::parse(&config.request.redirect_uri).map_err(|_| "the redirect URI isn't usable".to_owned())?;
        let dir = private_dir()?;
        let extensions = install_extension(&config.extension, dir.path())?;
        let (sender, guard_events) = mpsc::channel();
        let guard = Guard::new(config.log, redirect.clone(), sender);
        let guard = Arc::new((config.setup)(match &config.bound {
            Bound::Loopback => guard.local_only(),
            #[cfg(feature = "personal")]
            Bound::Hosts(hosts) => guard.hosts_only(Arc::clone(hosts)),
        }));
        let socket = dir.path().join("guard.sock");
        let server = GuardServer::start(&socket, Arc::clone(&guard))
            .map_err(|error| format!("the guard's socket can't be bound: {error}"))?;
        let (proxy, bypass) = egress(&config.bound, &guard)?;
        let (host, events) = Host::start_worker(HostConfig {
            frames: FrameMode::Shm,
            schemes: Vec::new(),
            extensions: Some(WebProcessExtensions {
                directory: extensions,
                data: socket.to_string_lossy().into_owned(),
                sandbox_paths: vec![dir.path().to_path_buf()],
            }),
        })
        .map_err(|error| format!("the page's engine can't start: {error}"))?;
        let session = host.create_session(SessionSpec::Guarded {
            proxy,
            ignore_hosts: bypass,
        });
        let view = host.create_view(ViewSpec {
            session,
            settings: Settings::JAVASCRIPT_MARKUP | Settings::MEDIA_REQUIRES_USER_GESTURE,
            scripts: Vec::new(),
            message_handlers: Vec::new(),
            zoom: 1.0,
            background: None,
            geometry: Geometry {
                width: u32::from(SIZE.0),
                height: u32::from(SIZE.1),
                scale: 1.0,
            },
            features_off: FEATURES_OFF.iter().map(|feature| (*feature).to_owned()).collect(),
        });
        host.load(view, Load::Uri("about:blank".into()));
        Ok(Self {
            host: Arc::new(host),
            view,
            events,
            guard_events,
            guard,
            redirect,
            bound: config.bound,
            authorize: config.request.authorize,
            stage: Stage::Arming,
            page_id: None,
            armed: Vec::new(),
            deadline: Instant::now() + ARM_TIMEOUT,
            refused: None,
            _dir: dir,
            _server: server,
        })
    }

    /// The engine reported the view's page id.
    pub(crate) fn created(&mut self, page_id: u64) {
        self.page_id = Some(page_id);
        self.open_once_armed();
    }

    /// Opens the authorize URL once the view's own page is armed (the engine's report of its id and the guard's of its
    /// arming come on different channels, in either order).
    fn open_once_armed(&mut self) {
        let armed = self.page_id.is_some_and(|id| self.armed.contains(&id));
        if armed && self.stage == Stage::Arming {
            self.stage = Stage::Open;
            self.host.load(self.view, Load::Uri(self.authorize.clone()));
        }
    }

    /// A navigation question: answered here, the redirect caught.
    pub(crate) fn navigation(&mut self, uri: &str, method: Option<&str>, new_window: bool) -> (bool, Outcome) {
        if new_window {
            return (false, Outcome::Continue);
        }
        match navigation(uri, self.stage, &self.redirect, &self.bound) {
            Navigation::Redirect => {
                self.guard.caught_navigation(uri);
                (false, Outcome::Finish(WindowAnswer::Redirect(Secret::new(uri))))
            }
            Navigation::Use => (true, Outcome::Continue),
            Navigation::Refuse => {
                self.guard.refuse(method.unwrap_or("GET"), uri);
                if let Some(host) = self.bound.idp_candidate(uri) {
                    self.refused = Some(host);
                }
                (false, Outcome::Continue)
            }
            Navigation::Hold => (false, Outcome::Continue),
        }
    }

    /// A response question: only what `WebKit` can show goes on (a download never does).
    pub(crate) const fn response(mime_supported: bool) -> bool {
        mime_supported
    }

    /// Reads the guard's events: the authorize URL opens once the view's own page is armed; a caught redirect ends
    /// the run; no arming within [`ARM_TIMEOUT`] fails it.
    pub(crate) fn pump(&mut self) -> Outcome {
        while let Ok(event) = self.guard_events.try_recv() {
            match event {
                GuardEvent::Armed { page } => self.armed.push(page),
                GuardEvent::Redirect(url) => return Outcome::Finish(WindowAnswer::Redirect(url)),
            }
        }
        self.open_once_armed();
        if self.stage == Stage::Arming && Instant::now() > self.deadline {
            return Outcome::Finish(WindowAnswer::Failed("the page wasn't armed in time".into()));
        }
        Outcome::Continue
    }

    /// The answer when the window is closed before a redirect.
    pub(crate) fn closed(&mut self) -> WindowAnswer {
        match self.refused.take() {
            Some(host) => WindowAnswer::Refused(host),
            None => WindowAnswer::Closed,
        }
    }

    /// Shuts the engine down off the foreground: every web process ended, the worker reaped.
    pub(crate) fn shut_down(self, cx: &App) {
        let host = Arc::clone(&self.host);
        cx.background_spawn(async move { host.shutdown() }).detach();
    }
}

/// The network session's proxy and what bypasses it, for `bound`: the black hole with numeric loopback bypassing it,
/// or the personal window's host gate with nothing bypassing it.
fn egress(bound: &Bound, guard: &Arc<Guard>) -> Result<(String, Vec<String>), String> {
    match bound {
        Bound::Loopback => {
            let black_hole = BlackHole::start(Arc::clone(guard))
                .map_err(|error| format!("the egress proxy can't be bound: {error}"))?;
            Ok((black_hole.uri(), BYPASS.iter().map(|host| (*host).to_owned()).collect()))
        }
        #[cfg(feature = "personal")]
        Bound::Hosts(hosts) => {
            let gate = crate::egress::HostGate::start(Arc::clone(guard), Arc::clone(hosts))
                .map_err(|error| format!("the egress gate can't be bound: {error}"))?;
            // nothing goes direct: every connection asks the gate
            Ok((gate.uri(), Vec::new()))
        }
    }
}

/// A private directory (`0700`) for the guard's socket and the extension.
fn private_dir() -> Result<tempfile::TempDir, String> {
    let dir = tempfile::Builder::new()
        .prefix("teams-signin-")
        .tempdir()
        .map_err(|error| format!("no private directory: {error}"))?;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("the private directory can't be made private: {error}"))?;
    Ok(dir)
}

/// Copies the module alone into `dir/extensions` (`WebKit` loads every module of its directory) after checking it
/// exports `WebKit`'s entry point. Returns that directory.
fn install_extension(module: &Path, dir: &Path) -> Result<PathBuf, String> {
    let bytes =
        fs::read(module).map_err(|error| format!("the extension {} can't be read: {error}", module.display()))?;
    if !bytes
        .windows(ENTRY_POINT.len())
        .any(|window| window == ENTRY_POINT.as_bytes())
    {
        return Err(format!("{} isn't the sign-in window's extension", module.display()));
    }
    let extensions = dir.join("extensions");
    fs::create_dir(&extensions).map_err(|error| format!("no extensions directory: {error}"))?;
    fs::write(extensions.join(EXTENSION_FILE), &bytes)
        .map_err(|error| format!("the extension can't be copied: {error}"))?;
    Ok(extensions)
}

/// What the navigation policy does with a navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Navigation {
    /// The registered redirect: caught, never loaded, handed to the parent.
    Redirect,
    /// `about:blank`, or a page on the bound once the page is armed.
    Use,
    /// Anything else before the page is armed: ignored.
    Hold,
    /// Off the bound once the page is armed: ignored and logged as denied.
    Refuse,
}

/// The navigation policy's verdict on `uri` at `stage`, within `bound`.
fn navigation(uri: &str, stage: Stage, redirect: &RedirectUri, bound: &Bound) -> Navigation {
    if redirect.matches(uri) {
        Navigation::Redirect
    } else if uri == "about:blank" {
        Navigation::Use
    } else if stage == Stage::Arming {
        Navigation::Hold
    } else if bound.admits(uri) {
        Navigation::Use
    } else {
        Navigation::Refuse
    }
}

#[cfg(test)]
mod tests {
    use teams_auth::token::NATIVE_CLIENT_REDIRECT;

    use super::*;

    #[test]
    fn once_armed_only_loopback_pages_and_about_blank_are_navigated_to() -> Result<(), String> {
        let redirect = RedirectUri::parse(NATIVE_CLIENT_REDIRECT).map_err(|_| "the redirect URI")?;
        let caught = format!("{NATIVE_CLIENT_REDIRECT}?code=made-up&state=made-up");
        for (uri, arming, open) in [
            (caught.as_str(), Navigation::Redirect, Navigation::Redirect),
            ("about:blank", Navigation::Use, Navigation::Use),
            ("http://127.0.0.1:8080/next", Navigation::Hold, Navigation::Use),
            ("http://[::1]:8080/next", Navigation::Hold, Navigation::Use),
            (
                "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?client_id=x",
                Navigation::Hold,
                Navigation::Refuse,
            ),
            ("http://192.0.2.1/nav", Navigation::Hold, Navigation::Refuse),
            ("http://localhost:8080/next", Navigation::Hold, Navigation::Refuse),
            ("data:text/html,x", Navigation::Hold, Navigation::Refuse),
            ("file:///etc/passwd", Navigation::Hold, Navigation::Refuse),
        ] {
            assert_eq!(
                navigation(uri, Stage::Arming, &redirect, &Bound::Loopback),
                arming,
                "{uri} arming"
            );
            assert_eq!(
                navigation(uri, Stage::Open, &redirect, &Bound::Loopback),
                open,
                "{uri} open"
            );
        }
        Ok(())
    }

    #[cfg(feature = "personal")]
    #[test]
    fn the_personal_window_navigates_only_to_its_hosts() -> Result<(), String> {
        let redirect = RedirectUri::parse(NATIVE_CLIENT_REDIRECT).map_err(|_| "the redirect URI")?;
        let hosts =
            crate::hosts::SignInHosts::entra_with(&["sso.example.test".to_owned()]).map_err(|e| e.to_string())?;
        let bound = Bound::Hosts(Arc::new(hosts));
        for (uri, open) in [
            (
                "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?client_id=x",
                Navigation::Use,
            ),
            ("https://sso.example.test/adfs/ls/", Navigation::Use),
            ("http://login.microsoftonline.com/x", Navigation::Refuse),
            ("https://login.microsoftonline.com:8443/x", Navigation::Refuse),
            ("https://teams.microsoft.com/v2/", Navigation::Refuse),
            ("https://graph.microsoft.com/v1.0/me", Navigation::Refuse),
            ("http://127.0.0.1:8080/next", Navigation::Refuse),
            ("https://evil.example.test/", Navigation::Refuse),
        ] {
            assert_eq!(navigation(uri, Stage::Open, &redirect, &bound), open, "{uri}");
            assert_eq!(
                navigation(uri, Stage::Arming, &redirect, &bound),
                Navigation::Hold,
                "{uri}"
            );
        }
        let caught = format!("{NATIVE_CLIENT_REDIRECT}?code=made-up&state=made-up");
        assert_eq!(
            navigation(&caught, Stage::Open, &redirect, &bound),
            Navigation::Redirect
        );
        Ok(())
    }
}
