//! Unix connects, bounded even when the listener's backlog stays full.
use std::io;
use std::net::Shutdown;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};

pub use std::os::unix::net::UnixStream as Stream;

/// Ends a response stream without discarding bytes already written. Unix shutdown preserves its queued output.
///
/// # Errors
/// The socket's shutdown error.
pub fn finish_writes(stream: &Stream, _deadline: Instant) -> io::Result<()> {
    stream.shutdown(Shutdown::Both)
}

/// Connect without sending a byte, within `timeout`.
///
/// # Errors
/// The socket error, or `TimedOut` when its backlog stays full.
pub fn connect(path: &Path, timeout: Duration) -> io::Result<Stream> {
    let address = SocketAddrUnix::new(path)?;
    let socket = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )?;
    let deadline = Instant::now() + timeout;
    loop {
        match rustix::net::connect(&socket, &address) {
            Ok(()) => break,
            Err(Errno::INTR) => {}
            Err(Errno::AGAIN) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Err(Errno::AGAIN) => return Err(io::ErrorKind::TimedOut.into()),
            Err(errno) => return Err(errno.into()),
        }
    }
    let stream = Stream::from(socket);
    stream.set_nonblocking(false)?;
    Ok(stream)
}
