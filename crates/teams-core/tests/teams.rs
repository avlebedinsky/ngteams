//! Teams and channels through a running engine (M2 Task 16), read-only, on the chat service mock (which serves CSA's
//! teams read too): the teams read from CSA in the background, stored and told as `Event::Teams`, not read again while
//! fresh, a failed read told and not tried again before its wait; a channel no conversation list names synced on
//! demand, its posts and replies stored with the post each belongs to.
//!
//! Every id and token here is the mock's made-up one.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;
use std::time::Duration;

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, Parts};
use teams_core::{NOTES, Outbox, Source};
use teams_mock::{CHANNEL, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, TEAM_CHANNEL};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::{ConversationKind, Store};

/// How long a test waits for an event before it fails.
const WAIT: Duration = Duration::from_secs(30);
const CSA_PATH: &str = "/api/csa/amer/api/v3/teams/users/me";

/// A team as compared: its id, whether it is archived, its channels' ids and whether each is the general one.
type TeamShape<'a> = (&'a str, bool, Vec<(&'a str, bool)>);

/// One account's mock token, for every audience.
struct Tokens(String);

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(&self, _audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }
}

struct Rig {
    mock: ChatServiceMock,
    handle: EngineHandle,
    _dir: tempfile::TempDir,
}

/// Lab account a's engine on the mock, without a push channel; its reads given CSA's base when `csa`.
async fn rig(csa: bool, teams_retry: Duration) -> Rig {
    rig_polling(csa, teams_retry, None).await
}

/// [`rig`], catching up at the start and then every `poll` when it is given (the read-only mode's
/// [`EngineOptions::poll`]).
async fn rig_polling(csa: bool, teams_retry: Duration, poll: Option<Duration>) -> Rig {
    let mock = ChatServiceMock::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let tokens = Arc::new(Tokens(mock.token(LAB_A).unwrap()));
    let origin = mock.origin();
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin).unwrap();
    let transport = Arc::new(http());

    let reads = if csa {
        http().with_csa(CSA_BASE).unwrap()
    } else {
        http()
    };
    let outbox = Outbox::new(transport, Author::new(Mri::new(LAB_A), "Lab account"));
    let handle = engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("store").join("a.sqlite")).unwrap(),
        client: Arc::new(Client::new(reads)),
        outbox,
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            catch_up_retry: Duration::from_millis(500),
            teams_retry,
            poll,
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap();
    Rig {
        mock,
        handle,
        _dir: dir,
    }
}

impl Rig {
    /// The next event `wanted` picks, failing after [`WAIT`].
    async fn until<T>(&mut self, mut wanted: impl FnMut(&Event) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let event = tokio::time::timeout_at(deadline, self.handle.next_event())
                .await
                .expect("no such event in time")
                .expect("the engine stopped");
            if let Some(found) = wanted(&event) {
                return found;
            }
        }
    }

    /// How many requests the mock got whose path is `path`.
    async fn requests_to(&self, path: &str) -> usize {
        self.mock
            .received_requests()
            .await
            .iter()
            .filter(|request| request.url.path() == path)
            .count()
    }
}

#[tokio::test]
async fn the_teams_are_read_from_csa_in_the_background_stored_and_told() {
    let mut rig = rig(true, Duration::from_mins(5)).await;
    let requests = rig.handle.requests();
    assert_eq!(requests.teams().await.unwrap(), None, "nothing read yet");
    rig.until(|event| match event {
        Event::Teams => Some(()),
        Event::Failed { during, error } => panic!("failed {during}: {error}"),
        _ => None,
    })
    .await;
    let teams = requests.teams().await.unwrap().expect("read");
    let shape: Vec<TeamShape<'_>> = teams
        .iter()
        .map(|team| {
            let channels = team
                .channels
                .iter()
                .map(|channel| (channel.id.as_str(), channel.general))
                .collect();
            (team.id.as_str(), team.archived, channels)
        })
        .collect();
    assert_eq!(
        shape,
        [
            (CHANNEL, false, vec![(CHANNEL, true), (TEAM_CHANNEL, false)]),
            (
                "19:channel0004@thread.tacv2",
                true,
                vec![("19:channel0004@thread.tacv2", true)]
            ),
        ]
    );
    // fresh: asked again, not read again
    requests.teams().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rig.requests_to(CSA_PATH).await, 1);
    let csa = rig
        .mock
        .received_requests()
        .await
        .into_iter()
        .find(|request| request.url.path() == CSA_PATH)
        .unwrap();
    assert_eq!(csa.method.as_str(), "GET");
    assert!(csa.body.is_empty());
}

