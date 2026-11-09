//! Compile the actual application's GPUI-independent listener on both OSes, including Windows while the rest of
//! the app's native integrations are being ported. Every request and event here is synthetic and offline.
#![cfg(test)]

#[path = "../../ngteams/src/ctl/listener.rs"]
pub mod listener;
pub use listener::BindError;

use std::io::{BufReader, Read as _, Write as _};
use std::net::Shutdown;
use std::time::{Duration, Instant};
use teams_ctl::codec::{self, LineReader};
use teams_ctl::protocol::{CtlEvent, EventLine, Outcome, Request, Response, ResponseLine};
use teams_ctl::transport::connect;

#[test]
fn actual_app_listener_answers_requests_and_releases_its_endpoint_on_stop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let (jobs, asked) = async_channel::unbounded();
    let mut listener = listener::bind(&path).unwrap().serve(jobs).unwrap();
    assert_eq!(listener.path(), path);
    assert!(matches!(listener::bind(&path), Err(BindError::AlreadyRunning(_))));
    let mut client = connect(&path, Duration::from_secs(1)).unwrap();
    client.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    client.write_all(b"{\"id\":1,\"cmd\":\"status\"}\n").unwrap();
    let job = asked.recv_blocking().unwrap();
    assert_eq!(job.line.request, Request::Status);
    job.reply
        .send(
            ResponseLine {
                id: job.line.id,
                outcome: Outcome::Ok(Response::Done),
            }
            .into(),
        )
        .unwrap_or_else(|_| panic!("reply lost"));
    let line: ResponseLine = LineReader::new(BufReader::new(client.try_clone().unwrap()))
        .read()
        .unwrap()
        .unwrap();
    assert_eq!(line.outcome, Outcome::Ok(Response::Done));
    let started = Instant::now();
    listener.stop();
    listener.stop();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(client.read(&mut [0]).unwrap(), 0);
    assert!(connect(&path, Duration::from_millis(100)).is_err());
    listener::bind(&path).unwrap();
}

#[test]
fn invalid_lines_are_answered_without_reaching_the_application() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let (jobs, asked) = async_channel::unbounded();
    let mut listener = listener::bind(&path).unwrap().serve(jobs).unwrap();
    let mut client = connect(&path, Duration::from_secs(1)).unwrap();
    client.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut reader = LineReader::new(BufReader::new(client.try_clone().unwrap()));
    for invalid in [b"not json\n".as_slice(), b"\xff\n", b"{\"id\":5,\"cmd\":\"missing\"}\n"] {
        client.write_all(invalid).unwrap();
        let line: ResponseLine = reader.read().unwrap().unwrap();
        assert!(matches!(line.outcome, Outcome::Error(_)));
    }
    // Transfer the complete 8 MiB cap before its refusal. Windows byte pipes deliberately pace quota-sized writes.
    client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    client.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let writing = std::thread::spawn(move || {
        let _ = client.write_all(&vec![b'x'; codec::MAX_LINE + 1]);
        let _ = client.write_all(b"\n");
    });
    let line: ResponseLine = reader.read().unwrap().unwrap();
    assert!(matches!(line.outcome, Outcome::Error(_)));
    // Closing with intentionally unread oversized input can deliver EOF or a reset on Unix.
    assert!(matches!(reader.read_line(), Ok(None) | Err(codec::CodecError::Io(_))));
    writing.join().unwrap();
    assert!(asked.try_recv().is_err());
    listener.stop();
}

#[test]
fn subscribers_receive_events_and_their_disconnect_closes_the_subscription() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let (jobs, asked) = async_channel::unbounded();
    let mut listener = listener::bind(&path).unwrap().serve(jobs).unwrap();
    let mut client = connect(&path, Duration::from_secs(1)).unwrap();
    client.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    client.write_all(b"{\"id\":1,\"cmd\":\"subscribe\"}\n").unwrap();
    let job = asked.recv_blocking().unwrap();
    let (send, events) = async_channel::unbounded();
    job.reply
        .send(listener::Answer {
            line: ResponseLine {
                id: 1,
                outcome: Outcome::Ok(Response::Subscribed),
            },
            events: Some(events),
        })
        .unwrap_or_else(|_| panic!("reply lost"));
    let mut lines = LineReader::new(BufReader::new(client.try_clone().unwrap()));
    let _: ResponseLine = lines.read().unwrap().unwrap();
    send.send_blocking(EventLine {
        account: None,
        at: 1,
        event: CtlEvent::TeamsChanged,
    })
    .unwrap();
    let event: EventLine = lines.read().unwrap().unwrap();
    assert_eq!(event.event, CtlEvent::TeamsChanged);
    client.shutdown(Shutdown::Both).unwrap();
    let started = Instant::now();
    while !send.is_closed() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "subscription kept a disconnected client"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    listener.stop();
}
