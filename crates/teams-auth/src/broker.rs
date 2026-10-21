//! The credential broker: one account's secrets served over a socket, and the [`SecretStore`] that uses them.
//!
//! A process in the UI sandbox holds no connection to the user's session bus, so it can't reach the Secret Service.
//! The broker, a process outside the sandbox, holds the one real store and serves `get`, `put` and `delete` of one
//! account's items in one application namespace ([`BrokerScope`]) on its end of a socket pair ([`serve`]); the
//! sandboxed process uses the other end through [`BrokerSecretStore`]. A request for any other account or application
//! is answered "refused" and reaches no store.
//!
//! The wire: a frame is a 4-byte big-endian length and that many bytes, 1 to [`MAX_FRAME`]. A request is an operation
//! byte (`get`, `put`, `delete`) and its fields, each a 4-byte big-endian length and that many UTF-8 bytes: the
//! application, the account, the audience, and for `put` the secret; nothing follows the last field. An answer is a
//! status byte (done, found, missing, refused, failed) and, when found, the secret as a field, when failed, the store's
//! error as one. A frame that is none of these, a length out of range, or an end of stream inside a frame ends the
//! conversation: the side that read it closes the socket, and the client's later calls fail at once
//! ([`BrokerError::Closed`]).
//!
//! No value is shown by either side: [`Secret`] has no `Display` and a redacted `Debug`, the frames that carry one are
//! zeroed on drop, and a [`Note`] names only the operation, the account and the audience.

use std::fmt;
use std::io::{self, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use zeroize::Zeroizing;

use crate::secrets::{Secret, SecretError, SecretKey, SecretStore};

/// The largest frame either side sends or reads, in bytes: far above any token (a few KiB), far below anything that
/// could hurt.
pub const MAX_FRAME: usize = 64 * 1024;

/// How long a failed store's error may be on the wire, in bytes.
const MAX_FAILURE_TEXT: usize = 1024;

const LENGTH: usize = 4;

const OP_GET: u8 = 1;
const OP_PUT: u8 = 2;
const OP_DELETE: u8 = 3;

const ANSWER_DONE: u8 = 0;
const ANSWER_FOUND: u8 = 1;
const ANSWER_MISSING: u8 = 2;
const ANSWER_REFUSED: u8 = 3;
const ANSWER_FAILED: u8 = 4;

/// What a broker serves: one application namespace (the lab's `teams-lab`) and one account (its UPN), every audience
/// under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerScope {
    application: String,
    account: String,
}

impl BrokerScope {
    #[must_use]
    pub fn new(application: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            application: application.into(),
            account: account.into(),
        }
    }

    #[must_use]
    pub fn application(&self) -> &str {
        &self.application
    }

    #[must_use]
    pub fn account(&self) -> &str {
        &self.account
    }

    /// Whether a request for `key` in `application` is this scope's: the same application and account, exactly, and
    /// an audience.
    fn admits(&self, application: &str, key: &SecretKey) -> bool {
        application == self.application && key.account() == self.account && !key.audience().is_empty()
    }
}

/// A store operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Get,
    Put,
    Delete,
}

impl Op {
    const fn code(self) -> u8 {
        match self {
            Self::Get => OP_GET,
            Self::Put => OP_PUT,
            Self::Delete => OP_DELETE,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            OP_GET => Some(Self::Get),
            OP_PUT => Some(Self::Put),
            OP_DELETE => Some(Self::Delete),
            _ => None,
        }
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Get => "get",
            Self::Put => "put",
            Self::Delete => "delete",
        })
    }
}

/// Why the broker didn't do what was asked, as the client sees it. Never carries a secret value.
#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error("the credential broker refused the request: not its account or application")]
    Refused,
    #[error("the credential broker's store failed: {0}")]
    Store(String),
    #[error("the credential broker's socket: {0}")]
    Io(#[from] io::Error),
    #[error("the credential broker sent a malformed answer")]
    Malformed,
    #[error("the request doesn't fit in one frame")]
    TooLarge,
    #[error("the connection to the credential broker is closed")]
    Closed,
}

