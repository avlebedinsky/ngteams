//! Bounded output and process-tree ownership for a synthetic Windows window.
use super::SandboxResult;
use std::io::{BufRead as _, BufReader, Read as _};
use std::process::ExitStatus;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// A running client: its stdout lines as they come, its stderr once it exits. Killed on drop.
pub struct Running {
    child: teams_platform::command::Tree,
    lines: mpsc::Receiver<String>,
    stderr: Option<thread::JoinHandle<String>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.child.stop();
    }
}

/// Why no stdout line came.
#[derive(Debug, PartialEq, Eq)]
pub enum LineError {
    Timeout,
    /// stdout closed: the process exited (or closed it).
    Closed,
}

impl Running {
    /// Starts reading `child`'s piped stdout and stderr.
    ///
    /// # Errors
    /// The child's stdout or stderr isn't piped.
    pub fn new(mut child: teams_platform::command::Tree) -> SandboxResult<Self> {
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let mut stderr = child.stderr.take().ok_or("no stderr")?;
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = sender.send(line);
            }
        });
        let stderr = thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        Ok(Self {
            child,
            lines,
            stderr: Some(stderr),
        })
    }

    /// The next stdout line within `timeout`.
    ///
    /// # Errors
    /// None came in time, or stdout closed.
    pub fn line(&self, timeout: Duration) -> Result<String, LineError> {
        self.lines.recv_timeout(timeout).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => LineError::Timeout,
            mpsc::RecvTimeoutError::Disconnected => LineError::Closed,
        })
    }

    /// The first stdout line `wanted` accepts within `timeout`; the lines before it are skipped.
    ///
    /// # Errors
    /// None came in time, or stdout closed first.
    pub fn line_where(&self, timeout: Duration, wanted: impl Fn(&str) -> bool) -> Result<String, LineError> {
        let start = Instant::now();
        loop {
            let left = timeout.checked_sub(start.elapsed()).ok_or(LineError::Timeout)?;
            let line = self.line(left)?;
            if wanted(&line) {
                return Ok(line);
            }
        }
    }

    /// Everything the client wrote to stderr; it must have exited or been killed.
    pub fn take_stderr(&mut self) -> String {
        match self.stderr.take().map(thread::JoinHandle::join) {
            Some(Ok(text)) => text,
            Some(Err(_)) => "<stderr reader panicked>".to_owned(),
            None => "<stderr already taken>".to_owned(),
        }
    }

    /// Kills the client and returns `error` with its exit status and stderr attached.
    pub fn fail(&mut self, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        self.child.stop();
        let status = self
            .child
            .wait()
            .map_or_else(|wait| format!("wait failed: {wait}"), |status| status.to_string());
        let stderr = self.take_stderr();
        format!("{error} (client: {status})\n--- client stderr\n{stderr}").into()
    }

    /// Waits up to `deadline` for the exit; returns the status and stderr.
    ///
    /// # Errors
    /// Still running at the deadline (it's killed, and the error carries its stderr).
    pub fn finish(mut self, deadline: Duration) -> SandboxResult<(ExitStatus, String)> {
        let start = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break status;
            }
            if start.elapsed() >= deadline {
                return Err(self.fail(&format!("still running {deadline:?} after the close request")));
            }
            thread::sleep(Duration::from_millis(10));
        };
        self.child.stop();
        Ok((status, self.take_stderr()))
    }
}
