//! A runtime broker pair, created privately before either endpoint is handed to a child. The named listener is
//! removed immediately; it is never a discoverable, long-lived credential service. Peers must be this exact process.
use std::fmt::Write as _;
use std::io;
use std::os::windows::io::OwnedHandle;
use std::time::Duration;

use interprocess::os::windows::named_pipe::{DuplexPipeStream, PipeListenerOptions, pipe_mode::Bytes};
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use widestring::U16CString;

use super::BrokerStream;

/// Create two connected, noninheritable byte-pipe endpoints protected for this exact user and SYSTEM only.
/// The runner transfers only the required end to each process. No data or credential is sent during creation.
///
/// # Errors
/// Randomness, security descriptor, pipe creation/connect, peer validation or handle conversion failed.
pub fn private_pair() -> io::Result<(BrokerStream, BrokerStream)> {
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(io::Error::other)?;
    let mut name = String::from(r"\\.\pipe\ngteams-broker-");
    for byte in nonce {
        let _ = write!(&mut name, "{byte:02x}");
    }
    let sddl = U16CString::from_str(teams_platform::windows_security::pipe_sddl()?).map_err(io::Error::other)?;
    let listener = PipeListenerOptions::new()
        .path(name.as_str())
        .nonblocking(true)
        .accept_remote(false)
        .security_descriptor(Some(SecurityDescriptor::deserialize(&sddl)?))
        .create_duplex::<Bytes>()?;
    let client = DuplexPipeStream::<Bytes>::connect_by_path_with_wait_mode(
        name.as_str(),
        interprocess::ConnectWaitMode::Timeout(Duration::from_secs(1)),
    )?;
    let server = listener.accept()?;
    server.set_nonblocking(false)?;
    if server.client_process_id()? != std::process::id() || client.server_process_id()? != std::process::id() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the broker pair has an unexpected peer",
        ));
    }
    let convert = |pipe: DuplexPipeStream<Bytes>| {
        let handle: OwnedHandle = pipe
            .try_into()
            .map_err(|_| io::Error::other("the broker endpoint is shared"))?;
        interprocess::os::windows::named_pipe::local_socket::Stream::try_from(handle)
            .map(BrokerStream::NamedPipe)
            .map_err(io::Error::from)
    };
    Ok((convert(server)?, convert(client)?))
}
