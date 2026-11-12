//! Only the explicitly authorized win11 VM console may run a GUI. Build supervision is display-free.
use clap::{Parser, Subcommand};
use private::Open;
use std::ffi::OsString;
use std::io;
use std::os::windows::io::{AsHandle as _, BorrowedHandle};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use teams_auth::broker::{BrokerStream, private_pair};
use teams_platform::{private, windows_process::Transfer, windows_sandbox};

mod supervisor;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Create a private run directory before PowerShell opens any log in it.
    Prepare {
        #[arg(long)]
        directory: PathBuf,
    },
    /// Supervise the normal lab build, never an install-marked build. Cargo must be an absolute native executable.
    Build {
        #[arg(long)]
        cargo: PathBuf,
        #[arg(long, default_value_t = 1800, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },
    /// Foreground supervisor. The PowerShell wrapper places this in the VM's interactive console.
    Run {
        #[arg(long)]
        app: PathBuf,
        #[arg(long)]
        directory: PathBuf,
        #[arg(long, conflicts_with = "account", required_unless_present = "account", value_parser = alias)]
        alias: Option<String>,
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value_t = 1800, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },
}

fn alias(value: &str) -> Result<String, String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err("an alias contains only ASCII letters, digits, '_' and '-'".into());
    }
    Ok(value.to_owned())
}

pub fn run() -> io::Result<u8> {
    execute(Cli::parse().command)
}

fn execute(action: Action) -> io::Result<u8> {
    execute_checked(action, || {
        windows_sandbox::window_allowed_here()?;
        Ok(std::env::var_os(windows_sandbox::DIRECTORY_ENV))
    })
}

// The production guard supplies the authorized console directory. Offline tests supply a synthetic directory
// and executable, exercising dispatch without opening a GUI or changing this process's environment.
fn execute_checked(action: Action, console: impl FnOnce() -> io::Result<Option<OsString>>) -> io::Result<u8> {
    match action {
        Action::Prepare { directory } => {
            if !directory.is_absolute() {
                return Err(io::Error::other("run directory must be absolute"));
            }
            private::create_dir_all(&directory)?;
            private::restrict_dir(&directory)?;
            Ok(0)
        }
        Action::Build { cargo, timeout } => {
            if !lab_build(
                std::env::var_os("TEAMS_LAB_BUILD").as_deref(),
                std::env::var_os("TEAMS_PERSONAL_INSTALL").as_deref(),
            ) {
                return Err(io::Error::other("build only through scripts/run-lab.ps1"));
            }
            let args = [
                "build",
                "--locked",
                "-p",
                "ngteams",
                "--bin",
                "ngteams",
                "-p",
                "teams-signin",
                "--bin",
                "ngteams-webview",
                "--target-dir",
                "target/lab",
            ]
            .map(OsString::from);
            supervisor::command(&cargo, &args, Duration::from_secs(timeout)).map(|code| u8::try_from(code).unwrap_or(1))
        }
        Action::Run {
            app,
            directory,
            alias,
            account,
            timeout,
        } => {
            if console()?.as_deref() != Some(directory.as_os_str()) {
                return Err(io::Error::other("the runner directory must equal UI_SANDBOX_DIR"));
            }
            supervise(
                &app,
                &directory,
                alias.as_deref(),
                account.as_deref(),
                Duration::from_secs(timeout),
            )
        }
    }
}

fn endpoint(stream: &BrokerStream) -> BorrowedHandle<'_> {
    let BrokerStream::NamedPipe(stream) = stream;
    stream.as_handle()
}

fn lab_build(lab: Option<&std::ffi::OsStr>, installed: Option<&std::ffi::OsStr>) -> bool {
    lab == Some(std::ffi::OsStr::new("1")) && installed.is_none()
}

fn home() -> io::Result<PathBuf> {
    home_at(std::env::var_os("LOCALAPPDATA"))
}

fn home_at(local: Option<OsString>) -> io::Result<PathBuf> {
    local
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| io::Error::other("LOCALAPPDATA must be an absolute path"))
}

