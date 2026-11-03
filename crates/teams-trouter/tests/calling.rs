//! The calling stack's socket against the synthetic Trouter: authentication, `SkypeSpacesWeb` registration,
//! invitations filtered by the account tracker, callback delivery and typed responses on their originating
//! connection. Reconnection preserves the endpoint and the tracker's call bindings.
//!
//! Every id, token and MRI here is the mocks' made-up one.

// test code throughout (its helpers unwrap and panic like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_call::Observed;
use teams_mock::{AB_CHAT, CALLING_APP, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, SocketRecord, TrouterMock};
use teams_proto::Mri;
use teams_proto::{
    AcceptanceAcknowledgement, CALL_START_URL, Credentials, HttpTransport, IC3_AUDIENCE, InvitationReceipt, Links,
    Response, TEST_CALL_SCENARIO,
};
use teams_trouter::{
    CallDelivery, CallStack, Down, Options, PushEvent, RegistrationReport, RespondFailure, Trouter, TrouterCredentials,
    TrouterHandle,
};
use tokio::sync::broadcast;

/// How long a test waits for an event before it fails.
const WAIT: Duration = Duration::from_secs(15);

/// The call agent the start hands out its callbacks under, on the calling endpoint.
const AGENT: &str = "callAgent/00000000-0000-4000-8000-0000000000a1";

/// b's tokens: the mock's, then a renewal the test announces.
struct Tokens {
    current: Mutex<AccessToken>,
    renewals: broadcast::Sender<Renewal>,
}

impl Tokens {
    fn new(token: &str) -> Arc<Self> {
        Arc::new(Self {
            current: Mutex::new(Self::token(token)),
            renewals: broadcast::channel(16).0,
        })
    }

    fn token(token: &str) -> AccessToken {
        AccessToken {
            token: Secret::new(token),
            expires_at: SystemTime::now() + Duration::from_secs(7_000),
            scopes: Vec::new(),
        }
    }

    fn current(&self) -> AccessToken {
        self.current.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Another part of the client renewed the token: `token` now, announced.
    fn renew(&self, token: &str) {
        let renewed = Self::token(token);
        let expires_at = renewed.expires_at;
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = renewed;
        let _ = self.renewals.send(Renewal::AccessToken {
            resource: IC3_AUDIENCE.to_owned(),
            expires_at,
        });
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.current()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.current()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(self.current().token))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(self.current().token))
    }
}

/// Short waits, so the tests run in seconds.
fn options() -> Options {
    Options {
        ping_interval: Duration::from_millis(300),
        pong_timeout: Duration::from_millis(1_500),
        connect_timeout: Duration::from_secs(5),
        hello_timeout: Duration::from_secs(5),
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
        backoff_jitter_percent: 0,
        renew_check: Duration::from_millis(50),
        ..Options::default()
    }
}

struct Lab {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    tracker: Arc<teams_call::Tracker>,
    tokens: Arc<Tokens>,
}

impl Lab {
    /// b's mock service, tracker and tokens.
    async fn start() -> Self {
        let mock = ChatServiceMock::start().await.unwrap();
        let trouter = mock.start_trouter().unwrap();
        let tracker = Arc::new(teams_call::Tracker::new(Mri::new(LAB_B)));
        let tokens = Tokens::new(&mock.token(LAB_B).unwrap());
        Self {
            mock,
            trouter,
            tracker,
            tokens,
        }
    }

    fn transport(&self) -> Arc<HttpTransport<Tokens>> {
        Arc::new(HttpTransport::with_mock_origin(Arc::clone(&self.tokens), CHAT_BASE, &self.mock.origin()).unwrap())
    }

    /// b's calling socket over its mock transport.
    fn calling(&self) -> Trouter<HttpTransport<Tokens>, Tokens> {
        let transport = self.transport();
        Trouter::with_mock_origin(transport, Arc::clone(&self.tokens), self.trouter.origin())
            .unwrap()
            .with_options(options())
            .for_calls(CallStack::new(Arc::clone(&self.tracker)))
    }
}

/// The next event `pick` takes, skipping others; fails after [`WAIT`].
async fn next<T>(push: &mut TrouterHandle, mut pick: impl FnMut(PushEvent) -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let event = tokio::time::timeout_at(deadline, push.next())
            .await
            .expect("the event didn't come in time")
            .expect("the client stopped");
        if let Some(picked) = pick(event) {
            return picked;
        }
    }
}