#[tokio::test]
async fn a_failed_teams_read_is_told_and_tried_again_only_after_its_wait() {
    // no CSA base: the read is refused before anything is sent
    let mut rig = rig(false, Duration::from_millis(300)).await;
    let requests = rig.handle.requests();
    assert_eq!(requests.teams().await.unwrap(), None);
    let failed = |event: &Event| match event {
        Event::Failed { during, .. } => Some(*during),
        Event::Teams => panic!("no teams can be read"),
        _ => None,
    };
    assert_eq!(rig.until(failed).await, "reading the teams");
    // within the wait: not tried
    assert_eq!(requests.teams().await.unwrap(), None);
    // no other read fails in the next 150 ms (other events, the search backfill's end, may come)
    let deadline = tokio::time::Instant::now() + Duration::from_millis(150);
    while let Ok(event) = tokio::time::timeout_at(deadline, rig.handle.next_event()).await {
        let event = event.expect("the engine stopped");
        assert!(!matches!(event, Event::Failed { .. } | Event::Teams), "{event:?}");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(requests.teams().await.unwrap(), None);
    assert_eq!(rig.until(failed).await, "reading the teams");
    assert_eq!(rig.requests_to(CSA_PATH).await, 0);
}

#[tokio::test]
async fn a_channel_no_list_names_is_synced_on_demand_with_the_post_of_each_message() {
    let mut rig = rig(true, Duration::from_mins(5)).await;
    let requests = rig.handle.requests();
    let synced = requests.sync_conversation(TEAM_CHANNEL).await.unwrap();
    assert_eq!(synced.inserted, 6);
    // told as a read's
    rig.until(|event| match event {
        Event::Message {
            conversation,
            source: Source::CatchUp,
            ..
        } if conversation == TEAM_CHANNEL => Some(()),
        _ => None,
    })
    .await;
    let stored = requests.conversations(50).await.unwrap();
    let channel = stored.iter().find(|row| row.id == TEAM_CHANNEL).expect("stored");
    assert_eq!(channel.kind, ConversationKind::Channel, "a topic's thread is a channel");

    let mut roots = requests.reply_roots(TEAM_CHANNEL).await.unwrap();
    roots.sort();
    let expected = [
        ("1760000250000", "1760000250000"),
        ("1760000300000", "1760000300000"),
        ("1760000300100", "1760000300000"),
        ("1760000300200", "1760000300000"),
        ("1760000400000", "1760000400000"),
        ("1760000400100", "1759000000000"),
    ]
    .map(|(message, root)| (message.to_owned(), root.to_owned()));
    assert_eq!(roots, expected);
    let page = requests.messages(TEAM_CHANNEL, None, 50).await.unwrap();
    assert_eq!(page.messages.len(), 6);
}

/// CSA's answer with no teams and the account's own notes as its one favourite, in the captured shape
/// (`tests/fixtures/csa/folders.json`).
fn notes_favourite() -> serde_json::Value {
    serde_json::json!({
        "teams": [],
        "metadata": {"isPartialData": false},
        "conversationFolders": {
            "isPartialResponse": false,
            "conversationFolders": [{
                "id": "made-up~Favorites",
                "sortType": "UserDefinedCustomOrder",
                "name": "Favorites",
                "folderType": "Favorites",
                "conversationFolderItems": [
                    {"conversationId": NOTES, "createdTime": 1_760_000_000_100_u64, "threadType": "streamofnotes"}
                ],
                "isDeleted": false
            }]
        }
    })
}

#[tokio::test]
async fn notes_that_become_a_favourite_after_the_catch_up_are_synced_at_once() {
    // the first run (and the first after the store kept favourites): the catch-up runs before the teams' read, so it
    // can't know the notes are a favourite; no other catch-up comes within the test (the next poll is an hour away)
    let mut rig = rig_polling(true, Duration::from_mins(5), Some(Duration::from_hours(1))).await;
    rig.until(|event| match event {
        Event::CaughtUp { .. } => Some(()),
        Event::CatchUpFailed { error, .. } => panic!("catch-up failed: {error}"),
        _ => None,
    })
    .await;
    let notes_path = "/api/chatsvc/amer/v1/users/ME/conversations/48:notes/messages";
    assert_eq!(rig.requests_to(notes_path).await, 0, "not a favourite yet");
    rig.mock
        .answer_next("GET", "/teams/users/me", 200, &notes_favourite())
        .await;
    let requests = rig.handle.requests();
    assert_eq!(
        requests.chat_listing().await.unwrap().favorites,
        None,
        "nothing read yet"
    );
    rig.until(|event| match event {
        Event::Teams => Some(()),
        Event::Failed { during, error } => panic!("failed {during}: {error}"),
        _ => None,
    })
    .await;
    // the notes synced right after the read stored them as a favourite: their row kept (the mock has none of them:
    // never written, which is none yet), and the list told
    rig.until(|event| match event {
        Event::ConversationChanged { conversation } if conversation == NOTES => Some(()),
        Event::Failed { during, error } => panic!("failed {during}: {error}"),
        _ => None,
    })
    .await;
    assert_eq!(
        requests.chat_listing().await.unwrap().favorites,
        Some(vec![NOTES.to_owned()])
    );
    let stored = requests.conversations(10).await.unwrap();
    assert!(stored.iter().any(|conversation| conversation.id == NOTES), "{stored:?}");
    assert_eq!(rig.requests_to(notes_path).await, 1);
    assert_eq!(rig.requests_to(CSA_PATH).await, 1);
}
