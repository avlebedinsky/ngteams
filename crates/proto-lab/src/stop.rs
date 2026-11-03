//! Stops a command on SIGINT or SIGTERM. Dropping its owned browser stops the browser process group before the runtime shuts down.

use std::future::Future;
use std::time::Duration;

use rustix::process::Signal;
use tokio::signal::unix::{SignalKind, signal};

/// The signal that stopped a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Stopped {
    #[error("stopped by SIGINT")]
    Interrupt,
    #[error("stopped by SIGTERM")]
    Terminate,
}

impl Stopped {
    /// The signal's number.
    #[must_use]
    pub const fn signal(self) -> i32 {
        match self {
            Self::Interrupt => Signal::INT.as_raw(),
            Self::Terminate => Signal::TERM.as_raw(),
        }
    }

    /// The exit status of a process the signal stopped, as a shell reports it: 128 and the signal's number (130 for
    /// SIGINT, 143 for SIGTERM).
    #[must_use]
    pub fn exit_code(self) -> u8 {
        u8::try_from(128 + self.signal()).unwrap_or(u8::MAX)
    }
}

/// Runs `command` until it ends or the process gets SIGINT or SIGTERM; on a signal, `command` is dropped (and every
/// browser it owns with it) and the signal is returned. A signal that can't be listened for is never waited for: the
/// command then runs as it would without this.
///
/// Once installed, the signals' handlers stay for the life of the process, so a later signal no longer ends it by the
/// default action: whatever runs after a stop must be bounded, or run [`until_stopped`] itself.
///
/// # Errors
/// [`Stopped`] when a signal came first.
pub async fn until_stopped<T>(command: impl Future<Output = T>) -> Result<T, Stopped> {
    tokio::select! {
        outcome = command => Ok(outcome),
        stopped = first_signal() => Err(stopped),
    }
}

/// Ends the runtime after a stopped command has dropped its browser owners, without waiting for blocking work.
pub fn after_stop(runtime: tokio::runtime::Runtime, stopped: Stopped, _grace: Duration) -> u8 {
    eprintln!("proto-lab: {stopped}; any lab browser it ran was killed");
    runtime.shutdown_background();
    stopped.exit_code()
}

/// The first of SIGINT and SIGTERM to come.
async fn first_signal() -> Stopped {
    let listen = |kind| signal(kind).ok();
    let (mut interrupt, mut terminate) = (listen(SignalKind::interrupt()), listen(SignalKind::terminate()));
    let interrupted = async {
        match interrupt.as_mut() {
            Some(stream) => stream.recv().await,
            None => std::future::pending().await,
        }
    };
    let terminated = async {
        match terminate.as_mut() {
            Some(stream) => stream.recv().await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        _ = interrupted => Stopped::Interrupt,
        _ = terminated => Stopped::Terminate,
    }
}
