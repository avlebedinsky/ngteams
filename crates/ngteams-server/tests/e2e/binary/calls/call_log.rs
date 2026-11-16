//! QA of task 288 (round 1) on the real binary with the real client: what a call leaves each party. Every way a 1:1
//! call ends gives both parties their call log entry (`48:calllogs`) and the callee who never took it "Missed call
//! from <caller>" in its activity feed, live at each of its clients on either node and after it was offline; the
//! client marks the feed and the call log read; nobody posts into the call log. An invitation whose answer is lost
//! on the way (a proxy in front of the callee's node drops it) comes again three times 2 s apart and rings once. A
//! group chat's call is refused on a server with no media node and leaves nothing.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use teams_core::activity::{ActivityFeed, ActivityItem, ActivityKind};
use teams_core::engine::{Action, Event};
use teams_core::{CallAction, CallStatus, End};
use teams_proto::Mri;
use teams_store::{Message, OutboxState};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::{self, http::HeaderValue};

use super::super::support::Node;
use super::support::{A_TONE, B_TONE, C_TONE, Phone, Rig, Turn, WAIT};
use super::{connect, hang_up, pair};
use crate::support::{PASSWORD, backends};

const CALL_LOG: &str = "48:calllogs";
/// A synthetic caller with a name no plain field expects.
const ODD_UPN: &str = "d@own.test";
const ODD_NAME: &str = "Zoë «Ångström» O'Brien-李 🛰 <b>x</b>";
/// How long an entry may take to reach a running client by itself (its push, or the catch-up the push makes due).
const LIVE: Duration = Duration::from_secs(15);

/// One call log entry as the stored message's `call-log` property tells it.
#[derive(Debug)]
struct Logged {
    call_id: String,
    direction: String,
    state: String,
    kind: String,
    originator: String,
    target: Option<String>,
    connected: bool,
    thread: Option<String>,
    sender: Option<String>,
    content: String,
}

impl Logged {
    fn of(message: &Message) -> Option<Self> {
        let properties: Value = serde_json::from_str(message.properties.as_deref()?).ok()?;
        let entry: Value = serde_json::from_str(properties["call-log"].as_str()?).ok()?;
        let text = |key: &str| entry[key].as_str().map(str::to_owned);
        Some(Self {
            call_id: text("callId")?,
            direction: text("callDirection")?,
            state: text("callState")?,
            kind: text("callType")?,
            originator: text("originator")?,
            target: text("target"),
            connected: entry["connectTime"].is_string(),
            thread: text("threadId"),
            sender: message.sender.clone(),
            content: message.content.clone(),
        })
    }

    fn says(&self, direction: &str, state: &str) -> bool {
        self.direction == direction && self.state == state
    }
}

/// The client's stored call log, the newest entry first.
async fn call_log(phone: &Phone) -> Vec<Logged> {
    let page = phone.engine().messages(CALL_LOG, None, 200).await.unwrap();
    page.messages.iter().filter_map(Logged::of).collect()
}

/// Waits (no catch-up asked, only what the client does by itself) until the stored call log holds `count` entries;
/// how long it took.
async fn log_holds(phone: &mut Phone, count: usize, within: Duration) -> (Vec<Logged>, Duration) {
    let start = Instant::now();
    loop {
        let log = call_log(phone).await;
        if log.len() >= count {
            assert_eq!(log.len(), count, "{}: more entries than calls: {log:#?}", phone.upn);
            return (log, start.elapsed());
        }
        assert!(
            start.elapsed() < within,
            "{}: the call log holds {} of {count} entries after {within:?}: {log:#?}",
            phone.upn,
            log.len()
        );
        let mark = phone.mark();
        phone.quiet(mark, Duration::from_millis(100), |_| false).await;
    }
}

/// The missed calls in `feed`, by their titles.
fn missed(feed: &ActivityFeed) -> Vec<String> {
    missed_items(feed).map(ActivityItem::title).collect()
}

fn missed_items(feed: &ActivityFeed) -> impl Iterator<Item = &ActivityItem> {
    feed.items.iter().filter(|item| item.kind == ActivityKind::MissedCall)
}

