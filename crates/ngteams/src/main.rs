//! `ngteams`: desktop, read-only and lab startup. Build refusals come before anything is read or opened; application
//! mode is checked after the internal sign-in page worker has had its chance to serve.

use std::cell::Cell;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::rc::Rc;
use std::sync::Arc;

use clap::Parser as _;
use gpui::{App, AppContext as _, AsyncApp, Entity, WindowingModes};
#[cfg(not(test))]
use ngteams::accounts::{self, Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
#[cfg(not(test))]
use ngteams::app;
use ngteams::app::{Launch, SignInAgain};
use ngteams::calling::CallDevices;
use ngteams::cli::{Cli, Command as Subcommand, LabMode, ReadOnlyMode};
use ngteams::descriptors::{adopt_socket, adopt_trace};
use ngteams::forget::Forgot;
use ngteams::setup::Setup;
use ngteams::{KEYRING_APPLICATION, lab, read_only, secrets, snapshot};
use teams_auth::secrets::LazyKeyring;
use teams_core::CallFloor;
use teams_media::devices::pipewire::Reach;
#[cfg(not(test))]
use teams_ui::account_prompt;
use teams_ui::account_prompt::{AccountPrompt, AccountPromptEvent};
use teams_ui::window::PERSONAL_APP_ID;
use teams_ui::{SettingsFile, ViewTrace};

#[cfg(test)]
#[path = "../tests/support/lab_startup.rs"]
mod lab_startup_tests;
#[cfg(test)]
#[path = "../tests/support/startup.rs"]
mod startup_tests;
#[cfg(test)]
use startup_tests::accounts::{self, Runner, Started};
#[cfg(test)]
use startup_tests::{app, platform as gpui_platform, prompt as account_prompt, window_runner};

#[cfg(not(test))]
fn window_runner(runner: Runner) -> Runner {
    runner
}

/// How the app ended.
enum Outcome {
    Quit,
    /// Signed in again after a sign-out: the process runs itself again, so a new engine starts.
    Restart,
    /// Lab sign-in keeps the broker and trace available for this same process's next image.
    LabRestart(ngteams::restart::RestartDescriptors),
}

fn main() -> ExitCode {
    if let Some(refusal) = ngteams::refusal() {
        eprintln!("teams: {refusal}");
        return ExitCode::FAILURE;
    }
    // run again as the sign-in window's page engine (`teams_wpe::worker`), it serves that and ends here
    // SAFETY: before anything else but the refusal (which starts no thread), while the process has one thread.
    unsafe { teams_wpe::worker::serve_if_asked(|| {}) };
    let cli = Cli::parse();
    if let Some(refusal) = ngteams::refusal::refusal_for(cli.lab_requested()) {
        return failed("startup", &refusal);
    }
    match &cli.command {
        Some(Subcommand::Secrets { account, lab, fd }) => {
            return match (account.as_deref(), lab.as_deref()) {
                (Some(upn), None) => broker(upn, *fd),
                (None, Some(alias)) => lab_broker(alias, *fd),
                _ => failed("secrets", &"give exactly one of --account or --lab"),
            };
        }
        Some(Subcommand::Snapshot { to }) => return take_snapshot(to),
        None => {}
    }
    let outcome = match cli.read_only_mode(std::env::var_os(read_only::ENV).as_deref()) {
        Ok(Some(mode)) => run_read_only(&mode),
        Ok(None) => match cli.lab_mode() {
            Ok(Some(mode)) => run_lab(&cli, &mode),
            Ok(None) => run(&cli, Reach::Desktop),
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    match outcome {
        Ok(Outcome::Quit) => ExitCode::SUCCESS,
        Ok(outcome @ (Outcome::Restart | Outcome::LabRestart(_))) => {
            let mut command = Command::new(std::env::current_exe().unwrap_or_else(|_| "ngteams".into()));
            command.args(std::env::args_os().skip(1));
            let error = match outcome {
                Outcome::LabRestart(descriptors) => descriptors.exec(&mut command),
                _ => command.exec(),
            };
            eprintln!("teams: signed in again, but it can't start again ({error}): start it again yourself");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("teams: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli, reach: Reach) -> Result<Outcome, String> {
    let setup = Setup::from_env().map_err(|error| error.to_string())?;
    run_setup(cli, reach, setup, ViewTrace::off())
}

fn run_setup(cli: &Cli, reach: Reach, setup: Setup, trace: ViewTrace) -> Result<Outcome, String> {
    if let Some(upn) = cli.forget.as_deref() {
        forget(&setup, upn)?;
        return Ok(Outcome::Quit);
    }
    if let Some(asked) = cli.account.as_deref() {
        // the shortcut: that account, through the sign-in window at once when it isn't signed in
        let account = setup.ask_for(asked).map_err(|error| error.to_string())?;
        if !(account.is_signed_in() && accounts::ready(&setup, &account)?) {
            return sign_in_first(setup, account);
        }
    }
    let (ready, unready) = ready_accounts(&setup)?;
    if ready.is_empty() {
        if setup.secrets().application() == lab::LAB_KEYRING_APPLICATION {
            let account = lab::account(&setup.paths)?;
            return sign_in_first(setup, account);
        }
        return ask_account(setup, unready.first().map(|account| account.upn.clone()));
    }
    let file = SettingsFile::new(setup.paths.settings_file());
    let diagnostics = app::install_log(setup.paths.log_dir(), file.load().settings.log_level);
    // the selected PipeWire, shared by every account; without it the app runs without calls
    let devices = match CallDevices::open(reach) {
        Ok(devices) => Some(devices),
        Err(error) => {
            eprintln!("teams: no calls: {error}");
            log::warn!("no calls: {error}");
            None
        }
    };
    let runner = Runner {
        setup: setup.clone(),
        floor: CallFloor::new(),
        devices,
    };
    let (first, others) = start_accounts(&runner, &ready)?;
    let restart = Rc::new(Cell::new(false));
    let failed = Rc::new(Cell::new(false));
    let (restart_in_app, failed_in_app) = (Rc::clone(&restart), Rc::clone(&failed));
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        let (again_setup, again_restart) = (setup.clone(), Rc::clone(&restart_in_app));
        let launch = Launch {
            first,
            others,
            settings: app::settings(file, Some(diagnostics)),
            // the desktop's own session bus: the notifications, the tray and nothing else
            bus: teams_ui::bus::address_from_env(),
            sign_in_again: Rc::new(move |account| {
                sign_in_again(again_setup.clone(), account.clone(), Rc::clone(&again_restart))
            }),
            runner: window_runner(runner),
            read_only: false,
            trace: Arc::new(trace),
        };
        if let Err(failure) = app::launch(cx, launch) {
            eprintln!("teams: the window can't be opened: {}", failure.error);
            failed_in_app.set(true);
            app::quit_after(failure.stopped, cx);
            return;
        }
        // an account a sign-in named that can't start without one: its sign-in window, once the others run
        if let Some(account) = unready.into_iter().next() {
            sign_in_again(setup, account, restart_in_app)(cx);
        }
    });
    if failed.get() {
        return Err("the window couldn't be opened".to_owned());
    }
    Ok(if restart.get() { Outcome::Restart } else { Outcome::Quit })
}

/// `ngteams secrets --account <UPN> --fd <n>`: the read-only mode's credential broker (`ngteams::read_only`), its
/// socket taken over first, before anything opens a descriptor of its own; `get` only, of an account
/// `accounts.toml` holds.
fn broker(upn: &str, fd: RawFd) -> ExitCode {
    let socket = match adopt_socket(fd) {
        Ok(socket) => socket,
        Err(error) => return failed("secrets", &error),
    };
    let account = PersonalPaths::from_env()
        .map_err(|error| error.to_string())
        .and_then(|paths| PersonalAccount::find(&paths, upn).map_err(|error| error.to_string()));
    let account = match account {
        Ok(Some(account)) if account.is_signed_in() => account,
        Ok(_) => return failed("secrets", &format!("{upn} isn't one of your signed-in accounts")),
        Err(error) => return failed("secrets", &error),
    };
    let scope = read_only::broker_scope(&account.upn);
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => return failed("secrets", &format!("can't start the async runtime: {error}")),
    };
    let store = LazyKeyring::new(KEYRING_APPLICATION);
    match runtime.block_on(read_only::serve_broker(socket, &scope, &store, &mut std::io::stderr())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => failed("secrets", &format!("{error}; the socket is closed")),
    }
}

/// `ngteams snapshot --to <dir>`: the snapshot of the user's files the read-only mode runs on (`ngteams::snapshot`).
fn take_snapshot(to: &Path) -> ExitCode {
    let made = PersonalPaths::from_env()
        .map_err(|error| error.to_string())
        .and_then(|mine| snapshot::snapshot(&mine, to).map_err(|error| error.to_string()));
    match made {
        Ok(paths) => {
            println!("snapshot: {}", to.display());
            println!("config: {}", paths.config_dir().display());
            println!("data: {}", paths.data_dir().display());
            ExitCode::SUCCESS
        }
        Err(error) => failed("snapshot", &error),
    }
}

fn failed(what: &str, error: &dyn std::fmt::Display) -> ExitCode {
    eprintln!("teams {what}: {error}");
    ExitCode::FAILURE
}

/// A lab alias uses only its isolated files and a writable, account-scoped broker. The inherited descriptors are
/// adopted before logging, files or media can allocate a descriptor of their own.
fn run_lab(cli: &Cli, mode: &LabMode) -> Result<Outcome, String> {
    let socket = adopt_socket(mode.secrets_fd).map_err(|error| error.to_string())?;
    let trace = mode
        .trace_fd
        .map(adopt_trace)
        .transpose()
        .map_err(|error| error.to_string())?;
    let descriptors = ngteams::restart::RestartDescriptors::new(socket, trace);
    let trace = descriptors
        .trace()
        .map_err(|error| error.to_string())?
        .map_or_else(ViewTrace::off, ViewTrace::new);
    lab::window_allowed_here()?;
    let paths = lab::paths_from_env(&mode.alias)?;
    let socket = descriptors.socket().map_err(|error| error.to_string())?;
    let store = secrets::AppSecrets::lab(socket).map_err(|error| format!("the lab broker's socket: {error}"))?;
    let setup = Setup::new(paths)
        .map_err(|error| error.to_string())?
        .with_secrets(store);
    let account = lab::account(&setup.paths)?;
    for requested in [cli.account.as_deref(), cli.forget.as_deref()].into_iter().flatten() {
        if !requested.eq_ignore_ascii_case(&account.upn) {
            return Err(format!("lab alias {} is configured for {}", mode.alias, account.upn));
        }
    }
    match run_setup(cli, Reach::Sandbox, setup, trace)? {
        Outcome::Restart => Ok(Outcome::LabRestart(descriptors)),
        outcome => Ok(outcome),
    }
}

/// The alias's one account in its lab namespace. It may still be pending: the broker must be available for its first
/// sign-in as well as token refresh and forget.
fn lab_broker(alias: &str, fd: RawFd) -> ExitCode {
    let socket = match adopt_socket(fd) {
        Ok(socket) => socket,
        Err(error) => return failed("secrets", &error),
    };
    let account = lab::paths_from_env(alias).and_then(|paths| lab::account(&paths));
    let account = match account {
        Ok(account) => account,
        Err(error) => return failed("secrets", &error),
    };
    let runtime = match ngteams::accounts::runtime() {
        Ok(runtime) => runtime,
        Err(error) => return failed("secrets", &error),
    };
    let store = LazyKeyring::new(lab::LAB_KEYRING_APPLICATION);
    let scope = secrets::lab_broker_scope(&account.upn);
    match runtime.block_on(read_only::serve_broker(socket, &scope, &store, &mut std::io::stderr())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => failed("secrets", &format!("{error}; the socket is closed")),
    }
}

/// The read-only mode (`ngteams::read_only`): its descriptors taken over first, then the sandbox check, the snapshot
/// (apart from the user's own files), the account, its engine (read-only, no push channel, no calls) over the broker's
/// store, and the window; nothing adds, removes or signs in an account.
fn run_read_only(mode: &ReadOnlyMode) -> Result<Outcome, String> {
    let socket = adopt_socket(mode.secrets_fd).map_err(|error| error.to_string())?;
    let trace = match mode.trace_fd {
        Some(fd) => ViewTrace::new(adopt_trace(fd).map_err(|error| error.to_string())?),
        None => ViewTrace::off(),
    };
    read_only::window_allowed_here()?;
    let mine = PersonalPaths::from_env().map_err(|error| error.to_string())?;
    let paths = read_only::snapshot_paths(&mode.data, &mine)?;
    let setup = Setup::new(paths).map_err(|error| error.to_string())?;
    let account = setup
        .signed_in_accounts()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|account| account.upn.eq_ignore_ascii_case(&mode.account))
        .ok_or_else(|| format!("read-only: {} isn't a signed-in account of the snapshot", mode.account))?;
    let secrets = read_only::secrets(socket).map_err(|error| format!("the broker's socket: {error}"))?;
    let file = SettingsFile::new(setup.paths.settings_file());
    let diagnostics = app::install_log(setup.paths.log_dir(), file.load().settings.log_level);
    log::info!("read-only mode: nothing that writes is sent");
    let engine = ngteams::engine::start_read_only(&setup.paths, &account, CallFloor::new(), secrets)
        .map_err(|error| format!("{}: {error}", account.upn))?;
    let first = ngteams::accounts::Started {
        account,
        engine,
        calls: None,
    };
    let runner = Runner {
        setup,
        floor: CallFloor::new(),
        devices: None,
    };
    let failed = Rc::new(Cell::new(false));
    let failed_in_app = Rc::clone(&failed);
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        let launch = Launch {
            first,
            others: Vec::new(),
            settings: app::settings(file, Some(diagnostics)).read_only(),
            // the bus it is given (the sandbox's): the notifications, and no tray item
            bus: teams_ui::bus::address_from_env(),
            // it never signs in: a sign-out only says so
            sign_in_again: Rc::new(|_| Rc::new(|_| {})),
            runner: window_runner(runner),
            read_only: true,
            trace: Arc::new(trace),
        };
        if let Err(failure) = app::launch(cx, launch) {
            eprintln!("teams: the window can't be opened: {}", failure.error);
            failed_in_app.set(true);
            app::quit_after(failure.stopped, cx);
        }
    });
    if failed.get() {
        return Err("the window couldn't be opened".to_owned());
    }
    Ok(Outcome::Quit)
}

