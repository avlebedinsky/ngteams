//! Windows Edge belongs to a kill-on-close job from its first instruction.
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use teams_platform::command::Tree;

use super::{ACTIVE_PORT, BrowserError, DevToolsEndpoint, Display, LaunchOptions, POLL};

pub struct Browser {
    child: Tree,
    endpoint: DevToolsEndpoint,
}

impl Browser {
    /// Starts only a headless lab browser in the authorized VM console.
    /// # Errors
    /// The console, private profile, job launch or endpoint check fails.
    pub async fn launch(executable: &Path, options: &LaunchOptions, timeout: Duration) -> Result<Self, BrowserError> {
        let profile_error = |source| BrowserError::Profile {
            dir: options.user_data_dir.clone(),
            source,
        };
        teams_platform::windows_sandbox::window_allowed_here().map_err(profile_error)?;
        if options.display != Display::Headless {
            return Err(profile_error(std::io::Error::other(
                "Windows lab sign-in requires a headless browser",
            )));
        }
        super::windows_edge::verify_edge(executable)
            .await
            .map_err(profile_error)?;
        let dir = &options.user_data_dir;
        teams_platform::private::create_dir_all(dir).map_err(profile_error)?;
        teams_platform::private::restrict_dir(dir).map_err(profile_error)?;
        match std::fs::remove_file(dir.join(ACTIVE_PORT)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(profile_error(error)),
            _ => {}
        }
        let mut command = Command::new(executable);
        command
            .args(options.args())
            .env("BREAKPAD_DUMP_LOCATION", dir.join("Crash Reports"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = Tree::spawn(&mut command).map_err(|source| BrowserError::Spawn {
            executable: executable.to_path_buf(),
            source,
        })?;
        let mut browser = Self {
            child,
            endpoint: DevToolsEndpoint {
                port: 0,
                path: String::new(),
            },
        };
        browser.endpoint = tokio::time::timeout(timeout, browser.wait_for_endpoint(dir))
            .await
            .map_err(|_| BrowserError::Timeout(timeout))??;
        Ok(browser)
    }

    async fn wait_for_endpoint(&mut self, dir: &Path) -> Result<DevToolsEndpoint, BrowserError> {
        loop {
            if let Some(status) = self.child.try_wait().map_err(BrowserError::Stop)? {
                return Err(BrowserError::Exited(status));
            }
            if let Ok(text) = tokio::fs::read_to_string(dir.join(ACTIVE_PORT)).await
                && let Ok(endpoint) = DevToolsEndpoint::parse(&text)
            {
                return Ok(endpoint);
            }
            tokio::time::sleep(POLL).await;
        }
    }

    #[must_use]
    pub fn endpoint(&self) -> &DevToolsEndpoint {
        &self.endpoint
    }

    /// Allows a CDP close to flush the profile before terminating the job.
    /// # Errors
    /// Reaping the browser fails.
    pub async fn wait_or_kill(mut self, grace: Duration) -> Result<ExitStatus, BrowserError> {
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            if let Some(status) = self.child.try_wait().map_err(BrowserError::Stop)? {
                return Ok(status);
            }
            if tokio::time::Instant::now() >= deadline {
                return self.kill().await;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Terminates this browser's job and reaps it.
    /// # Errors
    /// Reaping fails.
    pub async fn kill(self) -> Result<ExitStatus, BrowserError> {
        self.start_kill()?;
        self.reap().await
    }

    /// Terminates this browser's whole job.
    /// # Errors
    /// The job cannot be terminated.
    pub fn start_kill(&self) -> Result<(), BrowserError> {
        self.child.terminate().map_err(BrowserError::Signal)
    }

    /// Reaps a previously terminated browser.
    /// # Errors
    /// Reaping fails.
    pub async fn reap(mut self) -> Result<ExitStatus, BrowserError> {
        loop {
            if let Some(status) = self.child.try_wait().map_err(BrowserError::Stop)? {
                return Ok(status);
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

#[cfg(test)]
impl Browser {
    pub(crate) fn synthetic_process(child: Tree) -> Self {
        Self {
            child,
            endpoint: DevToolsEndpoint {
                port: 1,
                path: "/devtools/browser/synthetic".into(),
            },
        }
    }
}
