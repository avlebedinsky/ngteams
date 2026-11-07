//! Synthetic machine-to-tracker-to-socket regressions for callbacks generated after start.
use super::*;
use std::future::{self, Future};
use std::sync::{Arc, Mutex};
use teams_proto::{CallState as _, FrameSink, FrameText, FrameWriter, Request, RespondError, Response};

#[path = "tracked_links.rs"]
mod tracked_links;

#[derive(Clone, Default)]
struct Socket(Arc<Mutex<Vec<String>>>);

impl FrameWriter for Socket {
    type Error = std::io::Error;

    fn write(&mut self, text: FrameText) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.0.lock().unwrap().push(text.into_string());
        future::ready(Ok(()))
    }
}

struct Sent {
    id: WriteId,
    attempt: crate::track::Attempt,
    body: Value,
    name: &'static str,
}

struct Capture<'a> {
    tracker: &'a crate::Tracker,
    name: &'static str,
    sent: Vec<Sent>,
}

impl crate::WriteSink for Capture<'_> {
    fn submit<R: Request + Clone + Send + 'static>(&mut self, id: WriteId, write: R) {
        let body = write.body().unwrap_or(Value::Null);
        let url = write.call_url().unwrap();
        assert!(self.tracker.call_allowed(url, Some(&body)), "{}", self.name);
        self.sent.push(Sent {
            id,
            attempt: self.tracker.before(&write),
            body,
            name: self.name,
        });
    }
}

#[derive(Default)]
struct Batch {
    sent: Vec<Sent>,
    replies: Vec<DeliveryReply>,
    events: Vec<CallEvent>,
}

fn drain(calls: &mut Calls, tracker: &crate::Tracker) -> Batch {
    let mut batch = Batch::default();
    for effect in effects(calls) {
        match effect {
            Effect::Send { id, write } => {
                let mut capture = Capture {
                    tracker,
                    name: write.name(),
                    sent: Vec::new(),
                };
                write.submit_to(id, &mut capture);
                batch.sent.extend(capture.sent);
            }
            Effect::Respond { reply, .. } => batch.replies.push(reply),
            Effect::Event(event) => batch.events.push(event),
            Effect::LetGo { .. } => panic!("the placed call has no invitation to release"),
        }
    }
    batch
}

fn complete(calls: &mut Calls, tracker: &crate::Tracker, sent: &Sent, body: &Value, now: Instant) {
    tracker.answered(sent.attempt, &Response::new(200, body.to_string()));
    calls.on_outcome(sent.id, &ok(body), now);
}

async fn respond(sink: &mut FrameSink<Socket>, reply: DeliveryReply) -> Result<(), RespondError<std::io::Error>> {
    match reply {
        DeliveryReply::Acknowledgement(ack) => sink.respond(ack).await.map(|_| ()),
        DeliveryReply::Receipt(receipt) => sink.respond(receipt).await.map(|_| ()),
    }
}

struct Connected {
    calls: Calls,
    call: CallId,
    tracker: Arc<crate::Tracker>,
    sink: FrameSink<Socket>,
    socket: Socket,
    start: Value,
    now: Instant,
}

impl Connected {
    async fn new() -> Self {
        Self::with_links(&[]).await
    }

    /// As [`Self::new`], the acceptance handing out `links` (name, link) beside its own.
    async fn with_links(links: &[(&str, String)]) -> Self {
        let now = Instant::now();
        let mut calls = Calls::new(identity(A));
        calls.set_push_url(PUSH).unwrap();
        let tracker = Arc::new(crate::Tracker::new(Mri::new(A)));
        assert!(tracker.connected("calling/1", PUSH));
        let socket = Socket::default();
        let sink = FrameSink::new(socket.clone())
            .with_call_responses("calling/1")
            .with_call_state(tracker.clone());
        let call = calls
            .place(
                &Placement::Chat {
                    callees: vec![Mri::new(B)],
                    group_chat: None,
                },
                media(),
                now,
            )
            .unwrap();
        let batch = drain(&mut calls, &tracker);
        let [start] = batch.sent.as_slice() else {
            panic!("one start")
        };
        assert_eq!(start.name, "start");
        complete(&mut calls, &tracker, start, &conversation_answer(), now);
        let start = start.body.clone();
        drop(drain(&mut calls, &tracker));
        let mut setup = Self {
            calls,
            call,
            tracker,
            sink,
            socket,
            start,
            now,
        };
        setup.accept(links).await;
        setup
    }

