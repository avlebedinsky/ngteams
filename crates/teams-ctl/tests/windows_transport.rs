//! Real local pipes, synthetic bytes only. No tenant, accounts or credentials.
#![cfg(test)]
#![cfg(windows)]

use std::io::{Read as _, Write as _};
use std::net::Shutdown;
use std::thread;
use std::time::{Duration, Instant};
use teams_ctl::transport::{Listener, Stream, connect};

fn accept(listener: &Listener) -> Stream {
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok(stream) => return stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(started.elapsed() < Duration::from_secs(5));
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
}

fn pair() -> (tempfile::TempDir, Listener, Stream, Stream) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let listener = Listener::bind(&path).unwrap();
    assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    let client = connect(&path, Duration::from_secs(1)).unwrap();
    let server = accept(&listener);
    (dir, listener, client, server)
}

#[test]
fn a_second_listener_is_refused_and_dropping_it_releases_the_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let listener = Listener::bind(&path).unwrap();
    assert_eq!(Listener::bind(&path).unwrap_err().kind(), std::io::ErrorKind::AddrInUse);
    drop(listener);
    Listener::bind(&path).unwrap();
}

#[test]
fn a_busy_instance_times_out_without_a_worker_or_pending_connect() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let _listener = Listener::bind(&path).unwrap();
    let _client = connect(&path, Duration::from_secs(1)).unwrap();
    let started = Instant::now();
    assert_eq!(
        connect(&path, Duration::from_millis(100)).unwrap_err().kind(),
        std::io::ErrorKind::TimedOut
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    let timeouts = teams_ctl::Timeouts {
        connect: Duration::from_millis(100),
        ..teams_ctl::Timeouts::default()
    };
    assert!(matches!(
        teams_ctl::Client::connect(&path, timeouts),
        Err(teams_ctl::ClientError::Timeout)
    ));
}

#[test]
fn clones_share_timeouts_and_shutdown_wakes_pending_reads() {
    let (_dir, _listener, mut client, server) = pair();
    let mut clone = client.try_clone().unwrap();
    assert!(client.set_read_timeout(Some(Duration::ZERO)).is_err());
    assert!(client.set_write_timeout(Some(Duration::ZERO)).is_err());
    assert_eq!(client.read(&mut []).unwrap(), 0);
    client.write_all(&[]).unwrap();
    client.flush().unwrap();
    (&client).flush().unwrap();
    assert!(!server.peer_closed().unwrap());
    server.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    let mut buffer = [0; 4];
    assert_eq!(
        (&server).read(&mut buffer).unwrap_err().kind(),
        std::io::ErrorKind::TimedOut
    );
    client.write_all(b"test").unwrap();
    assert!(
        server.peer_closed().unwrap(),
        "unexpected subscriber input is detected without consuming it"
    );
    (&server).read_exact(&mut buffer).unwrap();
    assert_eq!(&buffer, b"test");
    client.set_read_timeout(None).unwrap();
    let waiting = thread::spawn(move || clone.read(&mut [0]));
    thread::sleep(Duration::from_millis(50));
    assert!(!waiting.is_finished());
    client.shutdown(Shutdown::Both).unwrap();
    assert_eq!(waiting.join().unwrap().unwrap(), 0);
    assert!(client.write(b"no").is_err());
    assert!(server.peer_closed().unwrap());
    client.shutdown(Shutdown::Both).unwrap();
}

#[test]
fn a_nonreading_peer_cannot_hold_a_writer_past_its_deadline() {
    let (_dir, _listener, mut client, _server) = pair();
    client.set_write_timeout(Some(Duration::from_millis(50))).unwrap();
    let started = Instant::now();
    let error = client.write_all(&vec![1; 1024 * 1024]).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(2));
    client.shutdown(Shutdown::Both).unwrap();
}

#[test]
fn writes_larger_than_the_native_pipe_quota_make_progress_in_both_directions() {
    let (_dir, _listener, mut client, mut server) = pair();
    client.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
    server.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
    let payload = vec![0x5a; 256 * 1024];
    let expected = payload.clone();
    let reading = thread::spawn(move || {
        let mut actual = vec![0; expected.len()];
        server.read_exact(&mut actual).unwrap();
        assert_eq!(actual, expected);
        server.write_all(&actual).unwrap();
    });
    client.write_all(&payload).unwrap();
    let mut echoed = vec![0; payload.len()];
    client.read_exact(&mut echoed).unwrap();
    assert_eq!(echoed, payload);
    reading.join().unwrap();
}
