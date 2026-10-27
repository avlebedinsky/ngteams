//! The personal fence on the mock chat service (`teams-mock`, through the `test-support` mock origins): it logs every
//! send and denies none. A write to a chat with someone outside the lab goes through it where the lab fence denies it,
//! and so does one to a conversation the account isn't in, a channel and a new chat, with no member read; a read by
//! `POST` naming anyone goes out under its permit (the presence subscription's too); a plain request goes out logged,
//! whatever the classifier makes of it (a search's shape, a write's, one it can't read), where a lab transport refuses
//! the last two; the registration goes as its own-state entry; the whole engine sends a message to the outsider's chat.
//! The calls are `tests/calls.rs`'s. Every id and token here is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, Action, EngineOptions, Event, Parts};
use teams_fence::{
    DenyReason, FailClosed, Fence, FenceLog, LiveMembershipSource, MembershipSource, Mri, Roster, Scope, Target,
    WriteKind,
};
use teams_mock::{
    ABC_CHAT, AX_CHAT, CHANNEL, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, OUTSIDER, PRESENCE_BASE,
};
use teams_proto::chat::MessageText;
use teams_proto::{
    Author, Client, Credentials, Endpoint, EndpointRegistration, HttpError, HttpTransport, Method,
    PresenceSubscription, Refusal, Request as PlainRequest, Service, ShortProfileLookup, register,
};
use teams_store::{OutboxState, Store};

const MIDDLE_TIER: &str = "https://teams.microsoft.com/api/mt/part/amer-03";
const WAIT: Duration = Duration::from_secs(30);

/// One account's token, as its session would give it.
struct Tokens(String);

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

/// A fence log whose lines can be read back.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl Write for Lines {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Lines {
    fn entries(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

/// The personal fence of `own`, and its log.
fn personal(own: &str) -> (Arc<Fence>, Lines) {
    let lines = Lines::default();
    (
        Arc::new(Fence::personal(Mri::new(own), FenceLog::from_writer(lines.clone()))),
        lines,
    )
}

/// `who`'s transport to the mock under `fence`, with the middle tier's base.
fn http(mock: &ChatServiceMock, who: &str, fence: &Arc<Fence>) -> HttpTransport<Tokens> {
    let tokens = Arc::new(Tokens(mock.token(who).unwrap()));
    HttpTransport::with_mock_origin(tokens, Arc::clone(fence), CHAT_BASE, &mock.origin())
        .unwrap()
        .with_middle_tier(MIDDLE_TIER)
        .unwrap()
}

fn transport(mock: &ChatServiceMock, who: &str, fence: &Arc<Fence>) -> Arc<HttpTransport<Tokens>> {
    Arc::new(http(mock, who, fence))
}

fn message(id: &str) -> Target {
    Target::new(WriteKind::Message, Scope::Conversation { id: id.to_owned() })
}

async fn thread_reads(mock: &ChatServiceMock, id: &str) -> usize {
    mock.received_requests()
        .await
        .iter()
        .filter(|request| request.method.as_str() == "GET" && request.url.path().ends_with(&format!("threads/{id}")))
        .count()
}

#[tokio::test]
async fn a_chat_with_an_outsider_is_writable_through_the_personal_fence_and_not_the_lab_s() {
    let mock = ChatServiceMock::start().await.unwrap();
    let (fence, log) = personal(LAB_A);
    let permit = fence.check(message(AX_CHAT), &FailClosed).await.unwrap();
    assert_eq!(permit.target(), &message(AX_CHAT));
    assert_eq!(thread_reads(&mock, AX_CHAT).await, 0, "no member read");
    assert_eq!(log.entries().last().unwrap()["verdict"], "allow");

    // the lab fence: the outsider isn't a lab account
    let lab = Arc::new(Fence::new(
        Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
        FenceLog::from_writer(std::io::sink()),
    ));
    let lab_transport = transport(&mock, LAB_A, &lab);
    let live = LiveMembershipSource::with_mock_origin(&mock.origin()).unwrap();
    live.set_credential_source(lab_transport);
    let denied = lab.check(message(AX_CHAT), &live).await.unwrap_err();
    assert_eq!(denied.reason(), &DenyReason::Outsiders { count: 1 });
}

#[tokio::test]
async fn a_conversation_the_account_isn_t_in_a_channel_and_a_new_chat_pass_logged() {
    let mock = ChatServiceMock::start().await.unwrap();
    // the outsider's fence: the a–b–c chat doesn't list it
    let (fence, log) = personal(OUTSIDER);
    let channel_post = Target::new(
        WriteKind::ChannelPost,
        Scope::Channel {
            team: CHANNEL.to_owned(),
            channel: CHANNEL.to_owned(),
        },
    );
    let create = Target::new(WriteKind::ChatCreate, Scope::New).with_added([Mri::new(OUTSIDER), Mri::new(LAB_A)]);
    for target in [message(ABC_CHAT), message(CHANNEL), channel_post, create] {
        assert!(fence.check(target.clone(), &FailClosed).await.is_ok(), "{target}");
    }
    assert!(mock.received_requests().await.is_empty(), "nothing read");
    let entries = log.entries();
    assert_eq!(entries.len(), 4);
    assert!(entries.iter().all(|entry| entry["verdict"] == "allow"), "{entries:?}");
}

/// A people search's shape: a `POST` to the middle tier listing other tenant users. Never mapped, never sent.
struct PeopleSearch;

impl PlainRequest for PeopleSearch {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["users", "searchV2"])
    }

    fn body(&self) -> Option<Value> {
        Some(json!({"queryText": "a"}))
    }
}

