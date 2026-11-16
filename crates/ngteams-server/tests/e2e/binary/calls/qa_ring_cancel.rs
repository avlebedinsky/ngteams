//! QA of task 371 (round 1) on the real binary with the real client: a caller who cancels at any moment of the ring
//! (when the caller's own client says it rings, the moment the callee's client rings, and a short time after) leaves
//! the callee a cancelled ring (487), never a failed call, and each such call is a missed call: in the callee's call
//! log, its activity feed and its chat. A ring whose `progress` the service really refuses (a proxy in front of the
//! callee's node answers it 500) still ends as a failed call.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse as _, Response};
use teams_core::engine::Event;
use teams_core::{CallAction, CallStatus, End};
use tokio::net::TcpListener;

use super::call_log::{feed_holds, log_holds, missed, states};
use super::pair;
use super::support::{Line, Phone, Rig, Turn};
use crate::support::backends;

/// When the caller cancels.
#[derive(Clone, Copy, Debug)]
enum Moment {
    /// As soon as the caller's own client says the call rings: the invitation may not have reached the callee yet.
    CallerRings,
    /// As soon as the callee's client rings: its `attach` and `progress` are on their way.
    CalleeRings,
    /// This long after the callee's client rang.
    After(Duration),
}

const MOMENTS: [Moment; 6] = [
    Moment::CallerRings,
    Moment::CalleeRings,
    Moment::After(Duration::from_millis(30)),
    Moment::After(Duration::from_millis(100)),
    Moment::After(Duration::from_millis(300)),
    Moment::After(Duration::from_millis(1000)),
];
/// How many times each moment is tried on each backend.
const ROUNDS: usize = 3;
/// How long the callee's client is given to ring after the caller cancelled at [`Moment::CallerRings`].
const LATE_RING: Duration = Duration::from_secs(3);
/// How long an entry may take to reach a running client by itself.
const LIVE: Duration = Duration::from_secs(20);

fn failed(event: &Event) -> bool {
    matches!(
        event,
        Event::Call {
            status: CallStatus::Ended(End::Failed(_)),
            ..
        }
    )
}

/// a calls b in `chat` and hangs up at `moment`; a's call ends as hung up. Where b's log stood before the call.
async fn cancel_at(a: &mut Phone, b: &mut Phone, chat: &str, moment: Moment) -> usize {
    let (from, to) = (a.mark(), b.mark());
    let call = a.act(CallAction::Call { chat: chat.to_owned() }).await;
    match moment {
        Moment::CallerRings => a.status(from, call, &CallStatus::Ringing).await,
        Moment::CalleeRings => {
            b.incoming(to).await;
        }
        Moment::After(wait) => {
            b.incoming(to).await;
            tokio::time::sleep(wait).await;
        }
    }
    a.act(CallAction::HangUp { call }).await;
    assert_eq!(a.ended(from, call).await, End::HungUp, "{moment:?}");
    if matches!(moment, Moment::CallerRings) {
        b.quiet(to, LATE_RING, |_| false).await;
    }
    to
}

