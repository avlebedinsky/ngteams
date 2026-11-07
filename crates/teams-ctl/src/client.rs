//! The client: connects to the app's socket, says `hello`, then asks one request at a time, or subscribes.
//!
//! Every wait is bounded ([`Timeouts`]): the connect (a listener whose backlog is full makes a Unix connect wait), and
//! each read and write. A `subscribe` stream waits for events without a bound unless one is set
//! ([`Events::set_read_timeout`]).

use std::io::{self, BufReader};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};

use crate::codec::{self, CodecError, LineReader};
use crate::protocol::{
    CtlError, EventFilter, EventLine, Hello, Outcome, PROTOCOL_VERSION, Request, RequestLine, Response, ResponseLine,
};

/// How long a connect, and each read and write, may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    pub connect: Duration,
    pub read: Duration,
    pub write: Duration,
}

impl Default for Timeouts {
    /// A second to connect; ten to answer, as a call command waits a few seconds for the engine's answer.
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(1),
            read: Duration::from_secs(10),
            write: Duration::from_secs(10),
        }
    }
}

/// Why a client's request didn't get its answer.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Nothing listens on the socket: the app isn't running (or runs without this socket).
    #[error("no ngteams is running on {}: {source}", path.display())]
    NotRunning { path: PathBuf, source: io::Error },
    /// The connect or an answer didn't come in time.
    #[error("the app didn't answer in time")]
    Timeout,
    /// The app answered with an error.
    #[error(transparent)]
    Server(#[from] CtlError),
    /// The app speaks another protocol version.
    #[error("the app speaks protocol {theirs}, this client {ours}")]
    VersionMismatch { ours: u32, theirs: u32 },
    /// The app answered something other than what was asked.
    #[error("unexpected answer: {0}")]
    Protocol(String),
    /// The app closed the connection before answering.
    #[error("the app closed the connection")]
    Closed,
    #[error(transparent)]
    Codec(CodecError),
}

impl From<CodecError> for ClientError {
    fn from(error: CodecError) -> Self {
        match error {
            CodecError::Io(error) => Self::from(error),
            error => Self::Codec(error),
        }
    }
}

impl From<io::Error> for ClientError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Self::Timeout,
            _ => Self::Codec(CodecError::Io(error)),
        }
    }
}

/// A connection to the app, past its `hello`.
#[derive(Debug)]
pub struct Client {
    writer: UnixStream,
    reader: LineReader<BufReader<UnixStream>>,
    next_id: u64,
    hello: Hello,
}

impl Client {
    /// Connect to the socket at `path` and say `hello`.
    ///
    /// # Errors
    /// [`ClientError::NotRunning`] when nothing listens there, [`ClientError::Timeout`] when the connect or the
    /// `hello` takes too long, [`ClientError::VersionMismatch`] or [`ClientError::Server`] when the app refuses the
    /// client's version, or the connection's own error.
    pub fn connect(path: &Path, timeouts: Timeouts) -> Result<Self, ClientError> {
        let stream = connect_within(path, timeouts.connect)?;
        stream.set_read_timeout(Some(timeouts.read))?;
        stream.set_write_timeout(Some(timeouts.write))?;
        let reader = LineReader::new(BufReader::new(stream.try_clone()?));
        let mut client = Self {
            writer: stream,
            reader,
            next_id: 1,
            hello: Hello {
                protocol: 0,
                app: String::new(),
            },
        };
        let hello = client.request(
            None,
            Request::Hello {
                protocol: PROTOCOL_VERSION,
                client: env!("CARGO_PKG_VERSION").to_owned(),
            },
        )?;
        let Response::Hello(hello) = hello else {
            return Err(ClientError::Protocol(format!("{hello:?} for hello")));
        };
        if hello.protocol != PROTOCOL_VERSION {
            return Err(ClientError::VersionMismatch {
                ours: PROTOCOL_VERSION,
                theirs: hello.protocol,
            });
        }
        client.hello = hello;
        Ok(client)
    }

    /// The app's half of the `hello`.
    #[must_use]
    pub fn hello(&self) -> &Hello {
        &self.hello
    }

