//! An invitation a callee's endpoint doesn't answer is delivered again, three times about 2 s apart, as Teams'
//! service does (`docs/protocol/calling.md`, "What the callee sends while it rings"); one that is answered is
//! delivered once. The client takes a redelivery as the invitation it already has, so the call rings once and is
//! answered as any other. The same with the callee's socket on another node than the call.

use std::time::Duration;

use teams_call::CallState;

use super::client::Client;
use crate::support::{Backend, TestServer, backends, redis_url};
use crate::trouter::Account;

fn mri(id: &str) -> String {
    format!("8:orgid:{id}")
}

/// Somewhat longer than the three redeliveries take.
const REDELIVERIES: Duration = Duration::from_millis(7_500);

async fn holding(socket: &TestServer, http: &TestServer, upn: &str, id: &str, name: &str, held: usize) -> Client {
    let account = Account::sign_in(http, upn, id).await;
    Client::start_holding(&account, name, (socket, http), held).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invitation_the_callee_doesnt_answer_comes_again_three_times() {
    for backend in backends() {
        let server = crate::trouter::server_with(backend, |config| {
            if backend == Backend::Postgres {
                config.redis_url = redis_url();
            }
        })
        .await;
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let upn = |name: &str| format!("{name}-{tag}@own.test");
        let a_id = server.add_user(&upn("a"), "Ada").await;
        let b_id = server.add_user(&upn("b"), "Bert").await;
        let c_id = server.add_user(&upn("c"), "Cora").await;
        let mut a = super::client(&server, &server, &upn("a"), &a_id, "Ada").await;
        // Bert's client answers no delivery at all; Cora's loses its answer to the first one only
        let mut b = holding(&server, &server, &upn("b"), &b_id, "Bert", usize::MAX).await;
        let mut c = holding(&server, &server, &upn("c"), &c_id, "Cora", 1).await;
        a.call(&mri(&b_id));
        let call = a.state(CallState::Ringing).await;
        b.incoming().await;
        let quiet = b.events_within(REDELIVERIES).await;
        assert!(quiet.is_empty(), "a redelivery rings no second time: {quiet:#?}");
        assert_eq!(b.invitations(), 4, "the invitation and three redeliveries");
        a.hang_up(call);
        a.ended().await;
        a.call(&mri(&c_id));
        a.state(CallState::Ringing).await;
        let rings = c.incoming().await;
        c.events_within(REDELIVERIES).await;
        assert_eq!(c.invitations(), 2, "delivered again until it was answered");
        c.answer(rings.call);
        c.state(CallState::Connected).await;
        a.state(CallState::Connected).await;
        assert_eq!(a.invitations(), 0, "the caller is delivered no invitation");
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answered_invitation_is_delivered_once() {
    let mut pair = super::Pair::start(Backend::Sqlite).await;
    pair.a.call(&mri(&pair.b_id));
    pair.a.state(CallState::Ringing).await;
    pair.b.incoming().await;
    pair.b.events_within(Duration::from_millis(4_500)).await;
    assert_eq!(pair.b.invitations(), 1);
    pair.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_node_that_holds_the_callees_socket_delivers_its_invitation_again() {
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
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let (a_upn, b_upn) = (format!("a-{tag}@own.test"), format!("b-{tag}@own.test"));
        let a_id = node_a.add_user(&a_upn, "Ada").await;
        let b_id = node_a.add_user(&b_upn, "Bert").await;
        let mut a = super::client(&node_a, &node_a, &a_upn, &a_id, "Ada").await;
        // Bert's socket on node B, the call all node A's
        let mut b = holding(&node_b, &node_a, &b_upn, &b_id, "Bert", 2).await;
        a.call(&mri(&b_id));
        a.state(CallState::Ringing).await;
        let rings = b.incoming().await;
        b.events_within(REDELIVERIES).await;
        assert_eq!(b.invitations(), 3, "two held back, the third answered");
        b.answer(rings.call);
        b.state(CallState::Connected).await;
        a.state(CallState::Connected).await;
        node_b.stop().await;
        node_a.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invitation_goes_no_more_to_an_account_disabled_while_it_rings() {
    let server = crate::trouter::server_with(Backend::Sqlite, |_| {}).await;
    let a_id = server.add_user("a@own.test", "Ada").await;
    let b_id = server.add_user("b@own.test", "Bert").await;
    let mut a = super::client(&server, &server, "a@own.test", &a_id, "Ada").await;
    let mut b = holding(&server, &server, "b@own.test", &b_id, "Bert", usize::MAX).await;
    a.call(&mri(&b_id));
    a.state(CallState::Ringing).await;
    b.incoming().await;
    let open = server.app.trouter.open();
    // the database alone changes, as when the operator's command runs beside a node without Redis: no socket hears
    // of it, so the redelivery's own look at the account is what stops it
    ngteams_server::admin::set_disabled(
        &server.app.db,
        &ngteams_server::fanout::Local::new(),
        "b@own.test",
        true,
    )
    .await
    .unwrap();
    b.events_within(REDELIVERIES).await;
    assert_eq!(b.invitations(), 1, "no redelivery to a disabled account");
    assert!(
        server.app.trouter.open() < open,
        "the callee's socket ended when its invitation was due again"
    );
    server.stop().await;
}
