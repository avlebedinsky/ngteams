//! `teams-app`: the lab's app for one or more lab accounts in one window, and its credential broker (see the
//! library's docs).

use std::io;
use std::os::fd::RawFd;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use gpui::{App, WindowingModes};
use teams_app::app::{self, Launch, LaunchAccount};
use teams_app::calling;
use teams_app::cli::{Cli, Mode, ModeError};
use teams_app::guard;
use teams_app::secrets::{self, LazyKeyring};
use teams_app::trace::adopt_trace;
use teams_auth::broker::BrokerSecretStore;
use teams_fence::lab::{Alias, LabConfig, LabLocations, LabPaths};
use teams_fence::{CallFloor, Fence};
use teams_ui::{SettingsFile, ViewTrace};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.mode() {
        Ok(Mode::Secrets { alias, fd }) => broker(alias, fd),
        Ok(Mode::App { accounts, trace_fd }) => run_app(&accounts, trace_fd),
        // refused before any descriptor is taken over
        Err(error) => fail("app", &error),
    }
}

/// `teams-app secrets`: the socket taken over first, before anything opens a descriptor of its own.
fn broker(alias: Alias, fd: RawFd) -> ExitCode {
    let socket = match secrets::adopt_socket(fd) {
        Ok(socket) => socket,
        Err(error) => return fail("secrets", &error),
    };
    let config = match LabLocations::from_env().and_then(|locations| LabConfig::load(&locations)) {
        Ok(config) => config,
        Err(error) => return fail("secrets", &error),
    };
    let scope = secrets::lab_scope(&config, alias);
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => return fail("secrets", &format!("can't start the async runtime: {error}")),
    };
    let store = LazyKeyring::new(LabPaths::KEYRING_APPLICATION);
    match runtime.block_on(secrets::run(socket, &scope, &store, &mut io::stderr())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail("secrets", &format!("{error}; the socket is closed")),
    }
}

/// The app: refuses a `test-support` build, takes over its descriptors, refuses a window outside the UI sandbox,
/// builds each account's engine over its broker's socket, and runs the window until it closes.
fn run_app(accounts: &[(Alias, RawFd)], trace_fd: Option<RawFd>) -> ExitCode {
    if let Some(refusal) = guard::test_support_refusal() {
        return fail("app", &refusal);
    }
    if let Some(refusal) = guard::personal_refusal() {
        return fail("app", &refusal);
    }
    let launch = match prepare(accounts, trace_fd) {
        Ok(launch) => launch,
        Err(error) => return fail("app", &error),
    };
    let failed = std::rc::Rc::new(std::cell::Cell::new(false));
    let failed_in_app = std::rc::Rc::clone(&failed);
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        if let Err(failed) = app::launch(cx, launch) {
            eprintln!("teams-app: the window can't be opened: {}", failed.error);
            failed_in_app.set(true);
            // the engines' threads are joined before the app quits
            app::quit_after(failed.stopped, cx);
        }
    });
    if failed.get() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// One account's broker socket, taken over: the app's end, and a second descriptor of it to shut down if the engine's
/// shutdown overruns, to end an exchange still waiting on the broker.
struct Adopted {
    alias: Alias,
    socket: UnixStream,
    broker: UnixStream,
}