async fn lookups(mock: &ChatServiceMock) -> usize {
    mock.received_requests()
        .await
        .iter()
        .filter(|request| request.url.path().contains("/api/mt/"))
        .count()
}

/// A plain `POST` to the chat service at `segments` with `body`: a write's shape or one no row maps.
struct ChatPost(Vec<String>, Value);

impl PlainRequest for ChatPost {
    fn service(&self) -> Service {
        Service::ChatService
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(self.0.clone())
    }

    fn body(&self) -> Option<Value> {
        Some(self.1.clone())
    }
}

/// The log's lines for requests sent to a path ending in `last`.
fn sent_to(log: &Lines, last: &str) -> Vec<Value> {
    log.entries()
        .into_iter()
        .filter(|entry| entry["path"].as_str().is_some_and(|path| path.ends_with(last)))
        .collect()
}

#[tokio::test]
async fn a_read_by_post_names_anyone_and_every_plain_request_goes_out_logged() {
    let mock = ChatServiceMock::start().await.unwrap();
    let (fence, log) = personal(LAB_A);
    let client = Client::new(http(&mock, LAB_A, &fence));

    // someone of none of the account's chats: it goes out, under its permit (the mock doesn't serve the middle tier,
    // so it answers with a refusal: the request was sent)
    let stranger = client.lookup(ShortProfileLookup::new([Mri::new(LAB_C)]).unwrap()).await;
    assert!(matches!(&stranger, Err(HttpError::Status(_))), "{stranger:?}");
    assert_eq!(lookups(&mock).await, 1);
    assert!(
        log.entries()
            .iter()
            .any(|entry| entry["post_read"].is_string() && entry["verdict"] == "allow")
    );
    // a search's shape, as a plain request: it goes out too (sent once more after the mock's refusal, as a read is),
    // every attempt logged by its method, host and path before it went
    let search = client.send(&PeopleSearch).await;
    assert!(matches!(&search, Err(HttpError::Status(_))), "{search:?}");
    let attempts = lookups(&mock).await - 1;
    assert!(attempts >= 1);
    let logged = sent_to(&log, "searchV2");
    assert_eq!(logged.len(), attempts, "{logged:?}");
    assert!(
        logged
            .iter()
            .all(|entry| entry["method"] == "POST" && entry["event"] == "allowed")
    );

    // a write's shape as a plain request, and one the classifier can't read: sent, logged with what it made of them
    let post = ChatPost(
        ["v1", "users", "ME", "conversations", AX_CHAT, "messages"]
            .map(str::to_owned)
            .to_vec(),
        json!({"content": "made up", "messagetype": "Text", "contenttype": "text", "clientmessageid": "1"}),
    );
    let unreadable = ChatPost(["v1", "made-up"].map(str::to_owned).to_vec(), json!({"made": "up"}));
    let _ = client.send(&post).await;
    let _ = client.send(&unreadable).await;
    assert_eq!(posts_to(&mock, "/messages").await, 1);
    assert_eq!(posts_to(&mock, "/made-up").await, 1);
    for (last, contact) in [("messages", "write"), ("made-up", "unreadable")] {
        let lines = sent_to(&log, last);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0]["event"], "sent_unjudged");
        assert_eq!(lines[0]["contact"], contact);
        assert!(lines[0].get("body").is_none() && lines[0].get("query").is_none());
    }

    // a lab transport refuses both, sending nothing
    let lab = Arc::new(Fence::new(Roster::new([], []), FenceLog::from_writer(std::io::sink())));
    let lab_client = Client::new(http(&mock, LAB_A, &lab));
    assert!(matches!(
        lab_client.send(&post).await,
        Err(HttpError::Refused(Refusal::Write))
    ));
    assert!(matches!(
        lab_client.send(&unreadable).await,
        Err(HttpError::Refused(Refusal::Unreadable))
    ));
    assert_eq!(posts_to(&mock, "/messages").await, 1);
    assert_eq!(posts_to(&mock, "/made-up").await, 1);
}

/// How many `POST`s the mock received to a path ending in `last`.
async fn posts_to(mock: &ChatServiceMock, last: &str) -> usize {
    mock.received_requests()
        .await
        .iter()
        .filter(|request| request.method.as_str() == "POST" && request.url.path().ends_with(last))
        .count()
}

