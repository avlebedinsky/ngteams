//! The command line: `serve`, `migrate`, the account commands, `setup-code` and `keys rotate`. Every command reads the same
//! configuration as the server (`--config`, then `NGTEAMS_SERVER_*`).

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing_subscriber::prelude::*;

use crate::admin::{Actor, NewAccount};
use crate::app::{self, App};
use crate::config::Config;
use crate::serve::Listener;

#[derive(Debug, Parser)]
#[command(
    name = "ngteams-server",
    version,
    about = "The own Teams-compatible server for ngteams"
)]
pub struct Cli {
    /// The configuration file (TOML); `NGTEAMS_SERVER_*` variables override it.
    #[arg(long, short, global = true, env = "NGTEAMS_SERVER_CONFIG")]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Serves until SIGINT or SIGTERM.
    Serve,
    /// Brings the database schema up to date, then exits.
    Migrate,
    /// Accounts.
    #[command(subcommand)]
    User(UserCommand),
    /// Token signing keys.
    #[command(subcommand)]
    Keys(KeysCommand),
    /// Issues a new first-admin setup code (replacing a pending one) and prints it; refused once an admin exists.
    SetupCode,
}

#[derive(Debug, Subcommand)]
pub enum UserCommand {
    /// Adds an account; the password comes from standard input's first line.
    Add {
        upn: String,
        /// The name others see.
        #[arg(long)]
        name: String,
        /// Makes the account an admin.
        #[arg(long)]
        admin: bool,
    },
    /// Lists the accounts.
    List,
    /// Signs an account out everywhere and refuses its sign-ins.
    Disable { upn: String },
    /// Lets a disabled account sign in again.
    Enable { upn: String },
    /// Sets a new password (standard input's first line) and signs the account out everywhere.
    ResetPassword { upn: String },
    /// Makes an account an admin.
    Promote { upn: String },
    /// Takes the admin role from an account; refused for the last enabled admin.
    Demote { upn: String },
    /// Deletes an account: it is signed out everywhere and its UPN is free again.
    Delete { upn: String },
}

#[derive(Debug, Subcommand)]
pub enum KeysCommand {
    /// Creates a new key; running nodes switch at their next hourly check (or unknown-key reload).
    Rotate,
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    #[error(transparent)]
    Start(#[from] app::StartError),
    #[error(transparent)]
    Admin(#[from] crate::admin::AdminError),
    #[error(transparent)]
    Key(#[from] crate::auth::keys::KeyError),
    #[error(transparent)]
    Db(#[from] crate::db::DbError),
    #[error(transparent)]
    Serve(#[from] crate::serve::ServeError),
    #[error("can't start the runtime: {0}")]
    Runtime(#[source] std::io::Error),
    #[error("can't read the password from standard input: {0}")]
    Input(#[source] std::io::Error),
    #[error("can't write the output: {0}")]
    Output(#[source] std::io::Error),
}

/// Runs the command line on the process's arguments, standard input and output.
#[must_use]
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logs();
    match block_on(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "ngteams-server failed");
            ExitCode::FAILURE
        }
    }
}

/// [`run`] on a new runtime, with the process's standard input and output, until SIGINT or SIGTERM.
fn block_on(cli: Cli) -> Result<(), CliError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(CliError::Runtime)?;
    let stdin = std::io::stdin();
    runtime.block_on(run(cli, &mut stdin.lock(), &mut std::io::stdout(), shutdown_signal()))
}

/// Logs to standard error, filtered by `NGTEAMS_SERVER_LOG` (default `info`).
fn init_logs() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("NGTEAMS_SERVER_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                    // Dependency debug events can contain raw SQL bind values, including private keys.
                    *metadata.level() <= tracing::Level::INFO || metadata.target().starts_with("ngteams_server")
                })),
        )
        .init();
}

