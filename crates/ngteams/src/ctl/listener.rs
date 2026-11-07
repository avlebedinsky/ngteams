//! The socket's side of the server: the bind, a listener thread accepting connections, and a thread per connection
//! reading its request lines, handing each to GPUI's foreground ([`Job`]) and writing back its answer.
//!
//! **The bind.** The socket's directory is made `0700` when missing ([`teams_ctl::prepare_dir`]) and the socket is
//! `0600`: only the user's own processes reach it. A socket file already there is asked whether anyone is behind it:
//! one that answers the connect is another app running ([`BindError::AlreadyRunning`]); one that refuses it was left by
//! an app that didn't quit (a crash, a kill), so it is removed and bound afresh. Anything there that isn't a socket is
//! left alone ([`BindError::NotASocket`]).
//!
//! **The stop** ([`Listener::stop`]): every connection is shut down (each thread ends at its next read or write), the
//! listening socket too (its blocked `accept` returns), the listener thread is joined, and the socket file is removed
//! when it is still the one bound here.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufReader};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use teams_ctl::codec::{self, CodecError, LineReader};
use teams_ctl::protocol::{CtlError, ErrorCode, Outcome, RequestLine, ResponseLine};
use teams_ctl::{ClientError, connect_stream};
use tokio::sync::oneshot;

/// How long the bind waits for a socket file's app to take its connect.
const PROBE: Duration = Duration::from_millis(500);

/// How long an answer's write may wait on a client that doesn't read.
const WRITE: Duration = Duration::from_secs(10);

/// A request for GPUI's foreground, and where its answer goes.
pub struct Job {
    pub line: RequestLine,
    pub reply: oneshot::Sender<ResponseLine>,
}

/// Why the socket wasn't bound.
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    /// Another app answers on the socket.
    #[error("another ngteams already serves {}", .0.display())]
    AlreadyRunning(PathBuf),
    /// Something other than a socket is at the path.
    #[error("{} is there and isn't a socket: it is left as it is", .0.display())]
    NotASocket(PathBuf),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

/// The socket bound, not served yet ([`Bound::serve`]).
#[derive(Debug)]
pub struct Bound {
    listener: UnixListener,
    path: PathBuf,
    /// The socket file's device and inode: the stop removes the file only while it is this one.
    file: (u64, u64),
}

