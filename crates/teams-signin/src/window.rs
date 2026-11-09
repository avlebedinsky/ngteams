//! The sign-in page and its GPUI window. A production window is bound to [`Bound::Hosts`]: page requests go
//! through the guard, all network connections through the host gate, and navigations stay on the same host bound.
//! `WebKit` features that bypass the page guard are disabled. The registered redirect is caught before it loads,
//! returning a [`SignInAnswer`] to the app; the engine and its web processes are then shut down.
//!
//! An ephemeral session keeps no website data. The authorize page opens only once its page ID has armed the guard.
//! A web-process crash ends the sign-in. Test-support builds also provide an offline numeric-loopback window with
//! a refusing proxy, for the same rendering, guard, navigation and process-lifecycle tests.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use async_channel::Receiver;
use gpui::{App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use teams_auth::redirect::RedirectUri;
use teams_auth::secrets::Secret;
use teams_auth::signin::{SignInAnswer, SignInRequest};
use teams_signin_protocol::ENTRY_POINT;
use teams_wpe_client::{
    EngineSource, Event, FrameMode, Geometry, Host, HostConfig, Load, SessionSpec, Settings, ViewId, ViewSpec,
    WebProcessExtensions,
};

#[cfg(feature = "test-support")]
use crate::egress::{BYPASS, BlackHole};
use crate::guard::{Guard, GuardEvent, GuardServer};
use crate::view::SignInView;

#[cfg(test)]
#[path = "../tests/support/windows.rs"]
mod service_tests;

/// The offline test window's Wayland app id (the app uses its own).
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
    /// Offline tests only: numeric loopback with a refusing proxy for every other connection.
    #[cfg(feature = "test-support")]
    Loopback,
    /// The sign-in hosts: the guard bound to them, the network
    /// session's only way out the host gate ([`crate::egress::HostGate`]).
    Hosts(Arc<crate::hosts::SignInHosts>),
}

impl Bound {
    /// Whether a navigation to `uri` stays on the bound once the page is armed.
    fn admits(&self, uri: &str) -> bool {
        match self {
            #[cfg(feature = "test-support")]
            Self::Loopback => is_local_page(uri),
            Self::Hosts(hosts) => hosts.admits_url(uri),
        }
    }

    /// The host of a refused navigation to `uri` the user could add to the account's `idp_hosts`: only the personal
    /// window's bound names one (`SignInHosts::idp_candidate`).
    fn idp_candidate(&self, uri: &str) -> Option<String> {
        match self {
            #[cfg(feature = "test-support")]
            Self::Loopback => {
                // the loopback window has no identity provider: a page off loopback is never one to name
                let _ = uri;
                None
            }
            Self::Hosts(hosts) => hosts.idp_candidate(uri),
        }
    }
}

/// What the window needs.
pub struct WindowConfig {
    pub request: SignInRequest,
    /// Where it may go.
    pub bound: Bound,
    /// The extension module (`libteams_signin_extension.so`).
    pub extension: PathBuf,
    /// The engine the page runs on ([`crate::engine`]).
    pub engine: EngineSource,
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

/// Runs the offline test window as an app of its own until it answers. Must run on the main thread, in a program whose
/// engine worker is pinned (`teams_wpe_client::worker`).
#[cfg(feature = "test-support")]
#[must_use]
pub fn run(config: WindowConfig) -> SignInAnswer {
    let answer = Arc::new(std::sync::Mutex::new(None));
    let kept = Arc::clone(&answer);
    gpui_platform::linux(gpui::WindowingModes::WAYLAND).run(move |cx: &mut App| {
        open(config, APP_ID, cx, move |given, cx| {
            *kept.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(given);
            cx.quit();
        });
    });
    let taken = answer.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    taken.unwrap_or_else(|| SignInAnswer::Failed("the window ended without an answer".into()))
}

/// What a sign-in is told once its window answered.
pub type Done = Box<dyn FnOnce(SignInAnswer, &mut App)>;

/// [`open`], its answer sent to `reply` (a [`Windows`] ask's).
fn open_answering(config: WindowConfig, app_id: &str, reply: mpsc::Sender<SignInAnswer>, cx: &mut App) {
    open(config, app_id, cx, move |answer, _| {
        let _ = reply.send(answer);
    });
}

/// Opens the sign-in window for `config` in the running app, under `app_id`; `done` gets its answer once (a page that
/// can't be set up answers at once, with no window).
pub fn open(config: WindowConfig, app_id: &str, cx: &mut App, done: impl FnOnce(SignInAnswer, &mut App) + 'static) {
    let page = match Page::start(config) {
        Ok(page) => page,
        Err(reason) => {
            done(SignInAnswer::Failed(reason), cx);
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
            done(SignInAnswer::Failed(format!("the window can't be opened: {error}")), cx);
        }
    }
}

/// Opens sign-in windows on GPUI's foreground for code that runs elsewhere: the personal build's sign-in, which
/// runs on a background thread and blocks on the window's answer ([`Windows::ask`]).
#[derive(Clone)]
pub struct Windows {
    asks: async_channel::Sender<Ask>,
}

/// What [`Windows`] asks GPUI's foreground to open.
enum Ask {
    SignIn(WindowConfig, mpsc::Sender<SignInAnswer>),
    EngineWait(crate::engine::WaitChannels),
}

impl Windows {
    /// Serves the asks in the running app, each a window under `app_id`, until every [`Windows`] is gone.
    #[must_use]
    pub fn serve(app_id: &str, cx: &mut App) -> Self {
        let (asks, waiting) = async_channel::unbounded::<Ask>();
        let app_id = app_id.to_owned();
        cx.spawn(async move |cx| {
            while let Ok(ask) = waiting.recv().await {
                cx.update(|cx| match ask {
                    Ask::SignIn(config, reply) => open_answering(config, &app_id, reply, cx),
                    Ask::EngineWait(channels) => crate::engine::open_wait(channels, &app_id, cx),
                });
            }
        })
        .detach();
        Self { asks }
    }

