//! An own-server engine shut down (the account signed out or removed, the window closed) while its first write waits
//! for a token the server holds back: a TCP proxy in front of the server keeps every new connection unanswered. The
//! engine ends without a panic, it leaves no socket open, and nothing goes out once its shutdown is over.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use teams_core::engine::Action;
use teams_proto::Mri;
use teams_store::OutboxState;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, watch};

use crate::first_write::{ADA, BOB, OwnServer, in_child, run_in_child};

/// Longer than the token client's own 30 s timeout, with room for the engine to stop after it.
const WITHIN: Duration = Duration::from_secs(90);

#[test]
fn a_write_held_at_its_token_never_goes_out_once_the_engine_is_shut_down() {
    if in_child() {
        held_and_shut_down(false);
        return;
    }
    run_in_child("first_write_cancel::a_write_held_at_its_token_never_goes_out_once_the_engine_is_shut_down");
}

#[test]
fn a_write_held_at_its_token_and_let_go_after_a_shutdown_leaves_no_socket() {
    if in_child() {
        held_and_shut_down(true);
        return;
    }
    run_in_child("first_write_cancel::a_write_held_at_its_token_and_let_go_after_a_shutdown_leaves_no_socket");
}

/// Ada's first write held at its token, then her engine shut down; with `let_go` the held connections go on to the
/// server two seconds after the shutdown was asked for.
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn held_and_shut_down(let_go: bool) {
    let proxy = Proxy::bind().await;
    let server = OwnServer::start(Some(proxy.port)).await;
    proxy.forward_to(server.port);
    let (ada, region) = server.mri_and_region(ADA).await;
    let (bob, _) = server.mri_and_region(BOB).await;

    let secrets = server.signed_in(ADA).await;
    proxy.hold();
    let forwarded = proxy.forwarded();
    let started = server.start_engine(ADA, &ada, region, secrets);
    let requests = started.handle.requests();
    let writing = tokio::spawn(async move {
        requests
            .write(Action::CreateChat {
                others: vec![Mri::new(&bob)],
                topic: None,
            })
            .await
    });
    proxy.wait_held(1).await;

    let asked = Instant::now();
    let handle = started.handle;
    let stopping = tokio::task::spawn_blocking(move || handle.shutdown());
    if let_go {
        tokio::time::sleep(Duration::from_secs(2)).await;
        proxy.let_go();
    }
    tokio::time::timeout(WITHIN, stopping)
        .await
        .expect("the engine stopped")
        .expect("no panic on the engine's shutdown");
    let stopped = asked.elapsed();
    let written = tokio::time::timeout(Duration::from_secs(5), writing)
        .await
        .expect("the write was answered")
        .expect("no panic in the write's caller");
    println!("qa: shutdown took {stopped:?}; the write: {written:?}");

    let accepted = proxy.accepted();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(proxy.accepted(), accepted, "a connection opened after the shutdown");
    assert_eq!(
        proxy.engines_open(),
        0,
        "an engine's socket left open after the shutdown"
    );
    if !let_go {
        assert_eq!(proxy.forwarded(), forwarded, "a held connection reached the server");
        let row = written.expect("the write's row");
        assert_eq!(row.state, OutboxState::Failed, "a write with no token");
    }
    server.stop().await;
}

/// A loopback TCP proxy that holds new connections, unanswered, while told to, and counts the client sockets opened
/// since it first held (the engine's: the test's own clients connected before) and still open.
struct Proxy {
    port: u16,
    shared: Arc<Shared>,
}

struct Shared {
    upstream: Mutex<Option<u16>>,
    /// Whether new connections are held; set back to `false`, the held ones go on to the server.
    holding: watch::Sender<bool>,
    /// Set once it first holds: the connections from then on are the engine's.
    engines: AtomicBool,
    held: AtomicUsize,
    changed: Notify,
    accepted: AtomicUsize,
    forwarded: AtomicUsize,
    engines_open: AtomicUsize,
}

impl Proxy {
    async fn bind() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = Arc::new(Shared {
            upstream: Mutex::new(None),
            holding: watch::channel(false).0,
            engines: AtomicBool::new(false),
            held: AtomicUsize::new(0),
            changed: Notify::new(),
            accepted: AtomicUsize::new(0),
            forwarded: AtomicUsize::new(0),
            engines_open: AtomicUsize::new(0),
        });
        let accepting = Arc::clone(&shared);
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                accepting.accepted.fetch_add(1, Ordering::SeqCst);
                let engines = accepting.engines.load(Ordering::SeqCst);
                if engines {
                    accepting.engines_open.fetch_add(1, Ordering::SeqCst);
                }
                tokio::spawn(Arc::clone(&accepting).serve(client, engines));
            }
        });
        Self { port, shared }
    }

    fn forward_to(&self, port: u16) {
        *self.shared.upstream.lock().unwrap_or_else(PoisonError::into_inner) = Some(port);
    }

    fn hold(&self) {
        self.shared.engines.store(true, Ordering::SeqCst);
        self.shared.holding.send_replace(true);
    }

    /// The held connections go on to the server, and new ones are no longer held.
    fn let_go(&self) {
        self.shared.holding.send_replace(false);
    }

    async fn wait_held(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let changed = self.shared.changed.notified();
                if self.shared.held.load(Ordering::SeqCst) >= count {
                    return;
                }
                changed.await;
            }
        })
        .await
        .expect("the engine opened a connection for its token");
    }

    fn accepted(&self) -> usize {
        self.shared.accepted.load(Ordering::SeqCst)
    }

    fn forwarded(&self) -> usize {
        self.shared.forwarded.load(Ordering::SeqCst)
    }

    /// The engine's sockets not yet closed by the engine.
    fn engines_open(&self) -> usize {
        self.shared.engines_open.load(Ordering::SeqCst)
    }
}

impl Shared {
    /// One client's connection: held while the proxy holds (what it sends kept), then forwarded; done once the client
    /// closes its end.
    async fn serve(self: Arc<Self>, mut client: TcpStream, engines: bool) {
        let mut sent = Vec::new();
        let mut holding = self.holding.subscribe();
        if *holding.borrow_and_update() {
            self.held.fetch_add(1, Ordering::SeqCst);
            self.changed.notify_waiters();
            let mut chunk = [0_u8; 8192];
            loop {
                tokio::select! {
                    read = client.read(&mut chunk) => match read {
                        Ok(0) | Err(_) => {
                            self.closed(engines);
                            return;
                        }
                        Ok(n) => sent.extend_from_slice(&chunk[..n]),
                    },
                    changed = holding.changed() => {
                        if changed.is_err() || !*holding.borrow_and_update() {
                            break;
                        }
                    }
                }
            }
        }
        let port = self.upstream.lock().unwrap_or_else(PoisonError::into_inner).unwrap();
        if let Ok(mut server) = TcpStream::connect(("127.0.0.1", port)).await {
            self.forwarded.fetch_add(1, Ordering::SeqCst);
            if server.write_all(&sent).await.is_ok() {
                let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
            }
        }
        // the server's end closing first leaves the client's open until the client closes it
        let mut rest = Vec::new();
        let _ = client.read_to_end(&mut rest).await;
        self.closed(engines);
    }

    fn closed(&self, engines: bool) {
        if engines {
            self.engines_open.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
