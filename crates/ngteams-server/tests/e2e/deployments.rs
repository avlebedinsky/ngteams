//! Deployments that share one Redis: each keeps its own sign-in holds, fan-out and journal, and the nodes of one
//! deployment still share theirs.

use std::time::Duration;

use bytes::Bytes;
use ngteams_server::config::Config;
use ngteams_server::fanout::Subscription;
use ngteams_server::fanout::journal::EntryId;
use teams_auth::token::Scope;

use crate::support::{Backend, PASSWORD, TestServer, backends, redis_url};

const UPN: &str = "a@own.test";
const WAIT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(300);

/// A deployment's first node on a database of its own, with a short sign-in limit.
async fn deployment(backend: Backend, redis: &str, configure: impl FnOnce(&mut Config)) -> TestServer {
    let redis = redis.to_owned();
    TestServer::start_with(backend, |config| {
        config.redis_url = Some(redis);
        config.login.max_failures = 2;
        configure(config);
    })
    .await
}

/// Another node of `first`'s deployment: its database, origin and Redis.
async fn node_beside(first: &TestServer, backend: Backend, configure: impl FnOnce(&mut Config)) -> TestServer {
    let shared = first.app.config.clone();
    TestServer::start_with(backend, |config| {
        config.database_url = shared.database_url;
        config.public_origin = shared.public_origin;
        config.redis_url = shared.redis_url;
        config.login = shared.login;
        configure(config);
    })
    .await
}

async fn next(subscription: &mut Subscription) -> Bytes {
    tokio::time::timeout(WAIT, subscription.recv()).await.unwrap().unwrap()
}

async fn hears_nothing(subscription: &mut Subscription) -> bool {
    tokio::time::timeout(QUIET, subscription.recv()).await.is_err()
}

async fn wrong_password(server: &TestServer) -> u16 {
    server.password_grant(UPN, "wrong password", &Scope::spaces()).await.0
}

async fn right_password(server: &TestServer) -> u16 {
    server.password_grant(UPN, PASSWORD, &Scope::spaces()).await.0
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_sign_ins_on_one_deployment_hold_off_its_nodes_and_no_other_deployment_on_the_same_redis() {
    let Some(redis) = redis_url() else { return };
    for backend in backends() {
        let one = deployment(backend, &redis, |_| {}).await;
        let one_b = node_beside(&one, backend, |_| {}).await;
        let other = deployment(backend, &redis, |_| {}).await;
        one.add_user(UPN, "Ada").await;
        other.add_user(UPN, "Another Ada").await;
        for _ in 0..2 {
            assert_eq!(wrong_password(&one).await, 400, "{backend:?}");
        }
        // the hold is the whole deployment's
        assert_eq!(right_password(&one_b).await, 429, "{backend:?}");
        // the client signs the other deployment's account of the same name in
        let session = other.session(UPN).await;
        session.skypetoken().await.unwrap();
        // and that sign-in cleared nothing of the first deployment's hold
        assert_eq!(right_password(&one).await, 429, "{backend:?}");
        other.stop().await;
        one_b.stop().await;
        one.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_push_published_in_one_deployment_reaches_its_nodes_and_no_other_deployment_on_the_same_redis() {
    let Some(redis) = redis_url() else { return };
    for backend in backends() {
        let one = deployment(backend, &redis, |_| {}).await;
        let one_b = node_beside(&one, backend, |_| {}).await;
        let other = deployment(backend, &redis, |_| {}).await;
        // the same account id in both, as a copied database has
        let topic = format!("push:8:orgid:{}", uuid::Uuid::new_v4());
        let mut on_one_b = one_b.app.fanout.subscribe(&topic).await.unwrap();
        let mut on_other = other.app.fanout.subscribe(&topic).await.unwrap();
        let event = Bytes::from_static(br#"{"resourceType":"NewMessage"}"#);
        one.app.fanout.publish(&topic, event.clone()).await.unwrap();
        assert_eq!(next(&mut on_one_b).await, event, "{backend:?}");
        assert!(hears_nothing(&mut on_other).await, "{backend:?}");
        // what the first deployment kept for replay, and its endpoints' cursors, are its own too
        let before = EntryId::default();
        let kept = one_b.app.journal.after(&topic, before).await.unwrap().entries;
        assert_eq!(kept.len(), 1, "{backend:?}");
        assert!(
            other
                .app
                .journal
                .after(&topic, before)
                .await
                .unwrap()
                .entries
                .is_empty()
        );
        one.app
            .journal
            .set_cursor(&topic, "endpoint", kept[0].id)
            .await
            .unwrap();
        assert_eq!(
            one_b.app.journal.cursor(&topic, "endpoint").await.unwrap(),
            Some(kept[0].id)
        );
        assert_eq!(other.app.journal.cursor(&topic, "endpoint").await.unwrap(), None);
        // the other deployment's own push stays with it
        let own = Bytes::from_static(br#"{"resourceType":"MessageUpdate"}"#);
        other.app.fanout.publish(&topic, own.clone()).await.unwrap();
        assert_eq!(next(&mut on_other).await, own, "{backend:?}");
        assert!(hears_nothing(&mut on_one_b).await, "{backend:?}");
        other.stop().await;
        one_b.stop().await;
        one.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_configured_namespace_parts_two_deployments_whose_databases_name_one_tenant() {
    let Some(redis) = redis_url() else { return };
    for backend in backends() {
        // one database stands for two that hold the same tenant id, as one restored from the other does
        let one = deployment(backend, &redis, |_| {}).await;
        let namespace = format!("staging-{}", uuid::Uuid::new_v4().simple());
        let staging = |config: &mut Config| config.redis_namespace = Some(namespace.clone());
        let copy = node_beside(&one, backend, staging).await;
        let copy_b = node_beside(&one, backend, staging).await;
        one.add_user(UPN, "Ada").await;
        for _ in 0..2 {
            assert_eq!(wrong_password(&copy).await, 400, "{backend:?}");
        }
        assert_eq!(right_password(&copy_b).await, 429, "{backend:?}");
        one.session(UPN).await.skypetoken().await.unwrap();
        let topic = format!("push:8:orgid:{}", uuid::Uuid::new_v4());
        let mut on_one = one.app.fanout.subscribe(&topic).await.unwrap();
        let mut on_copy_b = copy_b.app.fanout.subscribe(&topic).await.unwrap();
        let event = Bytes::from_static(br#"{"resourceType":"NewMessage"}"#);
        copy.app.fanout.publish(&topic, event.clone()).await.unwrap();
        assert_eq!(next(&mut on_copy_b).await, event, "{backend:?}");
        assert!(hears_nothing(&mut on_one).await, "{backend:?}");
        copy_b.stop().await;
        copy.stop().await;
        one.stop().await;
    }
}
