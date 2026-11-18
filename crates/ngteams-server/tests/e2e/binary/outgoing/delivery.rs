use std::collections::BTreeSet;

use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};

use super::receiver::{Receiver, Reply};
use super::{APOLOGY, Edit, Fixture, OPEN, RawPost, mention, mention_body, receiver, replies, thread};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_members_get_signed_replies_and_unrelated_posts_send_nothing() {
    for backend in backends() {
        let fixture = Fixture::new(backend, "owh-delivery", OPEN).await;
        let receiver = Receiver::start().await;
        let (hook, token) = fixture.create(&receiver).await;
        let [owner, member, _] = &fixture.rig.seats;
        let skype = owner.session.skypetoken().await.unwrap();
        let access = owner
            .session
            .access_token(teams_auth::token::SPACES_RESOURCE)
            .await
            .unwrap();
        let secrets = [token, skype.token.expose().to_owned(), access.token.expose().to_owned()];
        receiver.answer(Reply::json(&json!({"type":"message", "text":"**QA Markdown**"})));
        let post = mention(owner, &fixture.channel, &hook, "status").await;
        let got = receiver.next().await;
        assert!(got.signed);
        assert_eq!(
            got.authorization.as_deref(),
            Some(
                receiver::hmac(
                    &base64::engine::general_purpose::STANDARD.decode(&secrets[0]).unwrap(),
                    &got.body
                )
                .as_str()
            )
        );
        let activity = &got.activity;
        assert_eq!(activity["type"], "message");
        assert_eq!(activity["channelId"], "msteams");
        assert_eq!(activity["id"], post);
        assert_eq!(activity["text"], "<at>QA Echo</at> status");
        assert_eq!(activity["conversation"]["conversationType"], "channel");
        assert_eq!(activity["channelData"]["team"]["id"], fixture.team);
        assert_eq!(activity["channelData"]["channel"]["id"], fixture.channel);
        assert!(activity["recipient"].is_null());
        for secret in &secrets {
            assert!(!String::from_utf8_lossy(&got.body).contains(secret));
        }
        let first = replies(member, &fixture.channel, &post, &hook, 1).await;
        assert_eq!(first.len(), 1);
        assert!(first[0].content.contains("<strong>QA Markdown</strong>"));
        let chain = teams_proto::reply_chain_id(&fixture.channel, &post).unwrap();
        receiver.answer(Reply::json(
            &json!({"type":"message", "text":"QA plain", "textFormat":"plain"}),
        ));
        let reply = mention(member, &chain, &hook, "thread reply").await;
        let got = receiver.next().await;
        assert!(got.signed);
        assert_eq!(got.activity["id"], reply);
        assert_eq!(
            got.activity["conversation"]["id"],
            format!("{};messageid={post}", fixture.channel)
        );
        assert_eq!(
            got.activity["from"]["aadObjectId"],
            member.mri.trim_start_matches("8:orgid:")
        );
        let answers = replies(owner, &fixture.channel, &post, &hook, 2).await;
        assert_eq!(answers.len(), 2);
        assert!(answers.iter().any(|answer| answer.content.contains("QA plain")));
        let current = changed_callback(&fixture, &hook, &secrets[0]).await;
        ignored(&fixture, &hook, &post).await;
        assert!(!receiver.gets_within(Duration::from_secs(3)).await);
        assert!(!current.gets_within(Duration::from_secs(1)).await);
        assert_eq!(fixture.call(0, "DELETE", &format!("/{hook}"), None).await.0, 204);
        let removed_post = mention(owner, &fixture.channel, &hook, "removed webhook").await;
        assert!(!receiver.gets_within(Duration::from_secs(2)).await);
        assert!(!current.gets_within(Duration::from_secs(1)).await);
        assert!(thread(member, &fixture.channel, &removed_post, &hook).await.is_empty());
        fixture.stop(&secrets).await;
    }
}

async fn changed_callback(fixture: &Fixture, hook: &str, token: &str) -> Receiver {
    let receiver = Receiver::start().await;
    receiver.trust(base64::engine::general_purpose::STANDARD.decode(token).unwrap());
    let (status, answer) = fixture
        .call(0, "PUT", &format!("/{hook}"), Some(json!({"callbackUrl":receiver.url})))
        .await;
    assert_eq!(status, 200);
    assert!(answer["value"].get("securityToken").is_none());
    assert!(!answer.to_string().contains(token));
    let post = mention(&fixture.rig.seats[1], &fixture.channel, hook, "changed callback").await;
    assert!(receiver.next().await.signed);
    assert_eq!(
        replies(&fixture.rig.seats[0], &fixture.channel, &post, hook, 1)
            .await
            .len(),
        1
    );
    assert!(!receiver.gets_within(Duration::from_secs(1)).await);
    receiver
}

