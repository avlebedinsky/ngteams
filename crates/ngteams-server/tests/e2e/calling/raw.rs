//! The calling requests the client's own flows don't send in the other tests, sent raw with a party's skypetoken on
//! a call the clients placed and answered: a party's subscription to the conversation and its `DELETE`, the call's
//! keep-alive, the caller's acknowledgement of its acceptance by request, a renegotiation while another waits
//! (`491`) and one the far end turns away (`negotiations/…/reject`). What each tells a party is read off the
//! deliveries published for its calling endpoint.

use std::time::Duration;

use reqwest::Method;
use serde_json::{Value, json};
use teams_call::{CallState, End};

use super::Pair;
use crate::support::TestServer;
use crate::trouter::ORIGIN;

fn mri(id: &str) -> String {
    format!("8:orgid:{id}")
}

/// A raw calling request with `upn`'s skypetoken; `body` null sends none.
async fn raw(server: &TestServer, upn: &str, method: Method, path: &str, body: &Value) -> (u16, Value) {
    let session = server.session(upn).await;
    let token = session.skypetoken().await.unwrap();
    let request = reqwest::Client::new()
        .request(method, server.url(path))
        .header("x-skypetoken", token.token.expose());
    let request = if body.is_null() { request } else { request.json(body) };
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

/// The calling endpoint `id`'s client registered.
async fn endpoint_of(server: &TestServer, id: &str) -> String {
    let stack = ngteams_server::trouter::Stack::Calling;
    let registered = ngteams_server::registrar::registered(&server.app.db, id, stack.app_id())
        .await
        .unwrap();
    ngteams_server::trouter::endpoint_id(id, &registered[0], stack)
}

/// A callback of `endpoint`'s, as a raw request hands it out.
fn link(endpoint: &str, name: &str) -> String {
    format!("{ORIGIN}/trouter/v4/f/{endpoint}/callAgent/raw/{name}/")
}

/// What the call controller publishes for one calling endpoint.
struct Told(ngteams_server::fanout::Subscription);

impl Told {
    async fn of(server: &TestServer, endpoint: &str) -> Self {
        let topic = ngteams_server::calling::topic(endpoint);
        Self(server.app.fanout.subscribe(&topic).await.unwrap())
    }

    /// The next delivery at the raw callback `name`: its body.
    async fn at(&mut self, name: &str) -> Value {
        let path = format!("callAgent/raw/{name}/");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let payload = tokio::time::timeout_at(deadline, self.0.recv())
                .await
                .unwrap_or_else(|_| panic!("nothing came at {name}"))
                .unwrap();
            let published: Value = serde_json::from_slice(&payload).unwrap();
            if published["path"] == path.as_str() {
                return serde_json::from_str(published["body"].as_str().unwrap()).unwrap();
            }
        }
    }
}

