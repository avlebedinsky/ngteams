//! Others' presence through the personal fence, with the whole engine against wiremock as UPS (the transport's
//! `test-support` mock origin): the fence permits a read by `POST` naming anyone, logged, so the engine reads everyone
//! its views asked about but a bot or a service, which has no presence (`teams_core::has_presence`). Every id and token
//! here is made up; the answers follow the synthetic fixture `tests/fixtures/ups/getpresence-answer.json`.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, Parts};
use teams_core::{Availability, Outbox};
use teams_fence::{FailClosed, Fence, FenceLog, Mri};
use teams_mock::{LAB_A, LAB_B, LAB_C, OUTSIDER};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::{Conversation, ConversationKind, Member, Store};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const PRESENCE_BASE: &str = "https://teams.microsoft.com/ups/noam";
const GET_PRESENCE: &str = "/ups/noam/v1/presence/getpresence/";
/// A group chat of the account's, with b and the outsider.
const GROUP: &str = "19:0000000000000000000000000000beef@thread.v2";
/// A bot that posts in the group chat: a `28:` id, which has no presence and is never read.
const BOT: &str = "28:00000000-0000-4000-8000-0000000000b0";
const WAIT: Duration = Duration::from_secs(30);

struct Tokens;

impl Credentials for Tokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
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

/// UPS's answer: everyone the read asked about as Available.
fn available(request: &Request) -> ResponseTemplate {
    let items: Vec<Value> = asked(request)
        .into_iter()
        .map(|mri| {
            json!({
                "mri": mri,
                "presence": {"availability": "Available", "activity": "Available"},
                "etagMatch": false,
                "etag": "made-up-tag",
                "status": 20000,
            })
        })
        .collect();
    ResponseTemplate::new(200).set_body_raw(Value::Array(items).to_string(), "application/json")
}

/// The MRIs a presence read asked about.
fn asked(request: &Request) -> Vec<String> {
    let asked: Vec<Value> = serde_json::from_slice(&request.body).unwrap();
    asked
        .iter()
        .map(|ask| ask["mri"].as_str().unwrap().to_owned())
        .collect()
}

/// A store holding the group chat with the account, b and the outsider as its members.
fn store(dir: &tempfile::TempDir) -> Store {
    let mut store = Store::open(&dir.path().join("store.sqlite")).unwrap();
    store
        .upsert_conversation(&Conversation {
            id: GROUP.to_owned(),
            kind: ConversationKind::Group,
            topic: None,
            version: 1,
            last_activity: None,
        })
        .unwrap();
    let members = [LAB_A, LAB_B, OUTSIDER].map(|mri| Member::new(mri, None));
    store.replace_members(GROUP, 1, &members).unwrap();
    store
}

/// a's engine through its personal fence (logging to `log`), its reads on `server` as UPS, holding [`store`]'s chat.
fn start(server: &MockServer, dir: &tempfile::TempDir, log: &Lines) -> EngineHandle {
    let fence = Arc::new(Fence::personal(Mri::new(LAB_A), FenceLog::from_writer(log.clone())));
    let transport = || HttpTransport::with_mock_origin(Arc::new(Tokens), Arc::clone(&fence), CHAT_BASE, &server.uri());
    let reads = transport().unwrap().with_presence(PRESENCE_BASE).unwrap();
    assert!(reads.is_personal());
    let author = Author::new(Mri::new(LAB_A), "a");
    let outbox = Outbox::new(
        Arc::new(transport().unwrap()),
        Arc::clone(&fence),
        Arc::new(FailClosed),
        author,
    );
    engine::start(Parts {
        calls: None,
        store: store(dir),
        client: Arc::new(Client::new(reads)),
        outbox,
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            presence_poll: Duration::from_hours(1),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_leaves_out_a_bot_and_names_everyone_else_the_views_asked_about() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(GET_PRESENCE))
        .respond_with(available)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let log = Lines::default();
    let mut handle = start(&server, &dir, &log);

    // the group chat's senders as its view asks about them: a bot, b, someone of none of the account's chats (c, who
    // left it), the outsider
    for sender in [BOT, LAB_B, LAB_C, OUTSIDER] {
        assert_eq!(handle.presence(sender.to_owned()).await.unwrap(), Availability::Unknown);
    }
    let mut told = Vec::new();
    let deadline = tokio::time::Instant::now() + WAIT;
    while told.len() < 3 {
        let event = tokio::time::timeout_at(deadline, handle.next_event())
            .await
            .expect("the presence was told in time")
            .expect("the engine runs");
        match event {
            Event::Presence { mri, availability } => told.push((mri, availability)),
            Event::Failed { during, error } => panic!("failed {during}: {error}"),
            _ => {}
        }
    }
    told.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        told,
        [
            (LAB_B.to_owned(), Availability::Available),
            (LAB_C.to_owned(), Availability::Available),
            (OUTSIDER.to_owned(), Availability::Available)
        ]
    );

    // every read names only people (a read may have gone before the last was asked about, then another), the last all
    // three of them, each permitted by the fence
    let reads: Vec<Request> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == GET_PRESENCE)
        .collect();
    assert!(!reads.is_empty());
    for read in &reads {
        let named = asked(read);
        assert!(
            named.iter().all(|mri| [LAB_B, LAB_C, OUTSIDER].contains(&mri.as_str())),
            "{named:?}"
        );
    }
    let mut last = asked(reads.last().unwrap());
    last.sort();
    assert_eq!(last, [LAB_B, LAB_C, OUTSIDER]);
    let entries = log.entries();
    let decisions: Vec<&Value> = entries
        .iter()
        .filter(|entry| entry["event"] == "post_read_decision")
        .collect();
    assert_eq!(decisions.len(), reads.len(), "{entries:?}");
    assert!(
        decisions.iter().all(|decision| decision["verdict"] == "allow"),
        "{entries:?}"
    );
    // the bot stays unknown; c, of none of the account's chats, is read like anyone
    assert_eq!(handle.presence(BOT.to_owned()).await.unwrap(), Availability::Unknown);
    assert_eq!(
        handle.presence(LAB_C.to_owned()).await.unwrap(),
        Availability::Available
    );
    assert_eq!(
        handle.presence(LAB_B.to_owned()).await.unwrap(),
        Availability::Available
    );
    handle.shutdown();
}
