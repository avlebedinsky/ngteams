//! Native Windows sign-in: a disposable `WebView2` worker, a private pipe to the same request guard as Linux,
//! and a host-gated proxy with no direct bypass. The worker never receives tokens or credential-store handles.
//! All descendants belong to a private kill-on-close job before their first instruction. Its profile directory
//! is private and removed only after the complete tree exits, including on a failed setup or page crash.
use crate::guard::{Decision, Guard, GuardEvent};
use gpui::App;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader};
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsHandle as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::Duration;
use teams_auth::redirect::RedirectUri;
use teams_auth::signin::{SignInAnswer, SignInRequest};
use teams_platform::{
    private,
    windows_job::Job,
    windows_process::{Child, Transfer},
};

mod browser;
mod policy;
mod profile;
mod refusals;
mod wire;

pub const TITLE: &str = "Sign in to Microsoft Teams";
pub const EXTENSION_FILE: &str = "ngteams-webview.exe";
pub const SIZE: (u16, u16) = (520, 720);
pub type GuardSetup = Box<dyn FnOnce(Guard) -> Guard + Send>;

/// Windows selects Microsoft's installed evergreen runtime; no Linux runtime archive is loaded here.
pub struct EngineSource;

#[derive(Clone, Debug)]
pub enum Bound {
    Hosts(Arc<crate::hosts::SignInHosts>),
}

pub struct WindowConfig {
    pub request: SignInRequest,
    pub bound: Bound,
    /// The native worker beside the app; the common config retains the Linux field's name.
    pub extension: PathBuf,
    pub engine: EngineSource,
    pub setup: GuardSetup,
}

struct Pinned {
    path: PathBuf,
    _file: File,
}
static WORKER: OnceLock<Pinned> = OnceLock::new();

/// Locate and pin the native worker against replacement for this application's lifetime.
#[must_use]
pub fn extension_beside(exe: &Path) -> Option<PathBuf> {
    if let Some(worker) = WORKER.get() {
        return Some(worker.path.clone());
    }
    let path = exe.parent()?.join(EXTENSION_FILE);
    // FILE_SHARE_READ only: another process cannot replace, delete or write the worker while it is pinned.
    let file = OpenOptions::new().read(true).share_mode(1).open(&path).ok()?;
    let _ = WORKER.set(Pinned { path, _file: file });
    WORKER.get().map(|worker| worker.path.clone())
}

/// Hold the worker from app startup, so an update cannot replace it underneath this run.
///
/// # Errors
/// The installed native worker is missing or cannot be pinned.
pub fn pin_worker() -> io::Result<()> {
    extension_beside(&std::env::current_exe()?)
        .map(|_| ())
        .ok_or_else(|| io::Error::other("native sign-in worker missing"))
}

/// Sign-in is a native worker window; its STA/message loop never blocks GPUI's foreground.
#[derive(Clone)]
pub struct Windows {
    app_id: String,
}
impl Windows {
    #[must_use]
    pub fn serve(app_id: &str, _cx: &mut App) -> Self {
        Self::new(app_id)
    }

    #[must_use]
    pub fn new(app_id: &str) -> Self {
        Self {
            app_id: app_id.to_owned(),
        }
    }

    /// # Errors
    /// The installed `WebView2` runtime is missing.
    pub fn engine_source(&self, _data_dir: &Path) -> Result<EngineSource, SignInAnswer> {
        browser::runtime().map(|()| EngineSource).map_err(|_| {
            SignInAnswer::Failed("Install or repair the Microsoft Edge WebView2 Runtime, then sign in again.".into())
        })
    }

    #[must_use]
    pub fn ask(&self, config: WindowConfig) -> SignInAnswer {
        ask(config, &self.app_id).unwrap_or_else(|_| {
            SignInAnswer::Failed("The sign-in window could not finish. Close it and try again.".into())
        })
    }
}

