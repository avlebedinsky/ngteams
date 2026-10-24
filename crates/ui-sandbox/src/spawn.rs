//! Descriptor hygiene for every process the sandbox starts: it inherits stdin, stdout, stderr and the descriptors its
//! caller names (under their own numbers), and nothing else.
//!
//! `std` opens its own descriptors close-on-exec, but a descriptor this process inherited without the flag (a
//! jobserver pipe, a socket of the test harness) would reach the child. Right before `exec`, the child marks every
//! descriptor from 3 up close-on-exec (`close_range` with `CLOSE_RANGE_CLOEXEC`, Linux 5.11) except the named ones,
//! whose flag it clears. Marking rather than closing keeps `std`'s own exec-error pipe working until the `exec`.

use std::{
    ffi::{c_int, c_uint},
    io,
    os::{
        fd::{AsRawFd, BorrowedFd, RawFd},
        unix::process::CommandExt as _,
    },
    process::{Command, Output},
    thread,
    time::Duration,
};

/// The first descriptor past stdin, stdout and stderr.
const FIRST: c_uint = 3;

/// Makes `command` pass `fds` through under their own numbers and close every other descriptor from 3 up at `exec`.
/// A named descriptor must be open when the command spawns; one that isn't fails the spawn.
pub fn pass_only(command: &mut Command, fds: &[BorrowedFd<'_>]) {
    let mut keep: Vec<c_uint> = fds
        .iter()
        .map(AsRawFd::as_raw_fd)
        .filter_map(|fd| c_uint::try_from(fd).ok())
        .filter(|&fd| fd >= FIRST)
        .collect();
    keep.sort_unstable();
    keep.dedup();
    let cloexec = c_int::try_from(libc::CLOSE_RANGE_CLOEXEC).unwrap_or(c_int::MAX);
    // SAFETY: the hook runs in the forked child before `exec`; it makes only async-signal-safe system calls
    // (`close_range`, `fcntl`), allocates nothing and touches no lock (`keep` was built before the fork).
    unsafe {
        command.pre_exec(move || close_all_but(&keep, cloexec));
    }
}

/// In the child: every descriptor from 3 up close-on-exec but `keep` (sorted), whose flag is cleared.
fn close_all_but(keep: &[c_uint], cloexec: c_int) -> io::Result<()> {
    let mut next = FIRST;
    for &fd in keep {
        if fd > next {
            mark_cloexec(next, fd - 1, cloexec)?;
        }
        clear_cloexec(fd)?;
        next = fd.saturating_add(1);
    }
    mark_cloexec(next, c_uint::MAX, cloexec)
}

fn mark_cloexec(first: c_uint, last: c_uint, cloexec: c_int) -> io::Result<()> {
    // SAFETY: `close_range` with `CLOSE_RANGE_CLOEXEC` only sets a flag on descriptors in the range; it closes none and
    // touches no memory of this process.
    let result = unsafe { libc::close_range(first, last, cloexec) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn clear_cloexec(fd: c_uint) -> io::Result<()> {
    let fd = RawFd::try_from(fd).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `F_SETFD` only sets the descriptor's flags; a descriptor that isn't open fails with `EBADF`.
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// How often, and how far apart, [`output`] tries a script that is still busy.
const BUSY_TRIES: u32 = 50;
const BUSY_PAUSE: Duration = Duration::from_millis(20);

/// `command`'s output, tried again while its program is busy (`ETXTBSY`): a script this process has just written
/// can't be run while another thread's child, forked before the write's descriptor closed, hasn't reached its own
/// `exec` yet.
///
/// # Errors
/// The command can't be run, or its program stays busy.
pub fn output(command: &mut Command) -> io::Result<Output> {
    let mut tries = 1;
    loop {
        match command.output() {
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy && tries < BUSY_TRIES => {
                tries += 1;
                thread::sleep(BUSY_PAUSE);
            }
            result => return result,
        }
    }
}
