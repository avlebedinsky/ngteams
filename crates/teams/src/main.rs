//! `teams`: the personal build's app (see the library's docs). Its refusals come first, before anything else is read
//! or opened, the window included.

use std::cell::Cell;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, ExitCode};
use std::rc::Rc;

use clap::Parser as _;
use gpui::{App, AppContext as _, AsyncApp, Entity, WindowingModes};
use teams::accounts::{self, Runner, Started};
use teams::app::{self, Launch, SignInAgain};
use teams::calling::CallDevices;
use teams::cli::Cli;
use teams::forget::Forgot;
use teams::setup::Setup;
use teams_fence::CallFloor;
use teams_fence::personal::PersonalAccount;
use teams_ui::SettingsFile;
use teams_ui::account_prompt::{self, AccountPrompt, AccountPromptEvent};
use teams_ui::window::PERSONAL_APP_ID;

/// How the app ended.
enum Outcome {
    Quit,
    /// Signed in again after a sign-out: the process runs itself again, so a new engine starts.
    Restart,
}

fn main() -> ExitCode {
    if let Some(refusal) = teams::refusal() {
        eprintln!("teams: {refusal}");
        return ExitCode::FAILURE;
    }
    let cli = Cli::parse();
    match run(&cli) {
        Ok(Outcome::Quit) => ExitCode::SUCCESS,
        Ok(Outcome::Restart) => {
            let error = Command::new(std::env::current_exe().unwrap_or_else(|_| "teams".into()))
                .args(std::env::args_os().skip(1))
                .exec();
            eprintln!("teams: signed in again, but it can't start again ({error}): start it again yourself");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("teams: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<Outcome, String> {
    let setup = Setup::from_env().map_err(|error| error.to_string())?;
    if let Some(upn) = cli.forget.as_deref() {
        forget(&setup, upn)?;
        return Ok(Outcome::Quit);
    }
    if let Some(asked) = cli.account.as_deref() {
        // the shortcut: that account, through the sign-in window at once when it isn't signed in
        signed_in(&setup, asked)?;
    }
    let (ready, unready) = ready_accounts(&setup)?;
    if ready.is_empty() {
        return ask_account(setup, unready.first().map(|account| account.upn.clone()));
    }
    let file = SettingsFile::new(setup.paths.settings_file());
    let diagnostics = app::install_log(
        setup.paths.log_dir(),
        file.load().settings.log_level,
        setup.paths.fence_log(),
    );
    // the calls on the desktop's PipeWire, shared by every account; without it the app runs without calls
    let devices = match CallDevices::open(&setup.paths) {
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
            runner,
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

/// Every account's engine: the first one's must start (another `teams` running on it, say, ends the app); a later
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
/// screen, which asks again, and leaves no account behind; closing it quits.
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
    let signing = cx.background_spawn(async move { accounts::add(&setup, &upn) });
    cx.spawn(async move |cx: &mut AsyncApp| match signing.await {
        Ok(_) => {
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

/// The account `upn` (`--account`), signed in: added when it is new, then through the sign-in window first when it
/// isn't signed in yet (no object id, region or refresh token); one that doesn't go through leaves no account behind.
fn signed_in(setup: &Setup, upn: &str) -> Result<PersonalAccount, String> {
    let account = setup.ask_for(upn).map_err(|error| error.to_string())?;
    if account.is_signed_in() && accounts::ready(setup, &account)? {
        return Ok(account);
    }
    let signed = accounts::sign_in(setup, &account);
    if signed.is_err() {
        // a pending account only: one a sign-in named earlier stays
        let _ = setup.abandon(upn);
    }
    let signed = signed?;
    eprintln!("teams: signed in as {} (its tenant matched)", signed.upn);
    Ok(signed)
}

/// Once an account is signed out: its sign-in window again, off GPUI's foreground; a sign-in that goes through quits
/// the app (every account's engine shut down first: [`app::quit`]) to start it afresh, one that doesn't leaves the
/// account signed out.
fn sign_in_again(setup: Setup, account: PersonalAccount, restart: Rc<Cell<bool>>) -> SignInAgain {
    Rc::new(move |cx: &mut App| {
        let (setup, account, restart) = (setup.clone(), account.clone(), Rc::clone(&restart));
        let signing = cx.background_spawn(async move { accounts::sign_in(&setup, &account) });
        cx.spawn(async move |cx: &mut AsyncApp| match signing.await {
            Ok(_) => {
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

/// Signs out and forgets the account `upn` (see [`teams::forget`]): refused while Teams runs for it.
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
