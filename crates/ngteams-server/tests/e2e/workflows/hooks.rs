//! Posts to a workflow's webhook link with plain HTTP and no token, as an outside tool sends them: the card lands in
//! the channel or chat as the Workflows bot, another account's engine syncs it and hears it pushed, and the run
//! history shows each run. A stopped workflow, a wrong secret, a deleted workflow and an oversize body are refused.

use std::time::Duration;

use serde_json::{Value, json};
use teams_proto::WorkflowRequest;
use teams_trouter::ChatEvent;

use super::flows::group_chat;
use super::{ORIGIN, Seat, Target, ask, environment, rows, save, server, webhook_link};
use crate::support::{TestServer, backends};
use crate::trouter::{Account, delivery, registered_push};

const ADAPTIVE: &str = "application/vnd.microsoft.card.adaptive";
const BOT: &str = "28:358f0194-6b0e-4dd3-af35-c24fe8a9ec87";

/// The Teams Workflows payload carrying one Adaptive Card that says `text`.
pub(crate) fn card_payload(text: &str) -> Value {
    json!({"type": "message", "attachments": [{"contentType": ADAPTIVE, "content": {
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "type": "AdaptiveCard", "version": "1.4",
        "body": [
            {"type": "TextBlock", "text": text, "weight": "bolder", "wrap": true},
            {"type": "FactSet", "facts": [{"title": "Build", "value": "1042"}]},
        ],
        "actions": [{"type": "Action.OpenUrl", "title": "Open", "url": "https://ci.example.test/1042"}],
    }}]})
}

