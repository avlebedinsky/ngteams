//! The window: Microsoft's sign-in page (or, offline, a local fake of it) in a `WebKitGTK` webview behind the guard.
//!
//! [`run`] sets the webview up before anything loads:
//! - the web process extension (`teams-signin-extension`) copied alone into a private directory `WebKit` loads it from,
//!   given the guard's socket; the guard ([`crate::guard`]) serving on that socket;
//! - an ephemeral network session (no website data kept), Intelligent Tracking Prevention off, downloads cancelled,
//!   and its proxy set by the window's [`Bound`]: with [`Bound::Loopback`] it reaches numeric loopback addresses
//!   directly and anything else only through [`crate::egress::BlackHole`], which refuses it; with `Bound::Hosts` (the
//!   personal sign-in window, `personal` feature) nothing goes direct and every connection goes through
//!   `crate::egress::HostGate`, which lets out only the sign-in hosts;
//! - the guard in the bound's mode: loopback-only ([`Guard::local_only`]) or bound to the sign-in hosts
//!   (`Guard::hosts_only`);
//! - the `WebKit` features the guard can't see or that would send on their own switched off ([`FEATURES_OFF`]), WebRTC,
//!   developer extras, the page cache and script-opened windows off (`WebKitGTK` 2.50 dropped the hyperlink auditing
//!   setting and 2.48 the DNS prefetching one; an `<a ping>` goes through the cached-resource loader, so the guard
//!   decides it like any other request);
//! - `decide-policy`: no new window, no response `WebKit` can't show (a download), no navigation before the page is
//!   armed, none off the bound (refused and logged: anything but a page on a loopback address or `about:blank` for
//!   the loopback window, anything `SignInHosts::admits_url` doesn't take for the personal one), and the redirect to
//!   the registered redirect URI caught before it loads.
//!
//! Then it loads `about:blank`, which makes `WebKit` create the page, and opens the authorize URL only once that page
//! has told the guard it is armed. The redirect, from `decide-policy` or from the guard, whichever comes first, ends the
//! run with [`WindowAnswer::Redirect`]; closing the window with [`WindowAnswer::Closed`]; the page's web process
//! ending (a crash: a process `WebKit` started again might not be armed) with [`WEB_PROCESS_ENDED`]. However the run
//! ends, the view's web process is then terminated and the window destroyed (the black hole, or the host gate, keeps
//! deciding whatever a kept-alive load still sends from the network process).
//!
//! Not a complete fence (the plan's Task 24): a synchronous load's redirects and a kept-alive load's redirects after
//! its page is gone never reach the guard (on this machine only the black hole gets them), and only the view's first
//! arming is waited for: a later web process whose extension failed to load would be unguarded, and it is not
//! detected (`docs/backlog/webkit-sign-in-guard-redirects.md`).

use std::cell::{Cell, RefCell};
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use gtk4::glib;
use gtk4::prelude::*;
use teams_auth::redirect::RedirectUri;
use teams_auth::secrets::Secret;
use teams_auth::signin::{WindowAnswer, WindowRequest};
use teams_fence::FenceLog;
use teams_signin_extension::ENTRY_POINT;
use webkit6::prelude::*;
use webkit6::{
    NetworkProxyMode, NetworkProxySettings, NetworkSession, PolicyDecisionType, Settings, WebContext, WebView,
};

use crate::cli::is_local_page;
use crate::egress::{BYPASS, BlackHole};
use crate::guard::{Guard, GuardEvent, GuardServer};

/// The window's Wayland app id: the program name GTK gives a window that has no `GApplication`.
pub const APP_ID: &str = "dev.teams-native.signin";
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
const PUMP: Duration = Duration::from_millis(20);
/// Why the run failed when the page's web process ended.
pub const WEB_PROCESS_ENDED: &str = "the page's web process ended";

/// The guard's own setup: a hook for tests that add a local own-state rule (`Guard::with_own`).
pub type GuardSetup = Box<dyn FnOnce(Guard) -> Guard>;

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

/// Where the extension module is for the binary at `exe`: next to it, else in its `deps/` (where a `cargo build` of
/// `teams-signin` leaves it, as a dependency's).
#[must_use]
pub fn extension_beside(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    [dir.join(EXTENSION_FILE), dir.join("deps").join(EXTENSION_FILE)]
        .into_iter()
        .find(|path| path.is_file())
}

/// Runs the window until the redirect is caught, the window is closed, or it fails. Must run on the main thread.
#[must_use]
pub fn run(config: WindowConfig) -> WindowAnswer {
    match Window::open(config) {
        Ok(window) => window.wait(),
        Err(reason) => WindowAnswer::Failed(reason),
    }
}

