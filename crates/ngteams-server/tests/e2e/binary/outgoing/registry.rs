use std::time::Duration;

use base64::Engine as _;
use serde_json::json;

use super::receiver::Receiver;
use super::{APOLOGY, Fixture, OPEN, form, mention, replies};
use crate::support::{PASSWORD, backends};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owners_members_and_outsiders_manage_the_registry_with_one_time_tokens() {
    for backend in backends() {
        let fixture = Fixture::new(backend, "owh-registry", OPEN).await;
        let receiver = Receiver::start().await;
        fixture.members_manage(false).await;
        let mut body = form(&receiver.url);
        body["picture"] = json!(base64::engine::general_purpose::STANDARD.encode(crate::bots::png(24, 24)));
        let (status, made) = fixture.call(0, "POST", "", Some(body)).await;
        assert_eq!(status, 201, "the owner can register a webhook");
        let made = &made["value"];
        let hook = made["id"].as_str().unwrap();
        let token = made["securityToken"].as_str().unwrap().to_owned();
        assert_eq!(
            base64::engine::general_purpose::STANDARD.decode(&token).unwrap().len(),
            32
        );
        assert!(made["pictureUrl"].is_string());
        picture_access(&fixture, made["pictureUrl"].as_str().unwrap()).await;
        for who in [0, 1] {
            let (status, listed) = fixture.call(who, "GET", "", None).await;
            assert_eq!(status, 200);
            assert!(!listed.to_string().contains(&token));
            assert!(listed["value"][0].get("securityToken").is_none());
            assert_eq!(listed["value"][0]["canManage"], who == 0);
            assert_eq!(listed["value"][0].get("callbackUrl").is_some(), who == 0);
        }
        for method in ["GET", "POST", "PUT", "DELETE"] {
            let suffix = if method == "PUT" || method == "DELETE" {
                format!("/{hook}")
            } else {
                String::new()
            };
            assert_eq!(fixture.call(2, method, &suffix, Some(form(&receiver.url))).await.0, 404);
        }
        assert_eq!(fixture.call(1, "POST", "", Some(form(&receiver.url))).await.0, 403);
        for method in ["PUT", "DELETE"] {
            assert_eq!(
                fixture
                    .call(1, method, &format!("/{hook}"), Some(json!({"name":"Denied"})))
                    .await
                    .0,
                403
            );
        }
        let (status, updated) = fixture
            .call(
                0,
                "PUT",
                &format!("/{hook}"),
                Some(json!({
                    "name":"QA renamed", "description":"Changed description", "picture":null,
                })),
            )
            .await;
        assert_eq!(status, 200);
        assert_eq!(updated["value"]["description"], "Changed description");
        assert!(updated["value"].get("pictureUrl").is_none());
        assert!(!updated.to_string().contains(&token));
        let member_token = member_lifecycle(&fixture, &receiver).await;
        fixture.rig.first.cli(&["user", "promote", "c@own.test"], "");
        let (status, admin) = fixture
            .call(
                2,
                "PUT",
                &format!("/{hook}"),
                Some(json!({"description":"Admin changed"})),
            )
            .await;
        assert_eq!(status, 200);
        assert_eq!(admin["value"]["canManage"], true);
        let admin_token = admin_create(&fixture, &receiver).await;
        assert_eq!(fixture.call(2, "DELETE", &format!("/{hook}"), None).await.0, 204);
        assert_eq!(fixture.call(0, "GET", "", None).await.1["value"], json!([]));
        fixture.stop(&[token, member_token, admin_token, PASSWORD.into()]).await;
    }
}

