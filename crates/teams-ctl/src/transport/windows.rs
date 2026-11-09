//! Byte pipes are nonblocking at the OS boundary: retries implement the same bounded I/O as the Unix client without
//! leaving a timed-out read in flight. Peek distinguishes a silent open pipe from EOF. Partial byte writes are
//! returned to `write_all`; a full pipe is retried until the write deadline. No flush waits for an untrusted reader.
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use interprocess::os::windows::named_pipe::{DuplexPipeStream, PipeListener, PipeListenerOptions, pipe_mode::Bytes};
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use sha2::{Digest as _, Sha256};
use teams_platform::windows_security as security;
use widestring::U16CString;
use windows_sys::Win32::System::Pipes::{DisconnectNamedPipe, GetNamedPipeInfo, PeekNamedPipe};

const POLL: Duration = Duration::from_millis(10);
type Pipe = DuplexPipeStream<Bytes>;

/// A byte stream; clones share timeouts and shutdown state as Unix socket clones do.
#[derive(Clone, Debug)]
pub struct Stream {
    pipe: Arc<Mutex<Option<Pipe>>>,
    state: Arc<State>,
    write_chunk: usize,
}

#[derive(Debug, Default)]
struct State {
    read: Mutex<Option<Duration>>,
    write: Mutex<Option<Duration>>,
    closed: AtomicBool,
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A local-only named-pipe listener. The first instance refuses a second server at the same name.
#[derive(Debug)]
pub struct Listener(PipeListener<Bytes, Bytes>);

impl Listener {
    /// Bind a pipe derived from the logical socket path and current user, with a protected DACL and integrity label.
    ///
    /// # Errors
    /// An existing listener, invalid path, or native security/pipe failure.
    pub fn bind(path: &Path) -> io::Result<Self> {
        let name = pipe_name(path)?;
        let sddl = U16CString::from_str(security::pipe_sddl()?).map_err(io::Error::other)?;
        PipeListenerOptions::new()
            .path(name.as_str())
            .nonblocking(true)
            .input_buffer_size_hint(64 * 1024)
            .output_buffer_size_hint(64 * 1024)
            .security_descriptor(Some(SecurityDescriptor::deserialize(&sddl)?))
            .create_duplex::<Bytes>()
            .map(Self)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::PermissionDenied {
                    io::Error::new(io::ErrorKind::AddrInUse, error)
                } else {
                    error
                }
            })
    }

    /// Accept an already connected client; `WouldBlock` when no client is waiting. Never waits for a client.
    ///
    /// # Errors
    /// Native pipe or peer-user validation failure.
    pub fn accept(&self) -> io::Result<Stream> {
        let pipe = self.0.accept()?;
        security::check_user(pipe.client_process_id()?)?;
        Stream::new(pipe)
    }
}

/// A stable local pipe name. Hashes UTF-16 units without lossy conversion, including the user SID, so a different
/// user's profile, another mode or invalid-Unicode path cannot alias this endpoint. No tenant text enters the name.
///
/// # Errors
/// A non-absolute logical path, embedded NUL, or an unavailable process SID.
pub fn pipe_name(path: &Path) -> io::Result<String> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a control endpoint needs an absolute path",
        ));
    }
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if units.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an endpoint contains a NUL",
        ));
    }
    let mut digest = Sha256::new();
    digest.update(security::current_user_sid()?);
    digest.update([0]);
    for unit in units {
        digest.update(unit.to_le_bytes());
    }
    let mut name = String::from(r"\\.\pipe\ngteams-ctl-");
    for byte in digest.finalize() {
        let _ = write!(&mut name, "{byte:02x}");
    }
    Ok(name)
}

/// Connect without sending a byte, verify the server's user, then enable bounded I/O.
///
/// # Errors
/// `TimedOut` while all instances stay busy, any native pipe failure or a server owned by another user.
pub fn connect(path: &Path, timeout: Duration) -> io::Result<Stream> {
    let name = pipe_name(path)?;
    let pipe = Pipe::connect_by_path_with_wait_mode(name.as_str(), interprocess::ConnectWaitMode::Timeout(timeout))?;
    security::check_user(pipe.server_process_id()?)?;
    Stream::new(pipe)
}

impl Stream {
    fn new(pipe: Pipe) -> io::Result<Self> {
        pipe.set_nonblocking(true)?;
        let (mut output, mut input) = (0, 0);
        // SAFETY: the pipe and both size outputs remain alive; no flags or instance count are requested.
        if unsafe {
            GetNamedPipeInfo(
                pipe.as_raw_handle(),
                null_mut(),
                &raw mut output,
                &raw mut input,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // An overlapped NOWAIT write larger than the pipe's quota can complete with zero even when it is empty.
        // Cap each write to its actual outgoing quota; write_all carries the rest, preserving byte order.
        let quota = if pipe.is_server() { output } else { input };
        let write_chunk = usize::try_from(quota.max(1)).map_err(io::Error::other)?;
        Ok(Self {
            pipe: Arc::new(Mutex::new(Some(pipe))),
            state: Arc::default(),
            write_chunk,
        })
    }

    /// A handle to the same connection, with shared deadlines and shutdown state.
    ///
    /// # Errors
    /// This operation is infallible on Windows (no new OS handle is needed).
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(self.clone())
    }

    /// Bound subsequent reads; `None` waits until data, EOF or shutdown. Zero is refused.
    ///
    /// # Errors
    /// `InvalidInput` for zero.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        set_timeout(&self.state.read, timeout)
    }

