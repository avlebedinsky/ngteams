//! Only the broker's OS transport varies; its framing, scope and failure behavior stay shared.
use std::io;

#[cfg(unix)]
pub type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
pub type Stream = interprocess::local_socket::Stream;

pub(super) fn blocking(stream: &Stream) -> io::Result<()> {
    #[cfg(windows)]
    use interprocess::local_socket::traits::Stream as _;
    stream.set_nonblocking(false)
}