/// Bind the socket at `path` (see the module docs).
///
/// # Errors
/// [`BindError::AlreadyRunning`] when an app answers there, [`BindError::NotASocket`] when something else is there, or
/// the directory, the stale file's removal or the bind failed.
pub fn bind(path: &Path) -> Result<Bound, BindError> {
    let failed = |source: io::Error| BindError::Io {
        path: path.to_owned(),
        source,
    };
    teams_ctl::prepare_dir(path).map_err(failed)?;
    match fs::symlink_metadata(path) {
        Ok(found) if !found.file_type().is_socket() => return Err(BindError::NotASocket(path.to_owned())),
        Ok(_) => remove_stale(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(failed(error)),
    }
    let listener = UnixListener::bind(path).map_err(failed)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(failed)?;
    let bound = fs::symlink_metadata(path).map_err(failed)?;
    Ok(Bound {
        listener,
        path: path.to_owned(),
        file: (bound.dev(), bound.ino()),
    })
}

/// The socket file at `path`, asked whether an app is behind it: removed when nobody is.
fn remove_stale(path: &Path) -> Result<(), BindError> {
    let failed = |source: io::Error| BindError::Io {
        path: path.to_owned(),
        source,
    };
    match connect_stream(path, PROBE) {
        // a busy app (its backlog full) is running all the same
        Ok(_) | Err(ClientError::Timeout) => Err(BindError::AlreadyRunning(path.to_owned())),
        Err(ClientError::NotRunning { source, .. }) => match source.kind() {
            io::ErrorKind::ConnectionRefused => {
                log::info!("the control socket was left by an app that didn't quit: it is replaced");
                match fs::remove_file(path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(failed(error)),
                }
            }
            // gone meanwhile
            io::ErrorKind::NotFound => Ok(()),
            _ => Err(failed(source)),
        },
        Err(error) => Err(failed(io::Error::other(error.to_string()))),
    }
}

impl Bound {
    /// Accept connections on a thread of its own, each served on a thread of its own, their requests sent to `jobs`.
    ///
    /// # Errors
    /// The listener's thread couldn't be started (the socket is closed again and its file removed).
    pub fn serve(self, jobs: async_channel::Sender<Job>) -> io::Result<Listener> {
        let Self { listener, path, file } = self;
        let connections = Arc::new(Mutex::new(Connections::default()));
        let mut stopping = Listener {
            socket: listener.try_clone()?,
            path,
            file,
            connections: Arc::clone(&connections),
            thread: None,
        };
        let thread = thread::Builder::new()
            .name("ctl-listener".to_owned())
            .spawn(move || accept(&listener, &jobs, &connections));
        match thread {
            Ok(thread) => {
                stopping.thread = Some(thread);
                Ok(stopping)
            }
            Err(error) => {
                stopping.stop();
                Err(error)
            }
        }
    }
}

/// The connections open, each by a number of its own, so the stop can shut them down.
#[derive(Default)]
struct Connections {
    stopped: bool,
    next: u64,
    open: HashMap<u64, UnixStream>,
}

fn lock(connections: &Mutex<Connections>) -> MutexGuard<'_, Connections> {
    connections.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The socket served (see the module docs); stopped when dropped.
pub struct Listener {
    /// The listening socket, for its shutdown (the thread holds it too).
    socket: UnixListener,
    path: PathBuf,
    file: (u64, u64),
    connections: Arc<Mutex<Connections>>,
    thread: Option<JoinHandle<()>>,
}

impl Listener {
    /// Where it serves.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Close every connection and the socket, and remove its file (see the module docs). A second stop does nothing.
    pub fn stop(&mut self) {
        {
            let mut connections = lock(&self.connections);
            if connections.stopped {
                return;
            }
            connections.stopped = true;
            for (_, stream) in connections.open.drain() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        // a listening socket shut down wakes its blocked `accept` with an error
        let _ = rustix::net::shutdown(&self.socket, rustix::net::Shutdown::Both);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let ours = fs::symlink_metadata(&self.path).is_ok_and(|found| (found.dev(), found.ino()) == self.file);
        if ours && let Err(error) = fs::remove_file(&self.path) {
            log::warn!("the control socket's file wasn't removed: {error}");
        }
        log::info!("the control socket is closed");
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The listener thread: each connection accepted is served on a thread of its own, until the stop.
fn accept(listener: &UnixListener, jobs: &async_channel::Sender<Job>, connections: &Arc<Mutex<Connections>>) {
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(_) if lock(connections).stopped => return,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                // out of descriptors, say: tried again a little later rather than spun on
                log::warn!("the control socket didn't accept a connection: {error}");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let number = match register(connections, &stream) {
            Ok(Some(number)) => number,
            Ok(None) => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return;
            }
            Err(error) => {
                // out of descriptors, say: this client is turned away, the next one is accepted again
                log::warn!("a control connection wasn't held: {error}");
                let _ = stream.shutdown(std::net::Shutdown::Both);
                continue;
            }
        };
        let (jobs, held) = (jobs.clone(), Arc::clone(connections));
        let served = thread::Builder::new().name("ctl-connection".to_owned()).spawn(move || {
            serve(stream, &jobs);
            lock(&held).open.remove(&number);
        });
        if let Err(error) = served {
            // the closure (and its stream) is gone unrun: the clone held for the stop closes the connection, so the
            // client reads the end rather than waiting on an answer that never comes
            log::warn!("a control connection wasn't served: {error}");
            if let Some(stream) = lock(connections).open.remove(&number) {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

/// `stream` among the connections the stop shuts down: `Ok(None)` once stopped, an error when it can't be held (no
/// descriptor left for its clone).
fn register(connections: &Mutex<Connections>, stream: &UnixStream) -> io::Result<Option<u64>> {
    let held = stream.try_clone()?;
    let mut connections = lock(connections);
    if connections.stopped {
        return Ok(None);
    }
    let number = connections.next;
    connections.next += 1;
    connections.open.insert(number, held);
    Ok(Some(number))
}

/// One connection: each request line answered in turn, until the client closes it, the app stops serving, or a line
/// can't be read on from (one over the size cap leaves the stream mid-line).
fn serve(stream: UnixStream, jobs: &async_channel::Sender<Job>) {
    let _ = stream.set_write_timeout(Some(WRITE));
    let Ok(read) = stream.try_clone() else { return };
    let mut reader = LineReader::new(BufReader::new(read));
    let mut writer = stream;
    loop {
        let (answer, go_on) = match reader.read_line() {
            Ok(Some(line)) => match codec::decode_request(&line) {
                Ok(line) => match ask(jobs, line) {
                    Some(answer) => (answer, true),
                    // the app is going: nobody answers any more
                    None => return,
                },
                Err(bad) => (refused(bad.id, bad.error), true),
            },
            Err(CodecError::Utf8) => (bad_request("a line isn't UTF-8"), true),
            Err(CodecError::TooLong { limit }) => (bad_request(&format!("a line is over {limit} bytes")), false),
            // the client closed it, or it can't be read on from
            Ok(None) | Err(_) => return,
        };
        if codec::write_line(&mut writer, &answer).is_err() || !go_on {
            return;
        }
    }
}

/// `line` answered on GPUI's foreground; `None` when the app no longer takes requests.
fn ask(jobs: &async_channel::Sender<Job>, line: RequestLine) -> Option<ResponseLine> {
    let (reply, answer) = oneshot::channel();
    jobs.send_blocking(Job { line, reply }).ok()?;
    answer.blocking_recv().ok()
}

fn refused(id: u64, error: CtlError) -> ResponseLine {
    ResponseLine {
        id,
        outcome: Outcome::Error(error),
    }
}

fn bad_request(message: &str) -> ResponseLine {
    refused(0, CtlError::new(ErrorCode::BadRequest, message))
}