/// Every account's engine: the first one's must start (another `ngteams` running on it, say, ends the app); a later
/// one that can't is said and left out.
fn start_accounts(runner: &Runner, ready: &[PersonalAccount]) -> Result<(Started, Vec<Started>), String> {
    let (first, rest) = ready.split_first().ok_or("no account is signed in")?;
    let first = runner.start(first)?;
    let mut others = Vec::new();
    for account in rest {
        match runner.start(account) {
            Ok(started) => others.push(started),
            Err(error) => {
                eprintln!("teams: an account isn't started: {error}");
                log::warn!("an account isn't started");
            }
        }
    }
    Ok((first, others))
}

/// The accounts a sign-in named, in the order they were added: those that can start without a sign-in, and those
/// that can't (a refresh token or the region missing), said on stderr; the first of these signs in again once the
/// app runs, or on the first sign-in screen when none can start.
fn ready_accounts(setup: &Setup) -> Result<(Vec<PersonalAccount>, Vec<PersonalAccount>), String> {
    let (mut ready, mut unready) = (Vec::new(), Vec::new());
    for account in setup.signed_in_accounts().map_err(|error| error.to_string())? {
        if accounts::ready(setup, &account)? {
            ready.push(account);
        } else {
            eprintln!("teams: {} must sign in again", account.upn);
            unready.push(account);
        }
    }
    Ok((ready, unready))
}

