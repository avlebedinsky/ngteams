//! The control socket's server on GPUI's test context: the app launched with `Launch::ctl` over the personal build's
//! engine (no token in its store, so no request leaves the process), and `teams_ctl`'s client on a thread of its own
//! while the test thread steps GPUI (the requests are answered on its foreground). The round trip runs through the
//! real protocol and server; only the socket's path is the test's own.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::cell::Cell;
use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::rc::Rc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ctl_support::{UPN, WAIT, connect, launched, launched_with, pump_until_done, pumped, quit, socket_in};
use gpui::TestAppContext;
use ngteams::ctl::BindError;
use teams_ctl::protocol::{ErrorCode, Outcome, Request, Response, ResponseLine, WindowState};
use teams_ctl::{Client, ClientError, PROTOCOL_VERSION};

/// The status, asked by a client of its own.
fn status(cx: &TestAppContext, socket: &Path) -> teams_ctl::protocol::AppStatus {
    let socket = socket.to_owned();
    match pumped(cx, move || connect(&socket)?.request(None, Request::Status)) {
        Ok(Response::Status(status)) => status,
        other => panic!("not a status: {other:?}"),
    }
}

/// Raw lines written on a connection of their own, each answer line read back as it came.
fn raw(cx: &TestAppContext, socket: &Path, lines: &'static [&'static str]) -> Vec<ResponseLine> {
    let socket = socket.to_owned();
    pumped(cx, move || {
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        lines
            .iter()
            .map(|line| {
                stream.write_all(format!("{line}\n").as_bytes()).unwrap();
                let mut answer = String::new();
                reader.read_line(&mut answer).unwrap();
                serde_json::from_str(&answer).unwrap()
            })
            .collect()
    })
}

#[test]
fn hello_tells_the_app_s_version_and_another_protocol_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir);
    let cx = TestAppContext::single();
    let _launched = launched(&cx, dir.path(), &socket);

    // the socket is the user's alone: 0600 in a 0700 directory made for it
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&socket), 0o600);
    assert_eq!(mode(socket.parent().unwrap()), 0o700);

    let path = socket.clone();
    let hello = pumped(&cx, move || connect(&path).map(|client| client.hello().clone())).unwrap();
    assert_eq!(hello.protocol, PROTOCOL_VERSION);
    assert_eq!(hello.app, env!("CARGO_PKG_VERSION"));

    let answers = raw(
        &cx,
        &socket,
        &[r#"{"id":1,"cmd":"hello","protocol":999,"client":"0.0.0"}"#],
    );
    match &answers[0].outcome {
        Outcome::Error(error) => assert_eq!(error.code, ErrorCode::VersionMismatch, "{error:?}"),
        Outcome::Ok(response) => panic!("another protocol was answered: {response:?}"),
    }
    assert_eq!(answers[0].id, 1);
    quit(&cx);
}

#[test]
fn status_tells_the_accounts_the_window_and_no_call() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir);
    let cx = TestAppContext::single();
    let asked = Rc::new(Cell::new(0));
    let (_launched, requests) = launched_with(&cx, dir.path(), &socket, &asked, |_| {});
    // the first request that needs a token finds the account signed out (no token in the store): the engine stops and
    // the window asks to sign in again
    assert!(requests.sync().blocking_wait().is_err());
    let start = Instant::now();
    while asked.get() == 0 {
        cx.run_until_parked();
        assert!(start.elapsed() < WAIT, "the account wasn't signed out in time");
        thread::sleep(Duration::from_millis(10));
    }

    let status = status(&cx, &socket);
    assert_eq!(status.app, env!("CARGO_PKG_VERSION"));
    assert_eq!(status.window, WindowState::Shown);
    assert_eq!(status.call, None, "no call is up or rings");
    let [account] = status.accounts.as_slice() else {
        panic!("one account: {:?}", status.accounts);
    };
    assert_eq!((account.index, account.upn.as_str()), (0, UPN));
    assert_eq!(account.mri, "8:orgid:00000000-0000-4000-8000-0000000000ff");
    assert!(account.shown, "the account the window shows");
    // signed out wins over the engine stopped with it
    assert_eq!(account.state, "signed_out");
    quit(&cx);
}

