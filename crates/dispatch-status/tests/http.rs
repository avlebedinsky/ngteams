//! Synthetic ledgers and loopback requests; no session or transcript data from the workstation.

#[cfg(test)]
mod tests {

    use std::fs;
    use std::io::{Read as _, Write as _};
    use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
    const DEADLINE: Duration = Duration::from_secs(10);

    struct Files(PathBuf);

    impl Files {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "dispatch-http-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("ledger.json"), br#"{"_synthetic":true,"slots":3,"tasks":[]}"#).unwrap();
            Self(dir)
        }

        fn command(&self) -> Command {
            let mut command = Command::new(env!("CARGO_BIN_EXE_dispatch-status"));
            command.env_clear().env("HOME", &self.0).stdin(Stdio::null());
            if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
                command.env("LLVM_PROFILE_FILE", profile);
            }
            command
        }
    }

    impl Drop for Files {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    struct Server {
        child: Child,
        port: u16,
        files: Files,
    }

    impl Server {
        fn start(environment: &[(&str, &str)], extra: &[&str]) -> Self {
            let files = Files::new();
            let reserve = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = reserve.local_addr().unwrap().port();
            let mut command = files.command();
            command
                .args([
                    "--ledger",
                    files.0.join("ledger.json").to_str().unwrap(),
                    "--port",
                    &port.to_string(),
                ])
                .args(extra)
                .envs(environment.iter().copied())
                .stdout(fs::File::create(files.0.join("stdout")).unwrap())
                .stderr(fs::File::create(files.0.join("stderr")).unwrap());
            drop(reserve);
            let child = command.spawn().unwrap();
            let mut server = Self { child, port, files };
            let deadline = Instant::now() + DEADLINE;
            loop {
                if server.stdout().contains("http://127.0.0.1:") {
                    return server;
                }
                assert!(
                    server.child.try_wait().unwrap().is_none(),
                    "startup failed: {}",
                    server.stderr()
                );
                assert!(Instant::now() < deadline, "server did not announce readiness");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn stdout(&self) -> String {
            fs::read_to_string(self.files.0.join("stdout")).unwrap()
        }

        fn stderr(&self) -> String {
            fs::read_to_string(self.files.0.join("stderr")).unwrap()
        }

        fn request(&self, request: &[u8]) -> Vec<u8> {
            let mut stream = self.connect();
            stream.write_all(request).unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            response
        }

        fn connect(&self) -> TcpStream {
            let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port)).unwrap();
            stream.set_read_timeout(Some(DEADLINE)).unwrap();
            stream.set_write_timeout(Some(DEADLINE)).unwrap();
            stream
        }

        fn get(&self, path: &str) -> Vec<u8> {
            self.request(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", self.port).as_bytes())
        }

        fn wait(&mut self) -> ExitStatus {
            let deadline = Instant::now() + DEADLINE;
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status;
                }
                assert!(Instant::now() < deadline, "exit watcher did not stop server");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.child.kill();
            self.child.wait().unwrap();
        }
    }

    fn response(bytes: &[u8], code: &str, kind: &str) -> Vec<u8> {
        let split = bytes
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .expect("response headers");
        let head = std::str::from_utf8(&bytes[..split]).unwrap();
        let body = &bytes[split + 4..];
        assert_eq!(head.lines().next().unwrap(), format!("HTTP/1.1 {code}"));
        for header in [
            format!("Content-Type: {kind}"),
            format!("Content-Length: {}", body.len()),
            "Cache-Control: no-store".to_owned(),
            "X-Content-Type-Options: nosniff".to_owned(),
            "Connection: close".to_owned(),
        ] {
            assert!(head.lines().any(|line| line == header), "missing {header} in {head}");
        }
        body.to_vec()
    }

    #[test]
    fn serves_the_page_and_refreshes_the_synthetic_ledger() {
        let server = Server::start(&[], &["--subagents", "/synthetic/no-agents"]);
        let page = response(&server.get("/"), "200 OK", "text/html; charset=utf-8");
        assert_eq!(page, include_bytes!("../assets/index.html"));
        assert_eq!(server.get("/index.html?poll=1"), server.get("/"));
        let before = response(&server.get("/status.json?poll=1"), "200 OK", "application/json");
        let before: serde_json::Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(before["tasks"], serde_json::json!([]));
        assert_eq!(before["slots_max"], 3);
        assert!(before["ledger_error"].is_null());
        fs::write(server.files.0.join("ledger.json"), br#"{"_synthetic":true,"slots":4,"tasks":[{"num":7,"title":"Synthetic task","state":"queued","agent":"fake"}]}"#).unwrap();
        let after = response(&server.get("/status.json"), "200 OK", "application/json");
        let after: serde_json::Value = serde_json::from_slice(&after).unwrap();
        assert_eq!(after["slots_max"], 4);
        assert_eq!(after["tasks"][0]["title"], "Synthetic task");
        assert_eq!(after["queue_len"], 1);
    }

    #[test]
    fn refuses_foreign_hosts_methods_and_unknown_paths() {
        let server = Server::start(&[], &[]);
        for host in ["evil.invalid", "127.0.0.1:1", "localhost", "localhost:1"] {
            let wire = server.request(format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes());
            assert_eq!(
                response(&wire, "403 Forbidden", "text/plain; charset=utf-8"),
                b"wrong host\n"
            );
        }
        let wire = server.request(b"GET / HTTP/1.1\r\nX-Other: ignored\r\n\r\n");
        assert_eq!(
            response(&wire, "403 Forbidden", "text/plain; charset=utf-8"),
            b"wrong host\n"
        );
        for method in ["POST", "HEAD", "", "garbage"] {
            let wire = server.request(format!("{method} / HTTP/1.1\r\n\r\n").as_bytes());
            assert_eq!(
                response(&wire, "405 Method Not Allowed", "text/plain; charset=utf-8"),
                b"GET only\n"
            );
        }
        assert_eq!(
            response(
                &server.get("/absent?query=1"),
                "404 Not Found",
                "text/plain; charset=utf-8"
            ),
            b"not found\n"
        );
        let wire = server.request(format!("GET / HTTP/1.1\nhOsT: localhost:{}\n\n", server.port).as_bytes());
        assert_eq!(
            response(&wire, "200 OK", "text/html; charset=utf-8"),
            include_bytes!("../assets/index.html")
        );
    }

    #[test]
    fn discards_incomplete_and_oversized_heads_then_serves_the_next_client() {
        let server = Server::start(&[], &[]);
        assert!(server.request(b"").is_empty());
        assert!(server.request(b"GET / HTTP/1.1\r\n").is_empty());
        let mut large = format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Large: ", server.port).into_bytes();
        large.extend(vec![b'x'; 16 * 1024]);
        large.extend_from_slice(b"\r\n");
        assert!(server.request(&large).is_empty());
        let mut stalled = server.connect();
        stalled.write_all(b"GET / HTTP/1.1\r\n").unwrap();
        let mut ignored = Vec::new();
        stalled.read_to_end(&mut ignored).unwrap();
        assert!(ignored.is_empty());
        assert_eq!(
            response(&server.get("/"), "200 OK", "text/html; charset=utf-8"),
            include_bytes!("../assets/index.html")
        );
    }

    #[test]
    fn reports_argument_errors_without_starting_a_server() {
        let files = Files::new();
        let cases = [
            (vec![], "--ledger is required"),
            (vec!["--ledger"], "--ledger needs a value"),
            (vec!["--port"], "--port needs a value"),
            (vec!["--port", "invalid"], "--port takes a port number"),
            (vec!["--port", "65536"], "--port takes a port number"),
            (vec!["--subagents"], "--subagents needs a value"),
            (vec!["--exit-with"], "--exit-with needs a value"),
            (vec!["--exit-with", "invalid"], "--exit-with takes a pid"),
            (vec!["--unknown"], "unknown argument --unknown"),
        ];
        for (args, expected) in cases {
            let output = files.command().args(&args).output().unwrap();
            assert_eq!(output.status.code(), Some(2), "{args:?}");
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains(expected), "{args:?}: {stderr}");
            assert!(stderr.contains("usage: dispatch-status --ledger"));
        }
    }

    #[test]
    fn reports_a_port_that_is_already_in_use() {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let files = Files::new();
        let output = files
            .command()
            .args(["--ledger", "synthetic.json", "--port", &port.to_string()])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains(&format!("can't listen on 127.0.0.1:{port}:"))
        );
    }

    #[test]
    fn session_discovery_is_selected_only_with_a_nonempty_session() {
        let session = Server::start(&[("CLAUDE_CODE_SESSION_ID", "synthetic-session")], &[]);
        assert!(session.stdout().contains("synthetic-session"));
        assert!(
            session
                .stdout()
                .contains(&session.files.0.join(".claude/projects").display().to_string())
        );
        let empty = Server::start(&[("CLAUDE_CODE_SESSION_ID", ""), ("CLAUDE_PID", "invalid")], &[]);
        assert!(empty.stdout().contains("agents in "));
        assert!(!empty.stdout().contains(".claude/projects"));
        assert_eq!(
            response(&empty.get("/status.json"), "200 OK", "application/json").first(),
            Some(&b'{')
        );
    }

    #[test]
    fn exit_watcher_stops_after_the_named_process_ends() {
        let mut server = Server::start(&[], &["--exit-with", &std::process::id().to_string()]);
        assert!(server.child.try_wait().unwrap().is_none());
        let mut follower = Server::start(&[("CLAUDE_PID", &server.child.id().to_string())], &[]);
        assert!(follower.child.try_wait().unwrap().is_none());
        drop(server);
        assert!(follower.wait().success());
        assert!(follower.stdout().contains("ended, stopping"));
        // Override the environment PID: zero has no /proc directory, so it must stop even though this test is alive.
        let mut gone = Server::start(
            &[("CLAUDE_PID", &std::process::id().to_string())],
            &["--exit-with", "0"],
        );
        assert!(gone.wait().success());
        assert!(gone.stdout().contains("process 0 ended, stopping"));
    }
}