/// Everything before the window: the descriptors first (every broker's socket, then the trace), before anything opens
/// one of its own; then the sandbox check, the lab's config, the log, the calls' devices, and each account's fence
/// (one call floor for them all) and engine. The first account's engine that can't start fails the app; a later one's
/// is reported and the app runs without that account.
fn prepare(accounts: &[(Alias, RawFd)], trace_fd: Option<RawFd>) -> Result<Launch, String> {
    let sockets = accounts
        .iter()
        .map(|&(alias, fd)| Ok((alias, secrets::adopt_socket(fd).map_err(|error| error.to_string())?)))
        .collect::<Result<Vec<_>, String>>()?;
    let trace = match trace_fd {
        Some(fd) => ViewTrace::new(adopt_trace(fd).map_err(|error| error.to_string())?),
        None => ViewTrace::off(),
    };
    // a descriptor of our own only now: one made earlier could take the number of a descriptor named but not handed in
    let adopted = sockets
        .into_iter()
        .map(|(alias, socket)| adopted(alias, socket))
        .collect::<Result<Vec<_>, _>>()?;
    guard::window_allowed_here()?;
    let config = LabLocations::from_env()
        .and_then(|locations| LabConfig::load(&locations))
        .map_err(|error| error.to_string())?;
    let mut adopted = adopted.into_iter();
    let Some(first) = adopted.next() else {
        return Err(ModeError::Incomplete.to_string());
    };
    // the app's settings are the first account's file
    let settings = SettingsFile::new(config.paths().ui_settings_file(first.alias));
    // the log first, at the settings' level, so the engines' start is in it
    let diagnostics = app::install_log(
        config.paths().app_log_dir(first.alias),
        settings.load().settings.log_level,
        config.paths().fence_log(),
    );
    // the calls on the sandbox's PipeWire, opened once for every account; without it the app runs without calls
    let devices = match calling::CallDevices::connect(&config) {
        Ok(devices) => Some(devices),
        Err(error) => {
            eprintln!("teams-app: no calls: {error}");
            log::warn!("no calls: {error}");
            None
        }
    };
    // a video a message carries plays its sound on the calls' devices (silently without them)
    let playback = devices.as_ref().map(calling::CallDevices::playback).unwrap_or_default();
    let floor = CallFloor::new();
    let starting = Starting {
        config: &config,
        floor: &floor,
        devices: devices.as_ref(),
    };
    let first = starting.start(first)?;
    let others = adopted
        .filter_map(|account| {
            let alias = account.alias;
            starting
                .start(account)
                .inspect_err(|error| {
                    eprintln!("teams-app: lab account {alias} left out: {error}");
                    log::warn!("lab account {alias} left out: {error}");
                })
                .ok()
        })
        .collect();
    Ok(Launch {
        first,
        others,
        trace,
        settings,
        // the bus the app is given, read once: nothing else is ever connected to for notifications or the tray
        bus: teams_ui::bus::address_from_env(),
        diagnostics: Some(diagnostics),
        playback,
    })
}

/// `alias`'s broker socket, taken over, with a second descriptor of it ([`Adopted`]).
fn adopted(alias: Alias, socket: UnixStream) -> Result<Adopted, String> {
    let broker = socket
        .try_clone()
        .map_err(|error| format!("the broker's socket: {error}"))?;
    Ok(Adopted { alias, socket, broker })
}

/// What every account's engine starts with: the lab's config, the app's call floor, the calls' devices.
struct Starting<'a> {
    config: &'a LabConfig,
    floor: &'a CallFloor,
    devices: Option<&'a calling::CallDevices>,
}

impl Starting<'_> {
    /// `account`'s engine: its fence on the app's floor, its tokens through its broker, its calls on the app's devices.
    fn start(&self, account: Adopted) -> Result<LaunchAccount, String> {
        let Adopted { alias, socket, broker } = account;
        let fence = Fence::open_lab_account(alias)
            .map_err(|error| format!("the fence of lab account {alias} can't be opened: {error}"))?
            .sharing_calls(self.floor);
        let secrets = BrokerSecretStore::new(socket, LabPaths::KEYRING_APPLICATION)
            .map_err(|error| format!("the broker's socket of lab account {alias}: {error}"))?;
        let (engine_calls, app_calls) = self.devices.map(calling::CallDevices::account).unzip();
        let lab = teams_core::lab::start(self.config, alias, Arc::new(fence), secrets, true, engine_calls)
            .map_err(|error| app::start_failure(alias, &error))?;
        Ok(LaunchAccount {
            alias,
            broker: Some(broker),
            signed_out: Some(lab.session.watch_signed_out()),
            engine: lab.handle,
            calls: app_calls,
        })
    }
}

fn fail(what: &str, error: &dyn std::fmt::Display) -> ExitCode {
    eprintln!("teams-app {what}: {error}");
    ExitCode::FAILURE
}
