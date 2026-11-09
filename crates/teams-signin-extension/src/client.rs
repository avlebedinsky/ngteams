//! The web process's side of the guard: one connection per process, opened when `WebKit` initializes the extension,
//! asked once per request. Fails closed: no connection, a timeout, a broken socket or any answer but the expected one
//! is a deny, and after the first failure the connection is dropped and every later request is denied.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use teams_signin_protocol::protocol::{ALLOW, LINE_LIMIT, OK, Query};

/// How long one answer may take before the request is dropped.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// The connection to the guard, if there still is one.
#[derive(Debug, Default)]
pub struct GuardClient {
    stream: Option<BufReader<UnixStream>>,
}

impl GuardClient {
    /// Connects to the guard's socket at `path`, with [`ANSWER_TIMEOUT`] on reads and writes. A client that couldn't
    /// connect denies everything.
    #[must_use]
    pub fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path).ok().filter(|stream| {
            stream.set_read_timeout(Some(ANSWER_TIMEOUT)).is_ok()
                && stream.set_write_timeout(Some(ANSWER_TIMEOUT)).is_ok()
        });
        Self {
            stream: stream.map(BufReader::new),
        }
    }

    /// A client with no connection: denies everything.
    #[must_use]
    pub fn closed() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// Tells the guard `page`'s handler is connected. `false` when the guard didn't take it.
    pub fn armed(&mut self, page: u64, pid: u32) -> bool {
        self.ask(&Query::Armed { page, pid }, OK)
    }

    /// Whether the guard lets `page`'s request go. A `method` `WebKit` left out is `GET`.
    pub fn allows(&mut self, page: u64, method: Option<&str>, url: &str) -> bool {
        let query = Query::Request {
            page,
            method: method.unwrap_or("GET").to_owned(),
            url: url.to_owned(),
        };
        self.ask(&query, ALLOW)
    }

    fn ask(&mut self, query: &Query, wanted: &str) -> bool {
        let Some(line) = query.to_line() else {
            // a request that can't be put to the guard is denied, but the connection stays
            return false;
        };
        let answered = self.stream.as_mut().and_then(|stream| exchange(stream, &line));
        if let Some(answer) = answered {
            return answer == wanted;
        }
        self.stream = None;
        false
    }
}

/// Writes `line` and reads one answer line; `None` on any failure.
fn exchange(stream: &mut BufReader<UnixStream>, line: &str) -> Option<String> {
    let mut out = Vec::with_capacity(line.len() + 1);
    out.extend_from_slice(line.as_bytes());
    out.push(b'\n');
    stream.get_mut().write_all(&out).ok()?;
    let mut answer = Vec::new();
    let limit = u64::try_from(LINE_LIMIT).ok()?;
    stream.by_ref().take(limit).read_until(b'\n', &mut answer).ok()?;
    if answer.pop() != Some(b'\n') {
        return None;
    }
    String::from_utf8(answer).ok()
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::thread;

    use super::*;

    /// A guard that answers each line with `answer(line)`; `None` closes the connection.
    fn guard(answer: fn(&str) -> Option<&'static str>) -> (tempfile_dir::Dir, std::path::PathBuf) {
        let dir = tempfile_dir::Dir::new();
        let path = dir.path().join("guard.sock");
        let listener = UnixListener::bind(&path).unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let Some(reply) = answer(line.trim_end()) else {
                    return;
                };
                writer.write_all(format!("{reply}\n").as_bytes()).unwrap();
                line.clear();
            }
        });
        (dir, path)
    }

    /// A private directory of the test's own, removed on drop.
    mod tempfile_dir {
        pub struct Dir(std::path::PathBuf);

        impl Dir {
            pub fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "teams-signin-extension-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                std::fs::create_dir(&path).unwrap();
                Self(path)
            }

            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn the_guard_s_answers_decide() {
        let (_dir, path) = guard(|line| {
            Some(match Query::parse(line) {
                Some(Query::Armed { .. }) => OK,
                Some(Query::Request { method, .. }) if method == "GET" => ALLOW,
                _ => "deny",
            })
        });
        let mut client = GuardClient::connect(&path);
        assert!(client.is_connected());
        assert!(client.armed(1, 2));
        assert!(client.allows(1, None, "https://a.test/"), "no method is GET");
        assert!(!client.allows(1, Some("POST"), "https://a.test/"));
        assert!(!client.allows(1, Some("GET"), "https://a.test/a b"), "not asked");
        assert!(
            client.is_connected(),
            "a request that can't be asked keeps the connection"
        );
        assert!(client.allows(1, Some("GET"), "https://a.test/again"));
    }

    #[test]
    fn a_broken_guard_denies_everything_after() {
        let (_dir, path) = guard(|line| line.starts_with("armed").then_some("garbage-but-a-line"));
        let mut client = GuardClient::connect(&path);
        assert!(!client.armed(1, 2), "an unexpected answer isn't ok");
        assert!(client.is_connected());
        assert!(!client.allows(1, None, "https://a.test/"), "the guard closed");
        assert!(!client.is_connected());
        assert!(!client.allows(1, None, "https://a.test/"));
    }

    #[test]
    fn no_guard_denies_everything() {
        let dir = tempfile_dir::Dir::new();
        let mut client = GuardClient::connect(&dir.path().join("nobody.sock"));
        assert!(!client.is_connected());
        assert!(!client.armed(1, 2));
        assert!(!client.allows(1, None, "https://a.test/"));
        assert!(!GuardClient::closed().allows(1, None, "https://a.test/"));
    }
}