/// Where the run is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// `about:blank` loaded; waiting for the page to be armed.
    Arming,
    /// The authorize URL is open.
    Open,
}

struct Window {
    main: glib::MainLoop,
    answer: Rc<RefCell<Option<WindowAnswer>>>,
    view: WebView,
    toplevel: gtk4::Window,
    pump: glib::SourceId,
    // kept alive until the run ends: the guard's socket and the extension live there
    _dir: tempfile::TempDir,
    _server: GuardServer,
}

impl Window {
    fn open(config: WindowConfig) -> Result<Self, String> {
        let redirect =
            RedirectUri::parse(&config.request.redirect_uri).map_err(|_| "the redirect URI isn't usable".to_owned())?;
        glib::set_prgname(Some(APP_ID));
        gtk4::init().map_err(|error| format!("GTK can't start: {error}"))?;
        let dir = private_dir()?;
        let extensions = install_extension(&config.extension, dir.path())?;
        let (sender, events) = mpsc::channel();
        let guard = Guard::new(config.log, redirect.clone(), sender);
        let guard = Arc::new((config.setup)(match &config.bound {
            Bound::Loopback => guard.local_only(),
            #[cfg(feature = "personal")]
            Bound::Hosts(hosts) => guard.hosts_only(Arc::clone(hosts)),
        }));
        let socket = dir.path().join("guard.sock");
        let server = GuardServer::start(&socket, Arc::clone(&guard))
            .map_err(|error| format!("the guard's socket can't be bound: {error}"))?;
        let (proxy, bypass): (String, &[&str]) = match &config.bound {
            Bound::Loopback => {
                let black_hole = BlackHole::start(Arc::clone(&guard))
                    .map_err(|error| format!("the egress proxy can't be bound: {error}"))?;
                (black_hole.uri(), &BYPASS)
            }
            #[cfg(feature = "personal")]
            Bound::Hosts(hosts) => {
                let gate = crate::egress::HostGate::start(Arc::clone(&guard), Arc::clone(hosts))
                    .map_err(|error| format!("the egress gate can't be bound: {error}"))?;
                // nothing goes direct: every connection asks the gate
                (gate.uri(), &[])
            }
        };

        let context = WebContext::new();
        context.set_web_process_extensions_directory(&extensions.to_string_lossy());
        context.set_web_process_extensions_initialization_user_data(&socket.to_string_lossy().to_variant());
        context.add_path_to_sandbox(dir.path(), true);
        let session = network_session(&proxy, bypass);
        let view = WebView::builder()
            .web_context(&context)
            .network_session(&session)
            .settings(&settings()?)
            .build();

        let main = glib::MainLoop::new(None, false);
        let answer = Rc::new(RefCell::new(None));
        let finish = finisher(&main, &answer);
        let stage = Rc::new(Cell::new(Stage::Arming));
        connect_policy(
            &view,
            (redirect, guard, config.bound),
            Rc::clone(&stage),
            finish.clone(),
        );
        let window = gtk4::Window::builder()
            .title("Sign in to Microsoft Teams")
            .default_width(520)
            .default_height(720)
            .child(&view)
            .build();
        let closed = finish.clone();
        window.connect_close_request(move |_| {
            closed(WindowAnswer::Closed);
            glib::Propagation::Proceed
        });
        let ended = finish.clone();
        view.connect_web_process_terminated(move |_, reason| {
            ended(WindowAnswer::Failed(format!("{WEB_PROCESS_ENDED} ({reason:?})")));
        });
        let pump = pump(&view, events, stage, config.request.authorize, finish);
        window.present();
        view.load_uri("about:blank");
        Ok(Self {
            main,
            answer,
            view,
            toplevel: window,
            pump,
            _dir: dir,
            _server: server,
        })
    }

    /// Runs until the answer, then tears the page down: its web process terminated, the window destroyed.
    fn wait(self) -> WindowAnswer {
        self.main.run();
        self.pump.remove();
        self.view.terminate_web_process();
        self.toplevel.destroy();
        self.answer
            .borrow_mut()
            .take()
            .unwrap_or_else(|| WindowAnswer::Failed("the window ended without an answer".into()))
    }
}

type Finish = Rc<dyn Fn(WindowAnswer)>;

