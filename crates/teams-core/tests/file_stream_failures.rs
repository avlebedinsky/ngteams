//! A synthetic loopback file server that truncates, stalls, or keeps sending a body. The engine must report a
//! broken/stalled body once and must close a read when its owner seeks away; no account service is contacted.

#![cfg(test)]

use std::future::{self, Future};
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::json;
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::file_content::STALLED;
use teams_core::{EngineError, Outbox};
use teams_fence::{FailClosed, Fence, FenceLog, Mri, Roster};
use teams_mock::{CHAT_BASE, LAB_A};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;

const SHARE: &str = "https://contoso-my.sharepoint.com/:v:/g/personal/made_up_user_contoso_test/MadeUpBodyFailure01";
const LINK: &str = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/_layouts/15/download.aspx?UniqueId=made-up&tempauth=made-up";
const LARGE: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
enum Body {
    Truncated,
    Streaming,
    Silent,
}

struct Tokens;

impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("synthetic-stream-token")))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

fn request(stream: &mut TcpStream) {
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 1);
        bytes.push(byte[0]);
        assert!(bytes.len() < 16_384);
    }
}

fn peer(body: Body) -> (String, mpsc::Receiver<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (finished, result) = mpsc::channel();
    thread::spawn(move || {
        let (mut graph, _) = listener.accept().unwrap();
        request(&mut graph);
        let item = json!({"size": LARGE, "@microsoft.graph.downloadUrl": LINK}).to_string();
        write!(graph, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{item}", item.len()).unwrap();
        drop(graph);
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream);
        write!(stream, "HTTP/1.1 206 Partial Content\r\nContent-Length: {LARGE}\r\nContent-Range: bytes 0-{}/{LARGE}\r\nConnection: close\r\n\r\n", LARGE - 1).unwrap();
        stream.flush().unwrap();
        let sent = send_body(&mut stream, body);
        let _ = finished.send(sent);
    });
    (origin, result)
}

fn send_body(stream: &mut TcpStream, body: Body) -> usize {
    match body {
        Body::Truncated => {
            stream.write_all(b"short").unwrap();
            5
        }
        Body::Streaming => {
            stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut sent = 0;
            while sent < LARGE && stream.write_all(&[0; 4096]).is_ok() {
                sent += 4096;
                thread::sleep(Duration::from_millis(1));
            }
            sent
        }
        Body::Silent => {
            stream.set_read_timeout(Some(STALLED + Duration::from_secs(5))).unwrap();
            let _ = stream.read(&mut [0]);
            0
        }
    }
}

fn engine(origin: &str, dir: &tempfile::TempDir) -> EngineHandle {
    let fence = Arc::new(Fence::new(
        Roster::new([Mri::new(LAB_A)], []),
        FenceLog::from_writer(std::io::sink()),
    ));
    let tokens = Arc::new(Tokens);
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, origin).unwrap();
    engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("stream.sqlite")).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox: Outbox::new(
            Arc::new(http()),
            Arc::clone(&fence),
            Arc::new(FailClosed),
            Author::new(Mri::new(LAB_A), "Synthetic A"),
        ),
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap()
}

#[tokio::test]
async fn a_truncated_body_reports_a_failed_read_once_then_ends() {
    let (origin, finished) = peer(Body::Truncated);
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&origin, &dir);
    let mut stream = handle.file_stream(SHARE, 0, LARGE as u64).await.unwrap();
    let mut failed = 0;
    while let Some(piece) = stream.next().await {
        if let Err(error) = piece {
            assert!(matches!(error, EngineError::FileUnread));
            failed += 1;
        }
    }
    assert_eq!(failed, 1);
    assert_eq!(finished.recv_timeout(Duration::from_secs(2)).unwrap(), 5);
    handle.shutdown();
}

#[tokio::test]
async fn dropping_a_stream_closes_its_body_before_the_server_finishes_sending() {
    let (origin, finished) = peer(Body::Streaming);
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&origin, &dir);
    let mut stream = handle.file_stream(SHARE, 0, LARGE as u64).await.unwrap();
    assert!(!stream.next().await.unwrap().unwrap().is_empty());
    drop(stream);
    assert!(finished.recv_timeout(Duration::from_secs(3)).unwrap() < LARGE);
    handle.shutdown();
}

#[tokio::test]
async fn a_body_that_stays_silent_past_the_stall_limit_fails_and_closes() {
    let (origin, finished) = peer(Body::Silent);
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&origin, &dir);
    let mut stream = handle.file_stream(SHARE, 0, LARGE as u64).await.unwrap();
    let piece = tokio::time::timeout(STALLED + Duration::from_secs(2), stream.next())
        .await
        .unwrap();
    assert!(matches!(piece, Some(Err(EngineError::FileUnread))));
    assert!(stream.next().await.is_none());
    assert_eq!(finished.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
    handle.shutdown();
}
