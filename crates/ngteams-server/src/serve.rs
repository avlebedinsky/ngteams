//! The listener: plain HTTP (for a reverse proxy in front, or tests on loopback) or TLS with rustls.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::Request;
use axum::http::{HeaderValue, Version, header};
use axum::middleware::Next;
use axum::response::Response;
use axum_server::Handle;
use axum_server::tls_rustls::RustlsConfig;
use futures_util::StreamExt as _;
use hyper::body::{Frame, SizeHint};
use rustls::pki_types::pem::PemObject as _;

use crate::config::TlsConfig;

/// How long open connections get to finish once shutdown starts.
const GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("can't listen: {0}")]
    Listen(#[source] std::io::Error),
    #[error("can't load the TLS certificate or key: {0}")]
    Tls(#[source] std::io::Error),
}

/// A bound listener, ready to serve.
#[derive(Debug)]
pub struct Listener {
    listener: std::net::TcpListener,
}

impl Listener {
    /// Binds `address`.
    ///
    /// # Errors
    /// The address can't be bound.
    pub fn bind(address: SocketAddr) -> Result<Self, ServeError> {
        let listener = std::net::TcpListener::bind(address).map_err(ServeError::Listen)?;
        listener.set_nonblocking(true).map_err(ServeError::Listen)?;
        Ok(Self { listener })
    }

    /// The address it is bound to (the port, when bound to port 0).
    ///
    /// # Errors
    /// The socket can't say.
    pub fn local_addr(&self) -> Result<SocketAddr, ServeError> {
        self.listener.local_addr().map_err(ServeError::Listen)
    }

    /// Serves `router` until `shutdown` resolves, then lets open connections finish for a while.
    ///
    /// # Errors
    /// The TLS files can't be read, or the listener fails.
    pub async fn serve(
        self,
        router: Router,
        tls: Option<TlsConfig>,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), ServeError> {
        let handle: Handle<SocketAddr> = Handle::new();
        let watcher = handle.clone();
        tokio::spawn(async move {
            shutdown.await;
            watcher.graceful_shutdown(Some(GRACE));
        });
        let service = router.into_make_service_with_connect_info::<SocketAddr>();
        match tls {
            Some(tls) => {
                let config = Arc::new(tls_config(&tls).await?);
                axum_server::from_tcp_rustls(self.listener, RustlsConfig::from_config(config))
                    .map_err(ServeError::Listen)?
                    .handle(handle)
                    .serve(service)
                    .await
            }
            None => {
                axum_server::from_tcp(self.listener)
                    .map_err(ServeError::Listen)?
                    .handle(handle)
                    .serve(service)
                    .await
            }
        }
        .map_err(ServeError::Listen)
    }
}

/// The most of a request body the answer left unread that a node reads and drops, so that the connection can carry
/// the next request.
const DRAIN_MAX: u64 = 16 * 1024 * 1024;
/// How long a node reads such a body at most.
const DRAIN_TIME: Duration = Duration::from_secs(5);

/// Reads to its end an HTTP/1 request body that the answer left unread (a body over its limit, a request refused
/// before its body was read), before the answer is sent. If the rest is too big or too slow, the answer closes the
/// connection. A client that keeps such a connection loses its next request when the server closes it, and a
/// server that closes it at once breaks the client's write of the body before the client reads the answer.
pub(crate) async fn drain_unread_body(request: Request, next: Next) -> Response {
    if request.version() > Version::HTTP_11 || request.body().is_end_stream() {
        return next.run(request).await;
    }
    let left = Arc::new(Mutex::new(None));
    let watched = Arc::clone(&left);
    let request = request.map(|body| {
        Body::new(Watched {
            body: Some(body),
            left: watched,
        })
    });
    let mut response = next.run(request).await;
    let unread = left.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(body) = unread
        && !drained(body).await
    {
        response
            .headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    response
}

/// Whether `body` was read to its end within [`DRAIN_MAX`] and [`DRAIN_TIME`]. What it read is dropped.
async fn drained(body: Body) -> bool {
    if body.size_hint().lower() > DRAIN_MAX {
        return false;
    }
    let mut chunks = body.into_data_stream();
    let mut read: u64 = 0;
    let to_end = async {
        while let Some(chunk) = chunks.next().await {
            let Ok(chunk) = chunk else { return false };
            read = read.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if read > DRAIN_MAX {
                return false;
            }
        }
        true
    };
    tokio::time::timeout(DRAIN_TIME, to_end).await.unwrap_or(false)
}

/// A request body that, when it is dropped before its end, leaves the rest in `left`.
struct Watched {
    body: Option<Body>,
    left: Arc<Mutex<Option<Body>>>,
}

impl HttpBody for Watched {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let Some(body) = self.body.as_mut() else {
            return Poll::Ready(None);
        };
        let polled = Pin::new(body).poll_frame(cx);
        if matches!(polled, Poll::Ready(None)) {
            self.body = None;
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.body.as_ref().is_none_or(HttpBody::is_end_stream)
    }

    fn size_hint(&self) -> SizeHint {
        self.body
            .as_ref()
            .map_or_else(|| SizeHint::with_exact(0), HttpBody::size_hint)
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        if let Some(body) = self.body.take().filter(|body| !body.is_end_stream()) {
            *self.left.lock().unwrap_or_else(PoisonError::into_inner) = Some(body);
        }
    }
}

/// The rustls server configuration of a PEM chain and key, on the aws-lc-rs provider, HTTP/2 and HTTP/1.1.
async fn tls_config(tls: &TlsConfig) -> Result<rustls::ServerConfig, ServeError> {
    let chain = tokio::fs::read(&tls.cert).await.map_err(ServeError::Tls)?;
    let key = tokio::fs::read(&tls.key).await.map_err(ServeError::Tls)?;
    let invalid = |what: &str| ServeError::Tls(std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_owned()));
    let chain = rustls::pki_types::CertificateDer::pem_slice_iter(&chain)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid("unreadable certificate"))?;
    let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(&key).map_err(|_| invalid("unreadable key"))?;
    let mut config =
        rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| invalid("no TLS versions"))?
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|_| invalid("the key doesn't match the certificate"))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}