fn supervise(
    app: &Path,
    directory: &Path,
    alias: Option<&str>,
    account: Option<&str>,
    timeout: Duration,
) -> io::Result<u8> {
    private::restrict_dir(directory)?;
    let shared = home()?.join("teams-lab");
    private::create_dir_all(&shared)?;
    // The lock name is global to this user's lab, shared across all worktrees and console sessions.
    let _live = alias
        .map(|_| private::share_lock(&shared.join("live.lock"), true))
        .transpose()?;
    let _alias = private::share_lock(&directory.join("holder.lock"), false)?;
    let stop = directory.join("stop");
    match std::fs::remove_file(&stop) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let snapshot = directory.join("snapshot");
    let started = Instant::now();
    let result = (|| {
        if account.is_some() {
            if snapshot.try_exists()? {
                std::fs::remove_dir_all(&snapshot)?;
            }
            let code = supervisor::command(
                app,
                &["snapshot".into(), "--to".into(), snapshot.clone().into_os_string()],
                timeout,
            )?;
            if code != 0 {
                return Err(io::Error::other("personal snapshot failed"));
            }
        }
        session(
            app,
            directory,
            alias,
            account,
            timeout.saturating_sub(started.elapsed()),
        )
    })();
    if account.is_some() && snapshot.try_exists()? {
        std::fs::remove_dir_all(snapshot)?;
    }
    result
}

fn session(
    app: &Path,
    directory: &Path,
    alias: Option<&str>,
    account: Option<&str>,
    timeout: Duration,
) -> io::Result<u8> {
    let scope = match (alias, account) {
        (Some(alias), None) => [OsString::from("--lab"), alias.into()],
        (None, Some(account)) => [OsString::from("--account"), account.into()],
        _ => return Err(io::Error::other("select one lab alias or read-only personal account")),
    };
    let broker_job = supervisor::Tree::new()?;
    let app_job = supervisor::Tree::new()?;
    // Original endpoints stay alive here: the adopted handles validate the creating process's identity, including
    // after a restart. Neither endpoint, lock nor job handle is inherited by an ordinary WebView worker.
    let (broker_end, app_end) = private_pair()?;
    let trace = private::open(&directory.join("trace.jsonl"), Open::Append)?;
    let broker_transfer = Transfer::new(&[endpoint(&broker_end)])?;
    let mut broker_args = vec!["secrets".into()];
    broker_args.extend(scope.clone());
    broker_args.extend(["--fd".into(), handle(&broker_transfer, 0)?]);
    let broker = broker_transfer.spawn_in_job(app, &broker_args, &broker_job.0)?;
    drop(broker_transfer);
    let app_transfer = Transfer::new(&[endpoint(&app_end), trace.as_handle()])?;
    let mut app_args = scope.to_vec();
    if account.is_some() {
        app_args.extend([
            "--read-only".into(),
            "--data".into(),
            directory.join("snapshot").into_os_string(),
        ]);
    }
    app_args.extend([
        "--secrets-fd".into(),
        handle(&app_transfer, 0)?,
        "--trace-fd".into(),
        handle(&app_transfer, 1)?,
        "--ctl-socket".into(),
        directory.join("ctl.sock").into_os_string(),
    ]);
    let child = app_transfer.spawn_in_job(app, &app_args, &app_job.0)?;
    drop(app_transfer);
    println!("ready (VM console), app pid {}", child.id());
    let outcome = supervisor::observe(&app_job, &broker, &directory.join("stop"), timeout)?;
    // Process errors propagate when the original image exits without a surviving restart image.
    let code = if app_job.0.active_processes()? == 0 {
        supervisor::completed(&child, Duration::from_secs(10))?
    } else {
        0
    };
    Ok(if outcome == 0 {
        u8::try_from(code).unwrap_or(1)
    } else {
        outcome
    })
}

fn handle(transfer: &Transfer, index: usize) -> io::Result<OsString> {
    transfer
        .value(index)
        .map(|value| value.to_string().into())
        .ok_or_else(|| io::Error::other("missing transferred handle"))
}

#[cfg(test)]
#[path = "../tests/session.rs"]
mod tests;