/// Waits (as [`log_holds`]) until the client's feed holds `count` missed calls.
async fn feed_holds(phone: &mut Phone, count: usize, within: Duration) -> ActivityFeed {
    let start = Instant::now();
    loop {
        let feed = phone.engine().activity().await.unwrap();
        if missed_items(&feed).count() >= count {
            assert_eq!(missed(&feed).len(), count, "{}: {:#?}", phone.upn, feed.items);
            return feed;
        }
        assert!(
            start.elapsed() < within,
            "{}: the feed has {:?} after {within:?}",
            phone.upn,
            missed(&feed)
        );
        let mark = phone.mark();
        phone.quiet(mark, Duration::from_millis(100), |_| false).await;
    }
}

fn incoming_calls(phone: &Phone, mark: usize) -> usize {
    phone.log[mark..]
        .iter()
        .filter(|event| matches!(event, Event::IncomingCall(_)))
        .count()
}

/// `placer` calls in `chat`; `rung` declines.
async fn declined(placer: &mut Phone, rung: &mut Phone, chat: &str) {
    let (from, to) = (placer.mark(), rung.mark());
    let call = placer.act(CallAction::Call { chat: chat.to_owned() }).await;
    let rings = rung.incoming(to).await;
    rung.act(CallAction::Decline { call: rings.call }).await;
    let end = placer.ended(from, call).await;
    assert!(matches!(&end, End::Remote(end) if end.code == Some(603)), "{end:?}");
}

/// `placer` calls in `chat` and hangs up while it rings at `rung`.
async fn cancelled(placer: &mut Phone, rung: &mut Phone, chat: &str) {
    let (from, to) = (placer.mark(), rung.mark());
    let call = placer.act(CallAction::Call { chat: chat.to_owned() }).await;
    let rings = rung.incoming(to).await;
    // it rings a moment, as a person's cancel comes: an end that reaches the callee's client before it sent its
    // `progress` makes the client refuse its own write and end the ring as failed
    tokio::time::sleep(Duration::from_secs(1)).await;
    placer.act(CallAction::HangUp { call }).await;
    assert_eq!(placer.ended(from, call).await, End::HungUp);
    assert_eq!(rung.ring_ended(to, rings.call).await.code, Some(487));
}

/// `placer` calls in `chat`; nobody answers until the ring runs out (or nobody can, with `rung` `None`).
async fn rings_out(placer: &mut Phone, rung: Option<&mut Phone>, chat: &str) {
    let from = placer.mark();
    let to = rung.as_ref().map(|rung| rung.mark());
    let call = placer.act(CallAction::Call { chat: chat.to_owned() }).await;
    if let (Some(rung), Some(to)) = (rung, to) {
        let rings = rung.incoming(to).await;
        let ran_out = rung.ring_ended(to, rings.call).await;
        assert_eq!(ran_out.code, Some(487), "{ran_out:?}");
    }
    let end = placer.ended(from, call).await;
    assert!(matches!(&end, End::Remote(end) if end.code == Some(480)), "{end:?}");
}

/// A post into the call log, as no client sends it.
async fn post_into_call_log(phone: &Phone) -> u16 {
    let post = crate::chatsvc::activity::RawPost {
        chat: CALL_LOG.to_owned(),
        body: json!({"messagetype": "Text", "contenttype": "text", "content": "a call that never was"}),
    };
    phone.client().write(post).await.unwrap().status()
}

/// a calls b: answered on b's first client, declined, cancelled, rung out. Each entry reaches both of b's clients by
/// itself, each on its node.
async fn while_b_runs(rig: &Rig, a: &mut Phone, b: &mut Phone, b2: &mut Phone, chat: &str) {
    let talk = connect(a, b, chat).await;
    hang_up(a, talk.call, b, talk.answered).await;
    let (_, took) = log_holds(b2, 1, LIVE).await;
    eprintln!("qa: the first entry reached b's other client (first node) in {took:?}");
    let (_, took) = log_holds(b, 1, LIVE).await;
    eprintln!("qa: the first entry reached b's client (other node) in {took:?}");
    declined(a, b, chat).await;
    cancelled(a, b, chat).await;
    rings_out(a, Some(&mut *b), chat).await;
    for phone in [b, b2] {
        let (_, took) = log_holds(phone, 4, LIVE).await;
        eprintln!("qa: {} has 4 entries {took:?} after the 4th call", phone.upn);
        let feed = feed_holds(phone, 2, LIVE).await;
        assert_eq!(missed(&feed), ["Missed call from Ada", "Missed call from Ada"]);
    }
    rig.no_open_calls().await;
}

