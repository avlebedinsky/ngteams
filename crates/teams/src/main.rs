//! `teams`: the personal build's app (see the library's docs). Its refusals come first, before anything else is read
//! or opened, the window included.

use std::cell::Cell;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, ExitCode};
use std::rc::Rc;
use std::sync::Arc;

use clap::Parser as _;
use gpui::{App, AppContext as _, AsyncApp, Entity, WindowingModes};
use teams::KEYRING_APPLICATION;
use teams::app::{self, Launch, SignInAgain};
use teams::cli::Cli;
use teams::forget::Forgot;
use teams::setup::Setup;
use teams::signin::{self, SignInClients};
use teams_auth::secrets::LazyKeyring;
use teams_fence::Fence;
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
    if cli.forget {
        forget(&setup)?;
        return Ok(Outcome::Quit);
    }
    let account = match cli.account.as_deref() {
        // the shortcut: that account, through the sign-in window at once when it isn't signed in
        Some(asked) => signed_in(&setup, setup.account(Some(asked)).map_err(|error| error.to_string())?)?,
        None => match ready_account(&setup)? {
            Some(account) => account,
            None => return ask_account(setup),
        },
    };
    let fence = Arc::new(Fence::open_personal(&setup.paths).map_err(|error| error.to_string())?);
    let file = SettingsFile::new(setup.paths.settings_file());
    let diagnostics = app::install_log(
        setup.paths.log_dir(),
        file.load().settings.log_level,
        setup.paths.fence_log(),
    );
    // the calls on the desktop's PipeWire; without it the app runs without calls
    let (engine_calls, app_calls) = match teams::calling::open(&setup.paths) {
        Ok((engine_calls, app_calls)) => (Some(engine_calls), Some(app_calls)),
        Err(error) => {
            eprintln!("teams: no calls: {error}");
            log::warn!("no calls: {error}");
            (None, None)
        }
    };
    let engine = teams::engine::start(
        &setup.paths,
        &account,
        fence,
        LazyKeyring::new(KEYRING_APPLICATION),
        true,
        engine_calls,
    )
    .map_err(|error| error.to_string())?;
    let restart = Rc::new(Cell::new(false));
    let failed = Rc::new(Cell::new(false));
    let (restart_in_app, failed_in_app) = (Rc::clone(&restart), Rc::clone(&failed));
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        let again = sign_in_again(setup, account, restart_in_app);
        let launch = Launch {
            signed_out: engine.session.watch_signed_out(),
            engine: engine.handle,
            settings: app::settings(file, Some(diagnostics)),
            // the desktop's own session bus: the notifications, the tray and nothing else
            bus: teams_ui::bus::address_from_env(),
            sign_in_again: again,
            calls: app_calls,
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
    Ok(if restart.get() { Outcome::Restart } else { Outcome::Quit })
}

/// The account a sign-in named, when it can start without a sign-in; `None` when the first sign-in screen asks.
fn ready_account(setup: &Setup) -> Result<Option<PersonalAccount>, String> {
    let Some(account) = setup.signed_in_account().map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let store = LazyKeyring::new(KEYRING_APPLICATION);
    let ready = runtime()?
        .block_on(signin::ready(setup, &account, &store))
        .map_err(|error| error.to_string())?;
    Ok(ready.then_some(account))
}

/// The first sign-in screen (`teams_ui::account_prompt`): asks for the work account, signs it in through the sign-in
/// window, and once that goes through the app starts afresh (`main`: the process runs itself again) with the account
/// signed in. A sign-in that stores nothing is said on the screen, which asks again; closing it quits.
fn ask_account(setup: Setup) -> Result<Outcome, String> {
    let held = setup.held_upn().map_err(|error| error.to_string())?;
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

/// Signs `upn` in from the first sign-in screen, off GPUI's foreground: its account in `account.toml`
/// (`Setup::ask_for`), then the sign-in window. One that goes through quits the app to start it afresh; one that
/// doesn't is said on `prompt`.
fn sign_in_from_prompt(
    setup: Setup,
    upn: String,
    prompt: Entity<AccountPrompt>,
    restart: Rc<Cell<bool>>,
    cx: &mut App,
) {
    let signing = cx.background_spawn(async move {
        let account = setup.ask_for(&upn).map_err(|error| error.to_string())?;
        runtime()?.block_on(async {
            let store = LazyKeyring::new(KEYRING_APPLICATION);
            sign_in(&setup, &account, &store).await
        })
    });
    cx.spawn(async move |cx: &mut AsyncApp| match signing.await {
        Ok(_) => {
            restart.set(true);
            cx.update(|cx| cx.quit());
        }
        Err(error) => {
            eprintln!("teams: {error}");
            cx.update(|cx| prompt.update(cx, |prompt, cx| prompt.failed(error, cx)));
        }
    })
    .detach();
}

/// The account, signed in: through the sign-in window first when it isn't yet (no object id, region or refresh token).
fn signed_in(setup: &Setup, account: PersonalAccount) -> Result<PersonalAccount, String> {
    let runtime = runtime()?;
    runtime.block_on(async {
        let store = LazyKeyring::new(KEYRING_APPLICATION);
        if signin::ready(setup, &account, &store)
            .await
            .map_err(|error| error.to_string())?
        {
            return Ok(account);
        }
        sign_in(setup, &account, &store).await
    })
}

async fn sign_in(setup: &Setup, account: &PersonalAccount, store: &LazyKeyring) -> Result<PersonalAccount, String> {
    let clients = SignInClients::live().map_err(|error| error.to_string())?;
    let window = signin::window_command().map_err(|error| error.to_string())?;
    let signed = signin::sign_in(setup, account, &clients, store, window)
        .await
        .map_err(|error| format!("the sign-in stored nothing: {error}"))?;
    eprintln!("teams: signed in as {} (its tenant matched)", signed.upn);
    Ok(signed)
}

/// Once signed out: the sign-in window again, off GPUI's foreground; a sign-in that goes through quits the app to start
/// it afresh, one that doesn't leaves it signed out.
fn sign_in_again(setup: Setup, account: PersonalAccount, restart: Rc<Cell<bool>>) -> SignInAgain {
    Rc::new(move |cx: &mut App| {
        let (setup, account, restart) = (setup.clone(), account.clone(), Rc::clone(&restart));
        let signing = cx.background_spawn(async move {
            let runtime = runtime()?;
            runtime.block_on(async {
                let store = LazyKeyring::new(KEYRING_APPLICATION);
                sign_in(&setup, &account, &store).await
            })
        });
        cx.spawn(async move |cx: &mut AsyncApp| match signing.await {
            Ok(_) => {
                restart.set(true);
                cx.update(|cx| cx.quit());
            }
            Err(error) => {
                eprintln!("teams: {error}; quit and start Teams again to sign in");
                log::warn!("the sign-in after a sign-out stored nothing");
            }
        })
        .detach();
    })
}

/// Signs out and forgets the account (see [`teams::forget`]): refused while Teams runs for it.
fn forget(setup: &Setup) -> Result<(), String> {
    let store = LazyKeyring::new(KEYRING_APPLICATION);
    let forgot = runtime()?
        .block_on(teams::forget::forget(setup, &store))
        .map_err(|error| error.to_string())?;
    match forgot {
        Forgot::Nothing => eprintln!("teams: no account to forget"),
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

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("can't start the async runtime: {error}"))
}
