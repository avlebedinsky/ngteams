//! The engine in a process of its own: the worker, which a vendored WPE `WebKit` needs (crate `launcher`) and
//! [`crate::Host::start_worker`] asks for anywhere.
//!
//! The vendored package's helpers sit at a path compiled into its library, reachable only inside a mount namespace
//! that overlays the package on `/usr/lib` (`bwrap --ro-overlay`). That namespace comes with a user namespace and
//! `PR_SET_NO_NEW_PRIVS`, which every process started inside it inherits, so the app itself must never run there: its
//! terminals, their sessions and everything they start would lose `sudo` and every setuid program, and see root's
//! files as `nobody`'s. Only `WebKit` goes there: the app re-runs its own executable as the worker (the image it runs,
//! opened from `/proc/self/exe` and passed down as a descriptor, so a file replaced since does not matter), under the
//! overlay for the vendored package and plainly for the system's, and drives the engine through a [`crate::Host`] whose
//! calls travel over a socket. The worker runs the ordinary in-process engine and nothing else.
//!
//! - Every program that may start an engine calls [`serve_if_asked`] first thing in its `main`: in the worker it serves
//!   the engine and ends the process; anywhere else it returns at once.
//! - The transport (`wire`): an `AF_UNIX` `SOCK_SEQPACKET` socket pair, close-on-exec on both sides (the worker's end
//!   is kept open across its exec alone and made close-on-exec again first thing), one JSON message per packet
//!   (`message`), large bodies and blobs in sealed `memfd`s, a dmabuf's planes passed as descriptors. A call on the
//!   app's side only queues: a writer thread and a reader thread own the socket, so neither GPUI's main thread nor the
//!   engine's ever blocks on it. The app's queue is bounded: a worker that stops reading breaks the transport, as its
//!   death would.
//! - The start: the worker says hello (protocol, build, pid) and the app hands it the configuration; the engine's start
//!   comes back as the in-process one would (a missing render node fails as `StartError::Display`), all within
//!   `START_TIMEOUT`, after which the worker is killed. The worker runs in a process group of its own, so a terminal's
//!   Ctrl-C reaches the app alone; it ends when the app quits it or goes away (the end of its socket), never through
//!   `bwrap --die-with-parent`, which would kill it with the thread that started it.
//! - The end: [`crate::Host::shutdown`] quits the engine (pages closed and every web process ended, its last answer
//!   flushed before the worker exits) and waits `STOP_TIMEOUT` for it, then closes the socket, which frees a writer
//!   stuck on a worker that stopped reading, and reaps the worker, killing its group when it does not end at once. A
//!   worker whose app went away (the end of its socket) stops its engine the same way before it ends. A worker that
//!   ends unasked answers every evaluation still waiting with an error and closes the event channel: the consumer's
//!   pages cannot come back in this process.

mod client;
mod message;
mod serve;
mod wire;

use std::os::fd::{FromRawFd as _, OwnedFd};

pub(crate) use client::Client;

/// The variable that makes a process the worker: the number of the socket descriptor it inherited.
pub const WORKER_FD_ENV: &str = "TEAMS_WPE_WORKER_FD";
/// The number of the descriptor of the image the worker was started from (`/proc/self/fd/<n>`), closed at once.
pub const WORKER_IMAGE_ENV: &str = "TEAMS_WPE_WORKER_IMAGE_FD";

/// Serves the engine and ends the process when this process was started as the worker ([`WORKER_FD_ENV`]); returns
/// at once otherwise. `setup` runs first in the worker (the app's logging, say).
///
/// # Safety
///
/// Call it first thing in `main`, while the process has one thread: it changes the process's environment.
pub unsafe fn serve_if_asked(setup: impl FnOnce()) {
    let Some(value) = std::env::var_os(WORKER_FD_ENV) else {
        return;
    };
    let image = std::env::var(WORKER_IMAGE_ENV).ok();
    // SAFETY: the caller guarantees the process has one thread.
    unsafe { std::env::remove_var(WORKER_FD_ENV) };
    // SAFETY: as above.
    unsafe { std::env::remove_var(WORKER_IMAGE_ENV) };
    // the image this process was started from, inherited for the exec alone: nothing WebKit starts gets it
    if let Some(image) = image.and_then(|image| image.parse::<i32>().ok()) {
        // SAFETY: `close` on a descriptor the app named as ours alone; a closed one fails with EBADF.
        unsafe { libc::close(image) };
    }
    let Some(fd) = value.to_str().and_then(|value| value.parse::<i32>().ok()) else {
        eprintln!(
            "teams-wpe: {WORKER_FD_ENV} is not a descriptor number: {}",
            value.display()
        );
        crate::exit_process(2);
    };
    if !is_socket(fd) {
        eprintln!("teams-wpe: {WORKER_FD_ENV}={fd} names no socket this process inherited");
        crate::exit_process(2);
    }
    // SAFETY: the descriptor is open (checked), inherited for this process alone, and owned by nothing else here.
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    setup();
    let code = serve::serve(socket);
    crate::exit_process(code)
}

/// Whether descriptor `fd` is an open socket.
fn is_socket(fd: i32) -> bool {
    // SAFETY: `fstat` only reads the descriptor's metadata into `stat`, which it fully initializes on success.
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    // SAFETY: as above; a closed descriptor fails with EBADF.
    let ok = unsafe { libc::fstat(fd, &raw mut stat) } == 0;
    ok && (stat.st_mode & libc::S_IFMT) == libc::S_IFSOCK
}
