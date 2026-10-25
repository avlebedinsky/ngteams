//! The calling stack's socket against `teams-mock`'s Trouter (through the `test-support` mock origins): its own
//! connect, authentication and `SkypeSpacesWeb` registration (a plain request logged `allowed` through a lab fence; the
//! personal build's shape, its own-state entry through `Transport::send_own` and its frames through the logging sink),
//! the call agent's deliveries handed to their consumer once the fence read them, an invitation the fence doesn't offer
//! ignored and never answered, a delivery answered only by a typed response the fence permits for a registered or
//! offered call on the connection it came on, and a dropped socket or a token renewal that reconnects to the same
//! endpoint, registers again and keeps the call's links (`docs/protocol/calling.md`, "A call held past a dropped
//! socket").
//!
//! Every id, token and MRI here is the mocks' made-up one.

// test code throughout (its helpers unwrap and panic like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_fence::{
    CALL_START_URL, Contact, DeliveryDenyReason, FailClosed, Fence, FenceLog, Membership, MembershipFuture,
    MembershipSource, Mri, Observed, PartialReason, Policy, Request, Roster, Scope, TEST_CALL_SCENARIO, contacting,
};
use teams_mock::{AB_CHAT, CALLING_APP, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, OUTSIDER, SocketRecord, TrouterMock};
use teams_proto::{
    AcceptanceAcknowledgement, Authorized, Credentials, HttpTransport, IC3_AUDIENCE, InvitationReceipt, Links,
    Mutating, OwnState, Read, Request as PlainRequest, Response, Transport,
};
use teams_trouter::{
    CallDelivery, CallStack, Down, Ignored, Options, PushEvent, RegistrationReport, RespondFailure, Trouter,
    TrouterCredentials, TrouterHandle,
};
use tokio::sync::broadcast;

/// How long a test waits for an event before it fails.
const WAIT: Duration = Duration::from_secs(15);

/// The echo service, an approved service participant of the tests' roster.
const ECHO: &str = "28:00000000-0000-4000-8000-0000000000e0";

/// The a–b group chat a group call's invitation names.
const GROUP_CHAT: &str = "19:0123456789abcdef0123456789abcdef@thread.v2";

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

/// A `fence.log` the test reads back.
#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl SharedLog {
    fn lines(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Write for SharedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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
    log: SharedLog,
    fence: Arc<Fence>,
    tokens: Arc<Tokens>,
}

impl Lab {
    /// b's lab: b's fence (b its own account, a and b lab accounts, the echo service approved) logging to the lab's log.
    async fn start() -> Self {
        let mock = ChatServiceMock::start().await.unwrap();
        let trouter = mock.start_trouter().unwrap();
        let log = SharedLog::default();
        let fence = Arc::new(
            Fence::new(
                Roster::new([Mri::new(LAB_A), Mri::new(LAB_B)], [Mri::new(ECHO)]),
                FenceLog::from_writer(log.clone()),
            )
            .with_own_account(Mri::new(LAB_B)),
        );
        let tokens = Tokens::new(&mock.token(LAB_B).unwrap());
        Self {
            mock,
            trouter,
            log,
            fence,
            tokens,
        }
    }

    fn transport(&self) -> Arc<HttpTransport<Tokens>> {
        Arc::new(
            HttpTransport::with_mock_origin(
                Arc::clone(&self.tokens),
                Arc::clone(&self.fence),
                CHAT_BASE,
                &self.mock.origin(),
            )
            .unwrap(),
        )
    }