/// A server path of one of the server's own links.
fn path_of(url: &str) -> &str {
    url.strip_prefix(ORIGIN).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_calls_raw_requests_are_served_as_the_client_would_send_them() {
    let mut pair = Pair::start(crate::support::Backend::Sqlite).await;
    pair.a.call(&mri(&pair.b_id));
    pair.a.state(CallState::Ringing).await;
    let rings = pair.b.incoming().await;
    pair.b.answer(rings.call);
    pair.b.state(CallState::Connected).await;
    pair.a.state(CallState::Connected).await;
    let calls = ngteams_server::calling::open_calls(&pair.server.app.db).await.unwrap();
    let call = calls[0].clone();
    let server = &pair.server;
    let (a_ep, b_ep) = (
        endpoint_of(server, &pair.a_id).await,
        endpoint_of(server, &pair.b_id).await,
    );
    let mut told_a = Told::of(server, &a_ep).await;
    let mut told_b = Told::of(server, &b_ep).await;
    let conv = format!("/flightproxy/api/v2/ep/conv.own/conv/{}", call.conversation);
    let cc = format!("/flightproxy/api/v2/ep/cc.own/cc/v1");
    let (a, b) = ("a@own.test", "b@own.test");

    // a party's subscription to its conversation: the conversation, with its callbacks kept; no join of a 1:1 call
    let subscription = json!({"conversationRequest": {"links": {"conversationUpdate": link(&a_ep, "update")}}});
    let (status, conversation) = raw(server, a, Method::POST, &conv, &subscription).await;
    assert_eq!(status, 200, "{conversation}");
    assert!(conversation.to_string().contains(&call.conversation), "{conversation}");
    let join = json!({"callInvitation": {"mediaContent": {"blob": "v=0\r\n"}}});
    let (status, _) = raw(server, b, Method::POST, &conv, &join).await;
    assert_eq!(status, 403, "nobody joins a 1:1 call");

    // the keep-alive, on the party's own leg only
    let (status, _) = raw(
        server,
        a,
        Method::POST,
        &format!("{cc}/active/{}/1/k1/1", call.id),
        &Value::Null,
    )
    .await;
    assert_eq!(status, 202);
    let (status, _) = raw(
        server,
        a,
        Method::POST,
        &format!("{cc}/forked/{}/1/k1/1", call.id),
        &Value::Null,
    )
    .await;
    assert_eq!(status, 404, "only a call's active leg is kept alive");
    let (status, _) = raw(
        server,
        a,
        Method::POST,
        &format!("{cc}/active/{}/2/k1/1", call.id),
        &Value::Null,
    )
    .await;
    assert_eq!(status, 404, "a leg that isn't the party's");

    // the caller's acknowledgement of its acceptance by request: where the far end's renegotiation goes
    let acknowledge = format!("{cc}/callAcceptance/{}/1/a1/1/acknowledge", call.id);
    let links = json!({"links": {"mediaRenegotiation": link(&a_ep, "mediaRenegotiation")}});
    let (status, _) = raw(server, a, Method::POST, &acknowledge, &links).await;
    assert_eq!(status, 200);

    // Bert's renegotiation reaches Ada there; a second one while it waits is turned away with 491
    let renegotiate = format!("{cc}/active/{}/1/a4/1/renegotiate", call.id);
    let offer = json!({
        "mediaNegotiation": {"mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
            "callModalities": ["Audio", "Video"]},
        "links": {"mediaAnswer": link(&b_ep, "mediaAnswer"), "rejection": link(&b_ep, "rejection")},
    });
    let (status, _) = raw(server, b, Method::POST, &renegotiate, &offer).await;
    assert_eq!(status, 200);
    let forwarded = told_a.at("mediaRenegotiation").await;
    assert_eq!(
        forwarded["mediaNegotiation"]["callModalities"],
        json!(["Audio", "Video"])
    );
    let (status, _) = raw(server, b, Method::POST, &renegotiate, &offer).await;
    assert_eq!(status, 200);
    let busy = told_b.at("rejection").await;
    assert_eq!(busy["mediaNegotiationFailure"]["code"], 491, "{busy}");

    // Ada turns the first one away at its own link: Bert is told; the same link then has nothing to turn away
    let reject = forwarded["mediaNegotiation"]["links"]["rejection"].as_str().unwrap();
    assert!(reject.contains("/negotiations/"), "{reject}");
    let (status, _) = raw(server, a, Method::POST, path_of(reject), &Value::Null).await;
    assert_eq!(status, 200);
    let rejected = told_b.at("rejection").await;
    assert_eq!(rejected["mediaNegotiationFailure"]["code"], 488, "{rejected}");
    let (status, _) = raw(server, a, Method::POST, path_of(reject), &Value::Null).await;
    assert_eq!(status, 410, "nothing waits for an answer");
    let (status, _) = raw(server, b, Method::POST, path_of(reject), &Value::Null).await;
    assert_eq!(status, 410, "the offerer has nothing of its own to turn away");

    // the conversation's DELETE ends the call for both
    let (status, _) = raw(server, a, Method::DELETE, &conv, &json!({})).await;
    assert_eq!(status, 204);
    assert!(matches!(pair.b.ended().await, End::Remote(_)));
    // late requests: the conversation's are taken, the call controller's are gone
    let (status, _) = raw(server, b, Method::POST, &format!("{conv}/leave"), &json!({})).await;
    assert_eq!(status, 200);
    let (status, _) = raw(
        server,
        a,
        Method::POST,
        &format!("{cc}/active/{}/1/k1/1", call.id),
        &Value::Null,
    )
    .await;
    assert_eq!(status, 410);
    pair.server.stop().await;
}
