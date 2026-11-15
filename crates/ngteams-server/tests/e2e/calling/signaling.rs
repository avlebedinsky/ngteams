//! A 1:1 call's signaling, with the client's call machine on both ends: the start, the invitation over the callee's
//! calling socket, `attach`, `progress`, the answer and its acceptance at the caller, the hang-up; the decline, the
//! caller's cancel and the ring nobody answers; who may touch a call.

use std::time::Duration;

use serde_json::{Value, json};
use teams_call::{CallState, End};

use super::Pair;
use super::client::{answer, offer};
use crate::support::{TestServer, backends};

fn mri(id: &str) -> String {
    format!("8:orgid:{id}")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_rings_is_answered_by_the_callees_action_and_hung_up() {
    for backend in backends() {
        let mut pair = Pair::start(backend).await;
        pair.a.call(&mri(&pair.b_id));
        pair.a.state(CallState::Ringing).await;
        let rings = pair.b.incoming().await;
        assert_eq!(rings.caller.as_str(), mri(&pair.a_id));
        assert_eq!(rings.caller_name.as_deref(), Some("Ada"));
        assert_eq!(rings.group_chat, None);
        assert_eq!(rings.offer.unwrap().blob, offer().blob, "the caller's offer");
        // nobody acts: it keeps ringing
        let quiet = pair.b.events_within(Duration::from_millis(700)).await;
        assert!(quiet.is_empty(), "{quiet:#?}");
        pair.b.answer(rings.call);
        pair.b.state(CallState::Connected).await;
        assert_eq!(
            pair.a.remote_media().await.blob,
            answer().blob,
            "the callee's answer reaches the caller"
        );
        pair.a.state(CallState::Connected).await;
        pair.b.hang_up(rings.call);
        assert_eq!(pair.b.ended().await, End::HungUp);
        let end = pair.a.ended().await;
        assert!(matches!(end, End::Remote(_)), "{end:?}");
        pair.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_caller_hanging_up_ends_the_call_for_the_callee() {
    let mut pair = Pair::start(crate::support::Backend::Sqlite).await;
    pair.a.call(&mri(&pair.b_id));
    let call = pair.a.state(CallState::Ringing).await;
    let rings = pair.b.incoming().await;
    pair.b.answer(rings.call);
    pair.b.state(CallState::Connected).await;
    pair.a.state(CallState::Connected).await;
    pair.a.hang_up(call);
    assert_eq!(pair.a.ended().await, End::HungUp);
    assert!(matches!(pair.b.ended().await, End::Remote(_)));
    pair.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declined_call_ends_for_the_caller_as_declined() {
    for backend in backends() {
        let mut pair = Pair::start(backend).await;
        pair.a.call(&mri(&pair.b_id));
        pair.a.state(CallState::Ringing).await;
        let rings = pair.b.incoming().await;
        pair.b.decline(rings.call);
        let End::Remote(end) = pair.a.ended().await else {
            panic!("the caller's call ends as the far end's");
        };
        assert_eq!(end.code, Some(603), "{end:?}");
        pair.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_callers_cancel_ends_the_ring() {
    let mut pair = Pair::start(crate::support::Backend::Sqlite).await;
    pair.a.call(&mri(&pair.b_id));
    let call = pair.a.state(CallState::Ringing).await;
    let rings = pair.b.incoming().await;
    pair.a.hang_up(call);
    assert_eq!(pair.a.ended().await, End::HungUp);
    let end = pair.b.ring_ended().await;
    assert_eq!(end.code, Some(487), "{end:?}");
    // the ring is over: its answer comes too late
    let _ = rings;
    pair.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ring_nobody_answers_times_out_at_both_ends() {
    for backend in backends() {
        let mut pair = Pair::start_with(backend, |config| config.calling.ring_timeout = 5).await;
        pair.a.call(&mri(&pair.b_id));
        pair.a.state(CallState::Ringing).await;
        pair.b.incoming().await;
        let ring = pair.b.ring_ended().await;
        assert_eq!(ring.code, Some(487), "{ring:?}");
        assert_eq!(ring.phrase.as_deref(), Some("Call cancelled on timeout."));
        let End::Remote(end) = pair.a.ended().await else {
            panic!("the caller's call ends as the service's");
        };
        assert_eq!(end.code, Some(480), "{end:?}");
        pair.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_to_someone_with_no_calling_endpoint_rings_out() {
    let server = crate::trouter::server_with(crate::support::Backend::Sqlite, |config| {
        config.calling.ring_timeout = 1;
    })
    .await;
    let a_id = server.add_user("a@own.test", "Ada").await;
    let b_id = server.add_user("b@own.test", "Bert").await;
    let mut a = super::client(&server, &server, "a@own.test", &a_id, "Ada").await;
    a.call(&mri(&b_id));
    a.state(CallState::Ringing).await;
    assert!(matches!(a.ended().await, End::Remote(_)));
    server.stop().await;
}

/// A raw calling request with `upn`'s skypetoken.
async fn raw(server: &TestServer, upn: &str, method: reqwest::Method, path: &str, body: &Value) -> (u16, Value) {
    let session = server.session(upn).await;
    let token = session.skypetoken().await.unwrap();
    let response = reqwest::Client::new()
        .request(method, server.url(path))
        .header("x-skypetoken", token.token.expose())
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

fn start_body(from: &str, to: &str, callbacks: &str) -> Value {
    let link = |name: &str| format!("{callbacks}{name}/");
    json!({
        "conversationRequest": {
            "roster": {"type": "Delta", "rosterUpdate": link("rosterUpdate")},
            "links": {"conversationEnd": link("conversationEnd"), "conversationUpdate": link("conversationUpdate")},
        },
        "participants": {
            "from": {"id": from, "displayName": "Ada", "endpointId": "00000000-0000-4000-8000-0000000000aa",
                "participantId": "00000000-0000-4000-8000-0000000000ab", "languageId": "en-us"},
            "to": [{"id": to, "participantId": "00000000-0000-4000-8000-0000000000bb"}],
        },
        "groupChat": null,
        "callInvitation": {
            "callModalities": ["Audio"],
            "links": {"progress": link("progress"), "acceptance": link("acceptance"), "end": link("end"),
                "mediaRenegotiation": link("mediaRenegotiation")},
            "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
        },
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_needs_the_callers_own_identity_callbacks_and_a_real_callee() {
    let pair = Pair::start(crate::support::Backend::Sqlite).await;
    let server = &pair.server;
    let c_id = server.add_user("c@own.test", "Cora").await;
    let (a, b, c) = (mri(&pair.a_id), mri(&pair.b_id), mri(&c_id));
    let elsewhere = "https://own.example.test/trouter/v4/f/SomeoneElsesEndpoint00/callAgent/x/";
    let post = reqwest::Method::POST;
    let epconv = "/flightproxy/api/v2/epconv";
    // no credential, a bearer in place of the skypetoken
    let bare = reqwest::Client::new()
        .post(server.url(epconv))
        .json(&start_body(&a, &b, elsewhere))
        .send()
        .await
        .unwrap();
    assert_eq!(bare.status(), 401);
    // callbacks on an endpoint that isn't the caller's own
    let (status, _) = raw(
        server,
        "a@own.test",
        post.clone(),
        epconv,
        &start_body(&a, &b, elsewhere),
    )
    .await;
    assert_eq!(status, 403, "a callback on another endpoint");
    // someone else as the caller, a callee that doesn't exist, itself, a body that is no start
    let (status, _) = raw(
        server,
        "c@own.test",
        post.clone(),
        epconv,
        &start_body(&a, &b, elsewhere),
    )
    .await;
    assert_eq!(status, 403, "another account as the caller");
    let nobody = "8:orgid:00000000-0000-4000-8000-00000000dead";
    let (status, _) = raw(
        server,
        "c@own.test",
        post.clone(),
        epconv,
        &start_body(&c, nobody, elsewhere),
    )
    .await;
    assert_eq!(status, 404, "no such callee");
    let (status, _) = raw(
        server,
        "c@own.test",
        post.clone(),
        epconv,
        &start_body(&c, &c, elsewhere),
    )
    .await;
    assert_eq!(status, 400, "a call to oneself");
    let (status, _) = raw(server, "c@own.test", post.clone(), epconv, &json!({"nothing": true})).await;
    assert_eq!(status, 400);
    // a disabled account's live skypetoken places no call
    let session = server.session("c@own.test").await;
    let token = session.skypetoken().await.unwrap();
    ngteams_server::admin::set_disabled(&server.app.db, &*server.app.fanout, "c@own.test", true)
        .await
        .unwrap();
    let refused = reqwest::Client::new()
        .post(server.url(epconv))
        .header("x-skypetoken", token.token.expose())
        .json(&start_body(&c, &b, elsewhere))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 403);
    pair.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_calls_own_parties_operate_on_it() {
    let mut pair = Pair::start(crate::support::Backend::Sqlite).await;
    pair.server.add_user("c@own.test", "Cora").await;
    pair.a.call(&mri(&pair.b_id));
    pair.a.state(CallState::Ringing).await;
    pair.b.incoming().await;
    let calls = ngteams_server::calling::open_calls(&pair.server.app.db).await.unwrap();
    let [call] = calls.as_slice() else {
        panic!("one call: {calls:?}");
    };
    let conv = format!("/flightproxy/api/v2/ep/conv.own/conv/{}", call.conversation);
    let cc = "/flightproxy/api/v2/ep/cc.own/cc/v1";
    let server = &pair.server;
    for (method, path) in [
        (reqwest::Method::POST, format!("{conv}/leave")),
        (reqwest::Method::POST, format!("{conv}/updateEndpointState")),
        (reqwest::Method::POST, format!("{cc}/forked/{}/1/i1/1/attach", call.id)),
        (reqwest::Method::POST, format!("{cc}/incoming/{}/1/t/1/accept", call.id)),
        (
            reqwest::Method::DELETE,
            format!("{cc}/incoming/{}/1/t/1/reject", call.id),
        ),
        (
            reqwest::Method::POST,
            format!("{cc}/active/{}/1/a1/1/renegotiate", call.id),
        ),
        (reqwest::Method::POST, format!("{cc}/active/{}/1/a1/1/hold", call.id)),
    ] {
        let (status, _) = raw(server, "c@own.test", method, &path, &json!({})).await;
        assert_eq!(status, 404, "{path}: an outsider learns nothing of the call");
    }
    // the caller can't answer its own call; an unknown call or operation is not found
    let accept = format!("{cc}/incoming/{}/1/t/1/accept", call.id);
    let (status, _) = raw(server, "a@own.test", reqwest::Method::POST, &accept, &json!({})).await;
    assert_eq!(status, 403);
    let unknown = format!("{cc}/incoming/nothing/1/t/1/accept");
    let (status, _) = raw(server, "b@own.test", reqwest::Method::POST, &unknown, &json!({})).await;
    assert_eq!(status, 404);
    let (status, _) = raw(
        server,
        "a@own.test",
        reqwest::Method::POST,
        &format!("{conv}/nonsense"),
        &json!({}),
    )
    .await;
    assert_eq!(status, 404);
    // the call is untouched: it still rings and is answered
    let quiet = pair.b.events_within(Duration::from_millis(300)).await;
    assert!(quiet.is_empty(), "{quiet:#?}");
    pair.server.stop().await;
}

/// The `Event/Call` messages of `a`'s and `b`'s chat, the newest first, as `upn` reads them.
async fn call_events(server: &TestServer, upn: &str, id: &str, chat: &str) -> Vec<String> {
    let account = crate::trouter::Account::sign_in(server, upn, id).await;
    let reader = account.client(server);
    let page = teams_proto::chat::ChatReads::new(&reader)
        .messages(chat, 20)
        .await
        .unwrap();
    page.messages
        .iter()
        .filter(|message| message.message_type.as_str() == "Event/Call")
        .map(|message| message.content.clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_nobody_took_is_a_missed_call_in_the_chat_of_the_two() {
    let mut pair = Pair::start(crate::support::Backend::Sqlite).await;
    let (a, b) = (mri(&pair.a_id), mri(&pair.b_id));
    let account = crate::trouter::Account::sign_in(&pair.server, "a@own.test", &pair.a_id).await;
    let create =
        teams_proto::CreateChat::one_to_one(teams_proto::Mri::new(a.clone()), teams_proto::Mri::new(b.clone()));
    let created = account.client(&pair.server).write(create.unwrap()).await.unwrap();
    let body: Value = serde_json::from_slice(created.body()).unwrap();
    let chat = body["threadResource"]["id"].as_str().unwrap().to_owned();
    pair.a.call(&b);
    let call = pair.a.state(CallState::Ringing).await;
    pair.b.incoming().await;
    pair.a.hang_up(call);
    pair.b.ring_ended().await;
    let events = call_events(&pair.server, "b@own.test", &pair.b_id, &chat).await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(
        events[1].contains("<callEventType>callStarted</callEventType>"),
        "{events:?}"
    );
    // the caller alone in it: Teams words that "Call missed" for the callee
    assert!(
        events[0].starts_with("<ended/><partlist alt=\"\" count=\"1\">"),
        "{events:?}"
    );
    assert!(
        events[0].contains(&format!("<name>{a}</name>")) && !events[0].contains(&b),
        "{events:?}"
    );
    let parsed = teams_proto::chat::chat_event("Event/Call", &events[0]);
    assert!(
        matches!(&parsed, Some(teams_proto::chat::ChatEvent::CallEnded(end)) if end.participants == [a.clone()]),
        "{parsed:?}"
    );
    pair.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_is_signaled_between_clients_on_two_nodes() {
    let Some(redis) = crate::support::redis_url() else {
        return;
    };
    for backend in backends() {
        let url = redis.clone();
        let node_a = crate::trouter::server_with(backend, |config| {
            config.redis_url = Some(url);
            config.calling.ring_timeout = 3;
        })
        .await;
        let (shared, url) = (node_a.app.config.clone(), redis.clone());
        let node_b = crate::trouter::server_with(backend, |config| {
            config.database_url = shared.database_url;
            config.redis_url = Some(url);
            config.calling.ring_timeout = 3;
        })
        .await;
        let a_id = node_a.add_user("a@own.test", "Ada").await;
        let b_id = node_a.add_user("b@own.test", "Bert").await;
        // each client's socket is on one node and its requests go through the other
        let mut a = super::client(&node_a, &node_b, "a@own.test", &a_id, "Ada").await;
        let mut b = super::client(&node_b, &node_a, "b@own.test", &b_id, "Bert").await;
        a.call(&mri(&b_id));
        a.state(CallState::Ringing).await;
        let rings = b.incoming().await;
        b.answer(rings.call);
        b.state(CallState::Connected).await;
        assert_eq!(a.remote_media().await.blob, answer().blob);
        a.state(CallState::Connected).await;
        b.hang_up(rings.call);
        assert_eq!(b.ended().await, End::HungUp);
        let end = a.ended().await;
        assert!(matches!(end, End::Remote(_)), "{end:?}");
        // a ring nobody answers is ended once, by whichever node looks first
        a.call(&mri(&b_id));
        a.state(CallState::Ringing).await;
        b.incoming().await;
        assert_eq!(b.ring_ended().await.code, Some(487));
        assert!(matches!(a.ended().await, End::Remote(_)));
        let quiet = b.events_within(Duration::from_millis(1_200)).await;
        assert!(quiet.is_empty(), "the ring ended once: {quiet:#?}");
        node_b.stop().await;
        node_a.stop().await;
    }
}
