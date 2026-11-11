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
#[cfg(unix)]
#[path = "../../teams-platform/tests/support/privacy.rs"]
mod privacy;
#[expect(dead_code, reason = "this transport fixture only binds inactive listeners")]
#[path = "support/transport.rs"]
mod transport;
use std::path::Path;
use std::rc::Rc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use transport::{Listener, Stream};

use ctl_support::{UPN, WAIT, connect, launched, launched_with, pump_until_done, pumped, quit, socket_in};
use gpui::TestAppContext;
use ngteams::ctl::BindError;
use ngteams::ctl::listener::{Answer, Job};
use teams_ctl::codec;
use teams_ctl::protocol::{
    ErrorCode, EventFilter, EventLine, Outcome, Request, RequestLine, Response, ResponseLine, WindowState,
};
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
        let mut stream = transport::connect(&socket).unwrap();
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
    #[cfg(unix)]
    {
        privacy::assert_private(&socket, false);
        privacy::assert_private(socket.parent().unwrap(), false);
    }

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

/// The answer line the server writes next on `reader`.
fn answer_on(reader: &mut BufReader<Stream>) -> ResponseLine {
    let mut answer = String::new();
    reader.read_line(&mut answer).unwrap();
    serde_json::from_str(&answer).unwrap()
}

/// A line that isn't UTF-8 is answered as a bad request and the connection goes on; one over the size cap is answered
/// so too, and ends the connection (the stream is mid-line). Nothing reaches the app: each is refused by the listener.
#[test]
fn a_line_not_utf8_or_over_the_cap_is_a_bad_request_from_the_listener() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir);
    let (jobs, asked) = async_channel::unbounded::<Job>();
    let mut listener = ngteams::ctl::bind(&socket).unwrap().serve(jobs).unwrap();
    assert_eq!(listener.path(), socket);
    let mut client = transport::connect(&socket).unwrap();
    client.set_read_timeout(Some(WAIT)).unwrap();
    let mut reader = BufReader::new(client.try_clone().unwrap());

    client.write_all(b"{\"id\":1,\"cmd\":\"status\xff\"}\n").unwrap();
    let not_utf8 = answer_on(&mut reader);
    assert_eq!(not_utf8.id, 0);
    assert!(
        matches!(&not_utf8.outcome, Outcome::Error(error) if error.code == ErrorCode::BadRequest),
        "{not_utf8:?}"
    );

    // written from a thread of its own: the server stops reading at its cap, and the rest may never be taken
    let mut writer = client.try_clone().unwrap();
    let writing = thread::spawn(move || {
        let _ = writer.write_all(&vec![b'a'; codec::MAX_LINE + 1]);
        let _ = writer.write_all(b"\n");
    });
    let too_long = answer_on(&mut reader);
    assert!(
        matches!(&too_long.outcome, Outcome::Error(error) if error.code == ErrorCode::BadRequest
            && error.message.contains(&codec::MAX_LINE.to_string())),
        "{too_long:?}"
    );
    let mut rest = String::new();
    assert!(
        matches!(reader.read_line(&mut rest), Ok(0) | Err(_)),
        "the connection ends: {rest:?}"
    );
    writing.join().unwrap();
    assert!(asked.try_recv().is_err(), "nothing reached the app");
    listener.stop();
}

/// A socket whose directory can't be made (a file stands where it would be) is an I/O error naming its path.
#[cfg(unix)]
#[test]
fn a_socket_whose_directory_cant_be_made_isnt_bound() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plain");
    fs::write(&file, b"the user's").unwrap();
    let socket = file.join("ctl.sock");
    match ngteams::ctl::bind(&socket) {
        Err(BindError::Io { path, .. }) => assert_eq!(path, socket),
        other => panic!("bound under a file: {other:?}"),
    }
    assert_eq!(fs::read(&file).unwrap(), b"the user's");
}

#[test]
fn a_stale_socket_is_replaced_and_a_live_one_refused() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir);
    // an app that didn't quit left its socket file, nobody behind it
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    drop(Listener::bind(&socket).unwrap());
    assert_eq!(socket.exists(), cfg!(unix));
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

