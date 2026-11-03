//! Presence reads through the engine against a synthetic UPS mock origin.
//! The engine reads every requested person, excluding bots/services that have no presence.
//! Answers follow tests/fixtures/ups/getpresence-answer.json; all identities and tokens are made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, Parts};
use teams_core::{Availability, Outbox};
use teams_mock::{LAB_A, LAB_B, LAB_C, OUTSIDER};
use teams_proto::Mri;
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

/// a's engine, its reads on `server` as UPS, holding [`store`]'s chat.
fn start(server: &MockServer, dir: &tempfile::TempDir) -> EngineHandle {
    let transport = || HttpTransport::with_mock_origin(Arc::new(Tokens), CHAT_BASE, &server.uri());
    let reads = transport().unwrap().with_presence(PRESENCE_BASE).unwrap();
    let author = Author::new(Mri::new(LAB_A), "a");
    let outbox = Outbox::new(Arc::new(transport().unwrap()), author);
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
    let mut handle = start(&server, &dir);

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
    // all three requested by the engine
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
