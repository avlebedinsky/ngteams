//! `ngteams-signin`: the personal build's sign-in window (`docs/personal-build.md`, Choice C), `teams-signin`'s
//! window bound to Entra's sign-in hosts and the identity provider's hosts `accounts.toml` names for the account it signs
//! in, through its host gate. `ngteams` starts it from beside itself as `ngteams-signin --account <UPN>` (an
//! account `accounts.toml` holds; any other arguments, or an account it doesn't hold, are refused) with the request as
//! one line on stdin; the answer goes as one line on stdout, nothing else there. Its refusals come first (after stdout is taken for the answer), before the request is read and
//! before any window: a `test-support` build and one `scripts/install-personal.sh` didn't make. Its decisions go to the
//! personal `fence.log`.

use std::io;
use std::process::ExitCode;
use std::sync::Arc;

use ngteams::setup::Setup;
use ngteams::signin;
use teams_auth::signin::{WindowAnswer, WindowRequest, read_line};
use teams_fence::FenceLog;
use teams_fence::personal::PersonalAccount;
use teams_signin::cli;
use teams_signin::hosts::SignInHosts;
use teams_signin::window::{self, Bound, WindowConfig};

fn main() -> ExitCode {
    // stdout is the answer's alone from here: everything else printed goes to stderr
    let mut out = match cli::take_stdout() {
        Ok(out) => out,
        Err(error) => {
            eprintln!("ngteams-signin: can't take stdout for the answer: {error}");
            return ExitCode::FAILURE;
        }
    };
    let answer = match prepare() {
        Ok(config) => window::run(config),
        Err(reason) => WindowAnswer::Failed(reason),
    };
    let redirected = matches!(answer, WindowAnswer::Redirect(_));
    if let WindowAnswer::Failed(reason) = &answer {
        eprintln!("ngteams-signin: {reason}");
    }
    if let Err(error) = cli::write_answer(&mut out, &answer) {
        eprintln!("ngteams-signin: can't write the answer: {error}");
        return ExitCode::FAILURE;
    }
    if redirected {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn prepare() -> Result<WindowConfig, String> {
    if let Some(refusal) = ngteams::refusal() {
        return Err(refusal.into());
    }
    let upn = signin::window_account(std::env::args_os().skip(1))
        .ok_or("usage: ngteams-signin --account <UPN> (the request on stdin, the answer on stdout)")?;
    let setup = Setup::from_env().map_err(|error| error.to_string())?;
    let account = PersonalAccount::find(&setup.paths, &upn)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("{upn} isn't one of the accounts (accounts.toml): add it in Teams first"))?;
    account.check_not_lab(&setup.lab).map_err(|error| error.to_string())?;
    let hosts = SignInHosts::entra_with(&account.idp_hosts).map_err(|error| error.to_string())?;
    let line = read_line(io::stdin().lock())
        .map_err(|error| format!("the request can't be read: {error}"))?
        .ok_or("no request on stdin")?;
    let request = WindowRequest::parse(&line).map_err(|error| format!("the request: {error}"))?;
    if !hosts.admits_url(&request.authorize) {
        return Err("the authorize URL isn't on the sign-in window's hosts".into());
    }
    setup
        .paths
        .make_dirs()
        .map_err(|error| format!("the personal data dir: {error}"))?;
    let log =
        FenceLog::open(&setup.paths.fence_log()).map_err(|error| format!("fence.log can't be opened: {error}"))?;
    let exe = std::env::current_exe().map_err(|error| format!("where am I: {error}"))?;
    let extension = window::extension_beside(&exe)
        .ok_or_else(|| format!("no {} beside {}", window::EXTENSION_FILE, exe.display()))?;
    Ok(WindowConfig {
        request,
        bound: Bound::Hosts(Arc::new(hosts)),
        extension,
        log,
        setup: Box::new(|guard| guard),
    })
}
