//! `teams-app`: the lab's app for one lab account, and its credential broker (see the library's docs).

use std::io;
use std::os::fd::RawFd;
use std::process::ExitCode;

use clap::Parser;
use teams_app::cli::{Cli, Command};
use teams_app::secrets::{self, LazyKeyring};
use teams_fence::lab::{Alias, LabConfig, LabLocations, LabPaths};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Secrets { alias, fd } => broker(alias, fd),
    }
}

/// `teams-app secrets`: the socket taken over first, before anything opens a descriptor of its own.
fn broker(alias: Alias, fd: RawFd) -> ExitCode {
    let socket = match secrets::adopt_socket(fd) {
        Ok(socket) => socket,
        Err(error) => return fail(&error),
    };
    let config = match LabLocations::from_env().and_then(|locations| LabConfig::load(&locations)) {
        Ok(config) => config,
        Err(error) => return fail(&error),
    };
    let scope = secrets::lab_scope(&config, alias);
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => return fail(&format!("can't start the async runtime: {error}")),
    };
    let store = LazyKeyring::new(LabPaths::KEYRING_APPLICATION);
    match runtime.block_on(secrets::run(socket, &scope, &store, &mut io::stderr())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&format!("{error}; the socket is closed")),
    }
}

fn fail(error: &dyn std::fmt::Display) -> ExitCode {
    eprintln!("teams-app secrets: {error}");
    ExitCode::FAILURE
}
