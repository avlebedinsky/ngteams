//! `ngteams --prepare`: the installer runs it once, after it puts new program files in place, so that the first start
//! of the app does not do this work. Microsoft Defender checks each new program file the first time it runs or
//! loads. For an unsigned file, one check can take 10 s or more (a cloud lookup). This run starts the app's programs
//! and loads all `GStreamer` plugins once, which also brings `GStreamer`'s registry up to date. Defender keeps its
//! result for each file, also after a restart of Windows.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long the run can take before it stops: the installer waits for it.
pub const LIMIT: Duration = Duration::from_secs(120);

/// One part of the run, by its name.
pub struct Step {
    pub name: &'static str,
    pub work: Box<dyn FnOnce() -> Result<(), String> + Send>,
}

/// The parts of the run for this platform: on Windows, the sign-in worker started once; then `GStreamer` started.
#[must_use]
pub fn steps() -> Vec<Step> {
    let mut steps = Vec::new();
    #[cfg(windows)]
    steps.push(Step {
        name: "the sign-in worker",
        work: Box::new(|| {
            let app = std::env::current_exe().map_err(|error| error.to_string())?;
            let worker = teams_signin::window::extension_beside(&app).ok_or("it is not beside the app")?;
            run_once(&worker)
        }),
    });
    steps.push(Step {
        name: "GStreamer",
        work: Box::new(|| teams_media::player::prepare().map_err(|failure| format!("{failure:?}"))),
    });
    steps
}

/// Does `steps` in order on a thread of their own, also the steps after one that failed or panicked, and stops
/// waiting after `limit`.
///
/// # Errors
/// The steps that failed, or that the steps were not done after `limit`.
pub fn run(steps: Vec<Step>, limit: Duration) -> Result<(), String> {
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("prepare".into())
        .spawn(move || {
            let failed: Vec<String> = steps
                .into_iter()
                .filter_map(|step| {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(step.work))
                        .unwrap_or_else(|_| Err("it panicked".into()));
                    result.err().map(|error| format!("{}: {error}", step.name))
                })
                .collect();
            let _ = done.send(failed);
        })
        .map_err(|error| error.to_string())?;
    match finished.recv_timeout(limit) {
        Ok(failed) if failed.is_empty() => Ok(()),
        Ok(failed) => Err(failed.join("; ")),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(format!("not done after {} s", limit.as_secs())),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err("the steps stopped before their end".into()),
    }
}

/// Puts this process in a job that Windows stops when this process ends. Thus a program that a step started (the
/// sign-in worker, the `GStreamer` plugin scanner) does not stay after the run, also when the run stops at its limit:
/// a program that stays in the app's folder stops a later update and the uninstall.
///
/// # Errors
/// Windows cannot make the job or put this process in it.
#[cfg(windows)]
pub fn end_children_with_this_run() -> Result<(), String> {
    let job = teams_platform::windows_job::Job::new().map_err(|error| error.to_string())?;
    job.add_this_process().map_err(|error| error.to_string())?;
    // the last handle closes only when this process ends: then Windows stops the programs that are still in the job
    std::mem::forget(job);
    Ok(())
}

/// Runs `program` with no arguments, input or output, and waits for it to end; its exit code does not matter.
///
/// # Errors
/// The program cannot start.
pub fn run_once(program: &Path) -> Result<(), String> {
    Command::new(program)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|_| ())
        .map_err(|error| format!("{}: {error}", program.display()))
}

#[cfg(test)]
#[path = "../tests/support/prepare.rs"]
mod tests;
