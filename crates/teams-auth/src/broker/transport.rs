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

/// The broker server's native asynchronous byte stream.
#[cfg(unix)]
pub type AsyncStream = tokio::net::UnixStream;
#[cfg(windows)]
pub type AsyncStream = interprocess::os::windows::named_pipe::local_socket::tokio::Stream;

/// Register an established broker endpoint with the current Tokio runtime.
///
/// # Errors
/// The endpoint cannot be made nonblocking or registered with the runtime's I/O driver.
pub fn into_async(stream: Stream) -> io::Result<AsyncStream> {
    #[cfg(unix)]
    {
        stream.set_nonblocking(true)?;
        AsyncStream::from_std(stream)
    }
    #[cfg(windows)]
    {
        // NOWAIT pipes report a silent peer as ERROR_NO_DATA; Tokio's overlapped reads require WAIT mode.
        blocking(&stream)?;
        let Stream::NamedPipe(stream) = stream;
        let handle: std::os::windows::io::OwnedHandle = stream.into();
        AsyncStream::try_from(handle).map_err(io::Error::from)
    }
}
