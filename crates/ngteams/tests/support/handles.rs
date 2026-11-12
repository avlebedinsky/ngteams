//! Sole-ownership transfer for synthetic broker endpoints and trace files.
use ngteams::descriptors::RawDescriptor;
use teams_auth::broker::BrokerStream;

#[cfg(unix)]
pub const INVALID: RawDescriptor = -1;
#[cfg(windows)]
pub const INVALID: RawDescriptor = usize::MAX;

pub fn socket(stream: BrokerStream) -> RawDescriptor {
    #[cfg(unix)]
    {
        use std::os::fd::IntoRawFd as _;
        stream.into_raw_fd()
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{IntoRawHandle as _, OwnedHandle};
        let BrokerStream::NamedPipe(pipe) = stream;
        let handle: OwnedHandle = pipe.into();
        handle.into_raw_handle() as RawDescriptor
    }
}

pub fn file(file: std::fs::File) -> RawDescriptor {
    #[cfg(unix)]
    {
        use std::os::fd::IntoRawFd as _;
        file.into_raw_fd()
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle as _;
        file.into_raw_handle() as RawDescriptor
    }
}
