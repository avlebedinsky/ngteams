//! Launching the lab browser: Chrome for Testing with one user-data-dir, a remote-debugging port, headless or headed, and
//! optionally fake media devices.
//!
//! The debugging endpoint is read from the `DevToolsActivePort` file Chrome writes into its user-data-dir, so the port
//! can be left to Chrome (`--remote-debugging-port=0`) and tests never collide on a fixed one.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process_group};
use tokio::process::{Child, Command};

use crate::config::{Alias, LabPaths};

/// Flags every lab browser gets: no first-run UI, no Google background traffic (sync, component and field-trial
/// downloads), no extensions, no crash reporter, and the plain password store. The lab profiles were created with
/// `--password-store=basic` (Playwright's default), and that keeps Chrome away from the user's keyring.
pub const BASE_ARGS: &[&str] = &[
    "--no-first-run",
    "--no-default-browser-check",
    "--password-store=basic",
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-default-apps",
    "--disable-extensions",
    "--disable-field-trial-config",
    "--disable-sync",
    "--disable-breakpad",
    "--disable-crash-reporter",
];

/// Where the browser draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    /// No window at all (`--headless=new`). Tests and lab runs use this.
    Headless,
    /// A visible window, for a human watching a run.
    Headed,
}

/// The remote-debugging port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugPort {
    /// Chrome picks a free port (`0`) and reports it in `DevToolsActivePort`.
    Auto,
    Fixed(u16),
}

impl DebugPort {
    fn number(self) -> u16 {
        match self {
            Self::Auto => 0,
            Self::Fixed(port) => port,
        }
    }
}

/// Camera and microphone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Media {
    /// Whatever the system has (and a permission prompt).
    System,
    /// Chrome's fake camera and microphone, granted without a prompt. With `audio`, the microphone plays that WAV
    /// file instead of the default beep.
    Fake { audio: Option<PathBuf> },
}

/// How to launch one browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchOptions {
    pub user_data_dir: PathBuf,
    pub port: DebugPort,
    pub display: Display,
    pub media: Media,
}

impl LaunchOptions {
    /// Headless, on an automatic port, with system media.
    #[must_use]
    pub fn new(user_data_dir: PathBuf) -> Self {
        Self {
            user_data_dir,
            port: DebugPort::Auto,
            display: Display::Headless,
            media: Media::System,
        }
    }

    /// The persistent profile of one lab account. It is signed in to Microsoft: open it only under the armed fence.
    #[must_use]
    pub fn for_profile(paths: &LabPaths, alias: Alias) -> Self {
        Self::new(paths.profile_dir(alias))
    }

    /// The command-line arguments, starting page last (`about:blank`).
    #[must_use]
    pub fn args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = BASE_ARGS.iter().map(OsString::from).collect();
        args.push(flag("--user-data-dir=", self.user_data_dir.as_os_str()));
        args.push(format!("--remote-debugging-port={}", self.port.number()).into());
        if self.display == Display::Headless {
            args.push("--headless=new".into());
        }
        if let Media::Fake { audio } = &self.media {
            args.push("--use-fake-ui-for-media-stream".into());
            args.push("--use-fake-device-for-media-stream".into());
            if let Some(audio) = audio {
                args.push(flag("--use-file-for-fake-audio-capture=", audio.as_os_str()));
            }
        }
        args.push("about:blank".into());
        args
    }
}

fn flag(name: &str, value: &std::ffi::OsStr) -> OsString {
    let mut flag = OsString::from(name);
    flag.push(value);
    flag
}

/// The browser's debugging endpoint, from `DevToolsActivePort`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevToolsEndpoint {
    pub port: u16,
    /// `/devtools/browser/<id>`
    pub path: String,
}

impl DevToolsEndpoint {
    /// Parses `DevToolsActivePort`: the port on the first line, the browser target's path on the second.
    ///
    /// # Errors
    /// [`BrowserError::BadActivePort`] when either line is missing or malformed.
    pub fn parse(text: &str) -> Result<Self, BrowserError> {
        let bad = || BrowserError::BadActivePort(text.chars().take(200).collect());
        let mut lines = text.lines().map(str::trim);
        let port = lines
            .next()
            .and_then(|port| port.parse::<u16>().ok())
            .filter(|port| *port != 0)
            .ok_or_else(bad)?;
        let path = lines
            .next()
            .filter(|path| path.starts_with("/devtools/browser/"))
            .ok_or_else(bad)?;
        Ok(Self {
            port,
            path: path.to_owned(),
        })
    }

    /// The browser-level websocket URL.
    #[must_use]
    pub fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}{}", self.port, self.path)
    }
}

