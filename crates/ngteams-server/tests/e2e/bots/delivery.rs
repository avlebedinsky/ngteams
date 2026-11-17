//! Delivery to bots: retries of a bot that takes nothing, while other bots go on getting theirs; the address guard;
//! unsigned bots; two nodes on one database, each activity sent once. Bots and messages are synthetic.

use std::time::Duration;

use ngteams_server::bots::{FAILED, SENT, deliveries};
use reqwest::Method;
use serde_json::json;

use super::connector::{BOT_NAME, Setup, loopback, personal_chat};
use super::endpoint::{Connector, ORIGIN, TestBot};
use crate::support::{TestServer, backends, redis_url};

/// Waits until every activity to `bot` is sent or failed; their states, oldest first.
async fn settled(server: &TestServer, bot: &str, within: Duration) -> Vec<(String, i32, Option<String>)> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let rows = deliveries(&server.app.db, bot).await.unwrap();
        let done = !rows.is_empty() && rows.iter().all(|row| row.state == SENT || row.state == FAILED);
        if done || tokio::time::Instant::now() > deadline {
            return rows
                .into_iter()
                .map(|row| (row.state, row.attempts, row.last_error))
                .collect();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_endpoint_is_retried_then_failed_without_holding_up_another_bot() {
    for backend in backends() {
        let setup = Setup::open_with(backend, |config| config.bots.retry_delays = vec![1, 1]).await;
        setup.bot.answer(500);
        let other = TestBot::start().await;
        let answer = setup.registry.register("Other (synthetic)", &other.url).await;
        let other_id = answer["id"].as_str().unwrap().to_owned();

        let dead_chat = setup.chat();
        setup.send(&dead_chat, "first to the dead bot").await;
        setup.send(&dead_chat, "second to the dead bot").await;
        let live_chat = personal_chat(&setup.ada, &other_id);
        setup.send(&live_chat, "to the live bot").await;
        let got = other.next_of_within("message", Duration::from_secs(5)).await;
        assert_eq!(got.activity["text"], "to the live bot");

        let rows = settled(&setup.registry.server, &setup.bot_id, Duration::from_secs(20)).await;
        assert_eq!(rows.len(), 2, "{rows:?}");
        for (state, attempts, error) in &rows {
            assert_eq!(state, FAILED, "{rows:?}");
            assert_eq!(*attempts, 3, "{rows:?}");
            assert!(error.as_deref().unwrap().contains("500"), "{rows:?}");
        }
        assert_eq!(setup.bot.hits(), 6);
        // the second waited for the first: their attempts never interleave
        let mut order = Vec::new();
        for _ in 0..6 {
            order.push(
                setup
                    .bot
                    .next_of_within("message", Duration::from_secs(1))
                    .await
                    .activity["text"]
                    .clone(),
            );
        }
        assert!(
            order[..3].iter().all(|text| text == "first to the dead bot"),
            "{order:?}"
        );
        assert!(
            order[3..].iter().all(|text| text == "second to the dead bot"),
            "{order:?}"
        );
        let live = settled(&setup.registry.server, &other_id, Duration::from_secs(5)).await;
        assert_eq!(live, [(SENT.to_owned(), 1, None)]);
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn many_rows_waiting_behind_a_dead_bots_retry_dont_hold_up_another_bot() {
    for backend in backends() {
        let setup = Setup::open_with(backend, |config| config.bots.retry_delays = vec![60]).await;
        setup.bot.answer(500);
        let dead_chat = setup.chat();
        setup.send(&dead_chat, "first to the dead bot").await;
        setup.bot.next_of_within("message", Duration::from_secs(5)).await;
        // more than a sender's window of candidates (four batches of 32) waits behind the first one's retry
        for n in 0..130 {
            setup.send(&dead_chat, &format!("waiting {n}")).await;
        }
        let other = TestBot::start().await;
        let answer = setup.registry.register("Other (synthetic)", &other.url).await;
        let other_id = answer["id"].as_str().unwrap().to_owned();
        setup
            .send(&personal_chat(&setup.ada, &other_id), "to the live bot")
            .await;
        let got = other.next_of_within("message", Duration::from_secs(10)).await;
        assert_eq!(got.activity["text"], "to the live bot");
        assert_eq!(setup.bot.hits(), 1);
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_with_a_deleted_bot_still_takes_messages() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let chat = setup.chat();
        setup.send(&chat, "before the app is deleted").await;
        setup.bot.next_of("message").await;
        let (status, _) = setup
            .registry
            .call(Method::DELETE, &super::app_path(&setup.bot_id), None)
            .await;
        assert_eq!(status, 204);
        setup.send(&chat, "after the app is deleted").await;
        assert!(!setup.bot.gets_within("message", Duration::from_secs(3)).await);
        let rows = deliveries(&setup.registry.server.app.db, &setup.bot_id).await.unwrap();
        assert!(rows.is_empty(), "{rows:?}");
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_private_loopback_or_metadata_address_is_refused_unless_allowed() {
    for backend in backends() {
        let setup = Setup::open_with(backend, |config| config.bots.allowed_networks.clear()).await;
        let local = setup.bot.url.replace("127.0.0.1", "localhost");
        let metadata = setup
            .registry
            .register("Metadata (synthetic)", "http://169.254.169.254/latest")
            .await;
        let named = setup.registry.register("Named (synthetic)", &local).await;
        for bot in [
            setup.bot_id.clone(),
            metadata["id"].as_str().unwrap().to_owned(),
            named["id"].as_str().unwrap().to_owned(),
        ] {
            setup.send(&personal_chat(&setup.ada, &bot), "is anyone there").await;
            let rows = settled(&setup.registry.server, &bot, Duration::from_secs(10)).await;
            let [(state, attempts, error)] = rows.as_slice() else {
                panic!("{rows:?}");
            };
            assert_eq!(state, FAILED);
            assert_eq!(*attempts, 1, "a refused address isn't tried again");
            assert!(error.as_deref().unwrap().contains("network"), "{error:?}");
        }
        assert_eq!(setup.bot.hits(), 0);
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unsigned_bot_gets_no_token_and_answers_with_the_credential_in_its_service_url() {
    for backend in backends() {
        let setup = Setup::open_with(backend, |config| config.bots.allow_unsigned = true).await;
        let bot = TestBot::start().await;
        let body = json!({"name": "Plain (synthetic)", "endpoint": bot.url, "auth_mode": "unsigned"});
        let (status, answer) = setup.registry.call(Method::POST, super::APPS, Some(body)).await;
        assert_eq!(status, 201, "{answer}");
        let id = answer["id"].as_str().unwrap().to_owned();
        let chat = personal_chat(&setup.ada, &id);
        let sent = setup.send(&chat, "hello plain bot").await;
        let got = bot.next_of("message").await;
        assert!(got.authorization.is_none());
        let service_url = got.activity["serviceUrl"].as_str().unwrap().to_owned();
        assert!(
            service_url.starts_with(&format!("{ORIGIN}/bot/u/{id}.")),
            "{service_url}"
        );
        let connector = Connector::unsigned(&setup.registry.server, &service_url);
        let (status, answer) = connector.reply(&chat, &sent, Connector::text("plain answer")).await;
        assert_eq!(status, 200, "{answer}");
        // a wrong credential, a signed bot's id, or a token in place of the credential open nothing
        let wrong = Connector::unsigned(
            &setup.registry.server,
            &format!("{ORIGIN}/bot/u/{id}.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/"),
        );
        assert_eq!(wrong.send(&chat, Connector::text("x")).await.0, 401);
        let signed = Connector::unsigned(&setup.registry.server, &format!("{ORIGIN}/bot/u/{}.x/", setup.bot_id));
        assert_eq!(signed.send(&chat, Connector::text("x")).await.0, 401);
        let grant = Connector::signed(
            &setup.registry.server,
            &format!("{ORIGIN}/bot/"),
            &setup.bot_id,
            &setup.secret,
        )
        .await;
        let (status, _) = grant.send(&chat, Connector::text("not its chat")).await;
        assert_eq!(status, 403);
        let messages = setup.synced(&chat).await;
        assert!(messages.iter().any(|message| message.content.contains("plain answer")));
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_send_each_activity_once_and_take_the_bots_answer_on_either() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let url = setup.registry.server.app.config.database_url.clone();
        let second = TestServer::start_with(backend, |config| {
            loopback(config);
            config.database_url = url;
            config.redis_url = redis_url();
        })
        .await;
        let chat = setup.chat();
        let mut sent = Vec::new();
        for n in 0..5 {
            sent.push(setup.send(&chat, &format!("message {n}")).await);
        }
        let mut seen = Vec::new();
        for _ in 0..5 {
            seen.push(setup.bot.next_of("message").await.activity["id"].clone());
        }
        assert_eq!(seen, sent.iter().map(|id| json!(id)).collect::<Vec<_>>());
        assert!(!setup.bot.gets_within("message", Duration::from_secs(3)).await);
        assert_eq!(setup.bot.hits(), 5);

        // the token from the first node opens the second
        let service_url = format!("{ORIGIN}/bot/");
        let first = setup.connector(&service_url).await;
        let through_second = Connector {
            base: first.base.replacen(&setup.registry.server.origin, &second.origin, 1),
            token: first.token.clone(),
        };
        let (status, answer) = through_second
            .reply(&chat, &sent[0], Connector::text("answered through the second node"))
            .await;
        assert_eq!(status, 200, "{answer}");
        let messages = setup.synced(&chat).await;
        let answer = messages
            .iter()
            .find(|message| message.content.contains("answered through the second node"))
            .unwrap();
        assert_eq!(answer.sender_name.as_deref(), Some(BOT_NAME));
        second.stop().await;
        setup.stop().await;
    }
}