/// The endpoint of the next connect; its push URL (where the calls' callbacks go) is the endpoint's.
async fn connected(push: &mut TrouterHandle) -> String {
    next(push, |event| match event {
        PushEvent::Connected { endpoint, push_url, .. } => {
            assert!(
                push_url.starts_with("https://") && push_url.ends_with(&format!("/v4/f/{endpoint}/")),
                "{push_url}"
            );
            Some(endpoint)
        }
        _ => None,
    })
    .await
}

async fn registered(push: &mut TrouterHandle) {
    next(push, |event| match event {
        PushEvent::Registration(RegistrationReport::Registered) => Some(()),
        PushEvent::Registration(RegistrationReport::Failed(why)) => panic!("registration failed: {why}"),
        PushEvent::Registered(endpoint) => panic!("the calling registration registered an endpoint: {endpoint:?}"),
        _ => None,
    })
    .await;
}

async fn call_delivery(push: &mut TrouterHandle) -> CallDelivery {
    next(push, |event| match event {
        PushEvent::Call(delivery) => Some(delivery),
        _ => None,
    })
    .await
}

/// Waits until `check` holds of the mock's sockets.
async fn sockets_until(trouter: &TrouterMock, check: impl Fn(&[SocketRecord]) -> bool) -> Vec<SocketRecord> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let sockets = trouter.sockets();
        if check(&sockets) {
            return sockets;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the mock's sockets: {sockets:#?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Every delivery response (`3:::`) the client sent, on every socket.
fn responses(trouter: &TrouterMock) -> Vec<Value> {
    trouter
        .sockets()
        .iter()
        .flat_map(|socket| socket.frames.clone())
        .filter_map(|frame| frame.strip_prefix("3:::").map(str::to_owned))
        .map(|json| serde_json::from_str(&json).unwrap())
        .collect()
}

/// The client's delivery responses once the mock recorded `count` of them: a response written reaches the mock's
/// record a moment later.
async fn responses_after(trouter: &TrouterMock, count: usize) -> Vec<Value> {
    sockets_until(trouter, |_| responses(trouter).len() >= count).await;
    responses(trouter)
}

/// The calling endpoint's push URL as the mock hands it out.
fn push_url(endpoint: &str) -> String {
    format!(
        "https://{}-f.trouter.teams.microsoft.com:3443/v4/f/{endpoint}/",
        teams_mock::REGIONAL_NAME
    )
}

/// A callback under the calling endpoint's call agent: its URL and its path under the push URL.
fn callback(endpoint: &str, segment: &str, kind: &str, name: &str) -> (String, String) {
    let path = format!("{AGENT}/{segment}/{kind}/{name}/");
    (format!("{}{path}", push_url(endpoint)), path)
}

/// b's test call start, its callbacks on b's calling endpoint, in the synthetic echo-call shape.
fn start_body(endpoint: &str) -> Vec<u8> {
    let at = |segment: &str, kind: &str, name: &str| callback(endpoint, segment, kind, name).0;
    json!({
        "conversationRequest": {
            "conversationType": null,
            "suppressDialout": true,
            "links": {
                "conversationEnd": at("05050505", "conversation", "conversationEnd"),
                "conversationUpdate": at("06060606", "conversation", "conversationUpdate"),
            },
            "scenario": TEST_CALL_SCENARIO,
        },
        "participants": {"from": {"id": LAB_B, "displayName": "Lab B"}, "to": []},
        "groupChat": null,
        "callInvitation": {
            "callModalities": ["Audio"],
            "links": {
                "acceptance": at("03030303", "call", "acceptance"),
                "end": at("04040404", "call", "end"),
            },
            "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
        },
    })
    .to_string()
    .into_bytes()
}

const PROXY: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep";

/// Tracks b's test call around the start attempt and its successful answer.
fn register_test_call(tracker: &teams_call::Tracker, endpoint: &str) {
    struct Start {
        url: url::Url,
        body: Value,
    }
    impl teams_proto::Request for Start {
        fn service(&self) -> teams_proto::Service {
            teams_proto::Service::Calling
        }
        fn method(&self) -> teams_proto::Method {
            teams_proto::Method::Post
        }
        fn endpoint(&self) -> teams_proto::Endpoint {
            teams_proto::Endpoint::new([] as [&str; 0])
        }
        fn body(&self) -> Option<Value> {
            Some(self.body.clone())
        }
        fn call_url(&self) -> Option<&url::Url> {
            Some(&self.url)
        }
    }
    let request = Start {
        url: url::Url::parse(CALL_START_URL).unwrap(),
        body: serde_json::from_slice(&start_body(endpoint)).unwrap(),
    };
    let pending = tracker.before(&request);
    let conversation = format!("{PROXY}/conv-zz-01-prod-aks.conv.skype.com/conv/conv-1");
    let at = |op: &str| format!("{conversation}/{op}?i=10-0-0-1&e=1");
    let answer = json!({"conversationController":format!("{conversation}?i=10-0-0-1&e=1"),"links":{"leave":at("leave"),"updateEndpointMetadata":at("updateEndpointMetadata")}});
    tracker.answered(pending, &Response::new(200, answer.to_string()));
}

/// The call controller's acceptance of b's call.
fn acceptance() -> Value {
    let cc = format!("{PROXY}/cc-zz-01-prod-aks.cc.skype.com/cc/v1");
    json!({"callAcceptance": {
        "acceptedCallModalities": [],
        "links": {
            "acknowledgement": format!("{cc}/callAcceptance/call-1/1/a1/1/acknowledge?i=10-0-0-2"),
            "applyChannelParameters": format!("{cc}/mcProxy/call-1/1/k1/1/applyChannelParameters?i=10-0-0-2&e=1"),
        },
        "callKeepAliveInterval": 2700,
    }})
}

/// b's acknowledgement of `delivery`, handing out a callback on b's endpoint.
fn acknowledgement(delivery: &CallDelivery, endpoint: &str) -> AcceptanceAcknowledgement {
    let links = Links::new().with(
        "mediaRenegotiation",
        callback(endpoint, "08080808", "call", "mediaRenegotiation").0,
    );
    AcceptanceAcknowledgement::new(&delivery.delivery, "cv.1", 3, links).unwrap()
}

#[tokio::test]
async fn the_calling_socket_names_its_stack_authenticates_with_its_two_headers_and_registers_skype_spaces_web() {
    let lab = Lab::start().await;
    let client = lab.calling();
    let epid = client.endpoint_id().to_owned();
    let mut push = client.spawn();
    let endpoint = connected(&mut push).await;
    registered(&mut push).await;

    let sockets = lab.trouter.sockets();
    let [socket] = &sockets[..] else {
        panic!("one socket: {sockets:#?}");
    };
    assert!(socket.calling, "the connect names the calling stack");
    let query: Vec<(String, String)> = url::form_urlencoded::parse(socket.query.as_bytes())
        .into_owned()
        .collect();
    let names: Vec<&str> = query.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["tc", "timeout", "epid", "dom", "cor_id", "con_num"]);
    let tc: Value = serde_json::from_str(&query[0].1).unwrap();
    assert_eq!(
        (tc["ua"].as_str(), tc["v"].as_str()),
        (Some("SkypeSpaces"), Some(teams_proto::CLIENT_BUILD))
    );
    assert_eq!(
        (query[2].1.as_str(), query[3].1.as_str()),
        (epid.as_str(), "teams.microsoft.com")
    );
    let first: Value = serde_json::from_str(socket.frames[0].strip_prefix("5:::").unwrap()).unwrap();
    assert_eq!(
        first["args"][0]["headers"],
        json!({"Authorization": format!("Bearer {}", lab.mock.token(LAB_B).unwrap()), "X-MS-Migration": "True"})
    );
    // The registration names the calling stack's app and the endpoint's push URL.
    let registrations = lab.trouter.registrations();
    assert_eq!(registrations.len(), 1, "{registrations:?}");
    assert_eq!(registrations[0].app_id, CALLING_APP);
    assert_eq!(registrations[0].registration_id, epid);
    assert_eq!(registrations[0].path, push_url(&endpoint));
    // no chat event goes to the calling stack's endpoint: a message to b stays queued for a messaging endpoint
    lab.mock
        .post_as(LAB_A, AB_CHAT, "<p>to b's chats, not its calls</p>")
        .expect("a posts to the a–b chat");
    let before = lab.trouter.sockets()[0].frames.len();
    sockets_until(&lab.trouter, |sockets| sockets[0].frames.len() > before + 1).await;
    assert!(
        !lab.mock.take_events(LAB_B).is_empty(),
        "nothing taken by the calling socket"
    );

    push.stop().await;
    let sockets = sockets_until(&lab.trouter, |sockets| sockets.iter().all(|socket| !socket.open)).await;
    assert_eq!(sockets[0].frames.last().map(String::as_str), Some("0::"));
    assert!(responses(&lab.trouter).is_empty(), "nothing was answered");
}