async fn picture_access(fixture: &Fixture, picture_url: &str) {
    let path = url::Url::parse(picture_url).unwrap().path().to_owned();
    for (upn, expected) in [("b@own.test", 200), ("c@own.test", 404)] {
        let node = &fixture.rig.first;
        let session = node.sign_in(upn, PASSWORD).await;
        let access = session.access_token(teams_auth::token::SPACES_RESOURCE).await.unwrap();
        let response = node
            .http
            .get(format!("{}{path}", node.endpoint))
            .bearer_auth(access.token.expose())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected);
        if expected == 200 {
            assert_eq!(response.headers()["content-type"], "image/png");
            assert_eq!(response.bytes().await.unwrap().as_ref(), crate::bots::png(24, 24));
        }
    }
}

async fn admin_create(fixture: &Fixture, receiver: &Receiver) -> String {
    let mut body = form(&receiver.url);
    body["name"] = json!("Admin hook");
    let (status, made) = fixture.call(2, "POST", "", Some(body)).await;
    assert_eq!(status, 201);
    let token = made["value"]["securityToken"].as_str().unwrap().to_owned();
    let hook = made["value"]["id"].as_str().unwrap();
    assert_eq!(fixture.call(0, "DELETE", &format!("/{hook}"), None).await.0, 204);
    token
}

async fn member_lifecycle(fixture: &Fixture, receiver: &Receiver) -> String {
    fixture.members_manage(true).await;
    let mut member_form = form(&receiver.url);
    member_form["name"] = json!("Member hook");
    let (status, made) = fixture.call(1, "POST", "", Some(member_form)).await;
    assert_eq!(status, 201);
    let member_token = made["value"]["securityToken"].as_str().unwrap().to_owned();
    let member_id = made["value"]["id"].as_str().unwrap();
    assert_eq!(
        fixture
            .call(
                1,
                "PUT",
                &format!("/{member_id}"),
                Some(json!({"name":"Member renamed"}))
            )
            .await
            .0,
        200
    );
    assert_eq!(fixture.call(1, "DELETE", &format!("/{member_id}"), None).await.0, 204);
    member_token
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authz_and_the_address_guard_follow_the_server_settings() {
    for backend in backends() {
        let fixture = Fixture::new(backend, "owh-guard", "\n[bots]\nallow_http_endpoints = true\n").await;
        let receiver = Receiver::start().await;
        for url in [
            &receiver.url,
            "http://10.1.2.3/hook",
            "http://169.254.169.254/hook",
            "http://[::1]/hook",
        ] {
            assert_eq!(fixture.call(0, "POST", "", Some(form(url))).await.0, 400);
        }
        let session = fixture.rig.first.sign_in("a@own.test", PASSWORD).await;
        session.skypetoken().await.unwrap();
        assert!(session.service_base("outgoingWebhookService").is_some());
        let name_url = receiver.url.replace("127.0.0.1", "localhost");
        let (status, made) = fixture.call(0, "POST", "", Some(form(&name_url))).await;
        assert_eq!(status, 201);
        let hook = made["value"]["id"].as_str().unwrap();
        let token = made["value"]["securityToken"].as_str().unwrap().to_owned();
        let post = mention(
            &fixture.rig.seats[0],
            &fixture.channel,
            hook,
            "the guard refuses DNS loopback",
        )
        .await;
        let reply = replies(&fixture.rig.seats[1], &fixture.channel, &post, hook, 1).await;
        assert_eq!(reply.len(), 1);
        assert!(reply[0].content.contains(APOLOGY));
        assert!(!receiver.gets_within(Duration::from_secs(2)).await);
        fixture.stop(&[token]).await;
        let off = Fixture::new(backend, "owh-off", "\n[outgoing_webhooks]\nenabled = false\n").await;
        let session = off.rig.first.sign_in("a@own.test", PASSWORD).await;
        session.skypetoken().await.unwrap();
        assert!(session.service_base("outgoingWebhookService").is_none());
        assert_eq!(off.call(0, "GET", "", None).await.0, 404);
        assert_eq!(off.call(0, "POST", "", Some(form(&receiver.url))).await.0, 404);
        off.stop(&[]).await;
    }
}