    /// Bound subsequent writes; zero is refused.
    ///
    /// # Errors
    /// `InvalidInput` for zero.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        set_timeout(&self.state.write, timeout)
    }

    /// Close the connection both ways (Windows pipes do not support half-close), waking all polling clones.
    ///
    /// # Errors
    /// This is best effort and infallible, including for an already closed connection.
    pub fn shutdown(&self, _how: Shutdown) -> io::Result<()> {
        self.state.closed.store(true, Ordering::Release);
        // SAFETY: the pipe remains owned throughout the call. A client end or a disconnected pipe may fail; the
        // shared closed bit still terminates every pending operation on our clones within POLL.
        if let Some(pipe) = lock(&self.pipe).take() {
            // SAFETY: this borrowed handle is still owned by pipe. A client end may refuse DisconnectNamedPipe;
            // closing the owned handle below still delivers EOF to the server.
            unsafe { DisconnectNamedPipe(pipe.as_raw_handle()) };
            // A forced shutdown discards pending output; never leave interprocess's flush worker waiting on a peer.
            pipe.evade_limbo();
        }
        Ok(())
    }

    /// Whether a subscriber closed its connection or sent unexpected bytes after subscribing.
    ///
    /// # Errors
    /// A native peek failure other than EOF.
    pub fn peer_closed(&self) -> io::Result<bool> {
        Ok(self.state.closed.load(Ordering::Acquire) || self.available()? != Some(0))
    }

    fn available(&self) -> io::Result<Option<u32>> {
        let held = lock(&self.pipe);
        let Some(pipe) = held.as_ref() else {
            return Ok(None);
        };
        let mut available = 0;
        // SAFETY: no data is copied (null buffer, zero size); the pipe and output remain alive across the call.
        let ok = unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                null_mut(),
                0,
                null_mut(),
                &raw mut available,
                null_mut(),
            )
        };
        if ok != 0 {
            return Ok(Some(available));
        }
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(109 | 232 | 233)) {
            return Ok(None);
        }
        Err(error)
    }

    fn wait(&self, deadline: Option<Instant>) -> io::Result<()> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(io::ErrorKind::TimedOut.into());
        }
        thread::sleep(POLL);
        Ok(())
    }
}

fn set_timeout(value: &Mutex<Option<Duration>>, timeout: Option<Duration>) -> io::Result<()> {
    if timeout == Some(Duration::ZERO) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    *lock(value) = timeout;
    Ok(())
}

impl Read for &Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let deadline = lock(&self.state.read).map(|duration| Instant::now() + duration);
        loop {
            if buf.is_empty() || self.state.closed.load(Ordering::Acquire) {
                return Ok(0);
            }
            match self.available()? {
                None => return Ok(0),
                Some(0) => self.wait(deadline)?,
                Some(_) => {
                    return match lock(&self.pipe).as_ref() {
                        Some(pipe) => (&*pipe).read(buf),
                        None => Ok(0),
                    };
                }
            }
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self).read(buf)
    }
}

impl Write for &Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let deadline = lock(&self.state.write).map(|duration| Instant::now() + duration);
        let buf = &buf[..buf.len().min(self.write_chunk)];
        loop {
            if self.state.closed.load(Ordering::Acquire) {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            let written = match lock(&self.pipe).as_ref() {
                Some(pipe) => (&*pipe).write(buf),
                None => return Err(io::ErrorKind::BrokenPipe.into()),
            };
            match written {
                Ok(0) if !buf.is_empty() => self.wait(deadline)?,
                result => return result,
            }
        }
    }
    // WriteFile already places each byte in the pipe. FlushFileBuffers would wait unboundedly for the peer to read.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    };

    #[test]
    fn the_live_pipe_has_only_its_exact_user_and_system_grants_and_an_integrity_label() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ctl.sock");
        let listener = Listener::bind(&path).unwrap();
        let client = connect(&path, Duration::from_secs(1)).unwrap();
        let server = listener.accept().unwrap();
        let held = lock(&server.pipe);
        let pipe = held.as_ref().unwrap();
        let info = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION;
        let mut descriptor = null_mut();
        // SAFETY: all outputs not requested are null; descriptor outlives the query and is freed below.
        let status = unsafe {
            GetSecurityInfo(
                pipe.as_raw_handle(),
                SE_KERNEL_OBJECT,
                info,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &raw mut descriptor,
            )
        };
        assert_eq!(status, 0);
        let mut text = null_mut();
        // SAFETY: GetSecurityInfo returned a valid descriptor; text is a LocalAlloc output.
        let converted = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                info,
                &raw mut text,
                null_mut(),
            )
        };
        // SAFETY: descriptor was allocated by GetSecurityInfo and is no longer used.
        unsafe { LocalFree(descriptor) };
        assert_ne!(converted, 0);
        // SAFETY: the conversion returns a NUL-terminated UTF-16 allocation.
        let actual = unsafe { widestring::U16CStr::from_ptr_str(text) }.to_string().unwrap();
        // SAFETY: text was allocated by the conversion and is no longer borrowed.
        unsafe { LocalFree(text.cast()) };
        // Windows maps generic full access to file full access, orders label bits, and may mark the SACL auto-inherited.
        let canonical = actual
            .replace(";;FA;", ";;GA;")
            .replace("S:AI(", "S:(")
            .replace(";;NWNR;", ";;NRNW;");
        assert_eq!(canonical, security::pipe_sddl().unwrap());
        drop(held);
        client.shutdown(Shutdown::Both).unwrap();
    }
}
