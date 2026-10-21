//! The Trouter frame policy: a [`FrameSink`] sends only the reviewed own frames, in the shapes the messaging stack sent
//! in M1 Task 14's captures (the synthetic `tests/fixtures/trouter/session.json`), numbers the events that ask for an
//! acknowledgement itself, and logs every frame to `fence.log` before it is sent; a frame that can't be logged isn't
//! sent.
//!
//! Tokens, endpoint ids and URLs here are placeholders, never real ones.

use std::future::{self, Future};
use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_fence::{Fence, FenceLog, Mri, Roster};
use teams_proto::{
    Authenticate, ConnectParams, Disconnect, DroppedIndicator, FrameError, FrameSink, FrameText, FrameWriter,
    MessageLossAck, OwnFrame, Ping,
};

const SESSION: &str = include_str!("../../../tests/fixtures/trouter/session.json");
const SOCKET: &str = "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c?tc=%7B%7D&timeout=40&epid=placeholder";
const SOCKET_LOGGED: &str = "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c";
/// The fixture's token placeholder, sent as the frame's token so the texts compare whole.
const FIXTURE_TOKEN: &str = "<token:1>";
/// A token that must never reach the log or a `Debug`.
const PLANTED_TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJwbGFudGVkIn0.cGxhbnRlZC1zaWduYXR1cmU";

/// A `fence.log` the test can read back.
#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl SharedLog {
    #[cfg(test)]
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()).unwrap()
    }

    #[cfg(test)]
    fn lines(&self) -> Vec<Value> {
        self.text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Write for SharedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A `fence.log` that can't be written.
struct BrokenLog;

impl Write for BrokenLog {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("disk full"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("disk full"))
    }
}

/// What the socket was handed, each frame with the number of `fence.log` lines there were when it was.
#[derive(Clone, Default)]
struct Socket {
    sent: Arc<Mutex<Vec<(String, usize)>>>,
}

impl Socket {
    fn texts(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(text, _)| text.clone())
            .collect()
    }

    fn logged_before(&self) -> Vec<usize> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, logged)| *logged)
            .collect()
    }
}

/// The socket's write half: records each frame and how much of the log was written when it came.
struct Writer {
    socket: Socket,
    log: Option<SharedLog>,
    fail: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("the socket is closed")]
struct Closed;

impl FrameWriter for Writer {
    type Error = Closed;

    fn write(&mut self, text: FrameText) -> impl Future<Output = Result<(), Closed>> + Send {
        if self.fail {
            return future::ready(Err(Closed));
        }
        let logged = self.log.as_ref().map_or(0, |log| log.text().lines().count());
        self.socket
            .sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((text.into_string(), logged));
        future::ready(Ok(()))
    }
}

fn fence(log: impl Write + Send + 'static) -> Arc<Fence> {
    Arc::new(Fence::new(
        Roster::new([Mri::new("8:orgid:lab-a"), Mri::new("8:orgid:lab-b")], []),
        FenceLog::from_writer(log),
    ))
}

struct Setup {
    sink: FrameSink<Writer>,
    socket: Socket,
    log: SharedLog,
}

fn setup() -> Setup {
    let log = SharedLog::default();
    let socket = Socket::default();
    let writer = Writer {
        socket: socket.clone(),
        log: Some(log.clone()),
        fail: false,
    };
    Setup {
        sink: FrameSink::new(writer, fence(log.clone()), SOCKET),
        socket,
        log,
    }
}

/// The fixture's frames the client sent, by name.
#[cfg(test)]
fn fixture_frame(name: &str) -> String {
    let session: Value = serde_json::from_str(SESSION).unwrap();
    let frames = session["frames"].as_array().unwrap();
    let frame = frames
        .iter()
        .find(|frame| frame["name"] == name && frame["direction"] == "out")
        .unwrap_or_else(|| panic!("no outgoing frame {name:?} in the fixture"));
    frame["frame"].as_str().unwrap().to_owned()
}

/// A frame with its acknowledgement id (the `<n>` of `5:<n>+::`) taken out, the only part a socket numbers itself.
#[cfg(test)]
fn without_ack_id(text: &str) -> String {
    match text.strip_prefix("5:").and_then(|rest| rest.split_once("+::")) {
        Some((id, event)) if !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()) => {
            format!("5:<n>+::{event}")
        }
        _ => text.to_owned(),
    }
}

fn fixture_params() -> ConnectParams {
    ConnectParams {
        issuer: String::new(),
        scae: "1".to_owned(),
        sig: String::new(),
        sr: "SyntheticEndpoint00000A".to_owned(),
        sp: "pub-ent-zz-01".to_owned(),
        se: "1700000600000".to_owned(),
        st: "1700000000000".to_owned(),
    }
}

#[cfg(test)]
fn messaging_loss() -> MessageLossAck {
    MessageLossAck::new(vec![DroppedIndicator {
        tag: "messaging".to_owned(),
        etag: "2000-01-01T00:00:00.0000001Z".to_owned(),
    }])
    .unwrap()
}