#[tokio::test]
async fn an_offered_invitation_is_handed_on_and_gets_its_typed_receipt() {
    let lab = Lab::start().await;
    let mut push = lab.calling().spawn();
    connected(&mut push).await;
    registered(&mut push).await;

    let id = lab.trouter.deliver_call_invitation(LAB_B, LAB_A);
    let invitation = call_delivery(&mut push).await;
    assert_eq!(invitation.observed, Observed::Offered);
    assert_eq!(invitation.delivery.id, id);
    assert!(
        responses(&lab.trouter).is_empty(),
        "nothing answered before the consumer asked"
    );

    let receipt = InvitationReceipt::new(&invitation.delivery, 2).unwrap();
    invitation.respond(receipt.clone()).await.unwrap();
    let sent = responses_after(&lab.trouter, 1).await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["id"], id);
    assert_eq!(sent[0]["status"], 200);
    assert_eq!(sent[0]["body"], "");
    assert_eq!(
        sent[0]["headers"]["trouter-request"].as_str(),
        invitation.delivery.trouter_request.as_deref()
    );
    push.stop().await;
}

#[tokio::test]
async fn a_registered_call_s_acceptance_reaches_its_consumer_and_only_it_is_acknowledged() {
    let lab = Lab::start().await;
    let mut push = lab.calling().spawn();
    let endpoint = connected(&mut push).await;
    registered(&mut push).await;
    register_test_call(&lab.tracker, &endpoint);

    // the call agent's other deliveries reach the consumer too, and can't be answered
    let (_, roster) = callback(&endpoint, "06060606", "conversation", "conversationUpdate");
    let id = lab
        .trouter
        .deliver_call(LAB_B, &roster, &json!({"conversationUpdate": {}}));
    let update = call_delivery(&mut push).await;
    assert_eq!((update.delivery.id, update.observed), (id, Observed::Nothing));
    assert_eq!(
        update.delivery.body, br#"{"conversationUpdate":{}}"#,
        "its gzip body decoded"
    );
    // Typed responses are bound to their delivery connection.

    let (_, at) = callback(&endpoint, "03030303", "call", "acceptance");
    let id = lab.trouter.deliver_call(LAB_B, &at, &acceptance());
    let accepted = call_delivery(&mut push).await;
    assert_eq!((accepted.delivery.id, accepted.observed), (id, Observed::Acceptance));
    // the denied response took the pending delivery: the call controller's redelivery is answered
    let id = lab.trouter.deliver_call(LAB_B, &at, &acceptance());
    let again = call_delivery(&mut push).await;
    assert_eq!(again.observed, Observed::Acceptance);
    again.respond(acknowledgement(&again, &endpoint)).await.unwrap();
    let sent = responses_after(&lab.trouter, 1).await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(
        (sent[0]["id"].as_i64(), sent[0]["status"].as_i64()),
        (Some(id), Some(200))
    );
    let body: Value = serde_json::from_str(sent[0]["body"].as_str().unwrap()).unwrap();
    assert!(body["callAcceptanceAcknowledgement"]["links"]["mediaRenegotiation"].is_string());
    push.stop().await;
}

