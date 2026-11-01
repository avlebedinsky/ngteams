//! A synthetic CDP peer for scenario decisions: never opens a browser or contacts a service.

use std::sync::{Arc, Mutex};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::dsl::Driver;
use crate::cdp::{Cdp, Event, Events, SessionId};

pub(crate) struct Page {
    pub(crate) driver: Driver,
    pub(crate) events: mpsc::UnboundedSender<Event>,
    wire: mpsc::UnboundedSender<Value>,
    cdp: Cdp,
    incoming: Option<Events>,
    commands: Arc<Mutex<Vec<Value>>>,
    peer: tokio::task::JoinHandle<()>,
    close_process: Arc<Mutex<Option<rustix::process::Pid>>>,
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
                    outcome = rustix::process::kill_process(pid, rustix::process::Signal::TERM)
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
        let (events, queue) = mpsc::unbounded_channel();
        Self {
            driver: Driver::new(cdp.clone(), SessionId::new("synthetic-page"), queue),
            events,
            wire,
            cdp,
            incoming: Some(incoming),
            commands,
            peer,
            close_process,
        }
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
        let child = tokio::process::Command::new("/bin/sleep")
            .arg("300")
            .env_clear()
            .process_group(0)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        *self.close_process.lock().unwrap() = rustix::process::Pid::from_raw(pid.cast_signed());
        (
            self.session_with_process(dir, record, Some(crate::browser::Browser::synthetic_process(child))),
            pid,
        )
    }

    fn session_with_process(
        &mut self,
        dir: &std::path::Path,
        record: bool,
        browser: Option<crate::browser::Browser>,
    ) -> crate::session::LabSession {
        use crate::arming::ArmedBrowser;
        use crate::capture::{Capture, Recorder, RtcHooks};
        use crate::classify::Classifier;
        use crate::fence_http::HttpFence;
        use crate::fence_ws::{GuardScript, WsFence};
        use teams_fence::{FailClosed, Fence, FenceLog, MembershipSource, Roster};

        let fence = Arc::new(Fence::new(
            Roster::default(),
            FenceLog::open(&dir.join("fence.log")).unwrap(),
        ));
        let members: Arc<dyn MembershipSource + Send> = Arc::new(FailClosed);
        let http = HttpFence::new(Classifier::new(), Arc::clone(&fence), Arc::clone(&members));
        let ws = WsFence::new(fence, members, GuardScript::generate().unwrap());
        let recorder = record.then(|| Arc::new(Recorder::create(dir, "synthetic-offline").unwrap()));
        let capture = recorder
            .as_ref()
            .map(|recorder| Capture::new(Arc::clone(recorder), RtcHooks::default()));
        let incoming = self.incoming.take().unwrap();
        let armed = match browser {
            Some(browser) => ArmedBrowser::synthetic_managed(self.cdp.clone(), incoming, http, ws, capture, browser),
            None => ArmedBrowser::synthetic(self.cdp.clone(), incoming, http, ws, capture),
        };
        crate::session::LabSession::synthetic(armed, SessionId::new("synthetic-page"), recorder)
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