#[tokio::test]
async fn each_own_frame_has_the_shape_the_messaging_stack_sent() {
    let mut setup = setup();
    let token = Secret::new(FIXTURE_TOKEN);
    setup
        .sink
        .send(&Authenticate::new(&token, Some(fixture_params())))
        .await
        .unwrap();
    setup.sink.send(&Authenticate::new(&token, None)).await.unwrap();
    setup.sink.send(&messaging_loss()).await.unwrap();
    setup.sink.send(&Ping::new()).await.unwrap();
    setup.sink.send(&Disconnect::new()).await.unwrap();

    // byte for byte, key order included (the captured one, whatever serde_json's features in this build): only the
    // acknowledgement ids differ, the fixture's socket having numbered a `user.activity` the native client doesn't send
    let sent: Vec<String> = setup.socket.texts().iter().map(|text| without_ack_id(text)).collect();
    let expected: Vec<String> = [
        "authentication with the reconnect parameters an earlier trouter.connected handed over",
        "the first socket's authentication, without reconnect parameters",
        "the client's acknowledgement of that report, the indicators handed back as they came",
        "the keep-alive, every 40 s and after every response the client sends",
        "the client's disconnect",
    ]
    .into_iter()
    .map(|name| without_ack_id(&fixture_frame(name)))
    .collect();
    assert_eq!(sent, expected);
}

#[test]
fn only_the_ack_id_is_taken_out_of_a_frame() {
    assert_eq!(
        without_ack_id("5:12+::{\"name\":\"ping\"}"),
        "5:<n>+::{\"name\":\"ping\"}"
    );
    for text in ["5:::{\"name\":\"ping\"}", "0::", "5:+::{}", "5:1a+::{}"] {
        assert_eq!(without_ack_id(text), text);
    }
}

#[tokio::test]
async fn the_sink_numbers_acknowledged_events_itself_and_plain_ones_carry_no_id() {
    let mut setup = setup();
    let token = Secret::new(PLANTED_TOKEN);
    let auth = setup.sink.send(&Authenticate::new(&token, None)).await.unwrap();
    let loss = setup.sink.send(&messaging_loss()).await.unwrap();
    let first = setup.sink.send(&Ping::new()).await.unwrap();
    let second = setup.sink.send(&Ping::new()).await.unwrap();
    let bye = setup.sink.send(&Disconnect::new()).await.unwrap();

    assert_eq!(auth.ack_id(), None);
    assert_eq!(loss.ack_id(), Some(1));
    assert_eq!(first.ack_id(), Some(2));
    assert_eq!(second.ack_id(), Some(3));
    assert_eq!(bye.ack_id(), None);
    let headers: Vec<String> = setup
        .socket
        .texts()
        .iter()
        .map(|text| text.find('{').map_or(text.as_str(), |start| &text[..start]).to_owned())
        .collect();
    assert_eq!(headers, ["5:::", "5:1+::", "5:2+::", "5:3+::", "0::"]);
}

#[tokio::test]
async fn every_frame_is_logged_by_kind_before_it_is_sent_and_never_with_a_token() {
    let mut setup = setup();
    let token = Secret::new(PLANTED_TOKEN);
    setup
        .sink
        .send(&Authenticate::new(&token, Some(fixture_params())))
        .await
        .unwrap();
    setup.sink.send(&messaging_loss()).await.unwrap();
    setup.sink.send(&Ping::new()).await.unwrap();
    setup.sink.send(&Disconnect::new()).await.unwrap();

    assert_eq!(
        setup.socket.logged_before(),
        [1, 2, 3, 4],
        "each frame's entry was written before the frame reached the socket"
    );
    let kinds: Vec<Value> = setup
        .log
        .lines()
        .into_iter()
        .map(|mut entry| {
            let ts = entry.as_object_mut().unwrap().remove("ts_ms").unwrap();
            assert!(ts.as_u64().is_some_and(|ts| ts > 0));
            assert_eq!(entry["verdict"], "allow");
            assert_eq!(entry["event"], "frame");
            assert_eq!(entry["channel"], "web_socket");
            assert_eq!(entry["url"], SOCKET_LOGGED);
            entry["own"].clone()
        })
        .collect();
    assert_eq!(
        kinds,
        [Authenticate::KIND, MessageLossAck::KIND, Ping::KIND, Disconnect::KIND]
    );
    let text = setup.log.text();
    assert!(!text.contains(PLANTED_TOKEN), "{text}");
    assert!(!text.contains("epid"), "the socket's query isn't logged: {text}");
    assert!(
        setup.socket.texts()[0].contains(&format!("Bearer {PLANTED_TOKEN}")),
        "the token goes on the socket only"
    );
}