    /// The engine a sign-in under `data_dir` runs on, a window showing its download if one is needed (blocking: never
    /// call it on GPUI's foreground).
    ///
    /// # Errors
    ///
    /// The answer to give the sign-in: no engine can be had, or the user closed the window.
    pub fn engine_source(&self, data_dir: &Path) -> Result<EngineSource, SignInAnswer> {
        crate::engine::source(data_dir, |channels| self.open_engine_wait(channels))
    }

    /// Whether the app took the ask for an engine wait window; once it's gone, nothing would ever answer the download.
    fn open_engine_wait(&self, channels: crate::engine::WaitChannels) -> bool {
        self.asks.send_blocking(Ask::EngineWait(channels)).is_ok()
    }

    /// Opens a window for `config` and waits for its answer (blocking: never call it on GPUI's foreground).
    #[must_use]
    pub fn ask(&self, config: WindowConfig) -> SignInAnswer {
        let (reply, answer) = mpsc::channel();
        if self.asks.send_blocking(Ask::SignIn(config, reply)).is_err() {
            return SignInAnswer::Failed("the app has no window for the sign-in any more".into());
        }
        answer
            .recv()
            .unwrap_or_else(|_| SignInAnswer::Failed("the sign-in window went without an answer".into()))
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
    id: Option<u64>,
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
    Finish(SignInAnswer),
}

impl Page {
    /// Sets everything up (module docs) and loads `about:blank`.
    pub(crate) fn start(config: WindowConfig) -> Result<Self, String> {
        let redirect =
            RedirectUri::parse(&config.request.redirect_uri).map_err(|_| "the redirect URI isn't usable".to_owned())?;
        let dir = private_dir()?;
        let extensions = install_extension(&config.extension, dir.path())?;
        let (sender, guard_events) = mpsc::channel();
        let guard = match &config.bound {
            #[cfg(feature = "test-support")]
            Bound::Loopback => Guard::for_tests(redirect.clone(), sender).local_only(),
            Bound::Hosts(hosts) => Guard::new(Arc::clone(hosts), redirect.clone(), sender),
        };
        let guard = Arc::new((config.setup)(guard));
        let socket = dir.path().join("guard.sock");
        let server = GuardServer::start(&socket, Arc::clone(&guard))
            .map_err(|error| format!("the guard's socket can't be bound: {error}"))?;
        let (proxy, bypass) = egress(&config.bound, &guard)?;
        let engine = HostConfig {
            frames: FrameMode::Shm,
            schemes: Vec::new(),
            extensions: Some(WebProcessExtensions {
                directory: extensions,
                data: socket.to_string_lossy().into_owned(),
                sandbox_paths: vec![dir.path().to_path_buf()],
            }),
        };
        let (host, events) = Host::start_worker(&engine, &config.engine)
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
            id: None,
            armed: Vec::new(),
            deadline: Instant::now() + ARM_TIMEOUT,
            refused: None,
            _dir: dir,
            _server: server,
        })
    }

    /// The engine reported the view's page id.
    pub(crate) fn created(&mut self, page_id: u64) {
        self.id = Some(page_id);
        self.open_once_armed();
    }

    /// Opens the authorize URL once the view's own page is armed (the engine's report of its id and the guard's of its
    /// arming come on different channels, in either order).
    fn open_once_armed(&mut self) {
        let armed = self.id.is_some_and(|id| self.armed.contains(&id));
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
                (false, Outcome::Finish(SignInAnswer::Redirect(Secret::new(uri))))
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
                GuardEvent::Redirect(url) => return Outcome::Finish(SignInAnswer::Redirect(url)),
            }
        }
        self.open_once_armed();
        if self.stage == Stage::Arming && Instant::now() > self.deadline {
            return Outcome::Finish(SignInAnswer::Failed("the page wasn't armed in time".into()));
        }
        Outcome::Continue
    }

    /// The answer when the window is closed before a redirect.
    pub(crate) fn closed(&mut self) -> SignInAnswer {
        match self.refused.take() {
            Some(host) => SignInAnswer::Refused(host),
            None => SignInAnswer::Closed,
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
        #[cfg(feature = "test-support")]
        Bound::Loopback => {
            let black_hole = BlackHole::start(Arc::clone(guard))
                .map_err(|error| format!("the egress proxy can't be bound: {error}"))?;
            Ok((black_hole.uri(), BYPASS.iter().map(|host| (*host).to_owned()).collect()))
        }
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

/// Whether the offline test page is HTTP(S) on a numeric loopback address. Names never bypass its refusing proxy.
#[cfg(feature = "test-support")]
#[must_use]
pub fn is_local_page(text: &str) -> bool {
    let Ok(url) = url::Url::parse(text) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https")
        && match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(_)) | None => false,
        }
}

#[cfg(test)]
mod tests {
    use teams_auth::token::NATIVE_CLIENT_REDIRECT;

    use super::*;

    #[test]
    #[cfg(feature = "test-support")]
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
