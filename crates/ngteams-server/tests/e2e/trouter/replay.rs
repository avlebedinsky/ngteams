//! Reconnects: what the endpoint missed while its socket was down is replayed as stored deliveries, and what the
//! journal no longer holds is reported as a message loss the client acknowledges.

use std::time::Duration;

use bytes::Bytes;
use ngteams_server::fanout::journal::{CAPACITY, EntryId};
use teams_trouter::{ChatEvent, PushEvent};

use super::{Account, delivery};
use crate::chatsvc::messages::Pair;
use crate::support::{TestServer, backends, redis_url};

/// A pair of accounts in a chat, on Redis when the tests have one.
async fn pair(backend: crate::support::Backend) -> Pair {
    let redis = redis_url();
    let server = TestServer::start_with(backend, |config| {
        config.public_origin = url::Url::parse(super::ORIGIN).unwrap();
        config.redis_url = redis;
    })
    .await;
    Pair::start_on(server).await
}

/// The client's waits, with a backoff long enough for the test to act while the socket is down.
fn slow() -> teams_trouter::Options {
    teams_trouter::Options {
        backoff_base: Duration::from_secs(5),
        ..super::options()
    }
}

async fn connected(push: &mut super::Push) -> bool {
    push.until(|event| match event {
        PushEvent::Connected { new_endpoint, .. } => Some(new_endpoint),
        _ => None,
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reconnect_replays_what_the_endpoint_missed_as_stored_deliveries_and_nothing_twice() {
    for backend in backends() {
        let pair = pair(backend).await;
        let b = Account::sign_in(&pair.server, "b@own.test", &pair.b_id).await;
        // a backoff long enough for a to post before the client is back
        let mut push = super::Push(b.trouter(&pair.server).with_options(slow()).spawn());
        push.until(|event| matches!(event, PushEvent::Registration(_)).then_some(()))
            .await;
        let seen = pair.send("seen while connected").await;
        let live = delivery(&mut push).await;
        assert!(!live.stored);

        // the service disconnects; the client comes back after its backoff, and meanwhile a posts
        ngteams_server::trouter::disconnect(&pair.server.app, &b.mri, Some("maintenance"))
            .await
            .unwrap();
        push.until(|event| matches!(event, PushEvent::Disconnected { .. }).then_some(()))
            .await;
        let first = pair.send("missed, first").await;
        let second = pair.send("missed, second").await;
        assert!(!connected(&mut push).await, "the same endpoint");
        let mut replayed = Vec::new();
        for _ in 0..2 {
            let delivery = delivery(&mut push).await;
            assert!(delivery.stored);
            let ChatEvent::Message { message, .. } = delivery.event else {
                panic!("not a message");
            };
            replayed.push(message.id.clone());
        }
        assert_eq!(replayed, [first, second]);
        assert!(!replayed.contains(&seen));
        // and live again from there
        let after = pair.send("live again").await;
        let live = delivery(&mut push).await;
        assert!(!live.stored);
        assert!(matches!(live.event, ChatEvent::Message { message, .. } if message.id == after));
        push.stop().await;
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_endpoint_that_missed_more_than_the_journal_holds_is_told_of_the_loss_and_acknowledges_it() {
    for backend in backends() {
        let pair = pair(backend).await;
        let b = Account::sign_in(&pair.server, "b@own.test", &pair.b_id).await;
        // a backoff long enough for the journal to overflow before the client is back
        let mut push = super::Push(b.trouter(&pair.server).with_options(slow()).spawn());
        push.until(|event| matches!(event, PushEvent::Registration(_)).then_some(()))
            .await;
        ngteams_server::trouter::disconnect(&pair.server.app, &b.mri, Some("maintenance"))
            .await
            .unwrap();
        push.until(|event| matches!(event, PushEvent::Disconnected { .. }).then_some(()))
            .await;
        // more events than the account's journal keeps
        let topic = format!("push:{}", b.mri);
        let event = serde_json::json!({"resourceType": "ThreadUpdate", "resource": {"id": "19:overflow@thread.v2"}});
        for _ in 0..=CAPACITY {
            pair.server
                .app
                .fanout
                .publish(&topic, Bytes::from(event.to_string()))
                .await
                .unwrap();
        }
        let loss = push
            .until(|event| match event {
                PushEvent::MessageLoss(loss) => Some(loss),
                _ => None,
            })
            .await;
        assert!(loss.messaging());
        loss.caught_up();
        // what the journal still holds follows, stored
        let mut stored = 0;
        while stored < CAPACITY {
            assert!(delivery(&mut push).await.stored);
            stored += 1;
        }
        // the acknowledgement was taken: the socket stays up and delivers on
        let after = pair.send("after the loss").await;
        let live = delivery(&mut push).await;
        assert!(matches!(live.event, ChatEvent::Message { message, .. } if message.id == after));
        push.stop().await;
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_endpoint_whose_cursor_is_older_than_the_journal_keeps_is_told_of_a_loss() {
    for backend in backends() {
        let pair = pair(backend).await;
        let b = Account::sign_in(&pair.server, "b@own.test", &pair.b_id).await;
        let trouter = b.trouter(&pair.server);
        let endpoint = ngteams_server::trouter::endpoint_id(
            &pair.b_id,
            trouter.endpoint_id(),
            ngteams_server::trouter::Stack::Messaging,
        );
        // as if the endpoint had last been given an entry an hour ago
        let hour_ago = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap() - 3_600_000;
        pair.server
            .app
            .journal
            .set_cursor(&format!("push:{}", b.mri), &endpoint, EntryId { ms: hour_ago, seq: 0 })
            .await
            .unwrap();
        let mut push = super::Push(trouter.spawn());
        // the loss report and the registration come in either order
        let (mut told, mut registered) = (false, false);
        while !(told && registered) {
            match push.next().await {
                PushEvent::MessageLoss(loss) => {
                    assert!(loss.messaging());
                    loss.caught_up();
                    told = true;
                }
                PushEvent::Registration(_) => registered = true,
                _ => {}
            }
        }
        // told once: what the journal still held is replayed, and deliveries go on
        let id = pair.send("after the gap").await;
        loop {
            let next = delivery(&mut push).await;
            if matches!(&next.event, ChatEvent::Message { message, .. } if message.id == id) {
                assert!(!next.stored);
                break;
            }
            assert!(next.stored);
        }
        assert!(
            push.quiet(Duration::from_millis(300), |event| matches!(
                event,
                PushEvent::MessageLoss(_)
            ))
            .await
        );
        push.stop().await;
        pair.stop().await;
    }
}