/// Why the browser couldn't be launched or stopped.
#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("can't start {}: {source}", executable.display())]
    Spawn {
        executable: PathBuf,
        source: std::io::Error,
    },
    #[error("can't prepare the profile {}: {source}", dir.display())]
    Profile { dir: PathBuf, source: std::io::Error },
    #[error("the browser exited before its debugging endpoint was up ({0})")]
    Exited(ExitStatus),
    #[error("the browser's debugging endpoint wasn't up within {0:?}")]
    Timeout(Duration),
    #[error("a malformed DevToolsActivePort: {0:?}")]
    BadActivePort(String),
    #[error("can't stop the browser: {0}")]
    Stop(#[source] std::io::Error),
}

const ACTIVE_PORT: &str = "DevToolsActivePort";
const POLL: Duration = Duration::from_millis(50);
/// How long a browser gets to exit on `SIGTERM` before it is killed.
const TERM_GRACE: Duration = Duration::from_secs(5);

/// The browser's process group: the browser leads its own, so its zygotes, renderers and crash handler go with it.
/// Dropping it kills the whole group; a `SIGKILL` to the browser alone would leave the crash handler behind.
struct Group(Option<Pid>);

impl Group {
    fn signal(&self, signal: Signal) {
        if let Some(group) = self.0 {
            // the group may be gone already (ESRCH); nothing else can fail for our own child
            let _ = kill_process_group(group, signal);
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.signal(Signal::KILL);
    }
}

/// A running browser. Dropping it kills the browser and every process it started.
pub struct Browser {
    child: Child,
    group: Group,
    endpoint: DevToolsEndpoint,
}

impl Browser {
    /// Starts `executable` with `options` and waits up to `timeout` for its debugging endpoint.
    ///
    /// # Errors
    /// When the profile dir can't be prepared, the process can't start or exits early, or the endpoint isn't up in
    /// time.
    pub async fn launch(executable: &Path, options: &LaunchOptions, timeout: Duration) -> Result<Self, BrowserError> {
        let dir = &options.user_data_dir;
        let profile_error = |source| BrowserError::Profile {
            dir: dir.clone(),
            source,
        };
        tokio::fs::create_dir_all(dir).await.map_err(profile_error)?;
        // a stale file from an earlier run would point at a dead port
        match tokio::fs::remove_file(dir.join(ACTIVE_PORT)).await {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(profile_error(error)),
            _ => {}
        }
        let mut child = Command::new(executable)
            .args(options.args())
            // crash dumps stay in the profile, not in ~/.config/google-chrome-for-testing
            .env("BREAKPAD_DUMP_LOCATION", dir.join("Crash Reports"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| BrowserError::Spawn {
                executable: executable.to_path_buf(),
                source,
            })?;
        // from here on, an early return kills the whole group
        let group = Group(child.id().and_then(|pid| Pid::from_raw(pid.cast_signed())));
        let endpoint = tokio::time::timeout(timeout, wait_for_endpoint(&mut child, dir))
            .await
            .map_err(|_| BrowserError::Timeout(timeout))??;
        Ok(Self { child, group, endpoint })
    }

    #[must_use]
    pub fn endpoint(&self) -> &DevToolsEndpoint {
        &self.endpoint
    }

    /// Waits up to `grace` for the browser to exit (after a CDP `Browser.close`), then asks it to stop (`SIGTERM`,
    /// which lets a profile flush its state), and kills it if it is still there a few seconds later. Whatever is left
    /// of its process group is killed after that.
    ///
    /// # Errors
    /// [`BrowserError::Stop`] when the process can't be reaped.
    pub async fn wait_or_kill(mut self, grace: Duration) -> Result<ExitStatus, BrowserError> {
        if let Ok(status) = tokio::time::timeout(grace, self.child.wait()).await {
            return status.map_err(BrowserError::Stop);
        }
        self.group.signal(Signal::TERM);
        if let Ok(status) = tokio::time::timeout(TERM_GRACE, self.child.wait()).await {
            return status.map_err(BrowserError::Stop);
        }
        self.group.signal(Signal::KILL);
        self.child.wait().await.map_err(BrowserError::Stop)
    }
}

async fn wait_for_endpoint(child: &mut Child, dir: &Path) -> Result<DevToolsEndpoint, BrowserError> {
    let file = dir.join(ACTIVE_PORT);
    loop {
        if let Some(status) = child.try_wait().map_err(BrowserError::Stop)? {
            return Err(BrowserError::Exited(status));
        }
        // Chrome writes the file whole once the port is open; a partial read parses as malformed and is retried
        if let Ok(text) = tokio::fs::read_to_string(&file).await
            && let Ok(endpoint) = DevToolsEndpoint::parse(&text)
        {
            return Ok(endpoint);
        }
        tokio::time::sleep(POLL).await;
    }
}
