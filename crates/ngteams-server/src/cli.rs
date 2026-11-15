//! The command line: `serve`, `migrate`, the account commands, `media`, `setup-code` and `keys rotate`. Every command reads
//! the same configuration as the server (`--config`, then `NGTEAMS_SERVER_*`).

use std::io::{IsTerminal as _, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use async_trait::async_trait;
use bytes::Bytes;
use clap::{Parser, Subcommand};
use tracing_subscriber::prelude::*;

use crate::admin::{Actor, AdminError, NewAccount};
use crate::app::{self, App};
use crate::config::Config;
use crate::fanout::{Fanout, FanoutError, Local, Redis, Subscription};
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
    /// What accounts keep in the store.
    #[command(subcommand)]
    Media(MediaCommand),
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
pub enum MediaCommand {
    /// Lists an account's files and pictures: kind, id, bytes, when it was stored, name.
    List { upn: String },
    /// Deletes files or pictures of an account, by id: their bytes leave the store and the account's quota.
    Delete {
        upn: String,
        #[arg(required = true)]
        ids: Vec<String>,
    },
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
    Admin(#[from] AdminError),
    #[error(transparent)]
    Key(#[from] crate::auth::keys::KeyError),
    #[error(transparent)]
    Db(#[from] crate::db::DbError),
    #[error(transparent)]
    Serve(#[from] crate::serve::ServeError),
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),
    #[error("the account keeps nothing with the id {0}")]
    NotStored(String),
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

/// [`run`] on a new runtime, with the process's standard input, output and error, until SIGINT or SIGTERM.
fn block_on(cli: Cli) -> Result<(), CliError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(CliError::Runtime)?;
    let stdin = std::io::stdin();
    let (mut output, mut warnings) = (std::io::stdout(), std::io::stderr());
    runtime.block_on(run(
        cli,
        &mut stdin.lock(),
        &mut output,
        &mut warnings,
        shutdown_signal(),
    ))
}

/// Logs to standard error, filtered by `NGTEAMS_SERVER_LOG` (default `info`); in colour only on a terminal.
fn init_logs() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("NGTEAMS_SERVER_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(std::io::stderr().is_terminal())
                .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                    // Dependency debug events can contain raw SQL bind values, including private keys.
                    *metadata.level() <= tracing::Level::INFO || metadata.target().starts_with("ngteams_server")
                })),
        )
        .init();
}

/// Runs one command; `serve` runs until `shutdown` resolves. A command that did its work but not all that follows
/// from it says so on `warnings` and succeeds.
///
/// # Errors
/// See [`CliError`].
pub async fn run(
    cli: Cli,
    input: &mut dyn std::io::BufRead,
    output: &mut dyn Write,
    warnings: &mut dyn Write,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), CliError> {
    let config = Config::load(cli.config.as_deref())?;
    match cli.command {
        Command::Serve => serve(config, shutdown).await,
        Command::Migrate => {
            crate::db::Db::connect(&config.database_url, 1).await?;
            Ok(())
        }
        Command::User(command) => user(config, command, input, &mut Streams { output, warnings }).await,
        Command::Media(command) => media(&config, command, output).await,
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

/// How long a stopping node waits for store clears still running: a store out of reach gives up within it.
const CLEARING: std::time::Duration = std::time::Duration::from_secs(10);

async fn serve(config: Config, shutdown: impl Future<Output = ()> + Send + 'static) -> Result<(), CliError> {
    let listener = Listener::bind(config.listen)?;
    let app = App::start(config).await?;
    let tls = app.config.tls.clone();
    crate::admin::setup::announce(&app).await;
    tracing::info!(address = %listener.local_addr()?, origin = %app.config.origin(), tls = tls.is_some(), "serving");
    let maintenance = tokio::spawn(app::maintain(std::sync::Arc::clone(&app)));
    let sweeper = tokio::spawn(crate::calling::keep_sweeping(std::sync::Arc::clone(&app)));
    let shutdown = app::closing(std::sync::Arc::clone(&app), shutdown);
    let served = listener
        .serve(app::router(std::sync::Arc::clone(&app)), tls, shutdown)
        .await;
    maintenance.abort();
    sweeper.abort();
    app.cleared(CLEARING).await;
    Ok(served?)
}

/// Each thing the account keeps on a line (`file` or `picture`, id, bytes, when it was stored, name,
/// tab-separated), or the deletes one by one: the first id the account doesn't keep ends them.
async fn media(config: &Config, command: MediaCommand, output: &mut dyn Write) -> Result<(), CliError> {
    let db = crate::db::Db::connect(&config.database_url, 1).await?;
    let owner = async |upn: &str| {
        let found = crate::db::users::by_upn(&db, upn).await?;
        found.ok_or(CliError::Admin(AdminError::NoSuchUser))
    };
    match command {
        MediaCommand::List { upn } => {
            for kept in crate::media::stored(&db, &owner(&upn).await?.id).await? {
                let at = chrono::DateTime::from_timestamp(kept.created_at, 0).unwrap_or(chrono::DateTime::UNIX_EPOCH);
                writeln!(
                    output,
                    "{}\t{}\t{}\t{}\t{}",
                    kept.kind,
                    kept.id,
                    kept.size,
                    at.format("%Y-%m-%dT%H:%M:%SZ"),
                    kept.name
                )
                .map_err(CliError::Output)?;
            }
            Ok(())
        }
        MediaCommand::Delete { upn, ids } => {
            let owner = owner(&upn).await?.id;
            let storage = crate::storage::ObjectStorage::open_with(
                &config.storage_url,
                &config.storage_options,
                &config.storage,
            )?;
            for id in ids {
                if !crate::media::remove_stored(&db, &storage, &owner, &id).await? {
                    return Err(CliError::NotStored(id));
                }
                writeln!(output, "deleted {id}").map_err(CliError::Output)?;
            }
            Ok(())
        }
    }
}

/// Where a command writes.
struct Streams<'a> {
    output: &'a mut dyn Write,
    warnings: &'a mut dyn Write,
}

async fn user(
    config: Config,
    command: UserCommand,
    input: &mut dyn std::io::BufRead,
    streams: &mut Streams<'_>,
) -> Result<(), CliError> {
    let db = crate::db::Db::connect(&config.database_url, 1).await?;
    let nodes = Nodes(config.redis_url);
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
            writeln!(streams.output, "added {} ({})", user.upn, user.id).map_err(CliError::Output)
        }
        UserCommand::List => list(&db, streams.output).await,
        UserCommand::Promote { upn } => Ok(crate::admin::set_admin(&db, &upn, true).await?),
        UserCommand::Demote { upn } => Ok(crate::admin::set_admin(&db, &upn, false).await?),
        UserCommand::Delete { upn } => {
            let ended = crate::admin::delete_by_upn(&db, &nodes, &upn).await;
            changed(ended, &format!("{upn} IS deleted"), streams.warnings)
        }
        UserCommand::Disable { upn } => {
            let ended = crate::admin::set_disabled(&db, &nodes, &upn, true).await;
            changed(ended, &format!("{upn} IS disabled"), streams.warnings)
        }
        UserCommand::Enable { upn } => Ok(crate::admin::set_disabled(&db, &nodes, &upn, false).await?),
        UserCommand::ResetPassword { upn } => {
            let ended = crate::admin::reset_password(&db, &nodes, &upn, &password(input)?).await;
            changed(ended, &format!("the password of {upn} IS replaced"), streams.warnings)
        }
    }
}

