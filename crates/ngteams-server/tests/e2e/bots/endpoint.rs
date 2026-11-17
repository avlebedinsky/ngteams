//! A bot for the tests: an HTTP endpoint on loopback that records each activity the server sends it (with its
//! `Authorization`), and the bot's side of the Bot Connector API, as an SDK calls it. Everything here is synthetic.

use std::sync::Arc;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use reqwest::Method;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};

use super::claims;
use crate::support::TestServer;

/// The public origin the bot tests' server names; the test bot reaches it at the server's loopback origin.
pub const ORIGIN: &str = "https://own.example.test";

/// One activity as the bot got it.
#[derive(Debug)]
pub struct Received {
    pub activity: Value,
    pub authorization: Option<String>,
}

impl Received {
    pub fn kind(&self) -> &str {
        self.activity["type"].as_str().unwrap_or_default()
    }

    /// The claims of the server's token, unverified; none for an unsigned bot.
    pub fn token_claims(&self) -> Option<Value> {
        let token = self.authorization.as_deref()?.strip_prefix("Bearer ")?;
        Some(claims(token))
    }
}

struct Shared {
    sender: mpsc::UnboundedSender<Received>,
    status: AtomicU16,
    hits: AtomicUsize,
}

/// A bot endpoint on loopback.
pub struct TestBot {
    pub url: String,
    shared: Arc<Shared>,
    received: Mutex<mpsc::UnboundedReceiver<Received>>,
    task: tokio::task::JoinHandle<()>,
}

impl TestBot {
    /// A bot that answers each activity 200.
    pub async fn start() -> Self {
        Self::answering(200).await
    }

    /// A bot that answers each activity with `status`.
    pub async fn answering(status: u16) -> Self {
        let (sender, received) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            sender,
            status: AtomicU16::new(status),
            hits: AtomicUsize::new(0),
        });
        let router = Router::new()
            .route("/api/messages", post(receive))
            .with_state(Arc::clone(&shared));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/messages", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            url,
            shared,
            received: Mutex::new(received),
            task,
        }
    }

    /// How many requests reached it.
    pub fn hits(&self) -> usize {
        self.shared.hits.load(Ordering::SeqCst)
    }

    pub fn answer(&self, status: u16) {
        self.shared.status.store(status, Ordering::SeqCst);
    }

    /// The next activity of type `kind`, skipping others; panics after `wait`.
    pub async fn next_of_within(&self, kind: &str, wait: Duration) -> Received {
        let mut received = self.received.lock().await;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let next = tokio::time::timeout_at(deadline, received.recv())
                .await
                .unwrap_or_else(|_| panic!("no {kind} activity reached the bot"))
                .unwrap();
            if next.kind() == kind {
                return next;
            }
        }
    }

    pub async fn next_of(&self, kind: &str) -> Received {
        self.next_of_within(kind, Duration::from_secs(10)).await
    }

    /// Whether an activity of `kind` comes within `wait`.
    pub async fn gets_within(&self, kind: &str, wait: Duration) -> bool {
        let mut received = self.received.lock().await;
        let deadline = tokio::time::Instant::now() + wait;
        while let Ok(Some(next)) = tokio::time::timeout_at(deadline, received.recv()).await {
            if next.kind() == kind {
                return true;
            }
        }
        false
    }
}

impl Drop for TestBot {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn receive(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: String) -> StatusCode {
    shared.hits.fetch_add(1, Ordering::SeqCst);
    let activity = serde_json::from_str(&body).unwrap_or(Value::Null);
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let _ = shared.sender.send(Received {
        activity,
        authorization,
    });
    StatusCode::from_u16(shared.status.load(Ordering::SeqCst)).unwrap_or(StatusCode::OK)
}

/// The bot's side of the Bot Connector API: the `serviceUrl` it was given (reached at the server's loopback origin)
/// and its own token, when it is signed.
pub struct Connector {
    pub base: String,
    pub token: Option<String>,
}

impl Connector {
    /// A signed bot: its token from the client-credentials grant, as MSAL asks for it.
    pub async fn signed(server: &TestServer, service_url: &str, bot: &str, secret: &str) -> Self {
        let form = [
            ("grant_type", "client_credentials"),
            ("client_id", bot),
            ("client_secret", secret),
            ("scope", "https://api.botframework.com/.default"),
        ];
        let answer: Value = reqwest::Client::new()
            .post(server.url("/botframework.com/oauth2/v2.0/token"))
            .form(&form)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let token = answer["access_token"].as_str().unwrap().to_owned();
        Self {
            base: reached(server, service_url),
            token: Some(token),
        }
    }

    /// An unsigned bot: its own `serviceUrl` is its credential.
    pub fn unsigned(server: &TestServer, service_url: &str) -> Self {
        Self {
            base: reached(server, service_url),
            token: None,
        }
    }

    /// A call under the service URL; its status and JSON (null without one).
    pub async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = reqwest::Client::new().request(method, format!("{}{path}", self.base));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Sends `activity` to the conversation; the status and answer.
    pub async fn send(&self, conversation: &str, activity: Value) -> (u16, Value) {
        let path = format!("v3/conversations/{}/activities", encode(conversation));
        self.call(Method::POST, &path, Some(activity)).await
    }

    /// Replies to `reply_to` with `activity`.
    pub async fn reply(&self, conversation: &str, reply_to: &str, activity: Value) -> (u16, Value) {
        let path = format!("v3/conversations/{}/activities/{reply_to}", encode(conversation));
        self.call(Method::POST, &path, Some(activity)).await
    }

    pub async fn update(&self, conversation: &str, id: &str, activity: Value) -> (u16, Value) {
        let path = format!("v3/conversations/{}/activities/{id}", encode(conversation));
        self.call(Method::PUT, &path, Some(activity)).await
    }

    pub async fn delete(&self, conversation: &str, id: &str) -> (u16, Value) {
        let path = format!("v3/conversations/{}/activities/{id}", encode(conversation));
        self.call(Method::DELETE, &path, None).await
    }

    /// The text message an SDK's `sendActivity("…")` makes.
    pub fn text(text: &str) -> Value {
        json!({"type": "message", "text": text})
    }
}

/// `service_url` at the server's loopback origin.
fn reached(server: &TestServer, service_url: &str) -> String {
    service_url.replacen(ORIGIN, &server.origin, 1)
}

/// A conversation id as an SDK puts it in a path.
pub fn encode(conversation: &str) -> String {
    url::form_urlencoded::byte_serialize(conversation.as_bytes()).collect()
}
