//! The engine in a process of its own: the worker, which [`crate::Host::start_worker`] asks for.
//!
//! An in-process engine is once per process (`WebKit` binds its main run loop to a thread that never ends), so a
//! program that opens pages again later (the app's sign-in window, once per account added) runs each engine in a
//! worker: the sibling `ngteams-web` binary, the only one that links `WebKit` (the image pinned by descriptor at the
//! app's start, so a file replaced since does not matter). The app drives the engine through a
//! `teams_wpe_client::Host` whose calls travel over a socket. The worker runs the ordinary in-process engine and nothing
//! else.
//!
//! - The `ngteams-web` binary calls [`serve_if_asked`] first thing, as does a test program that serves as its own
//!   worker: in the worker it serves the engine and ends the process; anywhere else it returns at once.
//! - The transport (`wire`): an `AF_UNIX` `SOCK_SEQPACKET` socket pair, close-on-exec on both sides (the worker's end
//!   is kept open across its exec alone and made close-on-exec again first thing), one JSON message per packet
//!   (`message`), large bodies and blobs in sealed `memfd`s, a dmabuf's planes passed as descriptors. A call on the
//!   app's side only queues: a writer thread and a reader thread own the socket, so neither GPUI's main thread nor the
//!   engine's ever blocks on it. The app's queue is bounded: a worker that stops reading breaks the transport, as its
//!   death would.
//! - The start: the worker says hello (protocol, build, pid) and the app hands it the configuration; the engine's start
//!   comes back as the in-process one would (a missing render node fails as `StartError::Display`), all within
//!   `START_TIMEOUT`, after which the worker is killed. The worker runs in a process group of its own, so a terminal's
//!   Ctrl-C reaches the app alone; it ends when the app quits it or goes away (the end of its socket), never with the
//!   thread that started it.
//! - The end: [`crate::Host::shutdown`] quits the engine (pages closed and every web process ended, its last answer
//!   flushed before the worker exits) and waits `STOP_TIMEOUT` for it, then closes the socket, which frees a writer
//!   stuck on a worker that stopped reading, and reaps the worker, killing its group when it does not end at once. A
//!   worker whose app went away (the end of its socket) stops its engine the same way before it ends. A worker that
//!   ends unasked answers every evaluation still waiting with an error and closes the event channel: the consumer's
//!   pages cannot come back in this process.

pub use teams_wpe_client::worker::{message, wire};
mod serve;

use std::os::fd::{FromRawFd as _, OwnedFd};

pub(crate) use teams_wpe_client::worker::Client;
pub use teams_wpe_client::worker::{WORKER_FD_ENV, WORKER_IMAGE_ENV};

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
    // the runtime's shared lock stays with this process, and reaches nothing it starts
    let lock_fd = std::env::var(teams_wpe_client::RUNTIME_LOCK_ENV).ok();
    // SAFETY: the caller guarantees the process has one thread.
    unsafe { std::env::remove_var(teams_wpe_client::RUNTIME_LOCK_ENV) };
    let _runtime_lock = match lock_fd.map(|value| value.parse::<i32>().ok().and_then(|fd| retain_runtime_lock(fd).ok()))
    {
        Some(Some(lock)) => Some(lock),
        None => None,
        Some(None) => {
            eprintln!("teams-wpe: an invalid inherited web engine runtime lock");
            crate::exit_process(2);
        }
    };
    setup();
    let code = serve::serve(socket);
    crate::exit_process(code)
}

/// The WPE `WebKit` version this process loaded.
#[must_use]
pub fn engine_version() -> (u32, u32, u32) {
    // SAFETY: these getters have no preconditions and initialize no engine.
    let major = unsafe { crate::webkit_sys::webkit_get_major_version() };
    // SAFETY: as above.
    let minor = unsafe { crate::webkit_sys::webkit_get_minor_version() };
    // SAFETY: as above.
    let micro = unsafe { crate::webkit_sys::webkit_get_micro_version() };
    (major, minor, micro)
}

/// Adopts the inherited, read-only runtime lock and stops it at this process's next exec.
fn retain_runtime_lock(fd: i32) -> std::io::Result<OwnedFd> {
    // SAFETY: fcntl inspects the given descriptor; an invalid one fails with EBADF.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || flags & libc::O_ACCMODE != libc::O_RDONLY {
        return Err(std::io::Error::other("the runtime lock must be open read-only"));
    }
    // SAFETY: changes only this process's descriptor flags, before WebKit or its threads start.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the descriptor was inherited for the worker alone and checked above.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Whether descriptor `fd` is an open socket.
fn is_socket(fd: i32) -> bool {
    // SAFETY: `fstat` only reads the descriptor's metadata into `stat`, which it fully initializes on success.
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    // SAFETY: as above; a closed descriptor fails with EBADF.
    let ok = unsafe { libc::fstat(fd, &raw mut stat) } == 0;
    ok && (stat.st_mode & libc::S_IFMT) == libc::S_IFSOCK
}

#[cfg(test)]
mod tests {
    use std::os::fd::IntoRawFd as _;

    use super::*;

    #[test]
    fn a_read_only_lock_is_kept_and_stops_at_the_next_exec() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let fd = std::fs::File::open(file.path()).unwrap().into_raw_fd();
        let lock = retain_runtime_lock(fd).unwrap();
        // SAFETY: only inspects a descriptor this test owns.
        let flags = unsafe { libc::fcntl(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::F_GETFD) };
        assert_eq!(flags & libc::FD_CLOEXEC, libc::FD_CLOEXEC);
    }

    #[test]
    fn a_writable_or_closed_lock_is_refused() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let writable = std::fs::File::options().write(true).open(file.path()).unwrap();
        assert!(retain_runtime_lock(std::os::fd::AsRawFd::as_raw_fd(&writable)).is_err());
        assert!(retain_runtime_lock(-1).is_err());
    }

    #[test]
    fn this_process_reports_the_engine_it_loaded() {
        assert_eq!(engine_version().0, 2);
    }

    #[test]
    fn only_a_socket_counts_as_the_workers_descriptor() {
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(is_socket(std::os::fd::AsRawFd::as_raw_fd(&socket)));
        let file = tempfile::tempfile().unwrap();
        assert!(!is_socket(std::os::fd::AsRawFd::as_raw_fd(&file)));
        assert!(!is_socket(-1));
    }
}
