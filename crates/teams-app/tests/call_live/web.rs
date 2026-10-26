//! b's web client: a normally built `proto-lab run ab-call-app --headless --call <step>` (its path in
//! `PROTO_LAB_BIN`), spawned beside a's app and read by its progress lines (`ab-call-app: <step> at <ms>`). This
//! process opens no browser and sends nothing: the scenario arms its own fence, and its output is kept in the run's
//! dir. A run still going when this one fails is interrupted (SIGINT, as `proto-lab`'s stop handling expects: it closes
//! its browser and lets the fence's decisions land in `fence.log`), and killed only when it doesn't exit.

use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::Lab;

/// The prefix of the scenario's progress lines (`proto_lab::scenario::ab_call_app::PROGRESS`).
const PROGRESS: &str = "ab-call-app: ";
/// How long an interrupted run gets to close its browser and exit.
const INTERRUPTED_EXIT: Duration = Duration::from_secs(90);

/// A run of `ab-call-app`.
pub struct WebRun {
    child: Option<Child>,
    lines: mpsc::Receiver<String>,
    pub seen: Vec<String>,
    name: String,
    /// When it was started (Unix ms).
    pub started_ms: i64,
}

impl WebRun {
    /// Starts `proto-lab run ab-call-app --headless --call <step>`, its stdout kept in `<name>.out` (and read line by
    /// line), its stderr in `<name>.err`, in the run's dir.
    pub fn start(lab: &Lab, step: &str, name: &str) -> Self {
        let started_ms = super::common::now_ms();
        let mut child = Command::new(&lab.proto_lab)
            .args(["run", "ab-call-app", "--headless", "--call", step])
            .env_remove("TEAMS_HEADLESS_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(lab.out.join(format!("{name}.err"))).unwrap())
            .spawn()
            .unwrap_or_else(|error| panic!("can't start {}: {error}", lab.proto_lab.display()));
        let stdout = child.stdout.take().unwrap();
        let file: PathBuf = lab.out.join(format!("{name}.out"));
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            let mut kept = fs::File::create(file).unwrap();
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = writeln!(kept, "{line}");
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        Self {
            child: Some(child),
            lines,
            seen: Vec::new(),
            name: name.to_owned(),
            started_ms,
        }
    }

    /// Waits up to `limit` for the progress line of `step`; when it was reached (Unix ms).
    pub fn wait_step(&mut self, step: &str, limit: Duration) -> i64 {
        self.try_wait_step(step, limit)
            .unwrap_or_else(|| panic!("{}: no `{step}` within {limit:?}; it said {:#?}", self.name, self.seen))
    }

    /// [`Self::wait_step`], `None` when it didn't come in time (or the run ended without it).
    pub fn try_wait_step(&mut self, step: &str, limit: Duration) -> Option<i64> {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(at) = self.seen.iter().find_map(|line| step_at(line, step)) {
                return Some(at);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            match self.lines.recv_timeout(left.min(Duration::from_millis(250))) {
                Ok(line) => self.seen.push(line),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Waits up to `limit` for the run to exit by itself; its status (`None`: it was interrupted, then killed if it
    /// had to be) and everything it printed.
    pub fn finish(mut self, limit: Duration) -> (Option<ExitStatus>, Vec<String>) {
        let mut child = self.child.take().unwrap();
        let status = ui_sandbox::wait_until(limit, || child.try_wait().ok().flatten());
        let status = status.or_else(|| {
            stop(&mut child);
            None
        });
        while let Ok(line) = self.lines.recv_timeout(Duration::from_millis(500)) {
            self.seen.push(line);
        }
        (status, std::mem::take(&mut self.seen))
    }
}

/// When `line` says `step` was reached (Unix ms).
fn step_at(line: &str, step: &str) -> Option<i64> {
    let rest = line.strip_prefix(PROGRESS)?;
    let (named, at) = rest.split_once(" at ")?;
    (named == step).then(|| at.trim().parse().ok())?
}

/// SIGINT, then up to [`INTERRUPTED_EXIT`] for the exit, then SIGKILL.
fn stop(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: `pid` is a child this process spawned and hasn't reaped; `kill` touches no memory of ours.
        unsafe {
            libc::kill(pid, libc::SIGINT);
        }
    }
    if ui_sandbox::wait_until(INTERRUPTED_EXIT, || child.try_wait().ok().flatten()).is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for WebRun {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            eprintln!("{}: interrupted at the run's end", self.name);
            stop(&mut child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_is_read_from_its_progress_line_only() {
        assert_eq!(
            step_at("ab-call-app: ready at 1792988100172", "ready"),
            Some(1_792_988_100_172)
        );
        assert_eq!(step_at("ab-call-app: ready at 1792988100172", "saw"), None);
        assert_eq!(step_at("b: ready at 1", "ready"), None);
        assert_eq!(step_at("ab-call-app: ready", "ready"), None);
    }
}
