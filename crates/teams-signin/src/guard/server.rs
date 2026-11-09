//! Linux extension socket transport; native Windows calls the same guard through private worker IPC.
use super::Guard;
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use teams_signin_protocol::protocol::{DENY, LINE_LIMIT};

/// The guard on a Unix socket: one thread accepting, one per connection answering line by line. A line longer than
/// [`LINE_LIMIT`], or one that isn't UTF-8, is answered with a deny and ends the connection (so the extension denies
/// everything after it).
#[derive(Debug)]
pub struct GuardServer {
    path: PathBuf,
}

impl GuardServer {
    /// Binds `path` (a socket in a private directory) and serves `guard` there until the process exits.
    ///
    /// # Errors
    /// The socket can't be bound.
    pub fn start(path: &Path, guard: Arc<Guard>) -> io::Result<Self> {
        let listener = UnixListener::bind(path)?;
        thread::Builder::new().name("signin-guard".into()).spawn(move || {
            for stream in listener.incoming().map_while(Result::ok) {
                let guard = Arc::clone(&guard);
                let _ = thread::Builder::new()
                    .name("signin-guard-conn".into())
                    .spawn(move || serve(&guard, stream));
            }
        })?;
        Ok(Self { path: path.to_owned() })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn serve(guard: &Guard, stream: UnixStream) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let limit = u64::try_from(LINE_LIMIT).unwrap_or(u64::MAX) + 1;
    let mut reader = BufReader::new(stream);
    loop {
        let mut bytes = Vec::new();
        match reader.by_ref().take(limit).read_until(b'\n', &mut bytes) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let complete = bytes.pop() == Some(b'\n');
        let answer = match std::str::from_utf8(&bytes) {
            Ok(line) if complete => guard.answer(line),
            _ => {
                let _ = writer.write_all(format!("{DENY}\n").as_bytes());
                return;
            }
        };
        if writer.write_all(format!("{answer}\n").as_bytes()).is_err() {
            return;
        }
    }
}
