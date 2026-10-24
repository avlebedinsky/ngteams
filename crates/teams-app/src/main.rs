//! `teams-app`: the lab's app for one lab account, and its credential broker (see the library's docs).

use std::io;
use std::os::fd::RawFd;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use gpui::{App, WindowingModes};
use teams_app::app::{self, Launch};
use teams_app::cli::{Cli, Mode};
use teams_app::guard;
use teams_app::secrets::{self, LazyKeyring};
use teams_app::trace::adopt_trace;
use teams_auth::broker::BrokerSecretStore;
use teams_fence::Fence;
use teams_fence::lab::{Alias, LabConfig, LabLocations, LabPaths};
use teams_ui::{SettingsFile, ViewTrace};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.mode() {
        Ok(Mode::Secrets { alias, fd }) => broker(alias, fd),
        Ok(Mode::App {
            alias,
            secrets_fd,
            trace_fd,
        }) => run_app(alias, secrets_fd, trace_fd),
        // refused before either descriptor is taken over
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
/// builds the account's engine over the broker's socket, and runs the window until it closes.
fn run_app(alias: Alias, secrets_fd: RawFd, trace_fd: Option<RawFd>) -> ExitCode {
    if let Some(refusal) = guard::test_support_refusal() {
        return fail("app", &refusal);
    }
    if let Some(refusal) = guard::personal_refusal() {
        return fail("app", &refusal);
    }
    let launch = match prepare(alias, secrets_fd, trace_fd) {
        Ok(launch) => launch,
        Err(error) => return fail("app", &error),
    };
    let failed = std::rc::Rc::new(std::cell::Cell::new(false));
    let failed_in_app = std::rc::Rc::clone(&failed);
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        if let Err(failed) = app::launch(cx, launch) {
            eprintln!("teams-app: the window can't be opened: {}", failed.error);
            failed_in_app.set(true);
            // the engine's thread is joined before the app quits
            app::quit_after(failed.stopped, cx);
        }
    });
    if failed.get() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Everything before the window: the descriptors first, before anything opens one of its own; then the sandbox
/// check, the lab's config, the fence and the engine.
fn prepare(alias: Alias, secrets_fd: RawFd, trace_fd: Option<RawFd>) -> Result<Launch, String> {
    let socket = secrets::adopt_socket(secrets_fd).map_err(|error| error.to_string())?;
    let trace = match trace_fd {
        Some(fd) => ViewTrace::new(adopt_trace(fd).map_err(|error| error.to_string())?),
        None => ViewTrace::off(),
    };
    // shut down if the engine's shutdown overruns, to end an exchange still waiting on the broker
    let broker = socket
        .try_clone()
        .map_err(|error| format!("the broker's socket: {error}"))?;
    guard::window_allowed_here()?;
    let config = LabLocations::from_env()
        .and_then(|locations| LabConfig::load(&locations))
        .map_err(|error| error.to_string())?;
    let settings = SettingsFile::new(config.paths().ui_settings_file(alias));
    // the log first, at the settings' level, so the engine's start is in it
    let diagnostics = app::install_log(
        config.paths().app_log_dir(alias),
        settings.load().settings.log_level,
        config.paths().fence_log(),
    );
    let fence =
        Arc::new(Fence::open_lab_account(alias).map_err(|error| format!("the fence can't be opened: {error}"))?);
    let secrets = BrokerSecretStore::new(socket, LabPaths::KEYRING_APPLICATION)
        .map_err(|error| format!("the broker's socket: {error}"))?;
    let lab = teams_core::lab::start(&config, alias, fence, secrets, true)
        .map_err(|error| app::start_failure(alias, &error))?;
    Ok(Launch {
        alias,
        broker: Some(broker),
        signed_out: Some(lab.session.watch_signed_out()),
        engine: lab.handle,
        trace,
        settings,
        // the bus the app is given, read once: nothing else is ever connected to for notifications or the tray
        bus: teams_ui::bus::address_from_env(),
        diagnostics: Some(diagnostics),
    })
}

fn fail(what: &str, error: &dyn std::fmt::Display) -> ExitCode {
    eprintln!("teams-app {what}: {error}");
    ExitCode::FAILURE
}