/// Why [`serve`] ended the conversation and closed the socket.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("the socket: {0}")]
    Io(#[from] io::Error),
    #[error("a frame of {0} bytes (allowed: 1 to {MAX_FRAME})")]
    BadLength(usize),
    #[error("a malformed request")]
    Malformed,
    #[error("the stream ended inside a frame")]
    Truncated,
}

/// What [`serve`] did with one request: the operation and the key it named (as the peer sent them), and how it ended.
/// Its `Display` quotes the key's parts with escapes, since they come from the peer.
#[derive(Debug)]
pub struct Note<'a> {
    pub op: Op,
    pub key: &'a SecretKey,
    pub outcome: Outcome,
}

/// How one request ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Found,
    Missing,
    Done,
    Refused,
    Failed,
}

impl fmt::Display for Note<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let outcome = match self.outcome {
            Outcome::Found => "found",
            Outcome::Missing => "missing",
            Outcome::Done => "done",
            Outcome::Refused => "refused (not this broker's account or application)",
            Outcome::Failed => "the store failed",
        };
        write!(
            f,
            "{} {:?} {:?}: {outcome}",
            self.op,
            self.key.account(),
            self.key.audience()
        )
    }
}

/// A decoded request.
#[derive(Debug)]
struct Request {
    op: Op,
    application: String,
    key: SecretKey,
    secret: Option<Secret>,
}

/// A decoded answer.
#[derive(Debug)]
enum Answer {
    Done,
    Found(Secret),
    Missing,
    Refused,
    Failed(String),
}

/// Reads one field after another out of a frame's body.
struct Fields<'a> {
    rest: &'a [u8],
}

impl<'a> Fields<'a> {
    fn new(body: &'a [u8]) -> Self {
        Self { rest: body }
    }

    fn byte(&mut self) -> Option<u8> {
        let (&first, rest) = self.rest.split_first()?;
        self.rest = rest;
        Some(first)
    }

    fn text(&mut self) -> Option<&'a str> {
        let (length, rest) = self.rest.split_first_chunk::<LENGTH>()?;
        let length = usize::try_from(u32::from_be_bytes(*length)).ok()?;
        if length > rest.len() {
            return None;
        }
        let (text, rest) = rest.split_at(length);
        self.rest = rest;
        std::str::from_utf8(text).ok()
    }

    /// The fields read so far, if nothing follows them.
    fn end<T>(self, value: T) -> Option<T> {
        self.rest.is_empty().then_some(value)
    }
}

/// A whole frame (its length first) holding `tag` and `fields`, in a buffer allocated once at its final size, so no
/// copy of a secret is left behind in a freed one; zeroed on drop.
fn frame(tag: u8, fields: &[&str]) -> Option<Zeroizing<Vec<u8>>> {
    let body = 1 + fields.iter().map(|field| LENGTH + field.len()).sum::<usize>();
    if body > MAX_FRAME {
        return None;
    }
    let mut frame = Zeroizing::new(Vec::with_capacity(LENGTH + body));
    frame.extend_from_slice(&u32::try_from(body).ok()?.to_be_bytes());
    frame.push(tag);
    for field in fields {
        frame.extend_from_slice(&u32::try_from(field.len()).ok()?.to_be_bytes());
        frame.extend_from_slice(field.as_bytes());
    }
    Some(frame)
}

/// A frame's body length from its header, if it is in range.
fn body_length(header: [u8; LENGTH]) -> Result<usize, usize> {
    let length = usize::try_from(u32::from_be_bytes(header)).unwrap_or(usize::MAX);
    if (1..=MAX_FRAME).contains(&length) {
        Ok(length)
    } else {
        Err(length)
    }
}

impl Request {
    fn encode(op: Op, application: &str, key: &SecretKey, secret: Option<&Secret>) -> Option<Zeroizing<Vec<u8>>> {
        let mut fields = vec![application, key.account(), key.audience()];
        fields.extend(secret.map(Secret::expose));
        frame(op.code(), &fields)
    }

    fn decode(body: &[u8]) -> Option<Self> {
        let mut fields = Fields::new(body);
        let op = Op::from_code(fields.byte()?)?;
        let application = fields.text()?.to_owned();
        let key = SecretKey::new(fields.text()?, fields.text()?);
        let secret = match op {
            Op::Put => Some(Secret::new(fields.text()?)),
            Op::Get | Op::Delete => None,
        };
        fields.end(Self {
            op,
            application,
            key,
            secret,
        })
    }
}