/// The first sign-in screen (`teams_ui::account_prompt`): asks for the work account (`held`, or the last one typed
/// before, filled in), signs it in through the sign-in window, and once that goes through the app starts afresh
/// (`main`: the process runs itself again) with the account signed in. A sign-in that stores nothing is said on the
/// screen, which asks again; its entry goes as the rail's "+" leaves one ([`accounts::add`]: kept when the user wrote
/// its `idp_hosts` or the window refused an identity provider's host); closing it quits.
fn ask_account(setup: Setup, held: Option<String>) -> Result<Outcome, String> {
    let held = match held {
        Some(held) => Some(held),
        None => setup.pending_upns().map_err(|error| error.to_string())?.pop(),
    };
    let restart = Rc::new(Cell::new(false));
    let failed = Rc::new(Cell::new(false));
    let (restart_in_app, failed_in_app) = (Rc::clone(&restart), Rc::clone(&failed));
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        match account_prompt::open(cx, PERSONAL_APP_ID, app::TITLE.into(), held.as_deref()) {
            Ok((_, prompt)) => cx
                .subscribe(&prompt, move |prompt, event, cx| {
                    let AccountPromptEvent::SignIn(upn) = event;
                    sign_in_from_prompt(setup.clone(), upn.clone(), prompt, Rc::clone(&restart_in_app), cx);
                })
                .detach(),
            Err(error) => {
                eprintln!("teams: the window can't be opened: {error:#}");
                failed_in_app.set(true);
                cx.quit();
            }
        }
    });
    if failed.get() {
        return Err("the window couldn't be opened".to_owned());
    }
    Ok(if restart.get() { Outcome::Restart } else { Outcome::Quit })
}

