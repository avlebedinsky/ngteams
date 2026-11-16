//! What a call leaves behind for each of its parties, read by the client's engine (`teams-core`) as it syncs and is
//! pushed the account's own streams: an entry in the call log (`48:calllogs`, `docs/server.md`, "The call log") for
//! every party of a 1:1 or group call, and a missed call's item in the activity feed (`48:notifications`). The same
//! with the reader on another node than the call.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use teams_call::{CallState, End};
use teams_core::Outbox;
use teams_core::activity::{ActivityFeed, ActivityItem, ActivityKind};
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_proto::{Author, Client, Mri};
use teams_store::{Message, Store};
use tempfile::TempDir;

use super::Pair;
use super::group_signaling::Group;
use crate::support::{Backend, TestServer, backends, redis_url};
use crate::trouter::Account;

const CALL_LOG: &str = "48:calllogs";
const WAIT: Duration = Duration::from_secs(20);

fn mri(id: &str) -> String {
    format!("8:orgid:{id}")
}

/// An account's engine, its sockets on one node and its reads through another.
struct Reader {
    handle: EngineHandle,
    _store: TempDir,
}

impl Reader {
    async fn start(socket: &TestServer, http: &TestServer, upn: &str, id: &str) -> Self {
        let account = Account::sign_in(http, upn, id).await;
        let store = tempfile::tempdir().unwrap();
        let transport = Arc::new(account.transport(http));
        let handle = engine::start(Parts {
            store: Store::open(&store.path().join("store.sqlite")).unwrap(),
            client: Arc::new(Client::new(account.transport(http))),
            outbox: Outbox::new(transport, Author::new(Mri::new(mri(id)), upn)),
            calls: None,
            push: Some(Push::new(account.trouter_on(socket, http))),
            options: EngineOptions {
                tick: Duration::from_millis(20),
                // a delivery for a stream the engine doesn't list yet makes a catch-up due: sooner than 30 s here
                catch_up_retry: Duration::from_millis(300),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        Self { handle, _store: store }
    }

    /// The stored call log, the newest entry first.
    async fn stored_log(&self) -> Vec<Logged> {
        let page = self.handle.messages(CALL_LOG, None, 100).await.unwrap();
        page.messages.iter().filter_map(Logged::of).collect()
    }

    /// The call log once `wanted` takes it, catching up (`sync`) between looks when `catch_up`, else only as pushed.
    async fn log_until(&self, catch_up: bool, wanted: impl Fn(&[Logged]) -> bool) -> Vec<Logged> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if catch_up {
                assert!(self.handle.sync().await.unwrap().failed.is_empty());
            }
            let log = self.stored_log().await;
            if wanted(&log) {
                return log;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the call log never came: {log:#?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The activity feed once `wanted` takes it, as [`Self::log_until`].
    async fn feed_until(&self, catch_up: bool, wanted: impl Fn(&ActivityFeed) -> bool) -> ActivityFeed {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if catch_up {
                assert!(self.handle.sync().await.unwrap().failed.is_empty());
            }
            let feed = self.handle.activity().await.unwrap();
            if wanted(&feed) {
                return feed;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the feed never had it: {:#?}",
                feed.items
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn stop(self) {
        self.handle.shutdown();
    }
}

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
    participants: Vec<String>,
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
            participants: entry["participants"]
                .as_array()
                .map(|all| all.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default(),
            thread: text("threadId"),
            sender: message.sender.clone(),
            content: message.content.clone(),
        })
    }

    fn says(&self, direction: &str, state: &str) -> bool {
        self.direction == direction && self.state == state
    }
}

/// The missed calls in `feed`, by who placed them.
fn missed_from(feed: &ActivityFeed) -> Vec<String> {
    feed.items
        .iter()
        .filter(|item| item.kind == ActivityKind::MissedCall)
        .map(ActivityItem::title)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_party_of_a_1_1_call_finds_it_in_its_call_log() {
    for backend in backends() {
        let mut pair = Pair::start(backend).await;
        let (a, b) = (mri(&pair.a_id), mri(&pair.b_id));
        let ada = Reader::start(&pair.server, &pair.server, "a@own.test", &pair.a_id).await;
        let bert = Reader::start(&pair.server, &pair.server, "b@own.test", &pair.b_id).await;
        // answered, then hung up
        pair.a.call(&b);
        pair.a.state(CallState::Ringing).await;
        let rings = pair.b.incoming().await;
        pair.b.answer(rings.call);
        pair.a.state(CallState::Connected).await;
        pair.b.hang_up(rings.call);
        pair.a.ended().await;
        let log = ada.log_until(true, |log| log.len() == 1).await;
        let placed = &log[0];
        assert!(placed.says("outgoing", "accepted"), "{placed:?}");
        assert_eq!(
            (placed.kind.as_str(), placed.originator.as_str()),
            ("twoParty", a.as_str())
        );
        assert_eq!(placed.target.as_deref(), Some(b.as_str()));
        assert!(placed.connected && placed.thread.is_none(), "{placed:?}");
        assert_eq!(
            placed.sender.as_deref(),
            Some(a.as_str()),
            "the caller's, as Teams sends it"
        );
        assert_eq!(placed.content, format!("Call Logs for Call {}", placed.call_id));
        let log = bert.log_until(true, |log| log.len() == 1).await;
        assert!(log[0].says("incoming", "accepted"), "{:?}", log[0]);
        assert_eq!(log[0].call_id, placed.call_id, "one call, one id");
        // declined
        pair.a.call(&b);
        pair.a.state(CallState::Ringing).await;
        let rings = pair.b.incoming().await;
        pair.b.decline(rings.call);
        pair.a.ended().await;
        let log = bert.log_until(true, |log| log.len() == 2).await;
        assert!(log[0].says("incoming", "declined") && !log[0].connected, "{:?}", log[0]);
        // the caller of a call nobody took has it as missed, as Teams' logs show it
        let log = ada.log_until(true, |log| log.len() == 2).await;
        assert!(log[0].says("outgoing", "missed"), "{:?}", log[0]);
        // cancelled while it rang
        pair.a.call(&b);
        let call = pair.a.state(CallState::Ringing).await;
        pair.b.incoming().await;
        pair.a.hang_up(call);
        pair.b.ring_ended().await;
        let log = bert.log_until(true, |log| log.len() == 3).await;
        assert!(log[0].says("incoming", "missed"), "{:?}", log[0]);
        // the call log is the account's own stream: it is listed and its horizon moves
        let listed = bert.handle.conversation(CALL_LOG.to_owned()).await.unwrap();
        assert!(listed.is_some(), "the call log is in the conversation list");
        let account = Account::sign_in(&pair.server, "b@own.test", &pair.b_id).await;
        let newest = bert
            .handle
            .messages(CALL_LOG, None, 1)
            .await
            .unwrap()
            .messages
            .remove(0);
        let horizon = teams_proto::ReadReceipt::new(CALL_LOG, &newest.id, &newest.id);
        let put = account.client(&pair.server).write(horizon).await.unwrap();
        assert!(put.is_success(), "{}", put.status());
        // nobody but the server writes there
        let account = Account::sign_in(&pair.server, "a@own.test", &pair.a_id).await;
        let post = crate::chatsvc::activity::RawPost {
            chat: CALL_LOG.to_owned(),
            body: serde_json::json!({"messagetype": "Text", "contenttype": "text", "content": "hello"}),
        };
        let refused = account.client(&pair.server).write(post).await.unwrap();
        assert_eq!(refused.status(), 403, "a post into the call log");
        ada.stop();
        bert.stop();
        pair.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_party_of_a_group_call_finds_it_in_its_call_log() {
    for backend in [Backend::Sqlite] {
        let mut group = Group::start(backend).await;
        let server = &group.server;
        let ada = Reader::start(server, server, &group.a.upn, &group.a.id).await;
        let bert = Reader::start(server, server, &group.b.upn, &group.b.id).await;
        let cora = Reader::start(server, server, &group.c.upn, &group.c.id).await;
        group.a.client.call_group(&group.chat, &group.others);
        let call = group.a.client.state(CallState::Connected).await;
        let rings = group.b.client.incoming().await;
        group.b.client.answer(rings.call);
        group.b.client.state(CallState::Connected).await;
        let declined = group.c.client.incoming().await;
        group.c.client.decline(declined.call);
        // her decline is the service's before the call ends, else she missed it
        assert_eq!(group.c.client.ended().await, End::Declined);
        group.b.client.hang_up(rings.call);
        group.b.client.ended().await;
        group.a.client.hang_up(call);
        group.a.client.ended().await;
        let (a, b, c) = (mri(&group.a.id), mri(&group.b.id), mri(&group.c.id));
        let log = ada.log_until(true, |log| log.len() == 1).await;
        let placed = &log[0];
        assert!(placed.says("outgoing", "accepted") && placed.connected, "{placed:?}");
        assert_eq!(
            (placed.kind.as_str(), placed.originator.as_str()),
            ("multiParty", a.as_str())
        );
        assert_eq!(placed.thread.as_deref(), Some(group.chat.as_str()));
        for party in [&a, &b, &c] {
            assert!(placed.participants.contains(party), "{party} in {placed:?}");
        }
        let log = bert.log_until(true, |log| log.len() == 1).await;
        assert!(log[0].says("incoming", "accepted"), "{:?}", log[0]);
        let log = cora.log_until(true, |log| log.len() == 1).await;
        assert!(log[0].says("incoming", "declined"), "{:?}", log[0]);
        assert_eq!(log[0].call_id, placed.call_id);
        for reader in [ada, bert, cora] {
            reader.stop();
        }
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_the_callee_never_took_is_a_missed_call_in_its_activity_feed() {
    for backend in backends() {
        let mut pair = Pair::start(backend).await;
        let b = mri(&pair.b_id);
        let ada = Reader::start(&pair.server, &pair.server, "a@own.test", &pair.a_id).await;
        let bert = Reader::start(&pair.server, &pair.server, "b@own.test", &pair.b_id).await;
        // answered: no missed call
        pair.a.call(&b);
        pair.a.state(CallState::Ringing).await;
        let rings = pair.b.incoming().await;
        pair.b.answer(rings.call);
        pair.a.state(CallState::Connected).await;
        pair.a.hang_up(rings.call);
        pair.b.ended().await;
        // declined: none either
        pair.a.call(&b);
        pair.a.state(CallState::Ringing).await;
        let rings = pair.b.incoming().await;
        pair.b.decline(rings.call);
        pair.a.ended().await;
        // cancelled while it rang: Bert missed it
        pair.a.call(&b);
        let call = pair.a.state(CallState::Ringing).await;
        pair.b.incoming().await;
        pair.a.hang_up(call);
        pair.b.ring_ended().await;
        bert.log_until(true, |log| log.len() == 3).await;
        let feed = bert.feed_until(true, |feed| !missed_from(feed).is_empty()).await;
        assert_eq!(missed_from(&feed), ["Missed call from Ada"], "{:#?}", feed.items);
        let item = feed
            .items
            .iter()
            .find(|item| item.kind == ActivityKind::MissedCall)
            .unwrap();
        assert_eq!(item.actor.as_deref(), Some(mri(&pair.a_id).as_str()));
        assert!(!feed.is_read(item), "a new missed call is unread");
        assert_eq!(feed.unseen(), 1);
        // the caller missed nothing
        ada.log_until(true, |log| log.len() == 3).await;
        let feed = ada.handle.activity().await.unwrap();
        assert!(missed_from(&feed).is_empty(), "{:#?}", feed.items);
        ada.stop();
        bert.stop();
        pair.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_member_a_group_call_rang_in_vain_has_it_as_a_missed_call() {
    let mut group = Group::start_with(Backend::Sqlite, |config| config.calling.ring_timeout = 4).await;
    let server = &group.server;
    let bert = Reader::start(server, server, &group.b.upn, &group.b.id).await;
    let cora = Reader::start(server, server, &group.c.upn, &group.c.id).await;
    group.a.client.call_group(&group.chat, &group.others);
    let call = group.a.client.state(CallState::Connected).await;
    let rings = group.b.client.incoming().await;
    group.b.client.answer(rings.call);
    group.b.client.state(CallState::Connected).await;
    // Cora's ring runs out
    group.c.client.incoming().await;
    group.c.client.ring_ended().await;
    group.a.client.hang_up(call);
    group.b.client.hang_up(rings.call);
    group.b.client.ended().await;
    let feed = cora.feed_until(true, |feed| !missed_from(feed).is_empty()).await;
    assert_eq!(missed_from(&feed), ["Missed call from Ada"]);
    let log = cora.log_until(true, |log| log.len() == 1).await;
    assert!(log[0].says("incoming", "missed"), "{:?}", log[0]);
    bert.log_until(true, |log| log.len() == 1).await;
    let feed = bert.handle.activity().await.unwrap();
    assert!(missed_from(&feed).is_empty(), "Bert answered: {:#?}", feed.items);
    bert.stop();
    cora.stop();
    group.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_caller_of_a_group_call_nobody_took_has_it_as_missed() {
    let mut group = Group::start_with(Backend::Sqlite, |config| config.calling.ring_timeout = 4).await;
    let server = &group.server;
    let ada = Reader::start(server, server, &group.a.upn, &group.a.id).await;
    group.a.client.call_group(&group.chat, &group.others);
    let call = group.a.client.state(CallState::Connected).await;
    // both rings run out; the caller stays in the call alone, then hangs up
    group.b.client.incoming().await;
    group.c.client.incoming().await;
    group.b.client.ring_ended().await;
    group.c.client.ring_ended().await;
    group.a.client.hang_up(call);
    group.a.client.ended().await;
    let log = ada.log_until(true, |log| log.len() == 1).await;
    assert!(log[0].says("outgoing", "missed"), "{:?}", log[0]);
    assert!(!log[0].connected, "nobody came in: no connect time, {:?}", log[0]);
    assert_eq!(log[0].kind, "multiParty");
    ada.stop();
    group.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_call_log_and_a_missed_call_reach_an_account_whose_socket_is_on_another_node() {
    let Some(redis) = redis_url() else {
        return;
    };
    for backend in backends() {
        let url = redis.clone();
        let node_a = crate::trouter::server_with(backend, |config| config.redis_url = Some(url)).await;
        let (shared, url) = (node_a.app.config.clone(), redis.clone());
        let node_b = crate::trouter::server_with(backend, |config| {
            config.database_url = shared.database_url;
            config.redis_url = Some(url);
        })
        .await;
        // names of this test's own: with Redis, sign-ins are counted per name across every test running now
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let (a_upn, b_upn) = (format!("a-{tag}@own.test"), format!("b-{tag}@own.test"));
        let a_id = node_a.add_user(&a_upn, "Ada").await;
        let b_id = node_a.add_user(&b_upn, "Bert").await;
        let mut a = super::client(&node_a, &node_a, &a_upn, &a_id, "Ada").await;
        let mut b = super::client(&node_a, &node_a, &b_upn, &b_id, "Bert").await;
        // Bert's engine: its push socket on node B, its reads through node B; the call is all node A's
        let bert = Reader::start(&node_b, &node_b, &b_upn, &b_id).await;
        assert!(bert.handle.sync().await.unwrap().failed.is_empty());
        // the first entry of each stream: its delivery has the engine catch up the stream it doesn't know yet; the
        // second comes by its delivery alone
        for missed in 1..=2 {
            a.call(&mri(&b_id));
            let call = a.state(CallState::Ringing).await;
            b.incoming().await;
            a.hang_up(call);
            b.ring_ended().await;
            let log = bert.log_until(false, |log| log.len() == missed).await;
            assert!(log[0].says("incoming", "missed"), "{:?}", log[0]);
            let feed = bert.feed_until(false, |feed| missed_from(feed).len() == missed).await;
            assert!(missed_from(&feed).iter().all(|title| title == "Missed call from Ada"));
        }
        bert.stop();
        drop((a, b));
        node_b.stop().await;
        node_a.stop().await;
    }
}