    async fn accept(&mut self, links: &[(&str, String)]) {
        let mut body = acceptance();
        body["callAcceptance"]["links"]["mediaRenegotiation"] = json!(cc("active", "renegotiate"));
        for (name, link) in links {
            body["callAcceptance"]["links"][*name] = json!(link);
        }
        let incoming = delivery(&acceptance_path(&self.calls), &body);
        let batch = self.deliver(&incoming, Observed::Acceptance);
        assert!(batch.events.contains(&CallEvent::State {
            call: self.call,
            state: CallState::Connected,
        }));
        let [ack] = batch.replies.as_slice() else {
            panic!("one acceptance ack")
        };
        respond(&mut self.sink, ack.clone()).await.unwrap();
        assert!(matches!(
            respond(&mut self.sink, ack.clone()).await,
            Err(RespondError::NotCurrentCall)
        ));
        for sent in batch.sent {
            complete(&mut self.calls, &self.tracker, &sent, &json!({}), self.now);
        }
        drop(drain(&mut self.calls, &self.tracker));
    }

    fn deliver(&mut self, incoming: &Delivery, expected: Observed) -> Batch {
        let observed = self.tracker.observe_delivery(incoming);
        assert_eq!(observed, expected);
        self.calls.on_delivery(incoming, observed, self.now);
        drain(&mut self.calls, &self.tracker)
    }

    fn ack_callback(&self) -> String {
        let frames = self.socket.0.lock().unwrap();
        let envelope: Value = serde_json::from_str(frames[0].strip_prefix("3:::").unwrap()).unwrap();
        let body: Value = serde_json::from_str(envelope["body"].as_str().unwrap()).unwrap();
        body["callAcceptanceAcknowledgement"]["links"]["mediaRenegotiation"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

fn incoming(id: i64, callback: &str, body: &Value) -> Delivery {
    let mut incoming = delivery(url::Url::parse(callback).unwrap().path(), body);
    incoming.id = id;
    incoming.trouter_request = Some(format!("request-{id}"));
    incoming
}

#[tokio::test]
async fn acknowledgement_generated_callback_receipts_remain_current_and_single_use() {
    let mut setup = Connected::new().await;
    let callback = setup.ack_callback();
    assert!(
        !setup.start.to_string().contains(&callback),
        "this path was not in the start"
    );
    let incoming = incoming(8, &callback, &negotiation());
    let batch = setup.deliver(&incoming, Observed::Renegotiation);
    assert!(matches!(batch.events.as_slice(), [CallEvent::MediaOffer { .. }]));
    let [receipt] = batch.replies.as_slice() else {
        panic!("one media receipt")
    };
    let mut foreign = FrameSink::new(setup.socket.clone())
        .with_call_responses("another-connection")
        .with_call_state(setup.tracker.clone());
    assert!(matches!(
        respond(&mut foreign, receipt.clone()).await,
        Err(RespondError::OtherConnection)
    ));
    respond(&mut setup.sink, receipt.clone()).await.unwrap();
    assert!(matches!(
        respond(&mut setup.sink, receipt.clone()).await,
        Err(RespondError::NotCurrentCall)
    ));
    assert_eq!(setup.socket.0.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn camera_renegotiation_generated_answer_is_receipted_before_its_http_answer() {
    let mut setup = Connected::new().await;
    setup.calls.set_video(setup.call, true);
    setup.calls.renegotiate(setup.call, &media()).unwrap();
    let batch = drain(&mut setup.calls, &setup.tracker);
    let [request] = batch.sent.as_slice() else {
        panic!("one camera renegotiation")
    };
    assert_eq!(request.name, "renegotiate");
    assert!(request.body.to_string().contains("Video"));
    let callback = request.body["mediaNegotiation"]["links"]["mediaAnswer"]
        .as_str()
        .unwrap();
    assert!(!setup.start.to_string().contains(callback));
    let incoming = incoming(9, callback, &media_answer("v=1\r\n"));
    let answered = setup.deliver(&incoming, Observed::Renegotiation);
    let [receipt] = answered.replies.as_slice() else {
        panic!("one media answer receipt")
    };
    respond(&mut setup.sink, receipt.clone()).await.unwrap();
    assert!(matches!(answered.events.as_slice(), [CallEvent::RemoteMedia { .. }]));
    assert_eq!(setup.socket.0.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn ending_the_call_does_not_leave_a_new_callback_receipt_sendable() {
    let mut setup = Connected::new().await;
    let incoming = incoming(10, &setup.ack_callback(), &negotiation());
    let batch = setup.deliver(&incoming, Observed::Renegotiation);
    let [receipt] = batch.replies.as_slice() else {
        panic!("one pending media receipt")
    };
    setup.calls.hang_up(setup.call, setup.now).unwrap();
    let leaving = drain(&mut setup.calls, &setup.tracker);
    assert!(leaving.sent.iter().any(|sent| sent.name == "leave"));
    assert!(matches!(
        respond(&mut setup.sink, receipt.clone()).await,
        Err(RespondError::NotCurrentCall)
    ));
    assert_eq!(setup.socket.0.lock().unwrap().len(), 1);
}