    /// b's calling socket over `transport`.
    fn calling_over<T: Transport + Send + 'static>(&self, transport: Arc<T>) -> Trouter<T, Tokens> {
        self.calling_from(transport, Arc::new(FailClosed))
    }

    /// b's calling socket over `transport`, its fence reading an offered group call's chat from `source`.
    fn calling_from<T: Transport + Send + 'static>(
        &self,
        transport: Arc<T>,
        source: Arc<dyn MembershipSource + Send>,
    ) -> Trouter<T, Tokens> {
        Trouter::with_mock_origin(transport, Arc::clone(&self.tokens), self.trouter.origin())
            .unwrap()
            .with_options(options())
            .for_calls(CallStack::new(Arc::clone(&self.fence), source))
            .unwrap()
    }

    /// b's calling socket over its lab transport.
    fn calling(&self) -> Trouter<HttpTransport<Tokens>, Tokens> {
        self.calling_over(self.transport())
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

async fn connected(push: &mut TrouterHandle) -> String {
    next(push, |event| match event {
        PushEvent::Connected { endpoint, .. } => Some(endpoint),
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

/// b's test call start, its callbacks on b's calling endpoint (the echo-call capture's shape, as
/// `crates/teams-fence/tests/calls.rs` writes it).
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

/// Registers b's test call on b's fence, as the transport does around the start's send: permitted, pending, then
/// registered from the start's answer.
async fn register_test_call(fence: &Fence, endpoint: &str) {
    let body = start_body(endpoint);
    let policy = Policy::new().with_echo_callee(Mri::new(ECHO));
    let Contact::Write(target) = contacting(&Request::new("POST", CALL_START_URL).with_body(Some(&body)), &policy)
    else {
        panic!("the start isn't a write");
    };
    let permit = fence.check(target, &FailClosed).await.unwrap();
    let pending = fence.pend_start(&permit, &body).expect("pending");
    let conversation = format!("{PROXY}/conv-zz-01-prod-aks.conv.skype.com/conv/conv-1");
    let at = |op: &str| format!("{conversation}/{op}?i=10-0-0-1&e=1");
    let answer = json!({
        "conversationController": format!("{conversation}?i=10-0-0-1&e=1"),
        "links": {"leave": at("leave"), "updateEndpointMetadata": at("updateEndpointMetadata")},
    });
    fence.register_start(pending, answer.to_string().as_bytes()).unwrap();
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
    // the registration: the calling stack's app, the endpoint's push URL, a plain request logged `allowed` first
    let registrations = lab.trouter.registrations();
    assert_eq!(registrations.len(), 1, "{registrations:?}");
    assert_eq!(registrations[0].app_id, CALLING_APP);
    assert_eq!(registrations[0].registration_id, epid);
    assert_eq!(registrations[0].path, push_url(&endpoint));
    assert!(
        lab.log
            .lines()
            .iter()
            .any(|line| line["event"] == "allowed" && line["path"] == "/registrar/prod/V2/registrations")
    );
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
async fn an_offered_invitation_is_handed_on_and_gets_its_receipt_once_through_the_fence() {
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
    assert_eq!(
        invitation.respond(receipt).await,
        Err(RespondFailure::NotPending),
        "a delivery is answered once"
    );
    let sent = responses_after(&lab.trouter, 1).await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["id"], id);
    assert_eq!(sent[0]["status"], 200);
    assert_eq!(sent[0]["body"], "");
    assert_eq!(
        sent[0]["headers"]["trouter-request"].as_str(),
        invitation.delivery.trouter_request.as_deref()
    );
    let lines = lab.log.lines();
    assert!(
        lines
            .iter()
            .any(|line| line["response"] == "call_invitation_receipt" && line["verdict"] == "allow"),
        "{lines:?}"
    );
    push.stop().await;
}

#[tokio::test]
async fn an_invitation_from_outside_the_lab_is_ignored_never_handed_on_and_never_answered() {
    let lab = Lab::start().await;
    let mut push = lab.calling().spawn();
    connected(&mut push).await;
    registered(&mut push).await;

    let _ = lab.trouter.deliver_call_invitation(LAB_B, OUTSIDER);
    let ignored = next(&mut push, |event| match event {
        PushEvent::Ignored(ignored) => Some(ignored),
        PushEvent::Call(delivery) => panic!("an outsider's invitation was handed on: {delivery:?}"),
        _ => None,
    })
    .await;
    assert_eq!(ignored, Ignored::IncomingCall);
    // an invitation on b's socket addressed to another account (a lab account's, to a) isn't offered either, nor one
    // the fence can't read (an outsider's meeting invitation, a call it replaces)
    let elsewhere = TrouterMock::call_invitation(LAB_B, LAB_A, |_| {});
    let meeting = TrouterMock::call_invitation(OUTSIDER, LAB_B, |gp| gp["groupContext"] = json!({"id": "meeting"}));
    let replacing = TrouterMock::call_invitation(LAB_A, LAB_B, |gp| {
        gp["callNotification"]["replaces"] = json!("another-call");
    });
    for body in [elsewhere, meeting, replacing] {
        let _ = lab.trouter.deliver_call(LAB_B, "", &body);
        let ignored = next(&mut push, |event| match event {
            PushEvent::Ignored(ignored) => Some(ignored),
            PushEvent::Call(delivery) => panic!("an invitation the fence doesn't offer was handed on: {delivery:?}"),
            _ => None,
        })
        .await;
        assert_eq!(ignored, Ignored::IncomingCall);
    }
    let id = lab.trouter.deliver_call_invitation(LAB_B, LAB_A);
    let offered = call_delivery(&mut push).await;
    assert_eq!(
        offered.delivery.id, id,
        "only the lab caller's invitation to b was handed on"
    );
    // a ping afterwards shows the socket read on, and nothing answered any invitation
    let before = lab.trouter.sockets()[0].frames.len();
    sockets_until(&lab.trouter, |sockets| sockets[0].frames.len() > before).await;
    assert!(responses(&lab.trouter).is_empty());
    push.stop().await;
}

/// The a–b group chat, read in full only once the test lets the read finish.
struct HeldChat {
    reading: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}

impl Default for HeldChat {
    fn default() -> Self {
        Self {
            reading: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}

impl MembershipSource for HeldChat {
    fn members<'a>(&'a self, scope: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(async move {
            self.reading.notify_one();
            let _held = self.release.acquire().await;
            match scope {
                Scope::Conversation { id } if id == GROUP_CHAT => {
                    Membership::Complete(vec![Mri::new(LAB_A), Mri::new(LAB_B)])
                }
                _ => Membership::Partial(PartialReason::NoSource),
            }
        })
    }
}

#[tokio::test]
async fn the_calling_socket_reads_on_while_the_fence_decides_a_response() {
    let lab = Lab::start().await;
    let chat = Arc::new(HeldChat::default());
    let mut push = lab.calling_from(lab.transport(), chat.clone()).spawn();
    connected(&mut push).await;
    registered(&mut push).await;

    let group = TrouterMock::group_call_invitation(LAB_A, LAB_B, GROUP_CHAT, |_| {});
    let id = lab.trouter.deliver_call(LAB_B, "", &group);
    let invitation = call_delivery(&mut push).await;
    assert_eq!((invitation.delivery.id, invitation.observed), (id, Observed::Offered));
    let responder = invitation.responder();
    let receipt = InvitationReceipt::new(&invitation.delivery, 2).unwrap();
    let responding = tokio::spawn(async move { responder.respond(receipt).await });
    tokio::time::timeout(WAIT, chat.reading.notified())
        .await
        .expect("the fence didn't read the group chat");
    // while the fence reads the chat the socket reads on: a delivery that comes in the while is read and handed on
    let _ = lab.trouter.deliver_call_invitation(LAB_B, OUTSIDER);
    let ignored = next(&mut push, |event| match event {
        PushEvent::Ignored(ignored) => Some(ignored),
        _ => None,
    })
    .await;
    assert_eq!(ignored, Ignored::IncomingCall);
    assert!(
        responses(&lab.trouter).is_empty(),
        "nothing sent before the fence decided"
    );
    chat.release.add_permits(1);
    tokio::time::timeout(WAIT, responding)
        .await
        .expect("the response wasn't decided in time")
        .unwrap()
        .unwrap();
    let sent = responses_after(&lab.trouter, 1).await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["id"], id);
    push.stop().await;
}

#[tokio::test]
async fn a_registered_call_s_acceptance_reaches_its_consumer_and_only_it_is_acknowledged() {
    let lab = Lab::start().await;
    let mut push = lab.calling().spawn();
    let endpoint = connected(&mut push).await;
    registered(&mut push).await;
    register_test_call(&lab.fence, &endpoint).await;

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
    assert_eq!(
        update
            .respond(InvitationReceipt::new(&update.delivery, 1).unwrap())
            .await,
        Err(RespondFailure::NotPending)
    );

    let (_, at) = callback(&endpoint, "03030303", "call", "acceptance");
    let id = lab.trouter.deliver_call(LAB_B, &at, &acceptance());
    let accepted = call_delivery(&mut push).await;
    assert_eq!((accepted.delivery.id, accepted.observed), (id, Observed::Acceptance));
    // an acknowledgement handing out another endpoint's callback is denied, unsent
    let foreign = Links::new().with(
        "mediaRenegotiation",
        callback("SomeoneElsesEndpoint0", "08080808", "call", "mediaRenegotiation").0,
    );
    let wrong = AcceptanceAcknowledgement::new(&accepted.delivery, "cv.1", 3, foreign).unwrap();
    assert_eq!(
        accepted.respond(wrong).await,
        Err(RespondFailure::Denied(DeliveryDenyReason::Shape))
    );
    assert!(responses(&lab.trouter).is_empty());

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
    register_test_call(&lab.fence, &endpoint).await;
    // an invitation delivered on the first connection, not yet answered
    let _ = lab.trouter.deliver_call_invitation(LAB_B, LAB_A);
    let invitation = call_delivery(&mut push).await;
    assert_eq!(invitation.observed, Observed::Offered);

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

/// A transport in the personal build's shape over a lab one: its frames logged to `fence.log`
/// (`Transport::frame_log`), its own-state entries counted on their way through `Transport::send_own`.
struct PersonalShaped {
    inner: Arc<HttpTransport<Tokens>>,
    fence: Arc<Fence>,
    own: AtomicUsize,
}

impl Transport for PersonalShaped {
    type Error = teams_proto::HttpError;

    fn read<R: Read>(&self, request: &R) -> impl Future<Output = Result<Response, Self::Error>> + Send {
        self.inner.read(request)
    }

    fn send<R: PlainRequest>(&self, _request: &R) -> impl Future<Output = Result<Response, Self::Error>> + Send {
        future::ready(Err(teams_proto::HttpError::Refused(teams_proto::Refusal::Personal)))
    }

    fn send_own<R: OwnState + PlainRequest + Sync>(
        &self,
        request: &R,
    ) -> impl Future<Output = Result<Response, Self::Error>> + Send {
        assert_eq!(R::KIND, "calling_endpoint_registration");
        self.own.fetch_add(1, Ordering::SeqCst);
        self.inner.send(request)
    }

    fn deliver<R: Mutating + Send>(
        &self,
        write: Authorized<R>,
    ) -> impl Future<Output = Result<Response, Self::Error>> + Send {
        self.inner.deliver(write)
    }

    fn frame_log(&self) -> Option<Arc<Fence>> {
        Some(Arc::clone(&self.fence))
    }
}

#[tokio::test]
async fn in_the_personal_shape_the_registration_goes_as_its_own_state_entry_and_every_frame_is_logged() {
    let lab = Lab::start().await;
    let transport = Arc::new(PersonalShaped {
        inner: lab.transport(),
        fence: Arc::clone(&lab.fence),
        own: AtomicUsize::new(0),
    });
    let mut push = lab.calling_over(Arc::clone(&transport)).spawn();
    connected(&mut push).await;
    registered(&mut push).await;
    assert_eq!(transport.own.load(Ordering::SeqCst), 1, "through send_own, never send");

    let _ = lab.trouter.deliver_call_invitation(LAB_B, LAB_A);
    let invitation = call_delivery(&mut push).await;
    invitation
        .respond(InvitationReceipt::new(&invitation.delivery, 2).unwrap())
        .await
        .unwrap();
    push.stop().await;
    sockets_until(&lab.trouter, |sockets| sockets.iter().all(|socket| !socket.open)).await;

    let frames: Vec<String> = lab
        .log
        .lines()
        .into_iter()
        .filter(|line| line["event"] == "frame")
        .map(|line| line["own"].as_str().unwrap_or_default().to_owned())
        .collect();
    for kind in ["trouter_authenticate", "call_invitation_receipt", "trouter_disconnect"] {
        assert!(
            frames.iter().any(|logged| logged == kind),
            "{kind} not logged: {frames:?}"
        );
    }
    let sent = lab.trouter.sockets()[0].frames.len();
    assert_eq!(frames.len(), sent, "every frame the socket sent was logged first");
}