struct Worker {
    job: Job,
    child: Child,
    input: std::io::PipeWriter,
    output: BufReader<std::io::PipeReader>,
    // Must outlive every browser subprocess; dropped after explicit tree cleanup.
    directory: tempfile::TempDir,
}
impl Worker {
    fn start(path: &Path, authorize: String, proxy: String, app_id: &str) -> io::Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix(&format!("ngteams-signin-{}-", std::process::id()))
            .tempdir()?;
        private::restrict_dir(directory.path())?;
        let (input, to_worker) = io::pipe()?;
        let (output, from_worker) = io::pipe()?;
        let null = OpenOptions::new().read(true).write(true).open("NUL")?;
        let job = Job::new()?;
        let transfer = Transfer::with_standard_streams([input.as_handle(), from_worker.as_handle(), null.as_handle()])?;
        let child = transfer.spawn_in_job(path, &["--guarded-signin".into()], &job)?;
        drop((transfer, input, from_worker));
        let mut worker = Self {
            job,
            child,
            input: to_worker,
            output: BufReader::new(output),
            directory,
        };
        wire::send(
            &mut worker.input,
            &wire::Start {
                identity: wire::identity(),
                app_id: app_id.to_owned(),
                authorize,
                proxy,
                directory: worker.directory.path().to_owned(),
            },
        )?;
        Ok(worker)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.job.terminate(0);
        let _ = self.child.wait(Duration::from_secs(5));
        // Retain the directory rather than deleting browser state while a native query is failing.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while self.job.active_processes().ok() != Some(0) {
            if std::time::Instant::now() >= deadline {
                let path = self.directory.path().to_owned();
                if let Ok(replacement) = tempfile::tempdir() {
                    let _ = std::mem::replace(&mut self.directory, replacement).keep();
                }
                log::warn!("sign-in cleanup retained a private profile: {}", path.display());
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if let Err(error) = profile::remove(self.directory.path()) {
            log::warn!(
                "sign-in cleanup retained a private profile (OS error {:?})",
                error.raw_os_error()
            );
        }
    }
}

fn ask(config: WindowConfig, app_id: &str) -> io::Result<SignInAnswer> {
    let Bound::Hosts(hosts) = config.bound;
    let redirect =
        RedirectUri::parse(&config.request.redirect_uri).map_err(|_| io::Error::other("invalid redirect"))?;
    if !hosts.admits_url(&config.request.authorize) {
        return Err(io::Error::other("authorize host refused"));
    }
    let (events, hearing) = mpsc::channel();
    let guard = Arc::new((config.setup)(Guard::new(Arc::clone(&hosts), redirect.clone(), events)));
    guard.answer("armed 1 1");
    let proxy = crate::egress::HostGate::start(Arc::clone(&guard), Arc::clone(&hosts))?.uri();
    let mut worker = Worker::start(&config.extension, config.request.authorize, proxy, app_id)?;
    let mut refused = None;
    loop {
        let event: wire::Event = wire::read(&mut worker.output)?;
        let allow = match event {
            wire::Event::Request { method, url } => guard.decide(1, &method, &url) == Decision::Allow,
            wire::Event::Navigation(url) => {
                if redirect.matches(&url) {
                    guard.caught_navigation(&url);
                    wire::send(&mut worker.input, &false)?;
                    return Ok(SignInAnswer::Redirect(teams_auth::secrets::Secret::new(url)));
                }
                let allowed = hosts.admits_url(&url);
                if !allowed {
                    guard.refuse("GET", &url);
                    refused = hosts.idp_candidate(&url).or(refused);
                }
                allowed
            }
            wire::Event::NetworkFailure(code) => {
                log::warn!("sign-in network load failed: {code}");
                continue;
            }
            wire::Event::LoadFailed(code) => {
                return Ok(SignInAnswer::Failed(format!(
                    "The sign-in page could not load (WebView2 error {code}). Check your connection and try again."
                )));
            }
            wire::Event::Closed => return Ok(refused.map_or(SignInAnswer::Closed, SignInAnswer::Refused)),
            wire::Event::Failed(reason) => return Ok(SignInAnswer::Failed(reason)),
        };
        wire::send(&mut worker.input, &allow)?;
        while let Ok(event) = hearing.try_recv() {
            if let GuardEvent::Redirect(url) = event {
                return Ok(SignInAnswer::Redirect(url));
            }
        }
    }
}

/// Whether an offline test URL uses HTTP(S) on a numerical loopback address.
#[cfg(feature = "test-support")]
#[must_use]
pub fn is_local_page(text: &str) -> bool {
    url::Url::parse(text).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && match url.host() {
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                _ => false,
            }
    })
}

/// Private worker entry point; stdout carries protocol only, stderr is discarded by the parent.
pub fn worker() {
    let result = (|| {
        if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new("--guarded-signin")) {
            return Err("the sign-in worker must be started by ngteams");
        }
        let start: wire::Start =
            wire::read(&mut io::stdin().lock()).map_err(|_| "the sign-in worker's private pipe failed")?;
        if start.identity != wire::identity() {
            return Err("the sign-in worker and app versions differ; restart ngteams after updating");
        }
        browser::run(&start)
    })();
    if let Err(reason) = result {
        let _ = wire::send(&mut io::stdout().lock(), &wire::Event::Failed(reason.into()));
    }
}
