//! What a replay must neither lose nor invent: no loss is reported to a socket that followed its account, one is
//! reported for an event the journal couldn't keep, cursors are kept only for registered endpoints and only so many
//! of them, and a replay to a slow reader ends with its token.

use std::time::Duration;

use bytes::Bytes;
use ngteams_server::fanout::journal::{CURSORS, EntryId};
use ngteams_server::trouter::{Control, Stack, endpoint_id, push_url, tell};
use serde_json::{Value, json};

use super::refusals::{EPID, IC3, Raw, query, register, registration, token};
use super::{ORIGIN, server, server_with};
use crate::support::{Backend, TestServer, backends, redis_url};

const PING: &str = r#"5:1+::{"name":"ping"}"#;
const PONG: &str = r#"6:::1+["pong"]"#;

fn now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap()
}

fn message(content: &str) -> Bytes {
    let event = json!({"resourceType": "NewMessage", "resource": {
        "id": "1", "version": "1", "messagetype": "Text", "content": content, "conversationid": "19:x@thread.v2",
    }});
    Bytes::from(event.to_string())
}

/// Registers the messaging endpoint of `epid`, which needs no socket: its push URL is derived.
async fn registered(server: &TestServer, id: &str, ic3: &str, epid: &str) -> String {
    let endpoint = endpoint_id(id, epid, Stack::Messaging);
    let body = registration("TeamsCDLWebWorker", epid, &push_url(ORIGIN, &endpoint), 3600);
    assert_eq!(register(server, Some(ic3), &body).await, 202);
    endpoint
}

/// Whether the frame is `trouter.message_loss` for the messaging tag.
fn is_loss(frame: &str) -> bool {
    let Some((_, event)) = frame.split_once("::") else {
        return false;
    };
    serde_json::from_str::<Value>(event).is_ok_and(|event| {
        event["name"] == "trouter.message_loss" && event["args"][0]["droppedIndicators"][0]["tag"] == "messaging"
    })
}

