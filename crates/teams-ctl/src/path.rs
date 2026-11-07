//! Where the control socket of each mode is.
//!
//! - personal: `$XDG_RUNTIME_DIR/ngteams/ctl.sock`;
//! - a lab alias: `ctl.sock` in that alias's runner state dir, `$XDG_STATE_HOME/ngteams-lab/<alias>/`
//!   (`scripts/run-lab.sh`);
//! - read-only: `ctl.sock` in the read-only runner's state dir, `$XDG_STATE_HOME/ngteams-read-only/run/`
//!   (`scripts/run-personal-read-only.sh`);
//! - `$NGTEAMS_CTL_SOCKET` ([`ENV_SOCKET`]) overrides any of them for a client.
//!
//! A lab or read-only app runs in the UI sandbox, whose environment is cleared and whose runtime dir is its own, so
//! its runner hands it the path ([`Env::mode_path`]) rather than the app working it out inside.
//!
//! `XDG_STATE_HOME` falls back to `~/.local/state` when unset or relative, as both runners do; a relative
//! `XDG_RUNTIME_DIR` or `HOME` counts as unset.

use std::ffi::OsString;
use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

/// The variable naming the socket a client connects to, whatever the mode.
pub const ENV_SOCKET: &str = "NGTEAMS_CTL_SOCKET";

/// The socket's file name in its directory.
pub const SOCKET_NAME: &str = "ctl.sock";

/// The personal socket's directory under `$XDG_RUNTIME_DIR`. A constant, so the directory isn't written as a
/// keyring application argument is (`ngteams/tests/apart.rs` keeps that name to the app's own crate).
const PERSONAL_DIR: &str = "ngteams";

/// Which app a socket belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The installed app with the user's own accounts.
    Personal,
    /// A lab alias run by `scripts/run-lab.sh`.
    Lab(String),
    /// The installed app run read-only by `scripts/run-personal-read-only.sh`.
    ReadOnly,
}

/// Why a mode's socket has no path.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("XDG_RUNTIME_DIR isn't set to an absolute path")]
    NoRuntimeDir,
    #[error("neither XDG_STATE_HOME nor HOME is set to an absolute path")]
    NoStateHome,
    #[error("a lab alias is ASCII letters, digits, '_' and '-', not {0:?}")]
    BadAlias(String),
}

/// The environment the paths are worked out from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    /// `$NGTEAMS_CTL_SOCKET`.
    pub socket: Option<OsString>,
    /// `$XDG_RUNTIME_DIR`.
    pub runtime_dir: Option<OsString>,
    /// `$XDG_STATE_HOME`.
    pub state_home: Option<OsString>,
    /// `$HOME`.
    pub home: Option<OsString>,
}

impl Env {
    /// The process's own environment.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            socket: std::env::var_os(ENV_SOCKET),
            runtime_dir: std::env::var_os("XDG_RUNTIME_DIR"),
            state_home: std::env::var_os("XDG_STATE_HOME"),
            home: std::env::var_os("HOME"),
        }
    }

    /// The socket a client of `mode` connects to: `$NGTEAMS_CTL_SOCKET` when set, otherwise the mode's own path.
    ///
    /// # Errors
    /// As [`Env::mode_path`], when there is no override.
    pub fn socket_path(&self, mode: &Mode) -> Result<PathBuf, PathError> {
        match self.socket.as_ref().filter(|socket| !socket.is_empty()) {
            Some(socket) => Ok(PathBuf::from(socket)),
            None => self.mode_path(mode),
        }
    }

    /// The mode's own socket, with no override: what the app serves on by default and what a runner hands its app.
    ///
    /// # Errors
    /// [`PathError::NoRuntimeDir`] for the personal mode without an absolute `XDG_RUNTIME_DIR`;
    /// [`PathError::NoStateHome`] for the others without an absolute `XDG_STATE_HOME` or `HOME`;
    /// [`PathError::BadAlias`] for a lab alias that isn't a plain name.
    pub fn mode_path(&self, mode: &Mode) -> Result<PathBuf, PathError> {
        match mode {
            Mode::Personal => {
                let runtime = absolute(self.runtime_dir.as_ref()).ok_or(PathError::NoRuntimeDir)?;
                Ok(runtime.join(PERSONAL_DIR).join(SOCKET_NAME))
            }
            Mode::Lab(alias) => {
                if !plain_alias(alias) {
                    return Err(PathError::BadAlias(alias.clone()));
                }
                Ok(self.state_home()?.join("ngteams-lab").join(alias).join(SOCKET_NAME))
            }
            Mode::ReadOnly => Ok(self
                .state_home()?
                .join("ngteams-read-only")
                .join("run")
                .join(SOCKET_NAME)),
        }
    }

    fn state_home(&self) -> Result<PathBuf, PathError> {
        if let Some(state) = absolute(self.state_home.as_ref()) {
            return Ok(state);
        }
        absolute(self.home.as_ref())
            .map(|home| home.join(".local").join("state"))
            .ok_or(PathError::NoStateHome)
    }
}

/// The socket a client of `mode` connects to, from the process's environment ([`Env::socket_path`]).
///
/// # Errors
/// As [`Env::socket_path`].
pub fn socket_path(mode: &Mode) -> Result<PathBuf, PathError> {
    Env::from_process().socket_path(mode)
}

/// Make the socket's directory: each directory missing on the way is made `0700`. A directory already there is left
/// as it is (an override may name a socket in a directory of the user's, whose mode isn't ours to change); the socket
/// itself is bound `0600` by the server.
///
/// # Errors
/// The directory couldn't be made, or the path's parent is there but isn't a directory.
pub fn prepare_dir(socket: &Path) -> io::Result<()> {
    let dir = match socket.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    // A directory already there is Ok; anything else there is an error.
    DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

fn absolute(value: Option<&OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}

fn plain_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}