/// The one way the run ends: the first answer is kept and the main loop quits.
fn finisher(main: &glib::MainLoop, answer: &Rc<RefCell<Option<WindowAnswer>>>) -> Finish {
    let main = main.clone();
    let answer = Rc::clone(answer);
    Rc::new(move |given| {
        let mut kept = answer.borrow_mut();
        if kept.is_none() {
            *kept = Some(given);
            main.quit();
        }
    })
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

/// An ephemeral network session (no website data), ITP off, downloads cancelled, reaching anything but `bypass` only
/// through the proxy at `proxy` (the black hole, or the personal window's host gate with nothing bypassing it).
fn network_session(proxy: &str, bypass: &[&str]) -> NetworkSession {
    let session = NetworkSession::new_ephemeral();
    session.set_itp_enabled(false);
    session.connect_download_started(|_, download| download.cancel());
    let proxy = NetworkProxySettings::new(Some(proxy), bypass);
    session.set_proxy_settings(NetworkProxyMode::Custom, Some(&proxy));
    session
}

/// The webview's settings: [`FEATURES_OFF`] and the rest of the list in the module docs.
fn settings() -> Result<Settings, String> {
    let settings = Settings::new();
    let features = Settings::all_features().ok_or("WebKit lists no features")?;
    for wanted in FEATURES_OFF {
        let feature = (0..features.length())
            .filter_map(|index| features.get(index))
            .find(|feature| feature.identifier().as_deref() == Some(wanted))
            .ok_or_else(|| format!("WebKit has no feature {wanted} to switch off"))?;
        settings.set_feature_enabled(&feature, false);
    }
    settings.set_enable_webrtc(false);
    settings.set_enable_developer_extras(false);
    settings.set_enable_page_cache(false);
    settings.set_javascript_can_open_windows_automatically(false);
    Ok(settings)
}

/// `decide-policy` and `create`: see the module docs.
fn connect_policy(
    view: &WebView,
    (redirect, guard, bound): (RedirectUri, Arc<Guard>, Bound),
    stage: Rc<Cell<Stage>>,
    finish: Finish,
) {
    view.connect_create(|_, _| None);
    view.connect_decide_policy(move |_, decision, kind| match kind {
        PolicyDecisionType::NavigationAction => {
            let request = decision
                .downcast_ref::<webkit6::NavigationPolicyDecision>()
                .and_then(webkit6::NavigationPolicyDecision::navigation_action)
                .and_then(|action| action.request());
            let uri = request.as_ref().and_then(webkit6::URIRequest::uri).map(String::from);
            let Some(uri) = uri else {
                decision.ignore();
                return true;
            };
            match navigation(&uri, stage.get(), &redirect, &bound) {
                Navigation::Redirect => {
                    decision.ignore();
                    guard.caught_navigation(&uri);
                    finish(WindowAnswer::Redirect(Secret::new(uri.as_str())));
                }
                Navigation::Use => decision.use_(),
                Navigation::Refuse => {
                    decision.ignore();
                    let method = request.as_ref().and_then(webkit6::URIRequest::http_method);
                    guard.refuse(method.as_deref().unwrap_or("GET"), &uri);
                }
                Navigation::Hold => decision.ignore(),
            }
            true
        }
        PolicyDecisionType::Response => {
            let shown = decision
                .downcast_ref::<webkit6::ResponsePolicyDecision>()
                .is_some_and(webkit6::ResponsePolicyDecision::is_mime_type_supported);
            if !shown {
                decision.ignore();
                return true;
            }
            false
        }
        // a new window, or anything WebKit adds later
        _ => {
            decision.ignore();
            true
        }
    });
}

/// What the navigation policy does with a navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Navigation {
    /// The registered redirect: caught, never loaded, handed to the parent.
    Redirect,
    /// `about:blank`, or a page on a loopback address once the page is armed.
    Use,
    /// Anything else before the page is armed: ignored.
    Hold,
    /// Off loopback once the page is armed: ignored and logged as denied.
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

/// Moves the guard's events onto the main loop: the authorize URL opens once the view's own page is armed; a caught
/// redirect ends the run; no arming within [`ARM_TIMEOUT`] fails it.
fn pump(
    view: &WebView,
    events: mpsc::Receiver<GuardEvent>,
    stage: Rc<Cell<Stage>>,
    authorize: String,
    finish: Finish,
) -> glib::SourceId {
    let view = view.clone();
    let deadline = Instant::now() + ARM_TIMEOUT;
    glib::timeout_add_local(PUMP, move || {
        while let Ok(event) = events.try_recv() {
            match event {
                GuardEvent::Armed { page } if page == view.page_id() && stage.get() == Stage::Arming => {
                    stage.set(Stage::Open);
                    view.load_uri(&authorize);
                }
                GuardEvent::Armed { .. } => {}
                GuardEvent::Redirect(url) => finish(WindowAnswer::Redirect(url)),
            }
        }
        if stage.get() == Stage::Arming && Instant::now() > deadline {
            finish(WindowAnswer::Failed("the page wasn't armed in time".into()));
        }
        glib::ControlFlow::Continue
    })
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