#[tokio::test]
async fn a_frame_that_cant_be_logged_isnt_sent_and_neither_is_any_later_one() {
    let socket = Socket::default();
    let writer = Writer {
        socket: socket.clone(),
        log: None,
        fail: false,
    };
    let mut sink = FrameSink::new(writer, fence(BrokenLog), SOCKET);
    let token = Secret::new(PLANTED_TOKEN);

    let refused = sink.send(&Authenticate::new(&token, None)).await;
    assert!(matches!(refused, Err(FrameError::LogUnavailable(_))), "{refused:?}");
    let refused = sink.send(&Ping::new()).await;
    assert!(matches!(refused, Err(FrameError::LogUnavailable(_))), "{refused:?}");
    assert!(socket.texts().is_empty(), "nothing reached the socket");
    let shown = format!("{refused:?}");
    assert!(!shown.contains(PLANTED_TOKEN), "{shown}");
}

#[tokio::test]
async fn a_socket_failure_comes_back_after_the_frame_was_logged() {
    let log = SharedLog::default();
    let writer = Writer {
        socket: Socket::default(),
        log: None,
        fail: true,
    };
    let mut sink = FrameSink::new(writer, fence(log.clone()), SOCKET);
    let failed = sink.send(&Ping::new()).await;
    assert!(matches!(failed, Err(FrameError::Write(Closed))), "{failed:?}");
    assert_eq!(log.lines().len(), 1, "logged before the socket was tried");
    let next = sink.send(&Ping::new()).await;
    assert!(matches!(next, Err(FrameError::Write(Closed))));
    assert_eq!(log.lines().len(), 2, "each attempt is logged before it is tried");
}

/// A writer error that hands the unsent frame back and shows it in its `Debug` and `Display`, as tungstenite's
/// `WriteBufferFull` hands the message back.
#[derive(Debug, thiserror::Error)]
#[error("the write buffer is full: {0}")]
struct BufferFull(String);

struct FullWriter;

impl FrameWriter for FullWriter {
    type Error = BufferFull;

    fn write(&mut self, text: FrameText) -> impl Future<Output = Result<(), BufferFull>> + Send {
        future::ready(Err(BufferFull(text.into_string())))
    }
}

#[tokio::test]
async fn a_writer_error_that_holds_the_frame_shows_it_nowhere() {
    let mut sink = FrameSink::new(FullWriter, fence(SharedLog::default()), SOCKET);
    let failed = sink
        .send(&Authenticate::new(&Secret::new(PLANTED_TOKEN), None))
        .await
        .unwrap_err();

    let mut shown = vec![format!("{failed:?}"), failed.to_string()];
    let mut source = std::error::Error::source(&failed);
    while let Some(error) = source {
        shown.push(format!("{error:?} {error}"));
        source = error.source();
    }
    for shown in &shown {
        assert!(!shown.contains(PLANTED_TOKEN), "{shown}");
    }
    assert_eq!(shown.len(), 2, "the writer's error isn't the source: {shown:?}");
    assert!(
        matches!(&failed, FrameError::Write(BufferFull(text)) if text.contains(PLANTED_TOKEN)),
        "the caller still gets the writer's error by matching the variant"
    );
}

#[test]
fn a_message_loss_ack_needs_an_indicator() {
    assert!(
        MessageLossAck::new(Vec::new()).is_none(),
        "no report showed an empty list"
    );
}

#[test]
fn no_token_or_signature_shows_in_a_debug() {
    let token = Secret::new(PLANTED_TOKEN);
    let mut params = fixture_params();
    params.sig = PLANTED_TOKEN.to_owned();
    let frame = Authenticate::new(&token, Some(params.clone()));
    for shown in [format!("{frame:?}"), format!("{params:?}")] {
        assert!(!shown.contains(PLANTED_TOKEN), "{shown}");
    }
}

#[tokio::test]
async fn a_frame_text_shows_its_length_only() {
    struct Shown(Arc<Mutex<Vec<String>>>);
    impl FrameWriter for Shown {
        type Error = Closed;
        fn write(&mut self, text: FrameText) -> impl Future<Output = Result<(), Closed>> + Send {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("{text:?} {}", text.as_str().len()));
            future::ready(Ok(()))
        }
    }
    let shown = Arc::new(Mutex::new(Vec::new()));
    let mut sink = FrameSink::new(Shown(Arc::clone(&shown)), fence(SharedLog::default()), SOCKET);
    sink.send(&Authenticate::new(&Secret::new(PLANTED_TOKEN), None))
        .await
        .unwrap();
    let shown = shown.lock().unwrap().clone();
    assert_eq!(shown.len(), 1);
    assert!(!shown[0].contains(PLANTED_TOKEN), "{}", shown[0]);
    assert!(shown[0].starts_with("FrameText { len: "), "{}", shown[0]);
}

#[test]
fn the_reviewed_list_is_the_four_session_frames() {
    // no `user.activity` and no answer to a delivery (`3:::`): neither has a frame type, and the compile-fail cases in
    // crates/fence-seal (proto_raw_frame, proto_own_frame) show that nothing else can be sent or added
    let kinds = [Authenticate::KIND, Ping::KIND, MessageLossAck::KIND, Disconnect::KIND];
    assert_eq!(
        kinds,
        [
            "trouter_authenticate",
            "trouter_ping",
            "trouter_message_loss_ack",
            "trouter_disconnect"
        ]
    );
}
