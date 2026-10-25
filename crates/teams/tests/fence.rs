//! The personal fence on the mock chat service (`teams-mock`, through the `test-support` mock origins): a write to a
//! chat with someone outside the lab goes through it where the lab fence denies it; a conversation the account isn't a
//! member of (by a fresh read), a target the personal build doesn't write and a partial read are denied; a read by
//! `POST` naming someone of the account's chats goes out under its permit and one naming anyone else doesn't; a plain
//! request (a search's shape) never goes out through the personal fence; the registration goes as its own-state entry;
//! the whole engine sends a message to the outsider's chat; and, until its calls are turned on, the personal build
//! opens no calling socket and sends no calling registration. Every id and token here is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, Action, EngineOptions, Event, Parts, Push};
use teams_fence::personal::PersonalMembershipSource;
use teams_fence::{
    ChatPeople, DenyReason, Fence, FenceLog, LiveMembershipSource, MembershipSource, Mri, PartialReason, Roster, Scope,
    Target, WriteKind,
};
use teams_mock::{ABC_CHAT, AX_CHAT, CHANNEL, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, OUTSIDER};
use teams_proto::chat::MessageText;
use teams_proto::{
    Author, Client, Credentials, Endpoint, EndpointRegistration, HttpError, HttpTransport, Method, Refusal,
    Request as PlainRequest, Service, ShortProfileLookup, register,
};
use teams_store::{OutboxState, Store};
use teams_trouter::{BuildError, CallStack, RegistrationReport, Trouter, TrouterCredentials};
use tokio::sync::broadcast;

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

