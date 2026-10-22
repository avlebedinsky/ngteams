//! The app's diagnostic log at `info` (M2 Task 24a), as two native clients of the lab write it against the mock chat
//! service and its Trouter: a sends, b receives over its push channel, a edits, a's name in its message changes and b
//! catches up, every account with a real-shaped token (a JWT, made up here) and marker text and names; the log has the
//! actions, the requests, the push channel and the outbox rows by kind and id, and a grep of every file of it finds
//! none of the markers and neither token. One test: the logger is the process's.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::future::{self, Future};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Event, Parts, Push};
use teams_core::{Outbox, sync::CatchUpReport};
use teams_fence::{Fence, FenceLog, LiveMembershipSource, MembershipSource, Mri, Roster};
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, TrouterMock};
use teams_proto::chat::MessageText;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::{OutboxState, Store};
use teams_trouter::{Options, RegistrationReport, Trouter, TrouterCredentials};
use tokio::sync::broadcast;

/// How long a test waits for an event.
const WAIT: Duration = Duration::from_secs(30);

/// Text and names that must never reach the log at `info`.
const FIRST_TEXT: &str = "MARKERTEXTFIRST the first words";
const SECOND_TEXT: &str = "MARKERTEXTSECOND the edited words";
const A_NAME: &str = "MARKERNAMEALPHA Person";
const B_NAME: &str = "MARKERNAMEBRAVO Person";
const A_RENAMED: &str = "MARKERNAMERENAMED Person";

/// A JWT-shaped token, made up: what a real `ic3` token looks like.
fn jwt(seed: &str) -> String {
    let body = format!("{seed}LoGtEsTtOkEnAbCdEf0123456789-_").repeat(5);
    format!("eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9.eyJ{body}.{body}")
}

/// One account's token, for the transport and the push channel alike.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
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

/// `me`'s engine against the mocks, writing as `name`, knowing a and b by their marker names, its store in `dir`.
fn native(mock: &ChatServiceMock, trouter: &TrouterMock, me: &str, name: &str, dir: &Path) -> EngineHandle {
    let tokens = Arc::new(Tokens {
        token: mock.token(me).unwrap(),
        renewals: broadcast::channel(4).0,
    });
    let fence = Arc::new(Fence::new(
        Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
        FenceLog::from_writer(std::io::sink()),
    ));
    let origin = mock.origin();
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &origin).unwrap();
    let transport = Arc::new(http());
    let live = LiveMembershipSource::with_mock_origin(&origin).unwrap();
    live.set_credential_source(transport.clone());
    let membership: Arc<dyn MembershipSource + Send> = Arc::new(live);
    let push = Trouter::with_mock_origin(
        Arc::clone(&transport),
        Arc::clone(&tokens),
        Arc::clone(&fence),
        trouter.origin(),
    )
    .unwrap()
    .with_options(Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(50),
        backoff_jitter_percent: 0,
        ..Options::default()
    });
    let client = Arc::new(Client::new(http()));
    let outbox = Outbox::new(transport, fence, membership, Author::new(Mri::new(me), name));
    engine::start(Parts {
        store: Store::open(&dir.join("store.sqlite")).unwrap(),
        client,
        outbox,
        push: Some(Push::new(push)),
        options: EngineOptions {
            tick: Duration::from_millis(50),
            ..EngineOptions::default()
        },
        known_names: vec![
            (LAB_A.to_owned(), A_NAME.to_owned()),
            (LAB_B.to_owned(), B_NAME.to_owned()),
        ],
    })
    .unwrap()
}

/// The next event `pick` takes, skipping others.
async fn event<T>(handle: &mut EngineHandle, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let event = tokio::time::timeout_at(deadline, handle.next_event())
            .await
            .expect("the event came in time")
            .expect("the engine runs");
        if let Some(picked) = pick(&event) {
            return picked;
        }
    }
}

