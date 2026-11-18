//! A webhook's service for the tests: an HTTP endpoint on loopback that checks each request's
//! `Authorization: HMAC <base64>` against the token's key, as Microsoft's sample checks it, records the request and
//! answers as the test sets it. Everything here is synthetic.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine as _;
use serde_json::Value;
use tokio::sync::mpsc;

/// One request as the service got it.
#[derive(Debug)]
pub struct Received {
    pub activity: Value,
    pub body: Vec<u8>,
    pub authorization: Option<String>,
    /// The HMAC of the exact body bytes under the key matches the header.
    pub signed: bool,
}

/// How the service answers.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub delay: Duration,
}

impl Reply {
    pub fn json(body: &Value) -> Self {
        Self {
            status: 200,
            body: body.to_string(),
            delay: Duration::ZERO,
        }
    }
}

struct Shared {
    key: Mutex<Vec<u8>>,
    reply: Mutex<Reply>,
    sender: mpsc::UnboundedSender<Received>,
}

pub struct Receiver {
    pub url: String,
    shared: Arc<Shared>,
    received: tokio::sync::Mutex<mpsc::UnboundedReceiver<Received>>,
    task: tokio::task::JoinHandle<()>,
}

impl Receiver {
    pub async fn start() -> Self {
        let (sender, received) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            key: Mutex::new(Vec::new()),
            reply: Mutex::new(Reply::json(&serde_json::json!({"type": "message", "text": "ok"}))),
            sender,
        });
        let router = Router::new()
            .route("/hook", post(receive))
            .with_state(Arc::clone(&shared));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            url,
            shared,
            received: tokio::sync::Mutex::new(received),
            task,
        }
    }

    /// The key of the webhook's token, as its service keeps it.
    pub fn trust(&self, key: Vec<u8>) {
        *self.shared.key.lock().unwrap() = key;
    }

    pub fn answer(&self, reply: Reply) {
        *self.shared.reply.lock().unwrap() = reply;
    }

    /// The next request; panics after `wait`.
    pub async fn next_within(&self, wait: Duration) -> Received {
        let mut received = self.received.lock().await;
        tokio::time::timeout(wait, received.recv())
            .await
            .unwrap_or_else(|_| panic!("no request reached the webhook's service"))
            .unwrap()
    }

    pub async fn next(&self) -> Received {
        self.next_within(Duration::from_secs(10)).await
    }

    /// Whether a request comes within `wait`.
    pub async fn gets_within(&self, wait: Duration) -> bool {
        let mut received = self.received.lock().await;
        matches!(tokio::time::timeout(wait, received.recv()).await, Ok(Some(_)))
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// `Authorization`'s value for `body` under `key`, as Teams makes it.
pub fn hmac(key: &[u8], body: &[u8]) -> String {
    let key = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, key);
    let tag = aws_lc_rs::hmac::sign(&key, body);
    format!(
        "HMAC {}",
        base64::engine::general_purpose::STANDARD.encode(tag.as_ref())
    )
}

async fn receive(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: Bytes) -> Response {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let key = shared.key.lock().unwrap().clone();
    let signed = !key.is_empty() && authorization.as_deref() == Some(hmac(&key, &body).as_str());
    let _ = shared.sender.send(Received {
        activity: serde_json::from_slice(&body).unwrap_or(Value::Null),
        body: body.to_vec(),
        authorization,
        signed,
    });
    let reply = shared.reply.lock().unwrap().clone();
    tokio::time::sleep(reply.delay).await;
    let status = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::OK);
    (status, [("content-type", "application/json")], reply.body).into_response()
}