/// The personal membership source reading with `transport`'s credential.
fn source(mock: &ChatServiceMock, transport: &Arc<HttpTransport<Tokens>>) -> PersonalMembershipSource {
    let source = PersonalMembershipSource::with_mock_origin(&mock.origin()).unwrap();
    source.set_credential_source(transport.clone());
    source
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
    let writes = transport(&mock, LAB_A, &fence);
    let permit = fence.check(message(AX_CHAT), &source(&mock, &writes)).await.unwrap();
    assert_eq!(permit.target(), &message(AX_CHAT));
    assert_eq!(
        thread_reads(&mock, AX_CHAT).await,
        1,
        "read fresh when the fence decided"
    );
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
async fn a_conversation_whose_fresh_read_doesn_t_list_the_account_is_denied() {
    let mock = ChatServiceMock::start().await.unwrap();
    // the outsider's fence, reading with a's credential: the a–b–c chat's thread lists a, b and c
    let (fence, _) = personal(OUTSIDER);
    let reader = transport(&mock, LAB_A, &fence);
    let denied = fence
        .check(message(ABC_CHAT), &source(&mock, &reader))
        .await
        .unwrap_err();
    assert_eq!(denied.reason(), &DenyReason::NotAMember);
    assert_eq!(thread_reads(&mock, ABC_CHAT).await, 1, "by a fresh read");
}

#[tokio::test]
async fn an_unknown_target_or_a_partial_read_is_denied() {
    let mock = ChatServiceMock::start().await.unwrap();
    let (fence, _) = personal(LAB_A);
    let transport = transport(&mock, LAB_A, &fence);
    let source = source(&mock, &transport);
    let channel = fence.check(message(CHANNEL), &source).await.unwrap_err();
    assert_eq!(channel.reason(), &DenyReason::NotPersonalScope);
    let create = Target::new(WriteKind::ChatCreate, Scope::New).with_added([Mri::new(OUTSIDER)]);
    assert_eq!(
        fence.check(create, &source).await.unwrap_err().reason(),
        &DenyReason::NotPersonalKind
    );
    mock.refuse_next("GET", AX_CHAT, 503).await;
    let partial = fence.check(message(AX_CHAT), &source).await.unwrap_err();
    assert_eq!(
        partial.reason(),
        &DenyReason::Partial(PartialReason::FetchFailed(teams_fence::FetchFailure::Status(503)))
    );
    // read again at the next decision: the answer isn't kept
    assert!(fence.check(message(AX_CHAT), &source).await.is_ok());
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

#[tokio::test]
async fn a_read_by_post_names_only_people_of_the_account_s_chats_and_a_search_never_goes_out() {
    let mock = ChatServiceMock::start().await.unwrap();
    let (fence, log) = personal(LAB_A);
    let people = Arc::new(ChatPeople::new());
    people.replace([Mri::new(OUTSIDER), Mri::new(LAB_B)]);
    let client = Client::new(http(&mock, LAB_A, &fence).with_chat_people(people));

    // someone of the account's chats who isn't a lab account: it goes out, under its permit (the mock doesn't serve
    // the middle tier, so it answers with a refusal: the request was sent)
    let outsider = client
        .lookup(ShortProfileLookup::new([Mri::new(OUTSIDER)]).unwrap())
        .await;
    assert!(matches!(&outsider, Err(HttpError::Status(_))), "{outsider:?}");
    assert_eq!(lookups(&mock).await, 1);
    assert!(
        log.entries()
            .iter()
            .any(|entry| entry["post_read"].is_string() && entry["verdict"] == "allow")
    );

    // someone of none of its chats: denied, nothing sent
    let stranger = client.lookup(ShortProfileLookup::new([Mri::new(LAB_C)]).unwrap()).await;
    assert!(
        matches!(stranger, Err(HttpError::Refused(Refusal::PostReadDenied))),
        "{stranger:?}"
    );
    // a search's shape, as a plain request: the personal build sends none
    let search = client.send(&PeopleSearch).await;
    assert!(
        matches!(search, Err(HttpError::Refused(Refusal::Personal))),
        "{search:?}"
    );
    assert_eq!(lookups(&mock).await, 1, "neither went out");
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
    let membership: Arc<dyn MembershipSource + Send> = Arc::new(source(&mock, &transport));
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

/// The push channel's `ic3` token: the account's, never renewed.
struct PushTokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl TrouterCredentials for PushTokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        self.ic3()
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_personal_build_opens_no_calling_socket_and_sends_no_calling_registration() {
    let mock = ChatServiceMock::start().await.unwrap();
    let trouter = mock.start_trouter().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (fence, log) = personal(LAB_A);
    let transport = transport(&mock, LAB_A, &fence);
    let tokens = Arc::new(PushTokens {
        token: mock.token(LAB_A).unwrap(),
        renewals: broadcast::channel(4).0,
    });
    // no calling socket can be built through the personal fence until its calls are turned on
    let membership: Arc<dyn MembershipSource + Send> = Arc::new(source(&mock, &transport));
    let calling = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())
        .unwrap()
        .for_calls(CallStack::new(Arc::clone(&fence), Arc::clone(&membership)));
    assert!(matches!(calling, Err(BuildError::PersonalCalls)), "{calling:?}");

    // the engine with its push channel, as the personal build assembles it: the messaging stack's socket only
    let push = Trouter::with_mock_origin(Arc::clone(&transport), tokens, trouter.origin()).unwrap();
    let client = Arc::new(Client::new(http(&mock, LAB_A, &fence)));
    let outbox = Outbox::new(
        transport,
        Arc::clone(&fence),
        membership,
        Author::new(Mri::new(LAB_A), "a"),
    );
    let mut handle = engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("store.sqlite")).unwrap(),
        client,
        outbox,
        push: Some(Push::new(push)),
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let event = tokio::time::timeout_at(deadline, handle.next_event())
            .await
            .expect("the push channel registered in time")
            .expect("the engine runs");
        if let Event::Registration(report) = event {
            assert_eq!(report, RegistrationReport::Registered);
            break;
        }
    }
    handle.shutdown();

    let sockets = trouter.sockets();
    assert!(!sockets.is_empty());
    assert!(sockets.iter().all(|socket| !socket.calling), "{sockets:#?}");
    let registrations = trouter.registrations();
    assert!(
        registrations
            .iter()
            .all(|registration| registration.app_id == teams_mock::MESSAGING_APP),
        "{registrations:?}"
    );
    let own: Vec<Value> = log
        .entries()
        .into_iter()
        .filter_map(|entry| entry.get("own").cloned())
        .collect();
    assert!(own.contains(&json!("endpoint_registration")), "{own:?}");
    assert!(!own.contains(&json!("calling_endpoint_registration")), "{own:?}");
}