/// Posts `body` to `link` on `server` (the link names the public origin), with no credential; the status.
pub(crate) async fn post(server: &TestServer, link: &str, body: &[u8]) -> u16 {
    let local = link.replacen(ORIGIN, &server.origin, 1);
    reqwest::Client::new()
        .post(local)
        .header("content-type", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// The workflow's runs, newest first: each status and, for a failure, its code.
async fn runs(seat: &Seat, environment: &str, flow: &str) -> Vec<(String, Option<String>)> {
    let history = ask(
        seat,
        WorkflowRequest::History {
            environment: environment.to_owned(),
            flow: flow.to_owned(),
        },
    )
    .await
    .unwrap();
    rows(&history)
        .iter()
        .map(|run| {
            (
                run["properties"]["status"].as_str().unwrap().to_owned(),
                run["properties"]["code"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

/// The words of the card message's cards, as the client's card reader takes them.
fn card_words(content: &str) -> Vec<String> {
    let cards = teams_proto::chat::card::card_message(content).expect("a card message");
    assert!(!cards.unshown);
    assert!(cards.cards.iter().all(|card| !card.unshown));
    teams_proto::chat::card::plain_words(&cards)
}

/// Waits until `seat`'s engine has synced a message of `conversation` from the Workflows bot.
async fn synced(seat: &Seat, conversation: &str) -> teams_store::Message {
    for _ in 0..200 {
        seat.engine.sync().await.unwrap();
        let messages = seat.engine.messages(conversation, None, 50).await.unwrap().messages;
        if let Some(message) = messages
            .into_iter()
            .find(|message| message.sender.as_deref() == Some(BOT))
        {
            return message;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the engine never synced the workflow's message");
}

/// Ada's team (Bert on it) and her group chat with Bert, each with a webhook workflow of Ada's.
struct Rig {
    server: TestServer,
    ada: Seat,
    bert: Seat,
    environment: String,
    team: String,
    group: String,
    chat: String,
    channel_id: String,
    chat_id: String,
    channel_link: String,
    chat_link: String,
}

impl Rig {
    async fn start(backend: crate::support::Backend) -> Self {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let environment = environment(&ada).await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let chat = group_chat(&ada, &[&bert], "Project room").await;
        let channel = Target::Channel {
            group: &group,
            channel: &team,
        };
        let channel_id = save(&ada, &environment, &channel).await["name"]
            .as_str()
            .unwrap()
            .to_owned();
        let chat_id = save(&ada, &environment, &Target::Chat(&chat)).await["name"]
            .as_str()
            .unwrap()
            .to_owned();
        let channel_link = webhook_link(&ada, &environment, &channel_id).await;
        let chat_link = webhook_link(&ada, &environment, &chat_id).await;
        Self {
            server,
            ada,
            bert,
            environment,
            team,
            group,
            chat,
            channel_id,
            chat_id,
            channel_link,
            chat_link,
        }
    }

    async fn post(&self, link: &str, body: &Value) -> u16 {
        post(&self.server, link, body.to_string().as_bytes()).await
    }

    async fn stop(self) {
        self.ada.stop();
        self.bert.stop();
        self.server.stop().await;
    }
}

/// An Adaptive Card into the channel: Bert hears it pushed, his engine syncs it and reads its card; then the
/// plain-text form and a bare card into the chat.
async fn posts_land(rig: &Rig) {
    let listener = Account::sign_in(&rig.server, "bert@own.test", &rig.bert.id).await;
    let mut push = registered_push(&listener, &rig.server).await;
    assert_eq!(
        rig.post(&rig.channel_link, &card_payload("Build 1042 passed")).await,
        202
    );
    let pushed = loop {
        if let ChatEvent::Message { conversation, message } = delivery(&mut push).await.event
            && conversation == rig.team
        {
            break message;
        }
    };
    assert_eq!(pushed.message_type.as_str(), "RichText/Media_Card");
    assert!(pushed.from.as_deref().is_some_and(|from| from.ends_with(BOT)));
    assert_eq!(pushed.display_name.as_deref(), Some("Workflows"));
    assert!(
        card_words(&pushed.content)
            .iter()
            .any(|word| word.contains("Build 1042 passed"))
    );
    synced_and_chat_posts(rig, push).await;
}

/// Bert's engine syncs the channel's card; then the plain-text form and a bare card into the chat.
async fn synced_and_chat_posts(rig: &Rig, push: crate::trouter::Push) {
    let stored = synced(&rig.bert, &rig.team).await;
    assert_eq!(stored.sender_name.as_deref(), Some("Workflows"));
    assert!(card_words(&stored.content).iter().any(|word| word.contains("1042")));
    push.stop().await;

    assert_eq!(
        rig.post(&rig.chat_link, &json!({"text": "Deploy <b>started</b>"}))
            .await,
        202
    );
    let stored = synced(&rig.bert, &rig.chat).await;
    assert!(
        stored.content.contains("Deploy &lt;b&gt;started&lt;/b&gt;"),
        "{}",
        stored.content
    );
    let bare = card_payload("Bare card")["attachments"][0]["content"].clone();
    assert_eq!(rig.post(&rig.chat_link, &bare).await, 202);
}

/// The Overview's run history and a run's details.
async fn history_shows_the_runs(rig: &Rig) {
    let (ada, environment) = (&rig.ada, &rig.environment);
    assert_eq!(
        runs(ada, environment, &rig.channel_id).await,
        [("Succeeded".to_owned(), None)]
    );
    let history = ask(
        ada,
        WorkflowRequest::History {
            environment: environment.clone(),
            flow: rig.channel_id.clone(),
        },
    )
    .await
    .unwrap();
    let run = rows(&history)[0]["name"].as_str().unwrap().to_owned();
    assert!(
        rows(&history)[0]["properties"]["startTime"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    let actions = ask(
        ada,
        WorkflowRequest::RunActions {
            environment: environment.clone(),
            flow: rig.channel_id.clone(),
            run,
        },
    )
    .await
    .unwrap();
    assert_eq!(rows(&actions)[0]["properties"]["status"], "Succeeded");
    assert_eq!(runs(ada, environment, &rig.chat_id).await.len(), 2);
}

/// A card the client can't draw (its run shows the failure), too big, a wrong secret, no secret, another trigger.
async fn refusals(rig: &Rig) {
    let link = &rig.channel_link;
    let message_card = json!({"attachments": [{"contentType": "application/vnd.microsoft.teams.card.o365connector",
                                               "content": {"summary": "legacy"}}]});
    assert_eq!(rig.post(link, &message_card).await, 400);
    assert_eq!(post(&rig.server, link, b"not json").await, 400);
    assert_eq!(
        runs(&rig.ada, &rig.environment, &rig.channel_id).await[0],
        ("Failed".to_owned(), Some("InvalidRequestContent".to_owned()))
    );
    assert_eq!(rig.post(link, &card_payload(&"x".repeat(40 * 1024))).await, 413);
    bad_links(rig).await;
}

/// A wrong secret, no secret, another trigger, another workflow's path.
async fn bad_links(rig: &Rig) {
    let link = &rig.channel_link;
    let payload = card_payload("Refused");
    let mut wrong = url::Url::parse(link).unwrap();
    let queries: Vec<(String, String)> = wrong
        .query_pairs()
        .map(|(key, value)| {
            let value = if key == "sig" {
                format!("{value}x")
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect();
    wrong.query_pairs_mut().clear().extend_pairs(queries);
    assert_eq!(rig.post(wrong.as_str(), &payload).await, 404);
    assert_eq!(rig.post(link.split('?').next().unwrap(), &payload).await, 404);
    assert_eq!(
        rig.post(&link.replace("/triggers/manual/", "/triggers/other/"), &payload)
            .await,
        404
    );
    assert_eq!(
        rig.post(&link.replace(&rig.channel_id, &rig.chat_id), &payload).await,
        404
    );
}

/// Turned off, it refuses and runs nothing; on again, it posts; deleted, its link is gone and a new workflow has a
/// new link.
async fn off_on_and_deleted(rig: &Rig) {
    let (ada, environment, link) = (&rig.ada, &rig.environment, &rig.channel_link);
    let payload = card_payload("Again");
    let turn = |enabled| WorkflowRequest::SetEnabled {
        environment: environment.clone(),
        flow: rig.channel_id.clone(),
        enabled,
    };
    ask(ada, turn(false)).await.unwrap();
    let before = runs(ada, environment, &rig.channel_id).await.len();
    assert_eq!(rig.post(link, &payload).await, 409);
    assert_eq!(runs(ada, environment, &rig.channel_id).await.len(), before);
    ask(ada, turn(true)).await.unwrap();
    assert_eq!(rig.post(link, &payload).await, 202);
    deleted(rig, &payload).await;
}

/// Deleted, its link is gone; a new workflow has a new link.
async fn deleted(rig: &Rig, payload: &Value) {
    let (ada, environment, link) = (&rig.ada, &rig.environment, &rig.channel_link);
    ask(
        ada,
        WorkflowRequest::Delete {
            environment: environment.clone(),
            flow: rig.channel_id.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(rig.post(link, payload).await, 404);
    let channel = Target::Channel {
        group: &rig.group,
        channel: &rig.team,
    };
    let again = save(ada, environment, &channel).await;
    let new_link = webhook_link(ada, environment, again["name"].as_str().unwrap()).await;
    assert_ne!(&new_link, link);
    assert_eq!(rig.post(&new_link, payload).await, 202);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_webhook_post_lands_in_the_channel_and_the_chat_and_shows_in_the_run_history() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        posts_land(&rig).await;
        history_shows_the_runs(&rig).await;
        refusals(&rig).await;
        off_on_and_deleted(&rig).await;
        rig.stop().await;
    }
}

/// Waits until the posts that follow fall in one of Redis' minute windows, which start on the minute.
async fn within_one_redis_window() {
    let second = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            % 60
    };
    while second() > 55 {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_webhook_is_held_off() {
    for backend in backends() {
        let server = crate::support::TestServer::start_with(backend, |config| {
            config.public_origin = url::Url::parse(ORIGIN).unwrap();
            config.workflows.rate_per_minute = 3;
            config.redis_url = crate::support::redis_url();
        })
        .await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let environment = environment(&ada).await;
        let chat = group_chat(&ada, &[&bert], "Project room").await;
        let flow = save(&ada, &environment, &Target::Chat(&chat)).await;
        let link = webhook_link(&ada, &environment, flow["name"].as_str().unwrap()).await;
        let payload = card_payload("Nightly").to_string();
        within_one_redis_window().await;
        let mut statuses = Vec::new();
        for _ in 0..4 {
            statuses.push(post(&server, &link, payload.as_bytes()).await);
        }
        assert_eq!(statuses, [202, 202, 202, 429]);
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

/// Bert's chat holds no message from the Workflows bot.
async fn nothing_posted(bert: &Seat, chat: &str) {
    bert.engine.sync().await.unwrap();
    let messages = bert.engine.messages(chat, None, 50).await.unwrap().messages;
    assert!(messages.iter().all(|message| message.sender.as_deref() != Some(BOT)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workflow_whose_owners_may_no_longer_post_there_posts_nothing() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        // three, so that Ada can leave it
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let environment = environment(&ada).await;
        let chat = group_chat(&ada, &[&bert, &cora], "Project room").await;
        let flow = save(&ada, &environment, &Target::Chat(&chat)).await;
        let id = flow["name"].as_str().unwrap().to_owned();
        let link = webhook_link(&ada, &environment, &id).await;
        let payload = card_payload("Nobody's").to_string();
        let ada_id = ada.id.clone();
        ada.stop();

        // the link still answers as Power Automate does, but nothing is posted while no owner may post there
        let disable = |disabled| {
            ngteams_server::admin::set_disabled(&server.app.db, &*server.app.fanout, "ada@own.test", disabled)
        };
        disable(true).await.unwrap();
        assert_eq!(post(&server, &link, payload.as_bytes()).await, 202);
        nothing_posted(&bert, &chat).await;
        disable(false).await.unwrap();
        let ada = Seat::of(&server, "ada@own.test", ada_id).await;
        let forbidden = ("Failed".to_owned(), Some("Forbidden".to_owned()));
        assert_eq!(runs(&ada, &environment, &id).await, std::slice::from_ref(&forbidden));

        // an owner who left the chat no longer sees it
        let leave = crate::chatsvc::members::RemoveMember {
            chat: chat.clone(),
            member: ada.mri.clone(),
        };
        assert_eq!(ada.client.write(leave).await.unwrap().status(), 200);
        assert_eq!(post(&server, &link, payload.as_bytes()).await, 202);
        nothing_posted(&bert, &chat).await;
        assert_eq!(runs(&ada, &environment, &id).await, [forbidden.clone(), forbidden]);
        ada.stop();
        bert.stop();
        cora.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn with_workflows_off_a_node_issues_no_flow_token_and_serves_no_webhook() {
    for backend in backends() {
        let on = server(backend).await;
        let url = on.app.config.database_url.clone();
        let off = TestServer::start_with(backend, |config| {
            config.database_url = url;
            config.public_origin = url::Url::parse(ORIGIN).unwrap();
            config.workflows.enabled = false;
        })
        .await;
        let ada = Seat::new(&on, "ada@own.test", "Ada").await;
        let bert = Seat::new(&on, "bert@own.test", "Bert").await;
        let environment = environment(&ada).await;
        let chat = group_chat(&ada, &[&bert], "Project room").await;
        let flow = save(&ada, &environment, &Target::Chat(&chat)).await;
        let link = webhook_link(&ada, &environment, flow["name"].as_str().unwrap()).await;
        let payload = card_payload("Off").to_string();

        let off_ada = Seat::of(&off, "ada@own.test", ada.id.clone()).await;
        assert_eq!(
            ask(&off_ada, WorkflowRequest::Environments).await,
            Err(teams_proto::WorkflowFailure::Authentication)
        );
        // the workflow and its secret are good: only the node that serves Workflows takes the post
        assert_eq!(post(&off, &link, payload.as_bytes()).await, 404);
        assert_eq!(post(&on, &link, payload.as_bytes()).await, 202);
        off_ada.stop();
        ada.stop();
        bert.stop();
        off.stop().await;
        on.stop().await;
    }
}

/// Redis stops, or stops answering, while the webhook takes a post: the rate limit and the chat service's fan-out go
/// on without it (fail open), so the post is answered 202 at once, well inside the webhook's 30 s, posted once and
/// kept in the run history; a member's own send in the chat isn't held either.
#[tokio::test(flavor = "multi_thread")]
async fn with_redis_down_or_hung_a_post_is_answered_at_once_posted_once_and_kept() {
    use crate::support::{Gate, RedisGate};
    let Some(redis) = crate::support::redis_url() else {
        return;
    };
    for backend in backends() {
        for gone in [Gate::Down, Gate::Hung] {
            let gate = RedisGate::before(&redis).await;
            let server = TestServer::start_with(backend, |config| {
                config.public_origin = url::Url::parse(ORIGIN).unwrap();
                config.redis_url = Some(gate.url.clone());
            })
            .await;
            let ada = Seat::new(&server, "ada@own.test", "Ada").await;
            let bert = Seat::new(&server, "bert@own.test", "Bert").await;
            let environment = environment(&ada).await;
            let chat = group_chat(&ada, &[&bert], "Project room").await;
            let flow = save(&ada, &environment, &Target::Chat(&chat)).await;
            let id = flow["name"].as_str().unwrap().to_owned();
            let link = webhook_link(&ada, &environment, &id).await;

            gate.set(gone);
            let started = std::time::Instant::now();
            let status = post(&server, &link, card_payload("Gone").to_string().as_bytes()).await;
            let took = started.elapsed();
            assert_eq!(status, 202, "{gone:?}");
            assert!(took < Duration::from_secs(5), "{gone:?}: the post took {took:?}");
            let started = std::time::Instant::now();
            let sent = ada
                .engine
                .write(teams_core::engine::Action::Send {
                    conversation: chat.clone(),
                    text: teams_proto::chat::MessageText::plain("Still here").unwrap(),
                })
                .await
                .unwrap();
            let took = started.elapsed();
            assert_eq!(sent.state, teams_store::OutboxState::Sent, "{sent:?}");
            assert!(took < Duration::from_secs(5), "{gone:?}: the send took {took:?}");

            gate.set(Gate::Open);
            assert_eq!(runs(&ada, &environment, &id).await, [("Succeeded".to_owned(), None)]);
            bert.engine.sync().await.unwrap();
            let messages = bert.engine.messages(&chat, None, 50).await.unwrap().messages;
            let posted = messages
                .iter()
                .filter(|message| message.sender.as_deref() == Some(BOT))
                .count();
            assert_eq!(posted, 1, "{gone:?}");
            ada.stop();
            bert.stop();
            server.stop().await;
        }
    }
}
