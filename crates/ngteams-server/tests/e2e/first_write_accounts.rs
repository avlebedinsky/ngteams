//! Three accounts in one process, as one window runs them, writing at once with half a thread's default stack: two
//! Microsoft accounts on `teams-mock` sending to their 1:1 chat, and an own-server account whose first write (a new
//! chat) refreshes its token over TLS.

use std::future::{self, Future};
use std::sync::Arc;

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts};
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B};
use teams_proto::chat::MessageText;
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::{OutboxState, Store};

use crate::first_write::{ADA, BOB, OwnServer, in_child, run_in_child};

#[test]
fn two_microsoft_accounts_and_an_own_servers_write_at_once() {
    if in_child() {
        three_accounts();
        return;
    }
    run_in_child("first_write_accounts::two_microsoft_accounts_and_an_own_servers_write_at_once");
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn three_accounts() {
    let mock = ChatServiceMock::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let a = microsoft(&mock, LAB_A, &dir);
    let b = microsoft(&mock, LAB_B, &dir);
    let server = OwnServer::start(None).await;
    let (ada, region) = server.mri_and_region(ADA).await;
    let (bob, _) = server.mri_and_region(BOB).await;
    let own = server.start_engine(ADA, &ada, region, server.signed_in(ADA).await);

    let (from_a, from_b, from_own) = tokio::join!(
        a.write(send("QA synthetic from a")),
        b.write(send("QA synthetic from b")),
        own.handle.write(Action::CreateChat {
            others: vec![Mri::new(&bob)],
            topic: None,
        }),
    );
    for (who, row) in [("a", from_a), ("b", from_b), ("the own account", from_own)] {
        assert_eq!(row.unwrap().state, OutboxState::Sent, "{who}'s write");
    }

    for handle in [a, b, own.handle] {
        tokio::task::spawn_blocking(move || handle.shutdown()).await.unwrap();
    }
    server.stop().await;
}

fn send(text: &str) -> Action {
    Action::Send {
        conversation: AB_CHAT.to_owned(),
        text: MessageText::plain(text).unwrap(),
    }
}

/// A Microsoft account's engine on the mock, with the mock's made-up tokens and no push channel.
fn microsoft(mock: &ChatServiceMock, me: &str, dir: &tempfile::TempDir) -> EngineHandle {
    let tokens = Arc::new(Tokens {
        token: mock.token(me).unwrap(),
        skypetoken: mock.skypetoken(me).unwrap(),
    });
    let origin = mock.origin();
    let http = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)
            .unwrap()
            .for_account(Mri::new(me))
    };
    let name = if me == LAB_A { "Lab A" } else { "Lab B" };
    engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join(format!("{}.sqlite", &me[me.len() - 1..]))).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(me), name)),
        push: None,
        options: EngineOptions::default(),
        known_names: vec![
            (LAB_A.to_owned(), "Lab A".to_owned()),
            (LAB_B.to_owned(), "Lab B".to_owned()),
        ],
    })
    .unwrap()
}

/// A Microsoft account's tokens, the mock's made-up ones.
struct Tokens {
    token: String,
    skypetoken: String,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }

    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        future::ready(Ok(Some(Secret::new(self.skypetoken.clone()))))
    }
}
