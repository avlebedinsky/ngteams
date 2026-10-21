//! Stopping `proto-lab` by a signal without leaving its browsers behind.
//!
//! A lab browser runs in a process group of its own, so a signal meant for `proto-lab` doesn't reach it, and it goes
//! down when its owner ([`crate::browser::Browser`], inside [`crate::arming::ArmedBrowser`]) is dropped. A process
//! that a signal ends by its default action drops nothing: live, 2026-10-21, a hung `ab-image` run stopped with SIGINT
//! left both lab browsers running. So the CLI runs each command [`until_stopped`]: on SIGINT or SIGTERM the command's
//! future is dropped, and with it every browser it owns, before the process exits.

use std::future::Future;

use tokio::signal::unix::{SignalKind, signal};

/// The signal that stopped a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("stopped by {0}; its browsers were closed")]
pub struct Stopped(pub &'static str);

/// Runs `command` until it ends or the process gets SIGINT or SIGTERM; on a signal, `command` is dropped (and every
/// browser it owns with it) and the signal's name is returned. A signal that can't be listened for is never waited
/// for: the command then runs as it would without this.
///
/// # Errors
/// [`Stopped`] when a signal came first.
pub async fn until_stopped<T>(command: impl Future<Output = T>) -> Result<T, Stopped> {
    tokio::select! {
        outcome = command => Ok(outcome),
        name = first_signal() => Err(Stopped(name)),
    }
}

/// The name of the first of SIGINT and SIGTERM to come.
async fn first_signal() -> &'static str {
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
        _ = interrupted => "SIGINT",
        _ = terminated => "SIGTERM",
    }
}