impl Answer {
    /// The answer as a frame; a found secret too large for one is answered as a failure.
    fn encode(&self) -> Zeroizing<Vec<u8>> {
        let encoded = match self {
            Self::Done => frame(ANSWER_DONE, &[]),
            Self::Found(secret) => frame(ANSWER_FOUND, &[secret.expose()]),
            Self::Missing => frame(ANSWER_MISSING, &[]),
            Self::Refused => frame(ANSWER_REFUSED, &[]),
            Self::Failed(text) => frame(ANSWER_FAILED, &[truncated(text, MAX_FAILURE_TEXT)]),
        };
        encoded
            .or_else(|| frame(ANSWER_FAILED, &["the stored secret doesn't fit in one frame"]))
            .unwrap_or_default()
    }

    fn decode(body: &[u8]) -> Option<Self> {
        let mut fields = Fields::new(body);
        let answer = match fields.byte()? {
            ANSWER_DONE => Self::Done,
            ANSWER_FOUND => Self::Found(Secret::new(fields.text()?)),
            ANSWER_MISSING => Self::Missing,
            ANSWER_REFUSED => Self::Refused,
            ANSWER_FAILED => Self::Failed(fields.text()?.to_owned()),
            _ => return None,
        };
        fields.end(answer)
    }

    /// Whether this is an answer `op` can get.
    fn fits(&self, op: Op) -> bool {
        match self {
            Self::Refused | Self::Failed(_) => true,
            Self::Found(_) | Self::Missing => op == Op::Get,
            Self::Done => op != Op::Get,
        }
    }

    const fn outcome(&self) -> Outcome {
        match self {
            Self::Done => Outcome::Done,
            Self::Found(_) => Outcome::Found,
            Self::Missing => Outcome::Missing,
            Self::Refused => Outcome::Refused,
            Self::Failed(_) => Outcome::Failed,
        }
    }
}

/// `text` cut to at most `limit` bytes, at a character boundary.
fn truncated(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Serves `store`'s items in `scope` on `stream` until the peer closes it (`Ok`), one request at a time, each answered
/// before the next is read. A request outside `scope` is answered "refused" and reaches no store. A malformed or
/// oversized frame, or an end inside one, ends it with an error, and `stream` is closed on return either way.
/// `note` hears of each request answered.
///
/// # Errors
/// See [`ServeError`].
pub async fn serve<T, S>(
    mut stream: T,
    store: &S,
    scope: &BrokerScope,
    mut note: impl FnMut(&Note<'_>),
) -> Result<(), ServeError>
where
    T: AsyncRead + AsyncWrite + Unpin,
    S: SecretStore,
{
    while let Some(body) = read_request(&mut stream).await? {
        let request = Request::decode(&body).ok_or(ServeError::Malformed)?;
        let answer = answer(store, scope, &request).await;
        note(&Note {
            op: request.op,
            key: &request.key,
            outcome: answer.outcome(),
        });
        stream.write_all(&answer.encode()).await?;
        stream.flush().await?;
    }
    Ok(())
}

/// The next request's body, or `None` when the peer closed the stream between frames.
async fn read_request<T: AsyncRead + Unpin>(stream: &mut T) -> Result<Option<Zeroizing<Vec<u8>>>, ServeError> {
    let mut header = [0; LENGTH];
    let mut filled = 0;
    while filled < LENGTH {
        let read = stream.read(&mut header[filled..]).await?;
        if read == 0 {
            return if filled == 0 {
                Ok(None)
            } else {
                Err(ServeError::Truncated)
            };
        }
        filled += read;
    }
    let length = body_length(header).map_err(ServeError::BadLength)?;
    let mut body = Zeroizing::new(vec![0; length]);
    stream.read_exact(&mut body).await.map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            ServeError::Truncated
        } else {
            ServeError::Io(error)
        }
    })?;
    Ok(Some(body))
}