/// An account change's result for the operator: a change that is made but couldn't be told to the account's open
/// push sockets is a success with a warning, as the database holds it and every socket reads it there.
fn changed(ended: Result<(), AdminError>, done: &str, warnings: &mut dyn Write) -> Result<(), CliError> {
    match ended {
        Err(AdminError::Sockets(error)) => writeln!(
            warnings,
            "warning: {done}, but the account's open push sockets weren't told ({error}). Nothing more is pushed to \
             the account: each of its open sockets, on every node, ends at its next delivery check."
        )
        .map_err(CliError::Output),
        ended => Ok(ended?),
    }
}

/// The fan-out the running nodes listen on, connected to only when it is used: a command makes its database change
/// first, and keeps it when Redis is down. Without Redis there is none to reach from here: the one node reads the
/// change from the database.
#[derive(Debug)]
struct Nodes(Option<String>);

impl Nodes {
    async fn reach(&self) -> Result<Box<dyn Fanout>, FanoutError> {
        Ok(match &self.0 {
            Some(url) => Box::new(Redis::connect(url).await?),
            None => Box::new(Local::new()),
        })
    }
}

#[async_trait]
impl Fanout for Nodes {
    async fn publish(&self, topic: &str, payload: Bytes) -> Result<(), FanoutError> {
        self.reach().await?.publish(topic, payload).await
    }

    async fn subscribe(&self, topic: &str) -> Result<Subscription, FanoutError> {
        self.reach().await?.subscribe(topic).await
    }

    async fn ready(&self) -> Result<(), FanoutError> {
        self.reach().await?.ready().await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_change_the_sockets_werent_told_of_is_a_warning_and_any_other_failure_an_error() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let down = Nodes(Some(format!("redis://127.0.0.1:{closed}")));
        let untold = down.publish("topic", Bytes::new()).await.unwrap_err();

        let mut warnings = Vec::new();
        changed(
            Err(AdminError::Sockets(untold)),
            "a@own.test IS disabled",
            &mut warnings,
        )
        .unwrap();
        let warning = String::from_utf8(warnings).unwrap();
        assert!(
            warning.starts_with("warning: a@own.test IS disabled, but the account's open push sockets weren't told"),
            "{warning}"
        );
        assert!(warning.ends_with("ends at its next delivery check.\n"), "{warning}");

        let mut warnings = Vec::new();
        changed(Ok(()), "done", &mut warnings).unwrap();
        assert!(changed(Err(AdminError::NoSuchUser), "done", &mut warnings).is_err());
        assert!(warnings.is_empty());
    }

    #[tokio::test]
    async fn without_redis_there_is_nobody_to_tell() {
        let alone = Nodes(None);
        alone.ready().await.unwrap();
        alone.subscribe("topic").await.unwrap();
        alone.publish("topic", Bytes::new()).await.unwrap();
    }
}
