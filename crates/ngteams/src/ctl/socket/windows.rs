//! Local-only owner-protected Windows pipes. Instances vanish when the last handle closes, so there is no stale
//! filesystem entry to probe or remove. The listener polls nonblocking accepts; the stop bit ends its thread.
use super::super::BindError;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use teams_ctl::transport::{Listener, Stream};

#[derive(Debug)]
pub struct Socket {
    listener: Mutex<Option<Listener>>,
    pub path: PathBuf,
}

pub fn bind(path: &Path) -> Result<Socket, BindError> {
    Listener::bind(path)
        .map(|listener| Socket {
            listener: Mutex::new(Some(listener)),
            path: path.to_owned(),
        })
        .map_err(|source| {
            if source.kind() == io::ErrorKind::AddrInUse {
                BindError::AlreadyRunning(path.to_owned())
            } else {
                BindError::Io {
                    path: path.to_owned(),
                    source,
                }
            }
        })
}

impl Socket {
    pub fn accept(&self) -> io::Result<Stream> {
        self.listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotConnected))?
            .accept()
    }
    pub fn wake(&self) {
        self.cleanup();
    }
    pub fn cleanup(&self) {
        self.listener.lock().unwrap_or_else(PoisonError::into_inner).take();
    }
}
