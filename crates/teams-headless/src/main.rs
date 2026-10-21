//! `teams-headless`: one lab account's native client from the command line (see the library's docs).

use std::error::Error;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use teams_core::{Action, EngineHandle};
use teams_headless::chat;
use teams_headless::cli::{Cli, Command};
use teams_headless::commands::{self, WatchEnd};
use teams_headless::lab::Lab;
use teams_proto::chat::MessageText;
use teams_store::OutboxState;
use teams_trouter::Stop;
use tokio::signal::unix::{Signal, SignalKind, signal};

/// The exit code after SIGINT or SIGTERM cut a one-shot command short.
const INTERRUPTED: u8 = 130;

type Outcome = Result<ExitCode, Box<dyn Error>>;

fn main() -> ExitCode {
    let cli = Cli::parse();
    if teams_fence::test_support_enabled() {
        // cargo unified a dev-dependency's feature into this build (`cargo test`, `--all-targets`, `--all-features`):
        // its fence API would take any roster and any member list, and its transports a mock origin
        eprintln!(
            "teams-headless: built with teams-fence's test-support feature; rebuild with `cargo build -p teams-headless`"
        );
        return ExitCode::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("teams-headless: can't start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("teams-headless: {error}");
            ExitCode::FAILURE
        }
    }
}

/// SIGINT and SIGTERM, taken over before anything is set up, so a signal ends a command between its steps.
struct Signals {
    interrupt: Signal,
    terminate: Signal,
}

impl Signals {
    fn new() -> io::Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    async fn next(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
        }
    }
}

async fn run(cli: Cli) -> Outcome {
    let mut signals = Signals::new()?;
    let lab = Lab::load(cli.alias)?;
    let mut out = io::stdout().lock();
    match cli.command {
        Command::List { limit } => {
            commands::list(&lab.open_store()?, limit, &lab.names, &mut out)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Show { chat, limit } => {
            let conversation = conversation(&lab, &chat)?;
            commands::show(&lab.open_store()?, &conversation, limit, &lab.names, &mut out)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Sync => {
            let handle = lab.start_engine(false).await?;
            let synced = tokio::select! {
                synced = commands::sync(&handle, &mut out) => Some(synced),
                () = signals.next() => None,
            };
            finish(handle, synced.map(|synced| synced.map(|_| ExitCode::SUCCESS)))
        }
        Command::Watch { json } => watch(&lab, json, &mut signals, &mut out).await,
        command => {
            let (action, wait) = write_action(&lab, command)?;
            let mut handle = lab.start_engine(false).await?;
            let written = tokio::select! {
                written = write(&mut handle, action, wait, &mut out) => Some(written),
                () = signals.next() => None,
            };
            finish(handle, written)
        }
    }
}

/// The conversation a command's chat argument names for the lab account.
fn conversation(lab: &Lab, chat: &str) -> Result<String, chat::ChatError> {
    chat::resolve(chat, lab.alias, &lab.names, lab.abc.as_deref())
}

/// A write command's action, and how long to wait for it to settle.
fn write_action(lab: &Lab, command: Command) -> Result<(Action, Duration), Box<dyn Error>> {
    let (action, wait) = match command {
        Command::Send { chat, text, wait } => (
            Action::Send {
                conversation: conversation(lab, &chat)?,
                text: MessageText::plain(&text)?,
            },
            wait,
        ),
        Command::Edit {
            chat,
            message,
            text,
            wait,
        } => (
            Action::Edit {
                conversation: conversation(lab, &chat)?,
                message_id: message,
                text: MessageText::plain(&text)?,
            },
            wait,
        ),
        Command::Delete { chat, message, wait } => (
            Action::Delete {
                conversation: conversation(lab, &chat)?,
                message_id: message,
            },
            wait,
        ),
        Command::React {
            chat,
            message,
            key,
            remove,
            wait,
        } => {
            let conversation = conversation(lab, &chat)?;
            let action = if remove {
                Action::Unreact {
                    conversation,
                    message_id: message,
                    key,
                }
            } else {
                Action::React {
                    conversation,
                    message_id: message,
                    key,
                }
            };
            (action, wait)
        }
        Command::Sync | Command::List { .. } | Command::Show { .. } | Command::Watch { .. } => {
            return Err("not a write".into());
        }
    };
    Ok((action, Duration::from_secs(wait)))
}

/// A write after a catch-up (its summary on stderr, the write's lines on stdout): the store has the latest state its
/// checks read (an edit only of the account's own message, …). Exits non-zero unless it went through.
async fn write(handle: &mut EngineHandle, action: Action, wait: Duration, out: &mut impl Write) -> Outcome {
    commands::sync(handle, &mut io::stderr()).await?;
    let state = commands::write(handle, action, wait, out).await?;
    Ok(if state == OutboxState::Sent {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The push channel's events until a signal: exits 0 on the signal, non-zero when the channel stopped on its own.
async fn watch(lab: &Lab, json: bool, signals: &mut Signals, out: &mut impl Write) -> Outcome {
    let mut handle = lab.start_engine(true).await?;
    let end = commands::watch(&mut handle, json, &lab.names, out, signals.next()).await;
    handle.shutdown();
    match end? {
        WatchEnd::Stopped | WatchEnd::PushStopped(Stop::Requested) => Ok(ExitCode::SUCCESS),
        WatchEnd::PushStopped(why) => Err(format!("the push channel stopped: {why:?}").into()),
        WatchEnd::EngineStopped => Err("the engine stopped".into()),
    }
}

/// Stops the engine (after the request it is on) and gives the command's outcome; `None` when a signal cut it short.
fn finish<E: Into<Box<dyn Error>>>(handle: EngineHandle, outcome: Option<Result<ExitCode, E>>) -> Outcome {
    handle.shutdown();
    if let Some(outcome) = outcome {
        return outcome.map_err(Into::into);
    }
    eprintln!("teams-headless: interrupted");
    Ok(ExitCode::from(INTERRUPTED))
}