/// Waits until the push channel is registered and its first catch-up has run.
async fn ready(handle: &mut EngineHandle) {
    let (mut registered, mut caught_up) = (false, false);
    while !(registered && caught_up) {
        let seen = event(handle, |event| match event {
            Event::Registration(RegistrationReport::Registered) => Some(true),
            Event::CaughtUp { .. } => Some(false),
            _ => None,
        })
        .await;
        if seen {
            registered = true;
        } else {
            caught_up = true;
        }
    }
}

/// Waits for a message event about `id` in the a–b chat.
async fn message(handle: &mut EngineHandle, id: &str) {
    event(handle, |event| match event {
        Event::Message {
            conversation, id: got, ..
        } if conversation == AB_CHAT && got == id => Some(()),
        _ => None,
    })
    .await;
}

/// Every file in `dir`, as text.
fn every_file(dir: &Path) -> String {
    let mut text = String::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        text.push_str(&fs::read_to_string(entry.path()).unwrap());
    }
    text
}

/// a sends to b, edits, and its name in its message changes for b's next catch-up.
async fn send_edit_and_rename(mock: &ChatServiceMock, a: &mut EngineHandle, b: &mut EngineHandle) {
    let text = |words: &str| MessageText::plain(words).unwrap();
    let action = Action::Send {
        conversation: AB_CHAT.to_owned(),
        text: text(FIRST_TEXT),
    };
    let row = a.write(action).await.unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    let id = row.message_id.clone().unwrap();
    message(b, &id).await;
    let edit = Action::Edit {
        conversation: AB_CHAT.to_owned(),
        message_id: id.clone(),
        text: text(SECOND_TEXT),
    };
    assert_eq!(a.write(edit).await.unwrap().state, OutboxState::Sent);
    message(b, &id).await;
    let renamed = mock.change_message(AB_CHAT, &row.client_message_id, |message| {
        message["imdisplayname"] = A_RENAMED.into();
        message["version"] = "9999999999999".into();
    });
    assert!(renamed);
    let report: CatchUpReport = b.sync().await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
}

#[tokio::test]
async fn the_info_log_has_what_happened_and_no_text_name_or_token() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    let log = teams_log::install(&teams_log::Config {
        dir: logs.clone(),
        setting: teams_log::Level::Info,
        env: None,
        rotation: teams_log::Rotation::default(),
    })
    .unwrap();
    let mock = ChatServiceMock::start().await.unwrap();
    let trouter = mock.start_trouter().unwrap();
    let tokens = [(LAB_A, jwt("a")), (LAB_B, jwt("b"))];
    for (account, token) in &tokens {
        assert!(mock.set_token(account, token.clone()));
    }
    let (a_dir, b_dir) = (dir.path().join("a"), dir.path().join("b"));
    let mut a = native(&mock, &trouter, LAB_A, A_NAME, &a_dir);
    let mut b = native(&mock, &trouter, LAB_B, B_NAME, &b_dir);
    ready(&mut a).await;
    ready(&mut b).await;
    send_edit_and_rename(&mock, &mut a, &mut b).await;
    a.shutdown();
    b.shutdown();

    assert!(log.flush_written());
    let text = every_file(&logs);
    for kept in [
        "INFO  teams_core::engine: action: send to 19:",
        "INFO  teams_core::engine: action: edit of ",
        "INFO  teams_core::outbox: fence allowed message on ",
        "INFO  teams_core::outbox: outbox row ",
        "INFO  teams_proto::http: POST ",
        " -> HTTP 201 in ",
        "INFO  teams_trouter::session: push channel connected",
        "INFO  teams_core::sync: catch-up: ",
        "INFO  teams_store::schema: store migrated from schema version 0 to 1",
    ] {
        assert!(text.contains(kept), "the log has {kept:?}:\n{text}");
    }
    let tokens = tokens.map(|(_, token)| token);
    for absent in [
        FIRST_TEXT,
        SECOND_TEXT,
        A_NAME,
        B_NAME,
        A_RENAMED,
        "MARKER",
        &tokens[0],
        &tokens[1],
    ] {
        assert!(!text.contains(absent), "the log holds {absent:?}:\n{text}");
    }
    assert!(!text.contains(" DEBUG "), "nothing at debug at info:\n{text}");
}
