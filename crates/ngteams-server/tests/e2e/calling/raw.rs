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

/// A 1:1 call the clients placed and answered, with what its parties' endpoints are told.
struct Up {
    pair: Pair,
    call: ngteams_server::calling::OpenCall,
    a_ep: String,
    b_ep: String,
    told_a: Told,
    told_b: Told,
    conv: String,
}

const CC: &str = "/flightproxy/api/v2/ep/cc.own/cc/v1";
const A: &str = "a@own.test";
const B: &str = "b@own.test";

impl Up {
    async fn start() -> Self {
        let mut pair = Pair::start(crate::support::Backend::Sqlite).await;
        pair.a.call(&mri(&pair.b_id));
        pair.a.state(CallState::Ringing).await;
        let rings = pair.b.incoming().await;
        pair.b.answer(rings.call);
        pair.b.state(CallState::Connected).await;
        pair.a.state(CallState::Connected).await;
        let calls = ngteams_server::calling::open_calls(&pair.server.app.db).await.unwrap();
        let call = calls[0].clone();
        let a_ep = endpoint_of(&pair.server, &pair.a_id).await;
        let b_ep = endpoint_of(&pair.server, &pair.b_id).await;
        let told_a = Told::of(&pair.server, &a_ep).await;
        let told_b = Told::of(&pair.server, &b_ep).await;
        let conv = format!("/flightproxy/api/v2/ep/conv.own/conv/{}", call.conversation);
        Self {
            pair,
            call,
            a_ep,
            b_ep,
            told_a,
            told_b,
            conv,
        }
    }

    async fn raw(&self, upn: &str, method: Method, path: &str, body: &Value) -> (u16, Value) {
        raw(&self.pair.server, upn, method, path, body).await
    }

    /// A call controller path of the call's.
    fn cc(&self, kind: &str, rest: &str) -> String {
        format!("{CC}/{kind}/{}/{rest}", self.call.id)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_party_subscribes_to_its_conversation_and_keeps_the_call_alive_on_its_own_leg() {
    let up = Up::start().await;
    // the conversation, with its callbacks kept; no join of a 1:1 call
    let subscription = json!({"conversationRequest": {"links": {"conversationUpdate": link(&up.a_ep, "update")}}});
    let (status, conversation) = up.raw(A, Method::POST, &up.conv, &subscription).await;
    assert_eq!(status, 200, "{conversation}");
    assert!(
        conversation.to_string().contains(&up.call.conversation),
        "{conversation}"
    );
    let join = json!({"callInvitation": {"mediaContent": {"blob": "v=0\r\n"}}});
    let (status, _) = up.raw(B, Method::POST, &up.conv, &join).await;
    assert_eq!(status, 403, "nobody joins a 1:1 call");
    let (status, _) = up.raw(A, Method::POST, &up.cc("active", "1/k1/1"), &Value::Null).await;
    assert_eq!(status, 202);
    let (status, _) = up.raw(A, Method::POST, &up.cc("forked", "1/k1/1"), &Value::Null).await;
    assert_eq!(status, 404, "only a call's active leg is kept alive");
    let (status, _) = up.raw(A, Method::POST, &up.cc("active", "2/k1/1"), &Value::Null).await;
    assert_eq!(status, 404, "a leg that isn't the party's");
    up.pair.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renegotiation_goes_where_the_acknowledgement_said_and_is_turned_away_while_another_waits_or_by_the_far_end()
{
    let mut up = Up::start().await;
    // the caller's acknowledgement of its acceptance by request: where the far end's renegotiation goes
    let links = json!({"links": {"mediaRenegotiation": link(&up.a_ep, "mediaRenegotiation")}});
    let (status, _) = up
        .raw(A, Method::POST, &up.cc("callAcceptance", "1/a1/1/acknowledge"), &links)
        .await;
    assert_eq!(status, 200);
    // Bert's renegotiation reaches Ada there; a second one while it waits is turned away with 491
    let renegotiate = up.cc("active", "1/a4/1/renegotiate");
    let offer = json!({
        "mediaNegotiation": {"mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
            "callModalities": ["Audio", "Video"]},
        "links": {"mediaAnswer": link(&up.b_ep, "mediaAnswer"), "rejection": link(&up.b_ep, "rejection")},
    });
    let (status, _) = up.raw(B, Method::POST, &renegotiate, &offer).await;
    assert_eq!(status, 200);
    let forwarded = up.told_a.at("mediaRenegotiation").await;
    assert_eq!(
        forwarded["mediaNegotiation"]["callModalities"],
        json!(["Audio", "Video"])
    );
    let (status, _) = up.raw(B, Method::POST, &renegotiate, &offer).await;
    assert_eq!(status, 200);
    let busy = up.told_b.at("rejection").await;
    assert_eq!(busy["mediaNegotiationFailure"]["code"], 491, "{busy}");
    // Ada turns the first one away at its own link: Bert is told; the same link then has nothing to turn away
    let reject = forwarded["mediaNegotiation"]["links"]["rejection"].as_str().unwrap();
    assert!(reject.contains("/negotiations/"), "{reject}");
    let (status, _) = up.raw(A, Method::POST, path_of(reject), &Value::Null).await;
    assert_eq!(status, 200);
    let rejected = up.told_b.at("rejection").await;
    assert_eq!(rejected["mediaNegotiationFailure"]["code"], 488, "{rejected}");
    let (status, _) = up.raw(A, Method::POST, path_of(reject), &Value::Null).await;
    assert_eq!(status, 410, "nothing waits for an answer");
    let (status, _) = up.raw(B, Method::POST, path_of(reject), &Value::Null).await;
    assert_eq!(status, 410, "the offerer has nothing of its own to turn away");
    up.pair.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_conversations_delete_ends_the_call_for_both_and_later_requests_are_answered_as_after_an_end() {
    let mut up = Up::start().await;
    let (status, _) = up.raw(A, Method::DELETE, &up.conv, &json!({})).await;
    assert_eq!(status, 204);
    assert!(matches!(up.pair.b.ended().await, End::Remote(_)));
    // the conversation's operations are taken, the call controller's are gone
    let (status, _) = up.raw(B, Method::POST, &format!("{}/leave", up.conv), &json!({})).await;
    assert_eq!(status, 200);
    let (status, _) = up.raw(A, Method::POST, &up.cc("active", "1/k1/1"), &Value::Null).await;
    assert_eq!(status, 410);
    up.pair.server.stop().await;
}
