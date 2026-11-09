//! Synthetic broker transports, with real Unix sockets or Windows named pipes.
#![cfg(test)]

use std::io;
use teams_auth::broker::BrokerStream;

#[cfg(unix)]
pub type AsyncStream = tokio::net::UnixStream;
#[cfg(windows)]
pub type AsyncStream = interprocess::os::windows::named_pipe::local_socket::tokio::Stream;

pub fn pair_sync() -> io::Result<(BrokerStream, BrokerStream)> {
    #[cfg(unix)]
    {
        BrokerStream::pair()
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::{
            GenericNamespaced, ListenerOptions, ToNsName,
            traits::{Listener as _, Stream as _},
        };
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "ngteams-auth-synthetic-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        // Only synthetic test data crosses this test-only listener. Runtime broker listeners must supply their
        // own private security descriptor; the broker accepts an already established stream.
        let listener = ListenerOptions::new()
            .name(name.as_str().to_ns_name::<GenericNamespaced>()?)
            .create_sync()?;
        let client = BrokerStream::connect(name.as_str().to_ns_name::<GenericNamespaced>()?)?;
        let server = listener.accept()?;
        Ok((server, client))
    }
}

pub fn pair() -> io::Result<(AsyncStream, BrokerStream)> {
    let (server, client) = pair_sync()?;
    #[cfg(unix)]
    {
        server.set_nonblocking(true)?;
        Ok((AsyncStream::from_std(server)?, client))
    }
    #[cfg(windows)]
    {
        let interprocess::local_socket::Stream::NamedPipe(server) = server;
        let handle: std::os::windows::io::OwnedHandle = server.into();
        let server = AsyncStream::try_from(handle).map_err(|error| io::Error::other(format!("{error:?}")))?;
        Ok((server, client))
    }
}
