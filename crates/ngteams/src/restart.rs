//! Lab descriptors retained for the app's explicit self-exec after sign-in.
//!
//! The originals keep their command-line descriptor numbers while the app uses close-on-exec clones. They stay
//! close-on-exec during normal operation, including sign-in page-worker launches; only the final self-exec inherits
//! them. A failed exec restores their original flags before returning.

use std::fs::File;
use std::io;
use std::os::fd::{AsFd as _, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::process::Command;

use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};

/// The adopted broker/trace originals, held until the app either quits or executes itself again.
#[derive(Debug)]
pub struct RestartDescriptors {
    socket: UnixStream,
    trace: Option<File>,
}

impl RestartDescriptors {
    /// Keeps originals already made close-on-exec by [`crate::descriptors`].
    #[must_use]
    pub fn new(socket: UnixStream, trace: Option<File>) -> Self {
        Self { socket, trace }
    }

    /// A close-on-exec clone for the app's credential store.
    ///
    /// # Errors
    /// The socket cannot be duplicated.
    pub fn socket(&self) -> io::Result<UnixStream> {
        self.socket.try_clone()
    }

    /// A close-on-exec clone for the app's optional view trace.
    ///
    /// # Errors
    /// The file cannot be duplicated.
    pub fn trace(&self) -> io::Result<Option<File>> {
        self.trace.as_ref().map(File::try_clone).transpose()
    }

    /// Executes the app again, inheriting these originals under their existing descriptor numbers.
    ///
    /// Does not return on success. The returned error leaves both descriptors' original flags restored.
    pub fn exec(&self, command: &mut Command) -> io::Error {
        let inherited = match Inherited::new(self) {
            Ok(inherited) => inherited,
            Err(error) => return error,
        };
        let error = command.exec();
        drop(inherited);
        error
    }
}

/// Restores even a partly prepared descriptor set if a later flag operation fails.
struct Inherited<'a> {
    flags: Vec<(BorrowedFd<'a>, FdFlags)>,
}

impl<'a> Inherited<'a> {
    fn new(descriptors: &'a RestartDescriptors) -> io::Result<Self> {
        let mut inherited = Self {
            flags: Vec::with_capacity(2),
        };
        for fd in std::iter::once(descriptors.socket.as_fd()).chain(descriptors.trace.iter().map(File::as_fd)) {
            let flags = fcntl_getfd(fd)?;
            inherited.flags.push((fd, flags));
            fcntl_setfd(fd, flags & !FdFlags::CLOEXEC)?;
        }
        Ok(inherited)
    }
}

impl Drop for Inherited<'_> {
    fn drop(&mut self) {
        for (fd, flags) in &self.flags {
            // The owning RestartDescriptors remains borrowed through restoration, so these descriptors stay open.
            let _ = fcntl_setfd(fd, *flags);
        }
    }
}