/// Runs one command; `serve` runs until `shutdown` resolves.
///
/// # Errors
/// See [`CliError`].
pub async fn run(
    cli: Cli,
    input: &mut dyn std::io::BufRead,
    output: &mut dyn Write,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), CliError> {
    let config = Config::load(cli.config.as_deref())?;
    match cli.command {
        Command::Serve => serve(config, shutdown).await,
        Command::Migrate => {
            crate::db::Db::connect(&config.database_url, 1).await?;
            Ok(())
        }
        Command::User(command) => user(config, command, input, output).await,
        Command::SetupCode => {
            let db = crate::db::Db::connect(&config.database_url, 1).await?;
            let code = crate::admin::setup::issue(&db).await?;
            writeln!(
                output,
                "{code}\nEnter it in ngteams when it asks to create the first admin account."
            )
            .map_err(CliError::Output)
        }
        Command::Keys(KeysCommand::Rotate) => {
            let app = App::start(config).await?;
            let kid = app.issuer.keys().rotate().await?;
            writeln!(
                output,
                "created key {kid}; running nodes reload at their next hourly check"
            )
            .map_err(CliError::Output)
        }
    }
}

async fn serve(config: Config, shutdown: impl Future<Output = ()> + Send + 'static) -> Result<(), CliError> {
    let listener = Listener::bind(config.listen)?;
    let app = App::start(config).await?;
    let tls = app.config.tls.clone();
    crate::admin::setup::announce(&app).await;
    tracing::info!(address = %listener.local_addr()?, origin = %app.config.origin(), tls = tls.is_some(), "serving");
    let maintenance = tokio::spawn(app::maintain(std::sync::Arc::clone(&app)));
    let served = listener.serve(app::router(app), tls, shutdown).await;
    maintenance.abort();
    Ok(served?)
}

async fn user(
    config: Config,
    command: UserCommand,
    input: &mut dyn std::io::BufRead,
    output: &mut dyn Write,
) -> Result<(), CliError> {
    let db = crate::db::Db::connect(&config.database_url, 1).await?;
    match command {
        UserCommand::Add { upn, name, admin } => {
            let password = password(input)?;
            let new = NewAccount {
                upn: &upn,
                display_name: &name,
                password: &password,
                admin,
                must_change_password: false,
            };
            let user = crate::admin::create_user(&db, Actor::CLI, &new).await?;
            writeln!(output, "added {} ({})", user.upn, user.id).map_err(CliError::Output)
        }
        UserCommand::List => list(&db, output).await,
        UserCommand::Promote { upn } => Ok(crate::admin::set_admin(&db, &upn, true).await?),
        UserCommand::Demote { upn } => Ok(crate::admin::set_admin(&db, &upn, false).await?),
        UserCommand::Delete { upn } => Ok(crate::admin::delete_by_upn(&db, &upn).await?),
        UserCommand::Disable { upn } => Ok(crate::admin::set_disabled(&db, &upn, true).await?),
        UserCommand::Enable { upn } => Ok(crate::admin::set_disabled(&db, &upn, false).await?),
        UserCommand::ResetPassword { upn } => Ok(crate::admin::reset_password(&db, &upn, &password(input)?).await?),
    }
}

/// Each account on a line: UPN, id, `enabled` or `disabled`, name, `admin` or `user`, tab-separated.
async fn list(db: &crate::db::Db, output: &mut dyn Write) -> Result<(), CliError> {
    for user in crate::db::users::list(db).await? {
        let state = if user.disabled { "disabled" } else { "enabled" };
        let role = if user.is_admin { "admin" } else { "user" };
        writeln!(
            output,
            "{}\t{}\t{}\t{}\t{}",
            user.upn, user.id, state, user.display_name, role
        )
        .map_err(CliError::Output)?;
    }
    Ok(())
}

/// The first line of `input`, without its line end.
fn password(input: &mut dyn std::io::BufRead) -> Result<String, CliError> {
    let mut line = String::new();
    input.read_line(&mut line).map_err(CliError::Input)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

/// SIGINT or SIGTERM.
async fn shutdown_signal() {
    let interrupt = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let Ok(mut terminate) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) else {
            let _ = interrupt.await;
            return;
        };
        tokio::select! {
            _ = interrupt => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = interrupt.await;
    }
}
