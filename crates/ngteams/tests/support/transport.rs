//! Bounded real control transports for mock application tests. Windows endpoints have no filesystem inode.
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};
pub use teams_ctl::transport::Stream;
const WAIT: Duration = Duration::from_secs(30);

#[cfg(unix)]
type NativeListener = std::os::unix::net::UnixListener;
#[cfg(windows)]
type NativeListener = teams_ctl::transport::Listener;

pub struct Listener(NativeListener);
impl Listener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        let listener = NativeListener::bind(path)?;
        #[cfg(unix)]
        listener.set_nonblocking(true)?;
        Ok(Self(listener))
    }
    pub fn accept(&self) -> io::Result<Stream> {
        let deadline = Instant::now() + WAIT;
        loop {
            #[cfg(unix)]
            let accepted = self.0.accept().map(|(stream, _)| stream);
            #[cfg(windows)]
            let accepted = self.0.accept();
            match accepted {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                other => return other,
            }
        }
    }
}
pub fn connect(path: &Path) -> io::Result<Stream> {
    teams_ctl::transport::connect(path, WAIT)
}