async fn kept_cursor(server: &TestServer, topic: &str, endpoint: &str) -> Option<EntryId> {
    server.app.journal.cursor(topic, endpoint).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connected_endpoint_is_told_of_no_loss_after_its_account_was_quiet_for_longer_than_the_journal_keeps() {
    let Some(redis) = redis_url() else { return };
    for backend in backends() {
        let url = redis.clone();
        let server = server_with(backend, |config| config.redis_url = Some(url)).await;
        let id = server.add_user("a@own.test", "Ada").await;
        let ic3 = token(&server, "a@own.test", IC3).await;
        let topic = format!("push:8:orgid:{id}");
        // as if the endpoint had been given the account's last event an hour ago
        let hour_ago = EntryId {
            ms: now_ms() - 3_600_000,
            seq: 0,
        };
        let client = redis::Client::open(redis.as_str()).unwrap();
        let mut commands = client.get_multiplexed_async_connection().await.unwrap();
        let journal = format!("journal:{topic}");
        redis::cmd("XADD")
            .arg(&journal)
            .arg(hour_ago.to_string())
            .arg("p")
            .arg(message("an hour ago").as_ref())
            .query_async::<String>(&mut commands)
            .await
            .unwrap();
        let endpoint = registered(&server, &id, &ic3, EPID).await;
        server
            .app
            .journal
            .set_cursor(&topic, &endpoint, hour_ago)
            .await
            .unwrap();
        let mut socket = Raw::open(&server, &query(EPID, "TeamsCDL")).await;
        socket.connect(&ic3).await;
        socket.send(PING).await;
        assert_eq!(socket.next().await.as_deref(), Some(PONG), "nothing was missed");
        // the journal ran out meanwhile, as it does a quarter of an hour after the account's last event
        redis::cmd("DEL")
            .arg(&journal)
            .query_async::<i64>(&mut commands)
            .await
            .unwrap();
        server
            .app
            .fanout
            .publish(&topic, message("after the quiet"))
            .await
            .unwrap();
        let live = socket.next().await.unwrap();
        assert!(
            live.starts_with("3:::") && live.contains("after the quiet"),
            "{}",
            &live[..40]
        );
        assert!(!live.contains("mstore"));
        assert!(socket.next_within(Duration::from_millis(300)).await.is_err());
        assert!(kept_cursor(&server, &topic, &endpoint).await.unwrap() > hour_ago);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_the_journal_could_not_keep_is_reported_as_a_loss_to_a_registered_endpoint() {
    for backend in backends() {
        let server = server(backend).await;
        let id = server.add_user("a@own.test", "Ada").await;
        let ic3 = token(&server, "a@own.test", IC3).await;
        let mri = format!("8:orgid:{id}");
        registered(&server, &id, &ic3, EPID).await;
        let mut socket = Raw::open(&server, &query(EPID, "TeamsCDL")).await;
        socket.connect(&ic3).await;
        // an endpoint that isn't registered yet, and the calling stack's
        let later = "00000000-0000-4000-8000-0000000000e4";
        let mut unregistered = Raw::open(&server, &query(later, "TeamsCDL")).await;
        unregistered.connect(&ic3).await;
        let mut calling = Raw::open(&server, &query(EPID, "SkypeSpaces")).await;
        calling.connect(&ic3).await;

        tell(&server.app, &mri, &Control::Lost).await.unwrap();
        assert!(is_loss(&socket.next().await.unwrap()));
        assert!(socket.next_within(Duration::from_millis(300)).await.is_err());
        assert!(unregistered.next_within(Duration::from_millis(300)).await.is_err());
        // told once its deliveries start
        registered(&server, &id, &ic3, later).await;
        assert!(is_loss(&unregistered.next().await.unwrap()));
        assert!(calling.next_within(Duration::from_millis(300)).await.is_err());
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_registered_endpoints_cursor_is_kept() {
    for redis in [None, redis_url()].into_iter().skip(usize::from(redis_url().is_some())) {
        let server = server_with(Backend::Sqlite, |config| config.redis_url = redis).await;
        let id = server.add_user("a@own.test", "Ada").await;
        let ic3 = token(&server, "a@own.test", IC3).await;
        let topic = format!("push:8:orgid:{id}");
        let endpoint = endpoint_id(&id, EPID, Stack::Messaging);
        let mut socket = Raw::open(&server, &query(EPID, "TeamsCDL")).await;
        socket.connect(&ic3).await;
        socket.send(PING).await;
        assert_eq!(socket.next().await.as_deref(), Some(PONG));
        assert_eq!(kept_cursor(&server, &topic, &endpoint).await, None);
        // an event it isn't given, and the socket's end, keep none either
        server
            .app
            .fanout
            .publish(&topic, message("unregistered"))
            .await
            .unwrap();
        socket.send("0::").await;
        assert!(socket.next().await.is_none());
        assert_eq!(kept_cursor(&server, &topic, &endpoint).await, None);

        // registered, its cursor is kept from its first delivery on, at what was written to its socket
        let mut socket = Raw::open(&server, &query(EPID, "TeamsCDL")).await;
        socket.connect(&ic3).await;
        registered(&server, &id, &ic3, EPID).await;
        server.app.fanout.publish(&topic, message("registered")).await.unwrap();
        assert!(socket.next().await.unwrap().contains("registered"));
        socket.send(PING).await;
        assert_eq!(socket.next().await.as_deref(), Some(PONG));
        let kept = kept_cursor(&server, &topic, &endpoint).await.unwrap();
        assert_eq!(kept, server.app.journal.tail(&topic).await.unwrap());
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_topic_keeps_only_so_many_readers_cursors() {
    for redis in [None, redis_url()].into_iter().skip(usize::from(redis_url().is_some())) {
        let server = server_with(Backend::Sqlite, |config| config.redis_url = redis).await;
        let journal = &server.app.journal;
        let topic = format!("push:8:orgid:{}", uuid::Uuid::new_v4());
        let other = format!("push:8:orgid:{}", uuid::Uuid::new_v4());
        let at = |n: usize| EntryId {
            ms: 1 + u64::try_from(n).unwrap(),
            seq: 0,
        };
        journal.set_cursor(&other, "reader-0", at(7)).await.unwrap();
        for n in 0..CURSORS {
            journal.set_cursor(&topic, &format!("reader-{n}"), at(n)).await.unwrap();
            // the first two moved longest ago, in this order
            if n < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        // a reader the topic has moves without pushing one out
        journal.set_cursor(&topic, "reader-1", at(100)).await.unwrap();
        assert_eq!(journal.cursor(&topic, "reader-0").await.unwrap(), Some(at(0)));
        // one more than it keeps pushes out the one that moved longest ago
        journal.set_cursor(&topic, "one-more", at(200)).await.unwrap();
        assert_eq!(journal.cursor(&topic, "reader-0").await.unwrap(), None);
        assert_eq!(journal.cursor(&topic, "reader-1").await.unwrap(), Some(at(100)));
        assert_eq!(journal.cursor(&topic, "one-more").await.unwrap(), Some(at(200)));
        for n in 2..CURSORS {
            let reader = format!("reader-{n}");
            assert_eq!(journal.cursor(&topic, &reader).await.unwrap(), Some(at(n)), "{n}");
        }
        assert_eq!(journal.cursor(&other, "reader-0").await.unwrap(), Some(at(7)));
        assert_eq!(journal.cursor(&other, "one-more").await.unwrap(), None);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replay_to_a_reader_that_takes_nothing_ends_when_its_token_runs_out() {
    const EVENTS: usize = 400;
    let server = server_with(Backend::Sqlite, |config| config.tokens.access_lifetime = 4).await;
    let id = server.add_user("a@own.test", "Ada").await;
    let ic3 = token(&server, "a@own.test", IC3).await;
    let topic = format!("push:8:orgid:{id}");
    registered(&server, &id, &ic3, EPID).await;
    // the endpoint is known, and its socket down while more arrives than a socket's buffers take
    let mut socket = Raw::open(&server, &query(EPID, "TeamsCDL")).await;
    socket.connect(&ic3).await;
    socket.send(PING).await;
    assert_eq!(socket.next().await.as_deref(), Some(PONG));
    socket.send("0::").await;
    assert!(socket.next().await.is_none());
    let large = "x".repeat(64 * 1024);
    for _ in 0..EVENTS {
        server.app.fanout.publish(&topic, message(&large)).await.unwrap();
    }
    let ic3 = token(&server, "a@own.test", IC3).await;
    let mut socket = Raw::open(&server, &query(EPID, "TeamsCDL")).await;
    socket.connect(&ic3).await;
    // the client reads nothing until its token has run out
    tokio::time::sleep(Duration::from_secs(5)).await;
    let (mut replayed, mut last) = (0, String::new());
    while let Some(frame) = socket.next().await {
        replayed += usize::from(frame.starts_with("3:::"));
        last = frame;
    }
    assert_eq!(last, r#"0:::{"reason":"unauthorized"}"#);
    assert!(replayed < EVENTS, "the replay went on past the token's end");
    // what was written is not replayed again: the cursor was kept where the socket stopped
    let kept = kept_cursor(&server, &topic, &endpoint_id(&id, EPID, Stack::Messaging)).await;
    let end = server.app.journal.tail(&topic).await.unwrap();
    assert!(kept.is_some_and(|kept| kept < end));
    server.stop().await;
}
