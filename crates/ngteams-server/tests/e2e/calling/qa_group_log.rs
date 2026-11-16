//! QA of task 288 (round 1): a group call's call log and missed calls, on SQLite and on Postgres + Redis, with the
//! fixed media node 186's tests install (the binary has none: a group start is refused there). The client's engine
//! (`teams-core`) reads each party's call log and feed as it syncs. A callee who declined and was added back later
//! came into the call; members with no calling client missed it.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use teams_call::{CallState, End};
use teams_core::Outbox;
use teams_core::activity::{ActivityFeed, ActivityKind};
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_proto::{Author, Client, Mri};
use teams_store::Store;
use tempfile::TempDir;

use super::group_signaling::Group;
use crate::support::{TestServer, backends};
use crate::trouter::Account;

const CALL_LOG: &str = "48:calllogs";
const WAIT: Duration = Duration::from_secs(20);

/// An account's engine with its push socket, as the client runs it (no calls of its own).
struct Reader {
    handle: EngineHandle,
    _store: TempDir,
}

impl Reader {
    async fn start(server: &TestServer, upn: &str, id: &str) -> Self {
        let account = Account::sign_in(server, upn, id).await;
        let store = tempfile::tempdir().unwrap();
        let transport = Arc::new(account.transport(server));
        let handle = engine::start(Parts {
            store: Store::open(&store.path().join("store.sqlite")).unwrap(),
            client: Arc::new(Client::new(account.transport(server))),
            outbox: Outbox::new(transport, Author::new(Mri::new(format!("8:orgid:{id}")), upn)),
            calls: None,
            push: Some(Push::new(account.trouter_on(server, server))),
            options: EngineOptions {
                tick: Duration::from_millis(20),
                catch_up_retry: Duration::from_secs(2),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        Self { handle, _store: store }
    }

    /// The stored call log entries, newest first, once there are `count` (no catch-up asked).
    async fn log(&self, count: usize) -> Vec<Value> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let page = self.handle.messages(CALL_LOG, None, 100).await.unwrap();
            let entries: Vec<Value> = page
                .messages
                .iter()
                .filter_map(|message| {
                    let properties: Value = serde_json::from_str(message.properties.as_deref()?).ok()?;
                    serde_json::from_str(properties["call-log"].as_str()?).ok()
                })
                .collect();
            if entries.len() >= count {
                assert_eq!(entries.len(), count, "{entries:#?}");
                return entries;
            }
            assert!(tokio::time::Instant::now() < deadline, "the call log: {entries:#?}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn feed(&self) -> ActivityFeed {
        self.handle.activity().await.unwrap()
    }
}

fn missed(feed: &ActivityFeed) -> Vec<String> {
    feed.items
        .iter()
        .filter(|item| item.kind == ActivityKind::MissedCall)
        .map(teams_core::activity::ActivityItem::title)
        .collect()
}

fn says(entry: &Value, direction: &str, state: &str) -> bool {
    entry["callDirection"] == direction && entry["callState"] == state
}

/// Ada calls the group; Bert answers; Cora declines, Bert adds her back and she answers; the members with no client
/// ring until their rings run out; everyone hangs up.
async fn the_call(group: &mut Group) {
    let c = format!("8:orgid:{}", group.c.id);
    group.a.client.call_group(&group.chat, &group.others);
    let call = group.a.client.state(CallState::Connected).await;
    let rings = group.b.client.incoming().await;
    group.b.client.answer(rings.call);
    group.b.client.state(CallState::Connected).await;
    let declined = group.c.client.incoming().await;
    group.c.client.decline(declined.call);
    assert_eq!(group.c.client.ended().await, End::Declined);
    group.b.client.add(rings.call, &c, &group.chat);
    let again = group.c.client.incoming().await;
    group.c.client.answer(again.call);
    group.c.client.state(CallState::Connected).await;
    tokio::time::sleep(Duration::from_secs(7)).await;
    group.c.client.hang_up(again.call);
    group.c.client.ended().await;
    group.b.client.hang_up(rings.call);
    group.b.client.ended().await;
    group.a.client.hang_up(call);
    group.a.client.ended().await;
}

/// The caller's entry: outgoing, accepted, the group chat's, naming every party it rang; its call id.
async fn placed(ada: &Reader, group: &Group, rung: &[String]) -> Value {
    let placed = ada.log(1).await.remove(0);
    eprintln!("qa: Ada's entry {placed:#}");
    assert!(says(&placed, "outgoing", "accepted"), "{placed:#}");
    assert_eq!(placed["callType"], "multiParty");
    assert_eq!(placed["originator"], format!("8:orgid:{}", group.a.id));
    assert_eq!(
        placed["threadId"],
        group.chat.as_str(),
        "the entry names the group chat"
    );
    assert!(placed["target"].is_null(), "{placed:#}");
    let named: Vec<&str> = placed["participants"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for party in rung {
        assert!(named.contains(&party.as_str()), "{party} in {named:?}");
    }
    assert!(missed(&ada.feed().await).is_empty(), "the caller missed nothing");
    placed["callId"].clone()
}

/// The member with no client: a missed entry of the group chat and an unread missed call.
async fn sam_missed_it(sam: &Reader, group: &Group) {
    let entry = sam.log(1).await.remove(0);
    assert!(
        says(&entry, "incoming", "missed") && entry["connectTime"].is_null(),
        "{entry:#}"
    );
    assert_eq!(entry["threadId"], group.chat.as_str());
    let deadline = tokio::time::Instant::now() + WAIT;
    let feed = loop {
        let feed = sam.feed().await;
        if !missed(&feed).is_empty() || tokio::time::Instant::now() > deadline {
            break feed;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(missed(&feed), ["Missed call from Ada"], "{:#?}", feed.items);
    let item = feed
        .items
        .iter()
        .find(|item| item.kind == ActivityKind::MissedCall)
        .unwrap();
    assert!(!feed.is_read(item), "unread");
    eprintln!(
        "qa: Sam's missed call: thread {:?}, topic {:?}",
        item.thread, item.topic
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn qa_a_group_call_logs_each_party_and_its_chat_and_only_the_members_who_never_came_in_missed_it() {
    for backend in backends() {
        let mut group = Group::start_with(backend, |config| config.calling.ring_timeout = 6).await;
        let server = &group.server;
        // Silent Sam: a member with no calling client, made as `silent-<tag>@own.test`
        let tag = group
            .a
            .upn
            .trim_start_matches("ada-")
            .trim_end_matches("@own.test")
            .to_owned();
        let silent_mri = group.others[3].clone();
        let silent_id = silent_mri.trim_start_matches("8:orgid:").to_owned();
        let ada = Reader::start(server, &group.a.upn, &group.a.id).await;
        let bert = Reader::start(server, &group.b.upn, &group.b.id).await;
        let cora = Reader::start(server, &group.c.upn, &group.c.id).await;
        let sam = Reader::start(server, &format!("silent-{tag}@own.test"), &silent_id).await;
        the_call(&mut group).await;
        let rung = [
            format!("8:orgid:{}", group.b.id),
            format!("8:orgid:{}", group.c.id),
            silent_mri,
        ];
        let call_id = placed(&ada, &group, &rung).await;
        for (reader, who) in [(&bert, "Bert"), (&cora, "Cora")] {
            let entry = reader.log(1).await.remove(0);
            assert!(
                says(&entry, "incoming", "accepted") && entry["connectTime"].is_string(),
                "{who} came into the call: {entry:#}"
            );
            assert_eq!(entry["callId"], call_id);
            assert_eq!(entry["threadId"], group.chat.as_str());
            assert!(missed(&reader.feed().await).is_empty(), "{who} missed nothing");
        }
        sam_missed_it(&sam, &group).await;
        eprintln!("qa {backend:?}: the group call's log checked");
        for reader in [ada, bert, cora, sam] {
            reader.handle.shutdown();
        }
        group.server.stop().await;
    }
}
