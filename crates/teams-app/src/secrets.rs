//! The credential broker, `teams-app secrets --lab <alias> --fd <n>`.
//!
//! It takes over descriptor `n` ([`adopt_socket`]: open, a Unix stream socket), reads `lab.toml` for the alias's UPN
//! ([`lab_scope`]), and serves `get`, `put` and `delete` of that account's own items in the lab's keyring application
//! (`teams-lab`) on it ([`run`], over `teams_auth::broker::serve`): a request for any other account or application is
//! refused and reaches no store. The Secret Service is connected on the first request it serves ([`LazyKeyring`]), so
//! a broker that only refuses or only sees its socket close never reaches the session bus. It opens no window and no
//! other desktop client, prints nothing on stdout, writes on stderr only the requests it refused or failed (their
//! operation, account and audience, never a value) and why it stopped, and exits when the socket closes: with 0 when
//! the peer closed it between requests, else non-zero after closing it itself (a malformed or oversized frame).
//!
//! Whoever starts it keeps no copy of the broker's end: the app's end closing (the app exiting) is what ends it.
//!
//! The broker runs in any build, `cargo test`'s included: it reaches no fence, no transport and no mock origin, only
//! the Secret Service for the lab's own items. The app itself refuses a `test-support` build (Task 6).

use std::io::{self, Write};
use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

use rustix::io::{FdFlags, fcntl_setfd};
use rustix::net::{AddressFamily, SocketType, sockopt};
use teams_auth::broker::{BrokerScope, Note, Outcome, ServeError, serve};
use teams_auth::secrets::{KeyringSecretStore, Secret, SecretError, SecretKey, SecretStore};
use teams_fence::lab::{Alias, LabConfig, LabPaths};
use tokio::sync::OnceCell;

/// The first descriptor past stdin, stdout and stderr: the lowest the broker takes as its socket.
pub const FIRST_PASSED_FD: RawFd = 3;

/// Why the descriptor handed to the broker wasn't taken as its socket.
#[derive(Debug, thiserror::Error)]
pub enum AdoptError {
    #[error("descriptor {0} is stdin, stdout or stderr, or not a descriptor (the socket is 3 or above)")]
    Reserved(RawFd),
    #[error("descriptor {fd} isn't open: {source}")]
    NotOpen { fd: RawFd, source: io::Error },
    #[error("descriptor {fd} isn't a Unix stream socket")]
    NotAUnixStream { fd: RawFd },
    #[error("descriptor {fd}: {source}")]
    Io { fd: RawFd, source: io::Error },
}

/// Takes over descriptor `fd`, handed to this process at its start, as the broker's socket: it must be 3 or above,
/// open, and a Unix stream socket, and is made close-on-exec. A descriptor that is open but not such a socket is
/// closed; one below 3 or not open is left alone.
///
/// Called before anything else in the process opens a descriptor, so `fd` can only be one the process inherited.
///
/// # Errors
/// See [`AdoptError`].
pub fn adopt_socket(fd: RawFd) -> Result<UnixStream, AdoptError> {
    if fd < FIRST_PASSED_FD {
        return Err(AdoptError::Reserved(fd));
    }
    // SAFETY: `F_GETFD` only reads the descriptor's flags; a number that isn't an open descriptor fails with `EBADF`.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
        return Err(AdoptError::NotOpen {
            fd,
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: the descriptor is open (checked above), and the command line hands it to this process to own: nothing
    // else in the process uses it, since the broker takes it over before it opens anything of its own.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let domain = sockopt::socket_domain(&owned).map_err(|_| AdoptError::NotAUnixStream { fd })?;
    let kind = sockopt::socket_type(&owned).map_err(|_| AdoptError::NotAUnixStream { fd })?;
    if domain != AddressFamily::UNIX || kind != SocketType::STREAM {
        return Err(AdoptError::NotAUnixStream { fd });
    }
    fcntl_setfd(&owned, FdFlags::CLOEXEC).map_err(|error| AdoptError::Io {
        fd,
        source: error.into(),
    })?;
    Ok(UnixStream::from(owned))
}

/// What the broker for `alias` serves: the lab's keyring application and the account's UPN, as `lab.toml` names it
/// (the name `proto-lab auth` stored its items under).
#[must_use]
pub fn lab_scope(config: &LabConfig, alias: Alias) -> BrokerScope {
    BrokerScope::new(LabPaths::KEYRING_APPLICATION, config.account(alias).upn.clone())
}

/// The Secret Service store of `application`, connected on its first use and kept from then on; a failed connection
/// is tried again on the next request.
#[derive(Debug)]
pub struct LazyKeyring {
    application: String,
    keyring: OnceCell<KeyringSecretStore>,
}

impl LazyKeyring {
    #[must_use]
    pub fn new(application: impl Into<String>) -> Self {
        Self {
            application: application.into(),
            keyring: OnceCell::new(),
        }
    }

    /// Whether the Secret Service has been connected.
    #[must_use]
    pub fn connected(&self) -> bool {
        self.keyring.initialized()
    }

    async fn keyring(&self) -> Result<&KeyringSecretStore, SecretError> {
        self.keyring
            .get_or_try_init(|| KeyringSecretStore::new(self.application.clone()))
            .await
    }
}

impl SecretStore for LazyKeyring {
    async fn put(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        self.keyring().await?.put(key, secret).await
    }

    async fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        self.keyring().await?.get(key).await
    }

    async fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.keyring().await?.delete(key).await
    }
}

/// Serves `store`'s items in `scope` on `socket` until it closes (see the module docs), noting each refused or failed
/// request on `log`. Runs on the caller's tokio runtime.
///
/// # Errors
/// The socket can't be used, or a frame was malformed or oversized (the socket is closed then).
pub async fn run<S: SecretStore>(
    socket: UnixStream,
    scope: &BrokerScope,
    store: &S,
    log: &mut impl Write,
) -> Result<(), ServeError> {
    socket.set_nonblocking(true)?;
    let socket = tokio::net::UnixStream::from_std(socket)?;
    serve(socket, store, scope, |note: &Note<'_>| {
        if matches!(note.outcome, Outcome::Refused | Outcome::Failed) {
            // a log line that can't be written loses only the note
            let _ = writeln!(log, "teams-app secrets: {note}");
        }
    })
    .await
}