#[tokio::test]
async fn a_dropped_calling_socket_comes_back_on_the_same_endpoint_registers_again_and_keeps_the_call_s_links() {
    let lab = Lab::start().await;
    let mut push = lab.calling().spawn();
    let endpoint = connected(&mut push).await;
    registered(&mut push).await;
    // an invitation delivered on the first connection, not yet answered
    let _ = lab.trouter.deliver_call_invitation(LAB_B, LAB_A);
    let invitation = call_delivery(&mut push).await;
    assert_eq!(invitation.observed, Observed::Offered);
    register_test_call(&lab.tracker, &endpoint);

    lab.trouter.drop_sockets(LAB_B);
    let down = next(&mut push, |event| match event {
        PushEvent::Disconnected { down, .. } => Some(down),
        _ => None,
    })
    .await;
    assert!(matches!(down, Down::Closed), "{down:?}");
    let again = connected(&mut push).await;
    assert_eq!(
        again, endpoint,
        "the cached parameters bring back the same endpoint, so the call's links hold"
    );
    registered(&mut push).await;

    // the registration again: the same endpoint's push URL, the calling stack's
    let registrations = lab.trouter.registrations();
    assert_eq!(registrations.len(), 2, "{registrations:?}");
    assert!(
        registrations
            .iter()
            .all(|registration| registration.app_id == CALLING_APP && registration.path == push_url(&endpoint))
    );
    let sockets = lab.trouter.sockets();
    assert_eq!(sockets.len(), 2);
    assert!(
        sockets[1].reconnect,
        "the second socket sent the reconnect parameters back"
    );
    // a response to the first connection's delivery isn't sent on the second
    assert_eq!(
        invitation
            .respond(InvitationReceipt::new(&invitation.delivery, 2).unwrap())
            .await,
        Err(RespondFailure::Gone)
    );
    // the call's acceptance, at the link the start handed out before the drop, reaches the consumer on the new
    // connection and is acknowledged there
    let (_, at) = callback(&endpoint, "03030303", "call", "acceptance");
    let id = lab.trouter.deliver_call(LAB_B, &at, &acceptance());
    let accepted = call_delivery(&mut push).await;
    assert_eq!(accepted.observed, Observed::Acceptance);
    accepted.respond(acknowledgement(&accepted, &endpoint)).await.unwrap();
    let sockets = sockets_until(&lab.trouter, |sockets| {
        sockets[1].frames.iter().any(|frame| frame.starts_with("3:::"))
    })
    .await;
    assert!(
        sockets[0].frames.iter().all(|frame| !frame.starts_with("3:::")),
        "nothing answered on the dropped socket"
    );
    let sent = responses_after(&lab.trouter, 1).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["id"].as_i64(), Some(id));
    // the endpoint seen before got a message-loss report, acknowledged at once with nobody to catch up
    sockets_until(&lab.trouter, |sockets| {
        sockets[1]
            .frames
            .iter()
            .any(|frame| frame.contains("trouter.processed_message_loss"))
    })
    .await;
    // nothing went to a call: the registrar is the only service the client asked
    let requests = lab.mock.received_requests().await;
    assert!(
        requests
            .iter()
            .all(|request| request.url.path() == "/registrar/prod/V2/registrations"),
        "{:?}",
        requests.iter().map(|request| request.url.path()).collect::<Vec<_>>()
    );
    push.stop().await;
}

