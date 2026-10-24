//! `teams`: the personal build's app (see the library's docs). Its refusals come first, before anything else is read
//! or opened, the window included.

use std::cell::Cell;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, ExitCode};
use std::rc::Rc;
use std::sync::Arc;

use clap::Parser as _;
use gpui::{App, AppContext as _, AsyncApp, WindowingModes};
use teams::KEYRING_APPLICATION;
use teams::app::{self, Launch, SignInAgain};
use teams::cli::Cli;
use teams::setup::Setup;
use teams::signin::{self, SignInClients};
use teams_auth::authz::{skypetoken_expiry_key, skypetoken_key};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{LazyKeyring, SecretStore};
use teams_fence::Fence;
use teams_fence::personal::PersonalAccount;
use teams_ui::SettingsFile;

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
    let account = setup
        .account(cli.account.as_deref())
        .map_err(|error| error.to_string())?;
    let account = signed_in(&setup, account)?;
    let fence = Arc::new(Fence::open_personal(&setup.paths).map_err(|error| error.to_string())?);
    let file = SettingsFile::new(setup.paths.settings_file());
    let diagnostics = app::install_log(
        setup.paths.log_dir(),
        file.load().settings.log_level,
        setup.paths.fence_log(),
    );
    let engine = teams::engine::start(
        &setup.paths,
        &account,
        fence,
        LazyKeyring::new(KEYRING_APPLICATION),
        true,
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

/// Signs out and forgets the account (see `cli`).
fn forget(setup: &Setup) -> Result<(), String> {
    let account = PersonalAccount::load(&setup.paths).map_err(|error| error.to_string())?;
    let Some(account) = account else {
        eprintln!("teams: no account to forget");
        return Ok(());
    };
    runtime()?.block_on(async {
        let store = LazyKeyring::new(KEYRING_APPLICATION);
        for key in [
            refresh_token_key(&account.upn),
            skypetoken_key(&account.upn),
            skypetoken_expiry_key(&account.upn),
        ] {
            store.delete(&key).await.map_err(|error| error.to_string())?;
        }
        Ok::<(), String>(())
    })?;
    for file in [setup.paths.account_file(), setup.paths.region_file()] {
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", file.display())),
        }
    }
    eprintln!(
        "teams: {} is forgotten; its store stays at {} (delete it by hand)",
        account.upn,
        setup.paths.store_file().display()
    );
    Ok(())
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("can't start the async runtime: {error}"))
}