async fn ignored(fixture: &Fixture, hook: &str, post: &str) {
    let [owner, member, _] = &fixture.rig.seats;
    let plain = owner.send(&fixture.channel, "Plain post without a mention").await;
    let edited = owner
        .client
        .write(Edit {
            post: RawPost {
                chat: fixture.channel.clone(),
                body: mention_body(hook, "the edit adds a mention"),
            },
            id: plain,
        })
        .await
        .unwrap();
    assert_eq!(edited.status(), 200);
    let (private, _) = owner.make_private(&fixture.team, &fixture.group, "Private QA").await;
    mention(owner, &private, hook, "private").await;
    let (other, group) = owner.make_team("Other QA team", false).await;
    owner.add(&other, &group, &[member]).await;
    let channel = owner.make_channel(&other, &group, "Other channel", true).await;
    mention(owner, &channel, hook, "another team").await;
    assert_eq!(thread(member, &fixture.channel, post, hook).await.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failures_non_json_large_bodies_and_six_second_answers_get_one_apology() {
    for backend in backends() {
        let fixture = Fixture::new(backend, "owh-errors", OPEN).await;
        let receiver = Receiver::start().await;
        let (hook, token) = fixture.create(&receiver).await;
        let cases = [
            (
                "HTTP 500",
                Reply {
                    status: 500,
                    body: "{}".into(),
                    delay: Duration::ZERO,
                },
            ),
            (
                "non JSON",
                Reply {
                    status: 200,
                    body: "not JSON".into(),
                    delay: Duration::ZERO,
                },
            ),
            (
                "oversized",
                Reply::json(&json!({"type":"message", "text":"x".repeat(256 * 1024)})),
            ),
            (
                "six seconds",
                Reply {
                    status: 200,
                    body: json!({"type":"message", "text":"late answer"}).to_string(),
                    delay: Duration::from_secs(6),
                },
            ),
        ];
        for (case, response) in cases {
            receiver.answer(response);
            let post = mention(&fixture.rig.seats[0], &fixture.channel, &hook, case).await;
            let got = receiver.next().await;
            assert!(got.signed);
            let started = got.at;
            let answers = replies(&fixture.rig.seats[1], &fixture.channel, &post, &hook, 1).await;
            assert_eq!(answers.len(), 1, "{case}");
            assert!(answers[0].content.contains(APOLOGY), "{case}");
            if case == "six seconds" {
                let posted_after = answers[0].arrival_time - got.timestamp;
                eprintln!(
                    "QA timeout {backend:?}: apology posted after {posted_after} ms, client sync after {} ms",
                    started.elapsed().as_millis()
                );
                assert!(posted_after >= 4500);
                assert!(
                    posted_after < 6000,
                    "the server must post the apology before the late answer"
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
                let answers = thread(&fixture.rig.seats[1], &fixture.channel, &post, &hook).await;
                assert_eq!(answers.len(), 1);
                assert!(!answers[0].content.contains("late answer"));
            }
            assert!(
                !receiver.gets_within(Duration::from_millis(1200)).await,
                "{case} sent twice"
            );
        }
        fixture.stop(&[token]).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn card_answers_keep_only_open_url_actions() {
    for backend in backends() {
        let fixture = Fixture::new(backend, "owh-cards", OPEN).await;
        let receiver = Receiver::start().await;
        let (hook, token) = fixture.create(&receiver).await;
        let card = json!({"type":"AdaptiveCard", "version":"1.4", "body":[
            {"type":"TextBlock", "text":"QA card"},
            {"type":"ActionSet", "actions":[{"type":"Action.Submit", "title":"Unsafe submit"},
                {"type":"Action.OpenUrl", "title":"Open", "url":"https://example.test/qa"}]}],
            "actions":[{"type":"Action.Execute", "title":"Unsafe execute", "verb":"run"},
                {"type":"Action.OpenUrl", "title":"Open", "url":"https://example.test/qa"}],
            "selectAction":{"type":"Action.Submit", "title":"Unsafe select"}});
        receiver.answer(Reply::json(&json!({"type":"message", "attachments":[{
            "contentType":"application/vnd.microsoft.card.adaptive", "content":card}]})));
        let post = mention(&fixture.rig.seats[0], &fixture.channel, &hook, "card").await;
        assert!(receiver.next().await.signed);
        let answers = replies(&fixture.rig.seats[1], &fixture.channel, &post, &hook, 1).await;
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].message_type, "RichText/Media_Card");
        let content = &answers[0].content;
        let start = content.find("b64=\"").unwrap() + 5;
        let end = start + content[start..].find('"').unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&content[start..end])
            .unwrap();
        let swift: Value = serde_json::from_slice(&decoded).unwrap();
        let card = &swift["attachments"][0]["content"];
        assert_eq!(card["body"][0]["text"], "QA card");
        for actions in [&card["actions"], &card["body"][1]["actions"]] {
            assert_eq!(actions.as_array().unwrap().len(), 1);
            assert_eq!(actions[0]["type"], "Action.OpenUrl");
        }
        assert!(card.get("selectAction").is_none());
        fixture.stop(&[token]).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nodes_send_each_mention_once_and_a_restart_sends_nothing_again() {
    for backend in backends() {
        let mut fixture = Fixture::new(backend, "owh-once", OPEN).await;
        let receiver = Receiver::start().await;
        let (hook, token) = fixture.create(&receiver).await;
        let [owner, member, _] = &fixture.rig.seats;
        let mut expected = BTreeSet::new();
        for index in 0..8 {
            let seat = if index % 2 == 0 { owner } else { member };
            expected.insert(mention(seat, &fixture.channel, &hook, &format!("burst {index}")).await);
        }
        let mut got = BTreeSet::new();
        for _ in 0..8 {
            let activity = receiver.next().await;
            assert!(activity.signed);
            assert!(
                got.insert(activity.activity["id"].as_str().unwrap().to_owned()),
                "duplicate delivery"
            );
        }
        assert_eq!(got, expected);
        for post in &expected {
            assert_eq!(replies(member, &fixture.channel, post, &hook, 1).await.len(), 1);
        }
        assert!(!receiver.gets_within(Duration::from_secs(2)).await);
        fixture.rig.restart_nodes().await;
        assert!(!receiver.gets_within(Duration::from_secs(3)).await);
        for post in &expected {
            assert_eq!(
                thread(&fixture.rig.seats[1], &fixture.channel, post, &hook).await.len(),
                1
            );
        }
        fixture.stop(&[token]).await;
    }
}
