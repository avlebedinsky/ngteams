//! A synthetic CDP peer for scenario decisions: never opens a browser or contacts a service.

use std::sync::{Arc, Mutex};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

#[cfg(unix)]
use super::dsl::Driver;
#[cfg(unix)]
use crate::cdp::Event;
use crate::cdp::{Cdp, Events, SessionId};

pub(crate) struct Page {
    #[cfg(unix)]
    pub(crate) driver: Driver,
    #[cfg(unix)]
    pub(crate) events: mpsc::UnboundedSender<Event>,
    wire: mpsc::UnboundedSender<Value>,
    cdp: Cdp,
    incoming: Option<Events>,
    commands: Arc<Mutex<Vec<Value>>>,
    peer: tokio::task::JoinHandle<()>,
    close_process: Arc<Mutex<Option<u32>>>,
    action_targets: Option<Arc<crate::action_targets::ActionTargets>>,
}

impl Page {
    pub(crate) async fn new(mut answer: impl FnMut(&Value) -> Result<Value, String> + Send + 'static) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let commands = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&commands);
        let close_process = Arc::new(Mutex::new(None));
        let process = Arc::clone(&close_process);
        let (wire, mut outgoing) = mpsc::unbounded_channel::<Value>();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            loop {
                let text = tokio::select! {
                    Some(event) = outgoing.recv() => {
                        if socket.send(Message::Text(event.to_string().into())).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    message = socket.next() => {
                        let Some(Ok(Message::Text(text))) = message else { break };
                        text
                    }
                };
                let command: Value = serde_json::from_str(&text).unwrap();
                recorded.lock().unwrap().push(command.clone());
                let mut outcome = answer(&command);
                if command["method"] == "Browser.close"
                    && outcome.is_ok()
                    && let Some(pid) = *process.lock().unwrap()
                {
                    outcome = super::test_process::terminate(pid)
                        .map(|()| outcome.unwrap())
                        .map_err(|error| format!("synthetic process did not stop: {error}"));
                }
                let reply = match outcome {
                    Ok(result) => json!({"id": command["id"], "result": result}),
                    Err(message) => json!({"id": command["id"], "error": {"code": -32000, "message": message}}),
                };
                if socket.send(Message::Text(reply.to_string().into())).await.is_err() {
                    break;
                }
            }
        });
        let (cdp, incoming) = Cdp::connect(&endpoint).await.unwrap();
        #[cfg(unix)]
        let (events, queue) = mpsc::unbounded_channel();
        Self {
            #[cfg(unix)]
            driver: Driver::new(cdp.clone(), SessionId::new("synthetic-page"), queue),
            #[cfg(unix)]
            events,
            wire,
            cdp,
            incoming: Some(incoming),
            commands,
            peer,
            close_process,
            action_targets: None,
        }
    }

    /// A local synthetic page with an explicit selected conversation; call evidence is supplied separately.
    #[cfg(unix)]
    pub(crate) async fn new_lab_chat(
        expected: &str,
        mut answer: impl FnMut(&Value) -> Result<Value, String> + Send + 'static,
    ) -> Self {
        let selected = expected.to_owned();
        Self::new(move |command| {
            if command["method"] == "Runtime.evaluate"
                && command["params"]["expression"]
                    .as_str()
                    .is_some_and(|expression| expression.contains("__labSelectedConversation"))
            {
                return Ok(evaluated(json!(selected)));
            }
            answer(command)
        })
        .await
    }

    pub(crate) fn commands(&self) -> Vec<Value> {
        self.commands.lock().unwrap().clone()
    }

    pub(crate) fn send(&self, method: &str, params: Value) {
        let mut event = json!({"method": method, "sessionId": "synthetic-page"});
        event["params"] = params;
        self.sender().send(event).unwrap();
    }

    pub(crate) fn sender(&self) -> mpsc::UnboundedSender<Value> {
        self.wire.clone()
    }

    pub(crate) fn session(&mut self, dir: &std::path::Path, record: bool) -> crate::session::LabSession {
        self.session_with_process(dir, record, None)
    }

    /// The close acknowledgement stops a real disposable stand-in; ordinary close then waits for and reaps it.
    pub(crate) fn managed_session(&mut self, dir: &std::path::Path, record: bool) -> (crate::session::LabSession, u32) {
        let (browser, pid) = super::test_process::spawn();
        *self.close_process.lock().unwrap() = Some(pid);
        (self.session_with_process(dir, record, Some(browser)), pid)
    }

    fn session_with_process(
        &mut self,
        dir: &std::path::Path,
        record: bool,
        browser: Option<crate::browser::Browser>,
    ) -> crate::session::LabSession {
        use crate::arming::ArmedBrowser;
        use crate::capture::{Capture, Recorder, RtcHooks};
        let http = crate::capture_hold::CaptureHold::new();
        let recorder = record.then(|| Arc::new(Recorder::create(dir, "synthetic-offline").unwrap()));
        let capture = recorder
            .as_ref()
            .map(|recorder| Capture::new(Arc::clone(recorder), RtcHooks::default()));
        let incoming = self.incoming.take().unwrap();
        let armed = match browser {
            Some(browser) => ArmedBrowser::synthetic_managed(self.cdp.clone(), incoming, http, capture, browser),
            None => ArmedBrowser::synthetic(self.cdp.clone(), incoming, http, capture),
        };
        let mut session = crate::session::LabSession::synthetic(armed, SessionId::new("synthetic-page"), recorder);
        if let Some(targets) = &self.action_targets {
            session.set_action_targets(Arc::clone(targets));
        }
        session
    }

    pub(crate) async fn loaded_session(&mut self, dir: &std::path::Path, record: bool) -> crate::session::LabSession {
        let mut session = self.session(dir, record);
        self.send(
            "Page.frameNavigated",
            json!({"frame": {"id": "synthetic-frame", "url": crate::session::TEAMS_URL}}),
        );
        self.send(
            "Network.requestWillBeSent",
            json!({"requestId": "synthetic-load", "request": {
                "method": "GET", "url": "https://teams.microsoft.com/api/chatsvc/synthetic", "headers": {},
            }}),
        );
        self.send(
            "Network.responseReceived",
            json!({"requestId": "synthetic-load", "response": {"status": 200}}),
        );
        self.send("Page.frameStoppedLoading", json!({"frameId": "synthetic-frame"}));
        assert_eq!(
            session.settle(std::time::Duration::from_secs(2)).await,
            crate::session::SessionState::Loaded
        );
        session
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        self.peer.abort();
    }
}

pub(crate) fn evaluated(value: Value) -> Value {
    let mut answer = json!({"result": {}});
    answer["result"]["value"] = value;
    answer
}
