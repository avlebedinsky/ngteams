//! The control protocol's native byte stream. Unix sockets keep their kernel timeouts; Windows uses local-only,
//! owner-protected named pipes with server-user verification before the client sends any request.

#[cfg(unix)]
#[path = "transport/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "transport/windows.rs"]
mod platform;

pub use platform::*;
