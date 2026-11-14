//! Two nodes on one database and one Redis: a post on one node reaches a socket on the other, the registration
//! taken by one node starts the deliveries of a socket on the other, and an endpoint that reconnects to another node
//! is replayed what it missed.

use teams_core::Transient;
use teams_trouter::{ChatEvent, PushEvent, RegistrationReport};

use super::refusals::{EPID, IC3, Raw, query, register, registration, token};
use super::{Account, ORIGIN, delivery};
use crate::chatsvc::messages::Pair;
use crate::support::{TestServer, backends, redis_url};

/// Node A with a pair of accounts in a chat (their engines write through A), and node B beside it.
async fn nodes(backend: crate::support::Backend, redis: &str) -> (Pair, TestServer) {
    let url = redis.to_owned();
    let node_a = TestServer::start_with(backend, |config| {
        config.public_origin = url::Url::parse(ORIGIN).unwrap();
        config.redis_url = Some(url);
    })
    .await;
    let (shared, url) = (node_a.app.config.clone(), redis.to_owned());
    let node_b = TestServer::start_with(backend, |config| {
        config.public_origin = shared.public_origin;
        config.database_url = shared.database_url;
        config.redis_url = Some(url);
    })
    .await;
    (Pair::start_on(node_a).await, node_b)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_post_on_one_node_reaches_a_socket_on_the_other() {
    let Some(redis) = redis_url() else { return };
    for backend in backends() {
        let (pair, node_b) = nodes(backend, &redis).await;
        // b's socket is on node B; its registration goes through node A
        let b = Account::sign_in(&pair.server, "b@own.test", &pair.b_id).await;
        let socket_on_b = b.trouter_on(&node_b, &pair.server);
        let mut push = super::Push(socket_on_b.spawn());
        push.until(|event| matches!(event, PushEvent::Registration(RegistrationReport::Registered)).then_some(()))
            .await;
        assert_eq!(pair.server.app.trouter.open(), 0);
        assert_eq!(node_b.app.trouter.open(), 1);
        // a posts through node A
        let id = pair.send("from node A to a socket on node B").await;
        let pushed = delivery(&mut push).await;
        assert!(!pushed.stored);
        assert!(matches!(pushed.event, ChatEvent::Message { message, .. } if message.id == id));
        // typing, which no journal keeps, crosses the nodes too
        pair.a.sync().await.unwrap();
        pair.a
            .transient(Transient::Typing {
                conversation: pair.chat.clone(),
            })
            .await
            .unwrap();
        assert!(matches!(delivery(&mut push).await.event, ChatEvent::Typing { .. }));
        push.stop().await;
        node_b.stop().await;
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_endpoint_that_reconnects_to_another_node_is_replayed_what_it_missed() {
    let Some(redis) = redis_url() else { return };
    for backend in backends() {
        let (pair, node_b) = nodes(backend, &redis).await;
        let ic3 = token(&pair.server, "b@own.test", IC3).await;
        let q = query(EPID, "TeamsCDL");
        // the endpoint's first socket is on node A, its registration taken by node B
        let mut first = Raw::open(&pair.server, &q).await;
        let info = first.connect(&ic3).await;
        let body = registration("TeamsCDLWebWorker", EPID, info["surl"].as_str().unwrap(), 3600);
        assert_eq!(register(&node_b, Some(&ic3), &body).await, 202);
        let seen = pair.send("seen on node A").await;
        let live = first.next().await.unwrap();
        assert!(live.contains(&seen) && !live.contains("mstore"), "{}", live.len());
        first.send("0::").await;
        assert!(first.next().await.is_none());
        let missed = pair.send("missed between the nodes").await;
        // the same endpoint on node B, with the parameters node A handed over
        let mut second = Raw::open(&node_b, &q).await;
        second.authenticate(&ic3, Some(info["connectparams"].clone())).await;
        let connected = second.next().await.unwrap();
        assert!(connected.contains("trouter.connected") && connected.contains(info["id"].as_str().unwrap()));
        let stored = second.next().await.unwrap();
        assert!(stored.starts_with("3:::"), "no loss is reported: the journal held it");
        assert!(stored.contains(&missed) && stored.contains("mstore") && !stored.contains(&seen));
        let after = pair.send("live on node B").await;
        let live = second.next().await.unwrap();
        assert!(live.contains(&after) && !live.contains("mstore"));
        node_b.stop().await;
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn disabling_an_account_on_one_node_ends_its_sockets_on_the_other_at_once() {
    let Some(redis) = redis_url() else { return };
    for backend in backends() {
        let (pair, node_b) = nodes(backend, &redis).await;
        let ic3 = token(&pair.server, "b@own.test", IC3).await;
        let mut socket = Raw::open(&node_b, &query(EPID, "TeamsCDL")).await;
        socket.connect(&ic3).await;
        let app = &pair.server.app;
        ngteams_server::admin::set_disabled(&app.db, &*app.fanout, "b@own.test", true)
            .await
            .unwrap();
        let frame = socket.next_within(std::time::Duration::from_secs(4)).await;
        assert_eq!(
            frame.ok().flatten().as_deref(),
            Some(r#"0:::{"reason":"unauthorized"}"#)
        );
        assert!(socket.next().await.is_none());
        node_b.stop().await;
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_an_account_on_one_node_ends_its_sockets_on_the_other_at_once() {
    let Some(redis) = redis_url() else { return };
    for backend in backends() {
        let (pair, node_b) = nodes(backend, &redis).await;
        let ic3 = token(&pair.server, "b@own.test", IC3).await;
        let mut socket = Raw::open(&node_b, &query(EPID, "TeamsCDL")).await;
        socket.connect(&ic3).await;
        let app = &pair.server.app;
        ngteams_server::admin::delete_by_upn(&app.db, &*app.fanout, "b@own.test")
            .await
            .unwrap();
        let frame = socket.next_within(std::time::Duration::from_secs(4)).await;
        assert_eq!(
            frame.ok().flatten().as_deref(),
            Some(r#"0:::{"reason":"unauthorized"}"#)
        );
        assert!(socket.next().await.is_none());
        node_b.stop().await;
        pair.stop().await;
    }
}