fn states(log: &[Logged]) -> Vec<(&str, &str)> {
    log.iter()
        .map(|entry| (entry.direction.as_str(), entry.state.as_str()))
        .collect()
}

/// b's call log after its restart: the six calls, newest first, as Teams' entries tell them.
async fn b_caught_up(b: &mut Phone, a_mri: &str, d_mri: &str) {
    let (log, _) = log_holds(b, 6, LIVE).await;
    assert_eq!(
        states(&log),
        [
            ("incoming", "missed"),
            ("incoming", "missed"),
            ("incoming", "missed"),
            ("incoming", "missed"),
            ("incoming", "declined"),
            ("incoming", "accepted"),
        ],
        "b's call log, newest first: {log:#?}"
    );
    assert_eq!(log[0].originator, d_mri, "the odd caller's call is the newest");
    for entry in &log[1..] {
        assert_eq!(entry.originator, a_mri, "{entry:?}");
    }
    for entry in &log {
        assert_eq!(entry.kind, "twoParty");
        assert_eq!(entry.target.as_deref(), Some(b.mri.as_str()));
        assert_eq!(
            entry.sender.as_ref(),
            Some(&entry.originator),
            "from the caller: {entry:?}"
        );
        assert_eq!(entry.content, format!("Call Logs for Call {}", entry.call_id));
        assert_eq!(entry.connected, entry.state == "accepted", "{entry:?}");
        assert!(entry.thread.is_none(), "{entry:?}");
    }
    let ids: HashSet<&str> = log.iter().map(|entry| entry.call_id.as_str()).collect();
    assert_eq!(ids.len(), 6, "one entry a call");
}

/// b's feed after its restart: four missed calls, unread and new; none for the declined or the answered call.
async fn b_missed(b: &mut Phone, a_mri: &str) -> ActivityFeed {
    let feed = feed_holds(b, 4, LIVE).await;
    let mut titles = missed(&feed);
    titles.sort();
    let mut wanted = vec!["Missed call from Ada".to_owned(); 3];
    wanted.push(format!("Missed call from {ODD_NAME}"));
    wanted.sort();
    assert_eq!(titles, wanted, "a decline and an answered call are none");
    assert!(missed_items(&feed).all(|item| !feed.is_read(item)), "unread");
    assert_eq!(feed.unseen(), 4);
    let from_a = missed_items(&feed)
        .filter(|item| item.actor.as_deref() == Some(a_mri))
        .count();
    assert_eq!(from_a, 3);
    feed
}

/// The callers: every call is outgoing; one nobody took is missed; no missed call in their feeds.
async fn callers_logged(a: &mut Phone, d: &mut Phone) {
    let (log, _) = log_holds(a, 5, LIVE).await;
    assert_eq!(
        states(&log),
        [
            ("outgoing", "missed"),
            ("outgoing", "missed"),
            ("outgoing", "missed"),
            ("outgoing", "missed"),
            ("outgoing", "accepted"),
        ],
        "a's call log: {log:#?}"
    );
    let (log, _) = log_holds(d, 1, LIVE).await;
    assert!(log[0].says("outgoing", "missed"), "{:?}", log[0]);
    for caller in [&*a, &*d] {
        let feed = caller.engine().activity().await.unwrap();
        assert!(missed(&feed).is_empty(), "{}: {:#?}", caller.upn, feed.items);
    }
}

/// `action` written by `phone`'s client: it went out.
async fn sent(phone: &Phone, action: Action) {
    let row = phone.engine().write(action).await.unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
}

