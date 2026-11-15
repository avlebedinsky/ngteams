//! Two nodes on one database and one Redis: an account's availability reported on one node shows on the other and is
//! pushed to a watcher's socket there.

use teams_core::Availability;
use teams_core::engine::OwnStatus;
use teams_proto::{ManualAvailability, PresenceRead};

use super::{Seat, watched};
use crate::support::{TestServer, backends, redis_url};
use crate::trouter::ORIGIN;

async fn nodes(backend: crate::support::Backend, redis: &str) -> (TestServer, TestServer) {
    let url = redis.to_owned();
    let node_a = TestServer::start_with(backend, |config| {
        config.public_origin = url::Url::parse(ORIGIN).unwrap();
        config.redis_url = Some(url);
        config.presence.endpoint_ttl = 3;
    })
    .await;
    let (shared, url) = (node_a.app.config.clone(), redis.to_owned());
    let node_b = TestServer::start_with(backend, |config| {
        config.public_origin = shared.public_origin;
        config.database_url = shared.database_url;
        config.redis_url = Some(url);
        config.presence.endpoint_ttl = 3;
    })
    .await;
    (node_a, node_b)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_presence_change_on_one_node_is_pushed_to_a_watchers_socket_on_the_other() {
    let Some(redis) = redis_url() else { return };
    for backend in backends() {
        let (node_a, node_b) = nodes(backend, &redis).await;
        let upn = |name: &str| format!("{name}-{}@own.test", uuid::Uuid::new_v4().simple());
        let (ada_upn, bert_upn) = (upn("ada"), upn("bert"));
        let a = node_a.add_user(&ada_upn, "Ada").await;
        let b = node_a.add_user(&bert_upn, "Bert").await;
        // Ada's requests and socket are on node A, Bert's on node B
        let mut ada = Seat::start(&node_a, &node_a, &ada_upn, &a).await;
        let bert = Seat::start(&node_b, &node_b, &bert_upn, &b).await;
        ada.engine.presence(bert.mri.clone()).await.unwrap();
        ada.sees(&bert.mri, &Availability::Offline).await;
        watched(&node_b, &bert.mri).await;

        bert.report(OwnStatus::Manual(ManualAvailability::DoNotDisturb)).await;
        bert.report(OwnStatus::Available).await;
        ada.sees(&bert.mri, &Availability::DoNotDisturb).await;
        // what node B took, node A answers
        let read = PresenceRead::new([teams_proto::Mri::new(bert.mri.clone())]).unwrap();
        let answer = ada.client.post_read(read.clone()).await.unwrap();
        let shown = read.read_answer(answer.body()).unwrap();
        assert_eq!(shown[0].availability.as_deref(), Some("DoNotDisturb"));
        // the endpoint lapses on whichever node sweeps it, and the watcher on node A is told
        ada.sees(&bert.mri, &Availability::Offline).await;
        ada.engine.shutdown();
        bert.engine.shutdown();
        node_b.stop().await;
        node_a.stop().await;
    }
}
