//! A bounded conversation scan leaves concurrent writes for the following sync, on either database.

use serde_json::{Value, json};
use teams_proto::chat::ChatReads;
use teams_proto::{CreateChat, Mri};

use super::client;
use crate::support::{Backend, TestServer, backends, own_stream};

/// The ids of the chats among listed conversations: the account's own streams left out.
fn chat_ids(listed: &[teams_proto::chat::Conversation]) -> Vec<String> {
    listed
        .iter()
        .map(|entry| entry.id.clone())
        .filter(|id| !own_stream(id))
        .collect()
}

/// How many reactions of 60 KiB keys make a summary past the page's 3 MiB.
const OVERSIZED_KEYS: usize = 72;

/// Stores `count` reactions of 60 KiB keys by `a` on `message`, as that many writes would have left them, in one
/// update. Each write through the service rewrites the whole message, so writing them one by one is quadratic: tens
/// of seconds in a debug build, past the client's 30-second timeout on a loaded host.
async fn store_reactions(pair: &super::messages::Pair, backend: Backend, message: &str, count: usize) {
    use diesel::sql_types::{BigInt, Text};
    use diesel_async::RunQueryDsl as _;
    use ngteams_server::db::{DbError, with_conn};

    #[derive(diesel::QueryableByName)]
    struct Stored {
        #[diesel(sql_type = Text)]
        body: String,
    }

    let (select, update) = match backend {
        Backend::Sqlite => (
            "SELECT body FROM chat_messages WHERE chat_id = ? AND id = ?",
            "UPDATE chat_messages SET body = ? WHERE chat_id = ? AND id = ?",
        ),
        Backend::Postgres => (
            "SELECT body FROM chat_messages WHERE chat_id = $1 AND id = $2",
            "UPDATE chat_messages SET body = $1 WHERE chat_id = $2 AND id = $3",
        ),
    };
    let id: i64 = message.parse().unwrap();
    let mri = format!("8:orgid:{}", pair.a_id);
    async {
        with_conn!(&pair.server.app.db, |conn| {
            let stored: Stored = diesel::sql_query(select)
                .bind::<Text, _>(&pair.chat)
                .bind::<BigInt, _>(id)
                .get_result(conn)
                .await?;
            let mut body: Value = serde_json::from_str(&stored.body).unwrap();
            let emotions = body["properties"]["emotions"].as_array_mut().unwrap();
            for index in 0..count {
                let key = format!("{index}-{}", "x".repeat(60 * 1024));
                emotions.push(json!({"key": key, "users": [{"mri": mri, "time": 1, "value": "1"}]}));
            }
            diesel::sql_query(update)
                .bind::<Text, _>(body.to_string())
                .bind::<Text, _>(&pair.chat)
                .bind::<BigInt, _>(id)
                .execute(conn)
                .await?;
        });
        Ok::<_, DbError>(())
    }
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_reaction_summaries_do_not_block_conversation_paging() {
    for backend in backends() {
        let pair = super::messages::Pair::start(backend).await;
        let message = pair.send("the preview text survives").await;
        let writer = client(&pair.server, "a@own.test", &pair.a_id).await;
        let reader = client(&pair.server, "b@own.test", &pair.b_id).await;
        let reads = ChatReads::new(&reader);
        assert_eq!(
            writer
                .write(teams_proto::SetReaction::new(&pair.chat, &message, "like"))
                .await
                .unwrap()
                .status(),
            200
        );
        let normal = reads.conversations().await.unwrap();
        let chat = normal.conversations.iter().find(|entry| entry.id == pair.chat).unwrap();
        assert_eq!(
            chat.last_message_raw.as_ref().unwrap()["annotationsSummary"]["emotions"]["like"],
            1
        );
        store_reactions(&pair, backend, &message, OVERSIZED_KEYS - 1).await;
        let key = format!("{}-{}", OVERSIZED_KEYS - 1, "x".repeat(60 * 1024));
        let response = writer
            .write(teams_proto::SetReaction::new(&pair.chat, &message, key))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let c = pair.server.add_user("c@own.test", "Cora").await;
        let later = group_with_preview(&pair, &c, "After the oversized preview", "still reachable").await;
        let mut page = reads
            .conversations_from("https://own.example.test/api/chatsvc/own/v1/users/ME/conversations?pageSize=1")
            .await
            .unwrap();
        let sync = page.sync_state().unwrap().to_owned();
        let mut listed = Vec::new();
        loop {
            assert_eq!(page.conversations.len(), 1);
            assert_eq!(page.sync_state(), Some(sync.as_str()));
            let next = page.forward_link().map(str::to_owned);
            listed.extend(page.conversations.into_iter().filter(|entry| !own_stream(&entry.id)));
            let Some(next) = next else { break };
            page = reads.conversations_from(&next).await.unwrap();
        }
        let ids: Vec<&str> = listed.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, [pair.chat.as_str(), later.0.as_str()]);
        let preview = listed[0].last_message_raw.as_ref().unwrap();
        assert_eq!(preview["id"], message);
        assert!(
            preview["content"]
                .as_str()
                .unwrap()
                .contains("the preview text survives")
        );
        assert!(preview.get("annotationsSummary").is_none());
        assert!(reads.conversations_from(&sync).await.unwrap().conversations.is_empty());
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn large_conversation_previews_page_within_the_native_budget_and_sync_every_chat() {
    for backend in backends() {
        let pair = super::messages::Pair::start(backend).await;
        let c = pair.server.add_user("c@own.test", "Cora").await;
        let text = "x".repeat(50 * 1024);
        let mut chats = Vec::new();
        for index in 0..90 {
            chats.push(group_with_preview(&pair, &c, &format!("Room {index}"), &text).await);
        }
        let reader = client(&pair.server, "b@own.test", &pair.b_id).await;
        let reads = ChatReads::new(&reader);
        let mut page = reads.conversations().await.unwrap();
        assert!(page.conversations.len() < chats.len());
        let sync = page.sync_state().unwrap().to_owned();
        let later = group_with_preview(&pair, &c, "During paging", &text).await;
        let mut seen = Vec::new();
        loop {
            assert_eq!(page.sync_state(), Some(sync.as_str()));
            seen.extend(chat_ids(&page.conversations));
            let Some(next) = page.forward_link() else { break };
            page = reads.conversations_from(next).await.unwrap();
        }
        assert_eq!(seen.len(), chats.len() + 1);
        assert_eq!(seen.iter().collect::<std::collections::HashSet<_>>().len(), seen.len());
        assert!(seen.contains(&pair.chat));
        for (chat, _) in &chats {
            assert!(seen.contains(chat), "missing {chat}");
        }
        assert!(!seen.contains(&later.0));
        let next = reads.conversations_from(&sync).await.unwrap();
        assert_eq!(next.conversations.len(), 1);
        assert_eq!(next.conversations[0].id, later.0);
        assert!(pair.b.sync().await.unwrap().failed.is_empty());
        let stored = pair.b.conversations(200).await.unwrap();
        let stored = stored.iter().filter(|entry| !own_stream(&entry.id));
        assert_eq!(stored.count(), chats.len() + 2);
        chats.push(later);
        for (chat, message) in chats {
            let stored = pair.b.message(&chat, &message).await.unwrap().unwrap();
            assert!(stored.content.contains(&text));
        }
        pair.stop().await;
    }
}

async fn group_with_preview(pair: &super::messages::Pair, other: &str, topic: &str, text: &str) -> (String, String) {
    use teams_core::engine::Action;
    use teams_store::OutboxState;

    let row = pair
        .a
        .write(Action::CreateChat {
            others: vec![
                Mri::new(format!("8:orgid:{}", pair.b_id)),
                Mri::new(format!("8:orgid:{other}")),
            ],
            topic: Some(topic.into()),
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    let chat = row.message_id.unwrap();
    let row = pair
        .a
        .write(Action::Send {
            conversation: chat.clone(),
            text: teams_proto::chat::MessageText::plain(text).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    (chat, row.message_id.unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn conversation_pages_keep_their_snapshot_and_survive_another_node_starting() {
    for backend in backends() {
        let server = TestServer::start_with(backend, |config| {
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let a = server.add_user("a@own.test", "Ada").await;
        let b = server.add_user("b@own.test", "Bert").await;
        let c = server.add_user("c@own.test", "Cora").await;
        let client = client(&server, "a@own.test", &a).await;
        let create = || {
            CreateChat::group(
                Mri::new(format!("8:orgid:{a}")),
                [Mri::new(format!("8:orgid:{b}")), Mri::new(format!("8:orgid:{c}"))],
            )
            .unwrap()
        };
        for _ in 0..3 {
            assert_eq!(client.write(create()).await.unwrap().status(), 201);
        }
        let reads = ChatReads::new(&client);
        let mut page = reads
            .conversations_from("https://own.example.test/api/chatsvc/own/v1/users/ME/conversations?pageSize=1")
            .await
            .unwrap();
        let sync = page.sync_state().unwrap().to_owned();
        assert_eq!(page.conversations.len(), 1);
        assert_eq!(client.write(create()).await.unwrap().status(), 201);
        let mut seen = chat_ids(&page.conversations);
        while let Some(next) = page.forward_link() {
            page = reads.conversations_from(next).await.unwrap();
            assert_eq!(page.sync_state(), Some(sync.as_str()));
            seen.extend(chat_ids(&page.conversations));
        }
        assert_eq!(seen.len(), 3);
        assert_eq!(seen.iter().collect::<std::collections::HashSet<_>>().len(), 3);
        let changed = reads.conversations_from(&sync).await.unwrap();
        assert_eq!(changed.conversations.len(), 1);
        assert!(!seen.contains(&changed.conversations[0].id));
        let url = server.app.config.database_url.clone();
        let node = TestServer::start_with(backend, |config| {
            config.database_url = url;
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let reader = super::client(&node, "b@own.test", &b).await;
        assert_eq!(
            chat_ids(&ChatReads::new(&reader).conversations().await.unwrap().conversations).len(),
            4
        );
        node.stop().await;
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_creating_the_same_one_to_one_roster_return_one_persisted_thread() {
    for backend in backends() {
        let server = TestServer::start_with(backend, |config| {
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let a = server.add_user("a@own.test", "Ada").await;
        let b = server.add_user("b@own.test", "Bert").await;
        let db = server.app.config.database_url.clone();
        let node = TestServer::start_with(backend, |config| {
            config.database_url = db;
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let first = client(&server, "a@own.test", &a).await;
        let second = client(&node, "b@own.test", &b).await;
        let a = Mri::new(format!("8:orgid:{a}"));
        let b = Mri::new(format!("8:orgid:{b}"));
        let (one, two) = tokio::join!(
            first.write(CreateChat::one_to_one(a.clone(), b.clone()).unwrap()),
            second.write(CreateChat::one_to_one(b, a).unwrap()),
        );
        let one = one.unwrap();
        let two = two.unwrap();
        assert_eq!(one.status(), 201);
        assert_eq!(two.status(), 201);
        let first: serde_json::Value = serde_json::from_slice(one.body()).unwrap();
        let second: serde_json::Value = serde_json::from_slice(two.body()).unwrap();
        assert_eq!(first["threadResource"], second["threadResource"]);
        node.stop().await;
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn groups_supported_by_the_native_builder_are_bounded_by_request_size_without_an_invented_roster_cap() {
    for backend in backends() {
        let server = TestServer::start_with(backend, |config| {
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let a = server.add_user("a@own.test", "Ada").await;
        let password = ngteams_server::db::users::by_id(&server.app.db, &a)
            .await
            .unwrap()
            .unwrap()
            .password_hash;
        let mut others = Vec::new();
        for index in 0..251 {
            let member = ngteams_server::admin::repo::NewUser {
                upn: format!("member-{index}@own.test"),
                display_name: "Synthetic member".into(),
                password_hash: password.clone(),
                admin: false,
                must_change_password: false,
            };
            let user = ngteams_server::admin::repo::create(&server.app.db, ngteams_server::admin::Actor::CLI, member)
                .await
                .unwrap();
            others.push(Mri::new(format!("8:orgid:{}", user.id)));
        }
        let dir = tempfile::tempdir().unwrap();
        let engine = super::engine(&server, "a@own.test", &a, dir.path()).await;
        let row = engine
            .write(teams_core::engine::Action::CreateChat {
                others,
                topic: Some("Native roster".into()),
            })
            .await
            .unwrap();
        assert_eq!(row.state, teams_store::OutboxState::Sent, "{:?}", row.last_error);
        let reader = client(&server, "a@own.test", &a).await;
        let members = ChatReads::new(&reader)
            .members(row.message_id.as_deref().unwrap())
            .await
            .unwrap();
        assert_eq!(members.members.len(), 252);
        engine.shutdown();
        server.stop().await;
    }
}