/// b marks one missed call read, opens the feed (its horizon) and moves the call log's horizon; its other client,
/// started again, has the item read too.
async fn b_reads(b: &mut Phone, b2: &mut Phone, feed: &ActivityFeed) {
    let first = missed_items(feed).next().unwrap().id.clone();
    let read = Action::ActivityRead {
        item: first.clone(),
        read: true,
    };
    sent(b, read).await;
    sent(b, Action::ActivitySeen).await;
    let newest = b.engine().messages(CALL_LOG, None, 1).await.unwrap().messages.remove(0);
    let horizon = Action::Read {
        conversation: CALL_LOG.to_owned(),
        message_id: newest.id.clone(),
    };
    sent(b, horizon).await;
    let feed = b.engine().activity().await.unwrap();
    assert_eq!(feed.unseen(), 0, "the feed was opened");
    assert!(feed.is_read(feed.item(&first).unwrap()));
    b2.restart().await;
    let feed = feed_holds(b2, 4, LIVE).await;
    assert!(feed.is_read(feed.item(&first).unwrap()), "read on b's other client");
    let read = missed_items(&feed).filter(|item| feed.is_read(item)).count();
    assert_eq!(read, 1, "only that one");
    let listed = b2.engine().conversation(CALL_LOG.to_owned()).await.unwrap();
    eprintln!("qa: b's other client lists the call log: {:?}", listed.map(|c| c.id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qa_every_way_a_1_1_call_ends_reaches_both_call_logs_and_the_missed_ones_reach_the_feed() {
    for backend in backends() {
        let Some(rig) = Rig::start(backend, "qa-call-log", 5, Turn::Coturn).await else {
            return;
        };
        rig.first
            .cli(&["user", "add", ODD_UPN, "--name", ODD_NAME], &format!("{PASSWORD}\n"));
        // a at the first node, b at the other one; b's second client at the first node
        let (mut a, mut b, chat) = pair(&rig).await;
        let mut b2 = rig.phone(&rig.first, "b@own.test", "Bert", B_TONE).await;
        b2.sync().await;
        let (a_mri, b_mri) = (a.mri.clone(), b.mri.clone());
        while_b_runs(&rig, &mut a, &mut b, &mut b2, &chat).await;

        // b goes offline: a's call and the odd caller's ring out with no calling client of b's
        b.close().await;
        b2.close().await;
        let mut d = rig.phone(&rig.first, ODD_UPN, ODD_NAME, C_TONE).await;
        let d_chat = d.chat_with(&b_mri).await;
        rings_out(&mut a, None, &chat).await;
        rings_out(&mut d, None, &d_chat).await;
        rig.no_open_calls().await;

        // b's client starts again and catches up
        b.restart().await;
        b_caught_up(&mut b, &a_mri, &d.mri.clone()).await;
        let feed = b_missed(&mut b, &a_mri).await;
        callers_logged(&mut a, &mut d).await;
        b_reads(&mut b, &mut b2, &feed).await;

        // nobody but the server writes into the call log
        assert_eq!(post_into_call_log(&b).await, 403);
        assert_eq!(post_into_call_log(&a).await, 403);
        let (log, _) = log_holds(&mut b, 6, Duration::from_secs(1)).await;
        assert!(log.iter().all(|entry| entry.content.starts_with("Call Logs for Call ")));

        for phone in [&mut a, &mut b, &mut b2, &mut d] {
            phone.close().await;
        }
        rig.stop().await;
    }
}

/// How long a missed call may take to show in a running client's feed: Teams shows it as it rings out.
const AT_ONCE: Duration = Duration::from_secs(5);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "QA 288 Q1: a client whose feed is empty shows its first missed call only after the app's 30 s catch-up"]
async fn qa_the_first_missed_call_shows_at_once_in_a_client_that_runs_as_the_app() {
    let Some(rig) = Rig::start(crate::support::Backend::Sqlite, "qa-first-missed", 5, Turn::Coturn).await else {
        return;
    };
    let mut a = rig.phone(&rig.first, "a@own.test", "Ada", A_TONE).await;
    let mut b = Phone::signed_in(&rig.first, &rig.origin, ("b@own.test", "Bert"), B_TONE).await;
    b.catch_up_as_the_app();
    b.start();
    b.online(0).await;
    let chat = a.chat_with(&b.mri).await;
    b.sync().await;
    assert!(
        b.engine().activity().await.unwrap().items.is_empty(),
        "a new account's feed is empty"
    );
    cancelled(&mut a, &mut b, &chat).await;
    let rang_out = Instant::now();
    let feed = feed_holds(&mut b, 1, Duration::from_secs(45)).await;
    let feed_took = rang_out.elapsed();
    let (_, log_took) = log_holds(&mut b, 1, Duration::from_secs(45)).await;
    eprintln!("qa: the first missed call showed {feed_took:?} after the ring ended (the call log {log_took:?} later)");
    assert_eq!(missed(&feed), ["Missed call from Ada"]);
    // the second one comes by its push alone
    cancelled(&mut a, &mut b, &chat).await;
    let rang_out = Instant::now();
    feed_holds(&mut b, 2, Duration::from_secs(45)).await;
    eprintln!(
        "qa: the second missed call showed {:?} after the ring ended",
        rang_out.elapsed()
    );
    a.close().await;
    b.close().await;
    rig.stop().await;
    assert!(feed_took <= AT_ONCE, "the first missed call showed after {feed_took:?}");
}

/// What the proxy saw of one socket's invitations.
#[derive(Default)]
struct Seen {
    /// When each invitation delivery (an `evt` 107 or 109 at the endpoint's root) went to the client.
    invitations: Vec<Instant>,
    /// The deliveries whose answers are dropped, by id.
    lost: HashSet<u64>,
    /// How many answers to invitations it still drops.
    to_lose: usize,
    /// The answers it dropped.
    dropped: usize,
}

/// A loopback proxy in front of a node's push sockets: it passes every frame both ways but drops the client's
/// answer to the next `to_lose` invitations, as a lossy network would.
struct Proxy {
    url: String,
    seen: Arc<Mutex<Seen>>,
    task: tokio::task::JoinHandle<()>,
}

impl Proxy {
    async fn start(node: &Node) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let upstream = node.endpoint.replacen("http://", "ws://", 1);
        let seen = Arc::new(Mutex::new(Seen::default()));
        let shared = Arc::clone(&seen);
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                tokio::spawn(relay(client, upstream.clone(), Arc::clone(&shared)));
            }
        });
        Self { url, seen, task }
    }

    fn lose(&self, answers: usize) {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        *seen = Seen {
            to_lose: answers,
            ..Seen::default()
        };
    }

    fn invitations(&self) -> Vec<Instant> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .invitations
            .clone()
    }

    fn dropped(&self) -> usize {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner).dropped
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A delivery frame's id when it is an invitation.
fn invitation(frame: &str) -> Option<u64> {
    let delivery: Value = serde_json::from_str(frame.strip_prefix("3:::")?).ok()?;
    let url = delivery["url"].as_str()?;
    let after = url.split("/v4/f/").nth(1)?;
    let at_root = after.find('/').is_some_and(|slash| slash + 1 == after.len());
    let body: Value = serde_json::from_str(delivery["body"].as_str()?).ok()?;
    let evt = body["evt"].as_u64()?;
    (at_root && (evt == 107 || evt == 109))
        .then(|| delivery["id"].as_u64())
        .flatten()
}