/// Each ring b's client had since `to` ends as cancelled (487), and no call ends as failed there. Whether b's client
/// rang.
async fn rings_cancelled(b: &mut Phone, to: usize, moment: Moment) -> bool {
    let rings: Vec<_> = b.log[to..]
        .iter()
        .filter_map(|event| match event {
            Event::IncomingCall(incoming) => Some(incoming.call),
            _ => None,
        })
        .collect();
    for ring in &rings {
        let end = b.ring_ended(to, *ring).await;
        assert_eq!(end.code, Some(487), "{moment:?}: b's ring ended as {end:?}");
    }
    let quiet = b.quiet(to, Duration::from_millis(300), failed).await;
    assert!(
        quiet,
        "{moment:?}: a call failed at b: {:?}",
        b.log[to..].iter().filter(|event| failed(event)).collect::<Vec<_>>()
    );
    !rings.is_empty()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qa_a_cancel_at_any_moment_of_the_ring_is_a_missed_call_never_a_failed_one() {
    for backend in backends() {
        let Some(rig) = Rig::start(backend, "qa-ring-cancel", 30, Turn::Coturn).await else {
            return;
        };
        let (mut a, mut b, chat) = pair(&rig).await;
        let caller = a.mri.clone();
        let mut calls = 0;
        let mut rang = 0;
        let tries = (0..ROUNDS).flat_map(|round| MOMENTS.map(|moment| (round, moment)));
        for (round, moment) in tries {
            calls += 1;
            let to = cancel_at(&mut a, &mut b, &chat, moment).await;
            rang += usize::from(rings_cancelled(&mut b, to, moment).await);
            eprintln!("qa: {backend:?} round {round} {moment:?}: cancelled, nothing failed at b");
        }
        rig.no_open_calls().await;
        eprintln!("qa: {backend:?}: {calls} calls cancelled, {rang} rang at b");
        let (log, _) = log_holds(&mut b, calls, LIVE).await;
        assert!(
            states(&log).iter().all(|state| *state == ("incoming", "missed")),
            "b's call log: {log:#?}"
        );
        let (log, _) = log_holds(&mut a, calls, LIVE).await;
        assert!(
            states(&log).iter().all(|state| *state == ("outgoing", "missed")),
            "a's call log: {log:#?}"
        );
        let feed = feed_holds(&mut b, calls, LIVE).await;
        assert!(
            missed(&feed).iter().all(|title| title == "Missed call from Ada"),
            "{:?}",
            missed(&feed)
        );
        let lines = b.stored_lines(&chat).await;
        let missed_lines = lines.iter().filter(|line| line.missed_from(&caller)).count();
        let started = lines.iter().filter(|line| **line == Line::Started).count();
        assert_eq!((started, missed_lines), (calls, calls), "b's chat lines: {lines:?}");
        a.close().await;
        b.close().await;
        rig.stop().await;
    }
}

/// A proxy in front of `node` (its `http://` endpoint) that answers each `progress` with 500 and passes the rest;
/// its `http://` address and the count of `progress` writes it refused.
async fn progress_refusing_proxy(node: &str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let refused = Arc::new(AtomicUsize::new(0));
    let (node, counted) = (node.to_owned(), Arc::clone(&refused));
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let app = axum::Router::new().fallback(move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
        let (node, counted, http) = (node.clone(), Arc::clone(&counted), http.clone());
        async move {
            if uri.path().ends_with("/progress") {
                counted.fetch_add(1, Ordering::SeqCst);
                return (StatusCode::INTERNAL_SERVER_ERROR, "{}").into_response();
            }
            let target = format!("{node}{}", uri.path_and_query().map_or("/", |path| path.as_str()));
            let mut sent = http.request(method, target).body(body);
            for (name, value) in &headers {
                if name != "host" && name != "content-length" {
                    sent = sent.header(name, value);
                }
            }
            let Ok(answer) = sent.send().await else {
                return StatusCode::BAD_GATEWAY.into_response();
            };
            let status = answer.status();
            let mut copied = HeaderMap::new();
            for (name, value) in answer.headers() {
                if name != "content-length" && name != "transfer-encoding" && name != "connection" {
                    copied.append(name, value.clone());
                }
            }
            let body = answer.bytes().await.unwrap_or_default();
            let mut response = Response::new(axum::body::Body::from(body));
            *response.status_mut() = status;
            *response.headers_mut() = copied;
            response
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (address, refused)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qa_a_live_ring_whose_progress_the_service_refuses_still_ends_as_a_failed_call() {
    for backend in backends() {
        let Some(rig) = Rig::start(backend, "qa-progress-refused", 30, Turn::Coturn).await else {
            return;
        };
        let (mut a, mut b, chat) = pair(&rig).await;
        let (proxy, refused) = progress_refusing_proxy(&rig.other().endpoint).await;
        b.http_through(&proxy);
        b.restart().await;
        let (from, to) = (a.mark(), b.mark());
        let call = a.act(CallAction::Call { chat: chat.clone() }).await;
        let rings = b.incoming(to).await;
        let at = b
            .since(to, "b's call ends", |event| {
                matches!(event, Event::Call { call, status: CallStatus::Ended(_) } if *call == rings.call)
                    || matches!(event, Event::RingEnded { call, .. } if *call == rings.call)
            })
            .await;
        assert!(
            matches!(
                &b.log[at],
                Event::Call {
                    status: CallStatus::Ended(End::Failed("progress")),
                    ..
                }
            ),
            "b's ring ended as {:?}",
            b.log[at]
        );
        assert!(refused.load(Ordering::SeqCst) >= 1, "the proxy refused no progress");
        a.act(CallAction::HangUp { call }).await;
        assert_eq!(a.ended(from, call).await, End::HungUp);
        eprintln!(
            "qa: {backend:?}: b's progress refused {} time(s), b's ring ended as {:?}",
            refused.load(Ordering::SeqCst),
            b.log[at]
        );
        a.close().await;
        b.close().await;
        rig.stop().await;
    }
}