#[cfg(unix)]
#[test]
fn a_file_that_is_no_socket_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir);
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    fs::write(&socket, b"the user's").unwrap();
    assert!(matches!(ngteams::ctl::bind(&socket), Err(BindError::NotASocket(_))));
    assert_eq!(fs::read(&socket).unwrap(), b"the user's");
}

#[cfg(unix)]
#[test]
fn a_socket_the_probe_may_not_reach_is_refused_with_its_own_error_kind() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir);
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let _behind = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o000)).unwrap();
    // root reaches it regardless: nothing to check there
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        return;
    }
    match ngteams::ctl::bind(&socket) {
        Err(BindError::Io { path, source }) => {
            assert_eq!(path, socket);
            assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied, "{source}");
            assert!(source.raw_os_error().is_some(), "the OS error kept: {source:?}");
        }
        other => panic!("bound over a socket it couldn't probe: {other:?}"),
    }
    assert!(socket.exists(), "left in place");
}

#[cfg(windows)]
#[test]
fn a_native_endpoint_keeps_files_at_its_logical_path_and_can_be_rebound_after_quit() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plain");
    fs::write(&file, b"synthetic existing file").unwrap();
    // Windows pipe names are derived from an absolute logical path. Neither a file there nor a parent which is
    // a file is a kernel pipe, so binding must leave it alone while still serving the real application protocol.
    for socket in [file.clone(), file.join("ctl.sock")] {
        let cx = TestAppContext::single();
        let _launched = launched(&cx, dir.path(), &socket);
        assert_eq!(status(&cx, &socket).accounts.len(), 1);
        assert_eq!(fs::read(&file).unwrap(), b"synthetic existing file");
        assert!(matches!(ngteams::ctl::bind(&socket), Err(BindError::AlreadyRunning(_))));
        quit(&cx);
        assert_eq!(fs::read(&file).unwrap(), b"synthetic existing file");
        drop(ngteams::ctl::bind(&socket).unwrap());
    }
    assert!(matches!(
        ngteams::ctl::bind(Path::new("synthetic-relative-pipe")),
        Err(BindError::Io { source, .. }) if source.kind() == std::io::ErrorKind::InvalidInput
    ));
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

#[test]
fn a_subscriber_s_connection_ends_when_its_client_hangs_up_though_no_event_comes() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir);
    let (jobs, asked) = async_channel::unbounded::<Job>();
    let mut listener = ngteams::ctl::bind(&socket).unwrap().serve(jobs).unwrap();
    let mut client = transport::connect(&socket).unwrap();
    client.set_read_timeout(Some(WAIT)).unwrap();
    let subscribe = RequestLine {
        id: 1,
        account: None,
        request: Request::Subscribe {
            filter: EventFilter::default(),
        },
    };
    codec::write_line(&mut client, &subscribe).unwrap();
    // the stream answered by hand: no event is ever sent on it
    let job = asked.recv_blocking().unwrap();
    let (events, stream) = async_channel::bounded::<EventLine>(1);
    let answer = Answer {
        line: ResponseLine {
            id: job.line.id,
            outcome: Outcome::Ok(Response::Subscribed),
        },
        events: Some(stream),
    };
    assert!(job.reply.send(answer).is_ok());
    let mut subscribed = String::new();
    BufReader::new(&client).read_line(&mut subscribed).unwrap();
    let subscribed: ResponseLine = serde_json::from_str(&subscribed).unwrap();
    assert!(
        matches!(subscribed.outcome, Outcome::Ok(Response::Subscribed)),
        "{subscribed:?}"
    );
    // the connection's thread waits on the stream
    thread::sleep(Duration::from_millis(50));
    assert_eq!(events.receiver_count(), 1, "the stream is written from");

    drop(client);
    let start = Instant::now();
    while events.receiver_count() > 0 {
        assert!(start.elapsed() < WAIT, "the connection's thread didn't end");
        thread::sleep(Duration::from_millis(2));
    }
    assert!(events.is_closed(), "the stream is closed");
    listener.stop();
}