#[tokio::test]
async fn a_token_renewal_reconnects_the_calling_socket_with_the_new_token_on_the_same_endpoint() {
    let lab = Lab::start().await;
    let mut push = lab.calling().spawn();
    let endpoint = connected(&mut push).await;
    registered(&mut push).await;

    let renewed = "mock-ic3-token-renewed-b-calling";
    assert!(lab.mock.set_token(LAB_B, renewed));
    lab.tokens.renew(renewed);
    let down = next(&mut push, |event| match event {
        PushEvent::Disconnected { down, .. } => Some(down),
        _ => None,
    })
    .await;
    assert!(matches!(down, Down::Renewal), "{down:?}");
    assert_eq!(connected(&mut push).await, endpoint);
    registered(&mut push).await;

    let sockets = lab.trouter.sockets();
    assert_eq!(sockets.len(), 2);
    assert_eq!(sockets[0].frames.last().map(String::as_str), Some("0::"));
    let first: Value = serde_json::from_str(sockets[1].frames[0].strip_prefix("5:::").unwrap()).unwrap();
    assert_eq!(
        first["args"][0]["headers"],
        json!({"Authorization": format!("Bearer {renewed}"), "X-MS-Migration": "True"})
    );
    assert_eq!(first["args"][0]["connectparams"]["sr"], endpoint.as_str());
    let registrations = lab.trouter.registrations();
    assert_eq!(registrations.len(), 2);
    assert!(
        registrations
            .iter()
            .all(|registration| registration.path == push_url(&endpoint))
    );
    push.stop().await;
}

#[tokio::test]
async fn an_unregistered_delivery_can_receive_a_typed_response_on_its_connection() {
    let lab = Lab::start().await;
    let mut push = lab.calling().spawn();
    let endpoint = connected(&mut push).await;
    registered(&mut push).await;
    // A delivery at the call agent is handed on; its typed response goes on that same connection.
    let (_, roster) = callback(&endpoint, "06060606", "conversation", "conversationUpdate");
    let id = lab
        .trouter
        .deliver_call(LAB_B, &roster, &json!({"conversationUpdate": {}}));
    let update = call_delivery(&mut push).await;
    assert_eq!((update.delivery.id, update.observed), (id, Observed::Nothing));
    update
        .respond(InvitationReceipt::new(&update.delivery, 1).unwrap())
        .await
        .unwrap();
    let sent = responses_after(&lab.trouter, 1).await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["id"], id);
    push.stop().await;
}