#[test]
fn a_malformed_line_is_a_bad_request_and_the_connection_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir);
    let cx = TestAppContext::single();
    let _launched = launched(&cx, dir.path(), &socket);

    let answers = raw(
        &cx,
        &socket,
        &[
            "not json",
            r#"{"cmd":"status"}"#,
            r#"{"id":5,"cmd":"no_such_command"}"#,
            r#"{"id":6,"cmd":"status"}"#,
        ],
    );
    let codes: Vec<(u64, Option<ErrorCode>)> = answers
        .iter()
        .map(|answer| match &answer.outcome {
            Outcome::Error(error) => (answer.id, Some(error.code)),
            Outcome::Ok(_) => (answer.id, None),
        })
        .collect();
    assert_eq!(
        codes,
        [
            (0, Some(ErrorCode::BadRequest)),
            (0, Some(ErrorCode::BadRequest)),
            (5, Some(ErrorCode::BadRequest)),
            (6, None),
        ],
        "each line answered in turn, with its id when it has one"
    );
    quit(&cx);
}

#[test]
fn a_stale_socket_is_replaced_and_a_live_one_refused() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir);
    // an app that didn't quit left its socket file, nobody behind it
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    drop(UnixListener::bind(&socket).unwrap());
    assert!(socket.exists());
    let cx = TestAppContext::single();
    let _launched = launched(&cx, dir.path(), &socket);
    assert_eq!(status(&cx, &socket).accounts.len(), 1, "served afresh");

    // a second server on the same socket finds this app behind it
    match ngteams::ctl::bind(&socket) {
        Err(BindError::AlreadyRunning(path)) => assert_eq!(path, socket),
        other => panic!("a second server wasn't refused: {other:?}"),
    }
    assert_eq!(status(&cx, &socket).accounts.len(), 1, "the first serves on");
    quit(&cx);
}

#[test]
fn a_file_that_is_no_socket_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir);
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    fs::write(&socket, b"the user's").unwrap();
    assert!(matches!(ngteams::ctl::bind(&socket), Err(BindError::NotASocket(_))));
    assert_eq!(fs::read(&socket).unwrap(), b"the user's");
}

#[test]
fn two_clients_are_answered_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir);
    let cx = TestAppContext::single();
    let _launched = launched(&cx, dir.path(), &socket);

    // both connected before either asks: each has a thread of its own on the server
    let clients: Vec<Client> = (0..2)
        .map(|_| {
            let path = socket.clone();
            pumped(&cx, move || connect(&path)).unwrap()
        })
        .collect();
    let threads: Vec<JoinHandle<Result<Response, ClientError>>> = clients
        .into_iter()
        .map(|mut client| {
            thread::spawn(move || {
                client.request(None, Request::Status)?;
                client.request(None, Request::Status)
            })
        })
        .collect();
    for thread in threads {
        pump_until_done(&cx, &thread);
        assert!(matches!(thread.join().unwrap(), Ok(Response::Status(_))));
    }
    quit(&cx);
}

#[test]
fn quit_closes_the_connections_and_removes_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir);
    let cx = TestAppContext::single();
    let _launched = launched(&cx, dir.path(), &socket);
    let path = socket.clone();
    let mut client = pumped(&cx, move || connect(&path)).unwrap();

    quit(&cx);
    assert!(!socket.exists(), "the socket file is removed");
    let after = pumped(&cx, move || client.request(None, Request::Status));
    assert!(
        matches!(after, Err(ClientError::Closed | ClientError::Codec(_))),
        "the connection is closed: {after:?}"
    );
    let path = socket.clone();
    let again = pumped(&cx, move || connect(&path).map(|_| ()));
    assert!(matches!(again, Err(ClientError::NotRunning { .. })), "{again:?}");
}