async fn answer<S: SecretStore>(store: &S, scope: &BrokerScope, request: &Request) -> Answer {
    if !scope.admits(&request.application, &request.key) {
        return Answer::Refused;
    }
    let result = match (request.op, &request.secret) {
        (Op::Get, _) => store
            .get(&request.key)
            .await
            .map(|found| found.map_or(Answer::Missing, Answer::Found)),
        (Op::Put, Some(secret)) => store.put(&request.key, secret).await.map(|()| Answer::Done),
        (Op::Delete, _) => store.delete(&request.key).await.map(|()| Answer::Done),
        // decode gives every put its secret
        (Op::Put, None) => return Answer::Failed("a put without a secret".to_owned()),
    };
    result.unwrap_or_else(|error| Answer::Failed(error.to_string()))
}

/// The broker's client: a [`SecretStore`] whose every call is one request to the broker on the other end of `stream`,
/// in the namespace `application`.
///
/// The socket is used with blocking reads and writes on tokio's blocking pool, one exchange at a time, so the store
/// works from any tokio runtime (the engine's own included) and a slow answer (the Secret Service asking the user to
/// unlock it) holds up no runtime thread. After a socket error or a malformed answer the connection is dropped, and
/// every later call fails with [`BrokerError::Closed`].
pub struct BrokerSecretStore {
    application: String,
    connection: Arc<Mutex<Option<UnixStream>>>,
}

impl fmt::Debug for BrokerSecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let open = lock(&self.connection).is_some();
        f.debug_struct("BrokerSecretStore")
            .field("application", &self.application)
            .field("open", &open)
            .finish()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // a panic while holding the lock leaves either an open stream between exchanges or none
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl BrokerSecretStore {
    /// A store asking the broker on the other end of `stream` for `application`'s items. The stream is used in
    /// blocking mode.
    #[must_use]
    pub fn new(stream: UnixStream, application: impl Into<String>) -> Self {
        Self {
            application: application.into(),
            connection: Arc::new(Mutex::new(Some(stream))),
        }
    }

    #[must_use]
    pub fn application(&self) -> &str {
        &self.application
    }

    async fn ask(&self, op: Op, key: &SecretKey, secret: Option<&Secret>) -> Result<Answer, BrokerError> {
        let request = Request::encode(op, &self.application, key, secret).ok_or(BrokerError::TooLarge)?;
        let connection = Arc::clone(&self.connection);
        // a dropped caller doesn't cancel the exchange: it finishes, so the next one starts on a frame boundary
        let answer = tokio::task::spawn_blocking(move || exchange(&connection, op, &request))
            .await
            .map_err(|_| BrokerError::Closed)??;
        match answer {
            Answer::Refused => Err(BrokerError::Refused),
            Answer::Failed(text) => Err(BrokerError::Store(text)),
            answer => Ok(answer),
        }
    }
}

/// One request and its answer on the shared connection; any failure drops the connection.
fn exchange(connection: &Mutex<Option<UnixStream>>, op: Op, request: &[u8]) -> Result<Answer, BrokerError> {
    let mut connection = lock(connection);
    let stream = connection.as_mut().ok_or(BrokerError::Closed)?;
    let answer = round_trip(stream, op, request);
    if answer.is_err() {
        *connection = None;
    }
    answer
}

fn round_trip(stream: &mut UnixStream, op: Op, request: &[u8]) -> Result<Answer, BrokerError> {
    stream.write_all(request)?;
    stream.flush()?;
    let mut header = [0; LENGTH];
    stream.read_exact(&mut header)?;
    let length = body_length(header).map_err(|_| BrokerError::Malformed)?;
    let mut body = Zeroizing::new(vec![0; length]);
    stream.read_exact(&mut body)?;
    Answer::decode(&body)
        .filter(|answer| answer.fits(op))
        .ok_or(BrokerError::Malformed)
}

impl SecretStore for BrokerSecretStore {
    async fn put(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        self.ask(Op::Put, key, Some(secret)).await?;
        Ok(())
    }

    async fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        match self.ask(Op::Get, key, None).await? {
            Answer::Found(secret) => Ok(Some(secret)),
            _ => Ok(None),
        }
    }

    async fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.ask(Op::Delete, key, None).await?;
        Ok(())
    }
}