#[tokio::test]
async fn a_presence_subscription_names_anyone() {
    let mock = ChatServiceMock::start().await.unwrap();
    mock.serve_presence_subscriptions(&[]).await;
    let (fence, log) = personal(LAB_A);
    let client = Client::new(http(&mock, LAB_A, &fence).with_presence(PRESENCE_BASE).unwrap());
    let push_url = "https://pub-ent-euwe-01-f.trouter.teams.microsoft.com:3443/v4/f/made-up_endpoint-id/";
    let registration = EndpointRegistration::messaging("00000000-0000-4000-8000-0000000000e1", push_url).unwrap();
    let (_, endpoint) = register(client.transport(), &registration).await.unwrap();
    let endpoint = endpoint.unwrap();
    let subscriptions = || async {
        mock.received_requests()
            .await
            .iter()
            .filter(|request| request.url.path().contains("/pubsub/subscriptions/"))
            .count()
    };

    // someone outside the lab: it goes out under its permit, logged by its kind
    let subscription =
        PresenceSubscription::new(&endpoint, push_url, true, [Mri::new(OUTSIDER), Mri::new(LAB_B)], []).unwrap();
    let answer = client.lookup(subscription.clone()).await.unwrap();
    let statuses = subscription.read_answer(answer.body()).unwrap();
    assert!(statuses.iter().all(|status| status.taken), "{statuses:?}");
    assert_eq!(subscriptions().await, 1);
    assert!(
        log.entries()
            .iter()
            .any(|entry| { entry["post_read"] == "presence_subscription" && entry["verdict"] == "allow" })
    );

    // anyone at all beside them
    let others = PresenceSubscription::new(&endpoint, push_url, false, [Mri::new(LAB_B), Mri::new(LAB_C)], []).unwrap();
    client.lookup(others).await.unwrap();
    assert_eq!(subscriptions().await, 2);
}

#[tokio::test]
async fn the_registration_goes_as_its_own_state_entry_logged_by_its_kind() {
    let mock = ChatServiceMock::start().await.unwrap();
    let (fence, log) = personal(LAB_A);
    let transport = transport(&mock, LAB_A, &fence);
    let registration = EndpointRegistration::messaging(
        "00000000-0000-4000-8000-0000000000e1",
        "https://pub-ent-euwe-01-t.trouter.teams.microsoft.com/v4/f/made-up_endpoint-id/",
    )
    .unwrap();
    let (answer, endpoint) = register(&*transport, &registration).await.unwrap();
    assert!(answer.is_success(), "{answer:?}");
    assert!(endpoint.is_some_and(|endpoint| endpoint.is_for(&Mri::new(LAB_A))));
    let entries = log.entries();
    assert!(
        entries
            .iter()
            .any(|entry| entry["own"] == "endpoint_registration" && entry["verdict"] == "allow"),
        "{entries:?}"
    );
    assert!(transport.is_personal());
    // its push channel's frames are logged too (a lab transport's aren't)
    assert!(teams_proto::Transport::frame_log(&*transport).is_some());
    let lab = Arc::new(Fence::new(Roster::new([], []), FenceLog::from_writer(std::io::sink())));
    assert!(teams_proto::Transport::frame_log(&*lab_transport(&mock, &lab)).is_none());
}

fn lab_transport(mock: &ChatServiceMock, fence: &Arc<Fence>) -> Arc<HttpTransport<Tokens>> {
    transport(mock, LAB_A, fence)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_engine_sends_a_message_to_the_outsider_s_chat_through_the_personal_fence() {
    let mock = ChatServiceMock::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (fence, log) = personal(LAB_A);
    let transport = transport(&mock, LAB_A, &fence);
    let membership: Arc<dyn MembershipSource + Send> = Arc::new(FailClosed);
    let client = Arc::new(Client::new(http(&mock, LAB_A, &fence)));
    let outbox = Outbox::new(transport, fence, membership, Author::new(Mri::new(LAB_A), "a"));
    let mut handle = engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("store.sqlite")).unwrap(),
        client,
        outbox,
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap();
    let row = handle
        .requests()
        .write(Action::Send {
            conversation: AX_CHAT.to_owned(),
            text: MessageText::plain("hello from the personal build").unwrap(),
        })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let event = tokio::time::timeout_at(deadline, handle.next_event())
            .await
            .expect("the row settled in time")
            .expect("the engine runs");
        if let Event::Outbox(entry) = event
            && entry.id == row.id
            && entry.state != OutboxState::Pending
            && entry.state != OutboxState::Sending
        {
            assert_eq!(entry.state, OutboxState::Sent, "{entry:?}");
            break;
        }
    }
    let told = mock.take_events(OUTSIDER);
    assert!(
        told.iter()
            .any(|event| event.conversation == AX_CHAT && event.from == LAB_A),
        "{told:?}"
    );
    assert!(
        log.entries()
            .iter()
            .any(|entry| entry["kind"] == "message" && entry["verdict"] == "allow")
    );
    handle.shutdown();
}