/// An answer frame's id.
fn answer(frame: &str) -> Option<u64> {
    let answer: Value = serde_json::from_str(frame.strip_prefix("3:::")?).ok()?;
    answer.get("status")?;
    answer["id"].as_u64()
}

#[expect(
    clippy::result_large_err,
    reason = "the handshake callback's error type is tungstenite's"
)]
async fn relay(client: TcpStream, upstream: String, seen: Arc<Mutex<Seen>>) {
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
    let mut request = format!("{upstream}{path}").into_client_request().unwrap();
    if let Some(origin) = origin {
        request.headers_mut().insert("origin", origin);
    }
    let Ok((server, _)) = tokio_tungstenite::connect_async(request).await else {
        return;
    };
    let (mut to_client, mut from_client) = downstream.split();
    let (mut to_server, mut from_server) = server.split();
    let down_seen = Arc::clone(&seen);
    let down = async move {
        while let Some(Ok(frame)) = from_server.next().await {
            if let tungstenite::Message::Text(text) = &frame
                && let Some(id) = invitation(text.as_str())
            {
                let mut seen = down_seen.lock().unwrap_or_else(PoisonError::into_inner);
                seen.invitations.push(Instant::now());
                if seen.to_lose > 0 {
                    seen.to_lose -= 1;
                    seen.lost.insert(id);
                }
            }
            if to_client.send(frame).await.is_err() {
                break;
            }
        }
        let _ = to_client.close().await;
    };
    let up = async move {
        while let Some(Ok(frame)) = from_client.next().await {
            if let tungstenite::Message::Text(text) = &frame
                && let Some(id) = answer(text.as_str())
            {
                let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
                if seen.lost.remove(&id) {
                    seen.dropped += 1;
                    continue;
                }
            }
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

/// The gaps between consecutive instants.
fn gaps(times: &[Instant]) -> Vec<Duration> {
    times.windows(2).map(|pair| pair[1] - pair[0]).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qa_an_invitation_whose_answer_is_lost_comes_again_and_still_rings_once() {
    for backend in backends() {
        let Some(rig) = Rig::start(backend, "qa-redelivery", 12, Turn::Coturn).await else {
            return;
        };
        let mut a = rig.phone(&rig.first, "a@own.test", "Ada", A_TONE).await;
        // b's client at the other node, its sockets through the proxy
        let proxy = Proxy::start(rig.other()).await;
        let mut b = Phone::signed_in(rig.other(), &rig.origin, ("b@own.test", "Bert"), B_TONE).await;
        b.connect_through(&proxy.url);
        b.start();
        b.online(0).await;
        let chat = a.chat_with(&b.mri).await;
        b.sync().await;

        // every answer lost: the first delivery and three more, 2 s apart; one ring until it runs out
        proxy.lose(usize::MAX);
        let (from, to) = (a.mark(), b.mark());
        let call = a.act(CallAction::Call { chat: chat.clone() }).await;
        a.status(from, call, &CallStatus::Ringing).await;
        let rings = b.incoming(to).await;
        let ran_out = b.ring_ended(to, rings.call).await;
        assert_eq!(ran_out.code, Some(487), "{ran_out:?}");
        let end = a.ended(from, call).await;
        assert!(matches!(&end, End::Remote(end) if end.code == Some(480)), "{end:?}");
        let sent = proxy.invitations();
        eprintln!(
            "qa {backend:?}: {} deliveries, gaps {:?}, {} answers dropped",
            sent.len(),
            gaps(&sent),
            proxy.dropped()
        );
        assert_eq!(sent.len(), 4, "the invitation and three redeliveries");
        assert!(
            gaps(&sent)
                .iter()
                .all(|gap| *gap >= Duration::from_millis(1800) && *gap <= Duration::from_secs(4)),
            "2 s apart: {:?}",
            gaps(&sent)
        );
        assert_eq!(incoming_calls(&b, to), 1, "one ring: {:?}", b.log[to..].len());
        let (log, _) = log_holds(&mut b, 1, LIVE).await;
        assert!(log[0].says("incoming", "missed"), "{:?}", log[0]);
        let feed = feed_holds(&mut b, 1, LIVE).await;
        assert_eq!(missed(&feed), ["Missed call from Ada"], "one missed call");
        rig.no_open_calls().await;

        // the first answer lost: one redelivery, whose answer stops it; b answers the one ring
        proxy.lose(1);
        let (from, to) = (a.mark(), b.mark());
        let call = a.act(CallAction::Call { chat: chat.clone() }).await;
        let rings = b.incoming(to).await;
        let deadline = Instant::now() + WAIT;
        while proxy.invitations().len() < 2 {
            assert!(Instant::now() < deadline, "no redelivery");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        let sent = proxy.invitations();
        eprintln!(
            "qa {backend:?}: first answer lost: {} deliveries, gaps {:?}",
            sent.len(),
            gaps(&sent)
        );
        assert_eq!(sent.len(), 2, "the redelivery's answer stops it");
        b.quiet(to, Duration::from_millis(500), |_| false).await;
        assert_eq!(incoming_calls(&b, to), 1, "one ring");
        let taken = b.act(CallAction::Answer { call: rings.call }).await;
        b.status(to, taken, &CallStatus::Connected).await;
        a.status(from, call, &CallStatus::Connected).await;
        hang_up(&mut b, taken, &mut a, call).await;
        assert_eq!(incoming_calls(&b, to), 1, "one ring");
        let (log, _) = log_holds(&mut b, 2, LIVE).await;
        assert!(log[0].says("incoming", "accepted"), "{:?}", log[0]);
        let feed = b.engine().activity().await.unwrap();
        assert_eq!(missed(&feed).len(), 1, "an answered call is no missed call");
        rig.no_open_calls().await;

        a.close().await;
        b.close().await;
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qa_a_group_chats_call_on_a_server_with_no_media_node_is_refused_and_leaves_nothing() {
    let Some(rig) = Rig::start(crate::support::Backend::Sqlite, "qa-group", 5, Turn::Coturn).await else {
        return;
    };
    let mut a = rig.phone(&rig.first, "a@own.test", "Ada", A_TONE).await;
    let mut b = rig.phone(&rig.first, "b@own.test", "Bert", B_TONE).await;
    let c = Phone::signed_in(&rig.first, &rig.origin, ("c@own.test", "Cora"), C_TONE).await;
    let create = Action::CreateChat {
        others: vec![Mri::new(b.mri.clone()), Mri::new(c.mri.clone())],
        topic: Some("Night «shift» 🌙 & <ops>".to_owned()),
    };
    let row = a.engine().write(create).await.unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    let group = row.message_id.unwrap();
    b.sync().await;
    let (from, to) = (a.mark(), b.mark());
    let pressed = a.press(CallAction::Call { chat: group.clone() }).await;
    let end = match pressed {
        Ok(call) => Ok(a.ended(from, call).await),
        Err(error) => Err(error),
    };
    eprintln!("qa: a group chat's call with no media node: {end:?}");
    assert!(matches!(end, Ok(End::Failed(_)) | Err(_)), "{end:?}");
    assert!(
        b.quiet(to, Duration::from_secs(3), |event| matches!(
            event,
            Event::IncomingCall(_)
        ))
        .await,
        "nobody rings"
    );
    rig.no_open_calls().await;
    for phone in [&a, &b] {
        assert!(call_log(phone).await.is_empty(), "{}: no entry", phone.upn);
        assert!(missed(&phone.engine().activity().await.unwrap()).is_empty());
    }
    a.close().await;
    b.close().await;
    rig.stop().await;
}

/// Where [`qa_seed_synthetic_calls_for_the_ui`] copies the database it filled.
const SEED_DIR: &str = "NGTS_QA_SEED_DIR";

/// Synthetic callers with names no plain field expects.
const CALLERS: [(&str, &str); 8] = [
    ("d@own.test", ODD_NAME),
    ("e@own.test", "Émile Ünal-Øster"),
    ("f@own.test", "فاطمة الزهراء"),
    (
        "g@own.test",
        "A Very Long Synthetic Display Name That Keeps Going Past Any Column Width",
    ),
    ("h@own.test", "o'neil \"the caller\" & co"),
    ("i@own.test", "山田 太郎"),
    ("j@own.test", "Ǆemal Ŀuka 🧪🧪🧪"),
    ("k@own.test", "x"),
];

/// Not a check: with [`SEED_DIR`] set, fills a one-node SQLite deployment with synthetic callers and their calls to b
/// (one answered, one declined, three missed calls from each caller) and copies its database there
/// (`server.sqlite`), for the UI sandbox's shots of b's feed and chats. Skipped otherwise.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qa_seed_synthetic_calls_for_the_ui() {
    let Some(dir) = std::env::var_os(SEED_DIR) else {
        eprintln!("skipped: {SEED_DIR} names no directory");
        return;
    };
    let Some(mut rig) = Rig::start(crate::support::Backend::Sqlite, "qa-seed", 2, Turn::Coturn).await else {
        return;
    };
    for (upn, name) in CALLERS {
        rig.first
            .cli(&["user", "add", upn, "--name", name], &format!("{PASSWORD}\n"));
    }
    let (mut a, mut b, chat) = pair(&rig).await;
    let talk = connect(&mut a, &mut b, &chat).await;
    hang_up(&mut a, talk.call, &mut b, talk.answered).await;
    declined(&mut a, &mut b, &chat).await;
    let b_mri = b.mri.clone();
    b.close().await;
    a.close().await;
    for (upn, name) in CALLERS {
        let mut caller = rig.phone(&rig.first, upn, name, C_TONE).await;
        let chat = caller.chat_with(&b_mri).await;
        for _ in 0..3 {
            let from = caller.mark();
            let call = caller.act(CallAction::Call { chat: chat.clone() }).await;
            caller.status(from, call, &CallStatus::Ringing).await;
            caller.act(CallAction::HangUp { call }).await;
            assert_eq!(caller.ended(from, call).await, End::HungUp);
        }
        caller.close().await;
    }
    rig.no_open_calls().await;
    rig.stop_node(true).await;
    let database = rig.database.trim_start_matches("sqlite://").to_owned();
    let dir = std::path::PathBuf::from(dir);
    for suffix in ["", "-wal", "-shm"] {
        let from = format!("{database}{suffix}");
        if std::path::Path::new(&from).exists() {
            std::fs::copy(&from, dir.join(format!("server.sqlite{suffix}"))).unwrap();
        }
    }
    eprintln!("qa: seeded {} calls into {}", 2 + 3 * CALLERS.len(), dir.display());
    rig.stop().await;
}
