//! A loopback proxy in front of a server's Trouter sockets that can hold what the server sends to the client, as a
//! slow network would, while the client's own frames pass.

use std::sync::{Arc, Mutex, PoisonError};

use futures_util::{SinkExt as _, StreamExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub struct HeldSocket {
    /// The proxy's own origin (`ws://…`), the client's mock origin.
    pub origin: String,
    held: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl HeldSocket {
    /// A proxy in front of `upstream`'s sockets (an `http://` origin).
    pub async fn start(upstream: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("ws://{}/", listener.local_addr().unwrap());
        let upstream = upstream.replacen("http://", "ws://", 1);
        let (held, _) = watch::channel(false);
        let watched = held.clone();
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                tokio::spawn(relay(client, upstream.clone(), watched.subscribe()));
            }
        });
        Self { origin, held, task }
    }

    /// From now on the server's frames wait in the proxy.
    pub fn hold(&self) {
        self.held.send_replace(true);
    }

    /// The frames held go to the client, in order, and the next ones pass.
    pub fn release(&self) {
        self.held.send_replace(false);
    }
}

impl Drop for HeldSocket {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[expect(
    clippy::result_large_err,
    reason = "the handshake callback's error type is tungstenite's"
)]
async fn relay(client: TcpStream, upstream: String, mut held: watch::Receiver<bool>) {
    let asked = Arc::new(Mutex::new(None::<(String, Option<HeaderValue>)>));
    let keep = Arc::clone(&asked);
    let callback = move |request: &Request, response: Response| {
        let origin = request.headers().get("origin").cloned();
        *keep.lock().unwrap_or_else(PoisonError::into_inner) = Some((request.uri().to_string(), origin));
        Ok(response)
    };
    let Ok(downstream) = tokio_tungstenite::accept_hdr_async(client, callback).await else {
        return;
    };
    let Some((path, origin)) = asked.lock().unwrap_or_else(PoisonError::into_inner).take() else {
        return;
    };
    let mut request = format!("{}{path}", upstream.trim_end_matches('/'))
        .into_client_request()
        .unwrap();
    if let Some(origin) = origin {
        request.headers_mut().insert("origin", origin);
    }
    let Ok((server, _)) = tokio_tungstenite::connect_async(request).await else {
        return;
    };
    let (mut to_client, mut from_client) = downstream.split();
    let (mut to_server, mut from_server) = server.split();
    let down = async move {
        let mut waiting = Vec::new();
        loop {
            tokio::select! {
                frame = from_server.next() => {
                    let Some(Ok(frame)) = frame else { break };
                    if *held.borrow() {
                        waiting.push(frame);
                    } else if to_client.send(frame).await.is_err() {
                        break;
                    }
                }
                changed = held.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    if !*held.borrow() {
                        for frame in waiting.drain(..) {
                            if to_client.send(frame).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        }
        let _ = to_client.close().await;
    };
    let up = async move {
        while let Some(Ok(frame)) = from_client.next().await {
            if to_server.send(frame).await.is_err() {
                break;
            }
        }
        let _ = to_server.close().await;
    };
    tokio::select! {
        () = down => {}
        () = up => {}
    }
}