    /// Ask `request` of `account` (the app's default account without one) and wait for its answer.
    ///
    /// # Errors
    /// [`ClientError::Server`] for an error answer, [`ClientError::Timeout`] when it doesn't come in time,
    /// [`ClientError::Closed`] when the app closes the connection first, [`ClientError::Protocol`] for an answer to
    /// a request not yet asked, or the connection's own error. After a timeout the client can ask on: the late answer
    /// is skipped when it comes.
    pub fn request(&mut self, account: Option<&str>, request: Request) -> Result<Response, ClientError> {
        let id = self.send(account, request)?;
        self.answer(id)
    }

    /// Ask `request` and hand back the answer's line as it came: what `--json` prints.
    ///
    /// # Errors
    /// As [`Client::request`], except that an error answer is a line like any other.
    pub fn request_line(&mut self, account: Option<&str>, request: Request) -> Result<ResponseLine, ClientError> {
        let id = self.send(account, request)?;
        self.read_answer(id)
    }

    /// Subscribe to the events `filter` lets through: the connection becomes their stream.
    ///
    /// # Errors
    /// As [`Client::request`].
    pub fn subscribe(mut self, account: Option<&str>, filter: EventFilter) -> Result<Events, ClientError> {
        let id = self.send(account, Request::Subscribe { filter })?;
        match self.answer(id)? {
            Response::Subscribed => {}
            other => return Err(ClientError::Protocol(format!("{other:?} for subscribe"))),
        }
        self.writer.set_read_timeout(None)?;
        Ok(Events { reader: self.reader })
    }

    fn send(&mut self, account: Option<&str>, request: Request) -> Result<u64, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let line = RequestLine {
            id,
            account: account.map(str::to_owned),
            request,
        };
        codec::write_line(&mut self.writer, &line)?;
        Ok(id)
    }

    /// Read the answer to `id`. Ids only go up, so a line for an earlier id is the late answer to a request that
    /// timed out: it is skipped, which keeps the connection in step after a [`ClientError::Timeout`].
    fn read_answer(&mut self, id: u64) -> Result<ResponseLine, ClientError> {
        loop {
            let line: ResponseLine = self.reader.read()?.ok_or(ClientError::Closed)?;
            if line.id == id {
                return Ok(line);
            }
            if line.id > id {
                return Err(ClientError::Protocol(format!(
                    "the answer to request {} for {id}",
                    line.id
                )));
            }
        }
    }

    fn answer(&mut self, id: u64) -> Result<Response, ClientError> {
        match self.read_answer(id)?.outcome {
            Outcome::Ok(response) => Ok(response),
            Outcome::Error(error) => Err(ClientError::Server(error)),
        }
    }
}

/// A subscription's events, until the app closes the stream (after a `lagged` event, or when it quits).
#[derive(Debug)]
pub struct Events {
    reader: LineReader<BufReader<UnixStream>>,
}

impl Events {
    /// Bound the wait for each next event (`None`: wait as long as it takes). An event not come in time is
    /// [`ClientError::Timeout`], and the stream can be read on.
    ///
    /// # Errors
    /// The socket's own error.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<(), ClientError> {
        self.reader.get_ref().get_ref().set_read_timeout(timeout)?;
        Ok(())
    }

    /// The next event; `None` once the app closed the stream.
    ///
    /// # Errors
    /// [`ClientError::Timeout`] past the bound set, a line that isn't an event, or the connection's own error.
    pub fn next_event(&mut self) -> Result<Option<EventLine>, ClientError> {
        Ok(self.reader.read()?)
    }
}

impl Iterator for Events {
    type Item = Result<EventLine, ClientError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_event().transpose()
    }
}

/// Connect within `timeout`. A Unix connect doesn't wait on a non-blocking socket: it fails with `EAGAIN` while the
/// listener's backlog is full, so it is tried again until the time is up.
fn connect_within(path: &Path, timeout: Duration) -> Result<UnixStream, ClientError> {
    let not_running = |source: io::Error| ClientError::NotRunning {
        path: path.to_owned(),
        source,
    };
    let address = SocketAddrUnix::new(path).map_err(|errno| not_running(errno.into()))?;
    let socket = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )
    .map_err(io::Error::from)?;
    let deadline = Instant::now() + timeout;
    loop {
        match rustix::net::connect(&socket, &address) {
            Ok(()) => break,
            Err(Errno::INTR) => {}
            Err(Errno::AGAIN) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Err(Errno::AGAIN) => return Err(ClientError::Timeout),
            Err(errno) => return Err(not_running(errno.into())),
        }
    }
    let stream = UnixStream::from(socket);
    stream.set_nonblocking(false)?;
    Ok(stream)
}