/// Signs `upn` in from the first sign-in screen, off GPUI's foreground ([`accounts::add`]). One that goes through
/// quits the app to start it afresh; one that doesn't is said on `prompt`.
fn sign_in_from_prompt(
    setup: Setup,
    upn: String,
    prompt: Entity<AccountPrompt>,
    restart: Rc<Cell<bool>>,
    cx: &mut App,
) {
    let windows = ngteams::signin::windows(cx);
    let signing = cx.background_spawn(async move { accounts::add(&setup, &upn, &windows) });
    cx.spawn(async move |cx: &mut AsyncApp| match signing.await {
        Ok(_) => {
            log::info!("the first account was signed in: the app restarts");
            restart.set(true);
            cx.update(app::quit);
        }
        Err(error) => {
            eprintln!("teams: {error}");
            cx.update(|cx| prompt.update(cx, |prompt, cx| prompt.failed(error, cx)));
        }
    })
    .detach();
}

/// The selected account's sign-in window, alone. A successful sign-in restarts the app; a failed personal add
/// abandons its bare entry as the rail's "+" does ([`accounts::add`]). The lab identity was provisioned beforehand:
/// sign it in as an existing account so closing or failing its first window leaves it available for another try.
fn sign_in_first(setup: Setup, account: PersonalAccount) -> Result<Outcome, String> {
    let restart = Rc::new(Cell::new(false));
    let failure = Rc::new(std::cell::RefCell::new(None));
    let (restart_in_app, failure_in_app) = (Rc::clone(&restart), Rc::clone(&failure));
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        // the sign-in window is the only one: closing it mustn't quit before the sign-in is done
        cx.set_quit_mode(gpui::QuitMode::Explicit);
        let windows = ngteams::signin::windows(cx);
        let signing = cx.background_spawn(async move {
            if setup.secrets().application() == lab::LAB_KEYRING_APPLICATION {
                accounts::sign_in(&setup, &account, &windows)
            } else {
                accounts::add(&setup, &account.upn, &windows)
            }
        });
        cx.spawn(async move |cx: &mut AsyncApp| {
            match signing.await {
                Ok(signed) => {
                    eprintln!("teams: signed in as {} (its tenant matched)", signed.upn);
                    restart_in_app.set(true);
                }
                Err(error) => *failure_in_app.borrow_mut() = Some(error),
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    if let Some(error) = failure.borrow_mut().take() {
        return Err(error);
    }
    Ok(if restart.get() { Outcome::Restart } else { Outcome::Quit })
}

/// Once an account is signed out: its sign-in window again, off GPUI's foreground; a sign-in that goes through quits
/// the app (every account's engine shut down first: [`app::quit`]) to start it afresh, one that doesn't leaves the
/// account signed out.
fn sign_in_again(setup: Setup, account: PersonalAccount, restart: Rc<Cell<bool>>) -> SignInAgain {
    Rc::new(move |cx: &mut App| {
        let (setup, account, restart) = (setup.clone(), account.clone(), Rc::clone(&restart));
        let windows = ngteams::signin::windows(cx);
        let signing = cx.background_spawn(async move { accounts::sign_in(&setup, &account, &windows) });
        cx.spawn(async move |cx: &mut AsyncApp| match signing.await {
            Ok(_) => {
                log::info!("signed in again after a sign-out: the app restarts");
                restart.set(true);
                cx.update(app::quit);
            }
            Err(error) => {
                eprintln!("teams: {error}; quit and start Teams again to sign in");
                log::warn!("the sign-in after a sign-out stored nothing");
            }
        })
        .detach();
    })
}

/// Signs out and forgets the account `upn` (see [`ngteams::forget`]): refused while Teams runs for it.
fn forget(setup: &Setup, upn: &str) -> Result<(), String> {
    match accounts::forget(setup, upn)? {
        Forgot::Nothing => eprintln!("teams: {upn} isn't one of the accounts: nothing to forget"),
        Forgot::Account {
            upn,
            store: Some(store),
        } => eprintln!(
            "teams: {upn} is forgotten; its store stays at {} (delete it by hand)",
            store.display()
        ),
        Forgot::Account { upn, store: None } => eprintln!("teams: {upn} is forgotten"),
    }
    Ok(())
}
