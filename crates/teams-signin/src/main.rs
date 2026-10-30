//! `teams-signin --lab`: the lab's sign-in window, an app of its own in the UI sandbox (see the library's docs). The
//! request comes as one line on stdin, the answer goes as one line on stdout (`teams_auth::signin`); nothing else is
//! ever written there. The page's engine is this executable run again as its worker.

use std::io;
use std::process::ExitCode;

use teams_auth::signin::{WindowAnswer, WindowRequest, read_line};
use teams_fence::FenceLog;
use teams_fence::lab::LabLocations;
use teams_signin::cli::{self, NOT_LIVE};
use teams_signin::window::{self, Bound, WindowConfig};

fn main() -> ExitCode {
    // SAFETY: first thing in `main`, while the process has one thread.
    unsafe { teams_wpe::worker::serve_if_asked(|| {}) };
    // stdout is the answer's alone from here: everything else printed goes to stderr
    let mut out = match cli::take_stdout() {
        Ok(out) => out,
        Err(error) => {
            eprintln!("teams-signin: can't take stdout for the answer: {error}");
            return ExitCode::FAILURE;
        }
    };
    let answer = answer();
    let redirected = matches!(answer, WindowAnswer::Redirect(_));
    if let WindowAnswer::Failed(reason) = &answer {
        eprintln!("teams-signin: {reason}");
    }
    if let Err(error) = cli::write_answer(&mut out, &answer) {
        eprintln!("teams-signin: can't write the answer: {error}");
        return ExitCode::FAILURE;
    }
    if redirected {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn answer() -> WindowAnswer {
    match prepare() {
        Ok(config) => window::run(config),
        Err(reason) => WindowAnswer::Failed(reason),
    }
}

fn prepare() -> Result<WindowConfig, String> {
    cli::parse_args(std::env::args_os().skip(1))?;
    if let Some(refusal) = cli::test_support_refusal() {
        return Err(refusal.into());
    }
    if let Some(refusal) = cli::personal_refusal() {
        return Err(refusal.into());
    }
    let line = read_line(io::stdin().lock())
        .map_err(|error| format!("the request can't be read: {error}"))?
        .ok_or("no request on stdin")?;
    let request = WindowRequest::parse(&line).map_err(|error| format!("the request: {error}"))?;
    if !cli::is_local_page(&request.authorize) {
        return Err(NOT_LIVE.into());
    }
    if let Some(refusal) = cli::window_refusal(std::env::var_os("DISPLAY").as_deref()) {
        return Err(refusal);
    }
    let paths = LabLocations::from_env()
        .and_then(|locations| locations.lab_paths())
        .map_err(|error| format!("the lab's directories: {error}"))?;
    let log = FenceLog::open(&paths.fence_log()).map_err(|error| format!("fence.log can't be opened: {error}"))?;
    let exe = std::env::current_exe().map_err(|error| format!("where am I: {error}"))?;
    let extension = window::extension_beside(&exe)
        .ok_or_else(|| format!("no {} beside {}", window::EXTENSION_FILE, exe.display()))?;
    Ok(WindowConfig {
        request,
        bound: Bound::Loopback,
        extension,
        log,
        setup: Box::new(|guard| guard),
    })
}
