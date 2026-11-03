//! Typed socket frames, validated delivery responses and their exact synthetic wire representations.

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_proto::{
    Authenticate, ConnectParams, Disconnect, DroppedIndicator, FrameError, FrameSink, FrameText, FrameWriter,
    MessageLossAck, OwnFrame, Ping,
};

const SESSION: &str = include_str!("../../../tests/fixtures/trouter/session.json");
/// The fixture's token placeholder, sent as the frame's token so the texts compare whole.
const FIXTURE_TOKEN: &str = "<token:1>";
/// A token that must never reach a `Debug` or an error.
const PLANTED_TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJwbGFudGVkIn0.cGxhbnRlZC1zaWduYXR1cmU";

/// What the socket was handed.
#[derive(Clone, Default)]
struct Socket {
    sent: Arc<Mutex<Vec<String>>>,
}

impl Socket {
    fn texts(&self) -> Vec<String> {
        self.sent.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// The socket's write half: records each frame, or fails every write.
struct Writer {
    socket: Socket,
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
        self.socket
            .sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.into_string());
        future::ready(Ok(()))
    }
}

struct Setup {
    sink: FrameSink<Writer>,
    socket: Socket,
}

fn setup() -> Setup {
    let socket = Socket::default();
    let writer = Writer {
        socket: socket.clone(),
        fail: false,
    };
    Setup {
        sink: FrameSink::new(writer),
        socket,
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

#[tokio::test]
async fn the_calling_stack_s_authentication_carries_only_its_two_headers() {
    let mut setup = setup();
    let token = Secret::new(FIXTURE_TOKEN);
    setup.sink.send(&Authenticate::calling(&token, None)).await.unwrap();
    setup
        .sink
        .send(&Authenticate::calling(&token, Some(fixture_params())))
        .await
        .unwrap();
    // the calling stack's sockets sent `Authorization` and `X-MS-Migration` only, in that order
    let texts = setup.socket.texts();
    assert_eq!(
        texts[0],
        r#"5:::{"name":"user.authenticate","args":[{"headers":{"Authorization":"Bearer <token:1>","X-MS-Migration":"True"}}]}"#
    );
    let messaging =
        fixture_frame("authentication with the reconnect parameters an earlier trouter.connected handed over");
    assert_eq!(
        texts[1],
        messaging.replace(r#""X-Ms-Test-User":"False","#, ""),
        "the same frame as the messaging stack's but for X-Ms-Test-User"
    );
    assert_eq!(<Authenticate as OwnFrame>::KIND, "trouter_authenticate");
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
async fn the_sink_sends_every_typed_frame_and_the_token_only_on_the_socket() {
    let mut setup = setup();
    let token = Secret::new(PLANTED_TOKEN);
    let frame = Authenticate::new(&token, Some(fixture_params()));
    setup.sink.send(&frame).await.unwrap();
    setup.sink.send(&messaging_loss()).await.unwrap();
    setup.sink.send(&Ping::new()).await.unwrap();
    setup.sink.send(&Disconnect::new()).await.unwrap();

    let sent = setup.socket.texts();
    assert_eq!(sent.len(), 4, "every frame reached the socket");
    assert!(
        sent[0].contains(&format!("Bearer {PLANTED_TOKEN}")),
        "the token goes on the socket"
    );
    for shown in [format!("{:?}", setup.sink), format!("{frame:?}")] {
        assert!(!shown.contains(PLANTED_TOKEN), "{shown}");
    }
}

#[tokio::test]
async fn a_socket_failure_comes_back_for_each_frame_tried() {
    let socket = Socket::default();
    let writer = Writer {
        socket: socket.clone(),
        fail: true,
    };
    let mut sink = FrameSink::new(writer);
    let failed = sink.send(&Ping::new()).await;
    assert!(matches!(failed, Err(FrameError::Write(Closed))), "{failed:?}");
    let next = sink.send(&Ping::new()).await;
    assert!(matches!(next, Err(FrameError::Write(Closed))), "{next:?}");
    assert!(socket.texts().is_empty());
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
    let mut sink = FrameSink::new(FullWriter);
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
    let mut sink = FrameSink::new(Shown(Arc::clone(&shown)));
    sink.send(&Authenticate::new(&Secret::new(PLANTED_TOKEN), None))
        .await
        .unwrap();
    let shown = shown.lock().unwrap().clone();
    assert_eq!(shown.len(), 1);
    assert!(!shown[0].contains(PLANTED_TOKEN), "{}", shown[0]);
    assert!(shown[0].starts_with("FrameText { len: "), "{}", shown[0]);
}

#[tokio::test]
async fn no_frame_answers_a_delivery() {
    // a delivery is answered with a socket.io message (`3:::`); every frame there is an event (`5:`) or the disconnect
    // (`0::`), never a message, a JSON message (`4:`) or an acknowledgement (`6:`). The compile-fail cases
    // in tests/ui (proto_raw_frame, proto_own_frame) show that no other crate can send text or add a frame.
    let mut setup = setup();
    setup
        .sink
        .send(&Authenticate::new(&Secret::new(PLANTED_TOKEN), Some(fixture_params())))
        .await
        .unwrap();
    setup.sink.send(&messaging_loss()).await.unwrap();
    setup.sink.send(&Ping::new()).await.unwrap();
    setup.sink.send(&Disconnect::new()).await.unwrap();
    for text in setup.socket.texts() {
        assert!(text.starts_with("5:") || text == "0::", "{text}");
    }
    let kinds = [Authenticate::KIND, Ping::KIND, MessageLossAck::KIND, Disconnect::KIND];
    assert_eq!(
        kinds,
        [
            "trouter_authenticate",
            "trouter_ping",
            "trouter_message_loss_ack",
            "trouter_disconnect"
        ],
        "each typed frame's diagnostic kind"
    );
}
