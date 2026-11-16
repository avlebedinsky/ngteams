//! A bot is an account row of its own kind (`users.kind` `app`), and no people read finds it: not the admin's
//! account list or account changes, not a sign-in, not a profile lookup by MRI or by address, not a team's people
//! search.

use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::admin::AdminClient;
use teams_auth::secrets::Secret;
use teams_proto::{Client, Mri, ShortProfileLookup};

use super::Registry;
use crate::presence::transport;
use crate::support::{PASSWORD, backends};
use crate::trouter::ORIGIN;

const WORKFLOWS_BOT: &str = "358f0194-6b0e-4dd3-af35-c24fe8a9ec87";

#[tokio::test(flavor = "multi_thread")]
async fn no_people_read_finds_a_bot() {
    for backend in backends() {
        let registry = Registry::open_with(backend, |config| {
            config.public_origin = url::Url::parse(ORIGIN).unwrap();
        })
        .await;
        let server = &registry.server;
        let ada = server.add_user("ada@own.test", "Ada Echo").await;
        let echo = registry
            .register("Echo (synthetic)", "https://bot.example.test/api/messages")
            .await;
        let bot = echo["id"].as_str().unwrap().to_owned();

        // the admin's list and every account change
        let admin = AdminClient::with_loopback(&server.origin).unwrap();
        let token = Secret::new(registry.token().await);
        let page = admin.users(&token, "", None, 50).await.unwrap();
        let listed: Vec<&str> = page.users.iter().map(|user| user.id.as_str()).collect();
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert!(!listed.contains(&bot.as_str()) && !listed.contains(&WORKFLOWS_BOT));
        let page = admin.users(&token, "echo", None, 50).await.unwrap();
        assert_eq!(
            page.users.iter().map(|user| user.id.as_str()).collect::<Vec<_>>(),
            [ada.as_str()]
        );
        for id in [bot.as_str(), WORKFLOWS_BOT] {
            let user = format!("/api/admin/v1/users/{id}");
            let password = json!({"password": PASSWORD});
            let calls: [(Method, String, Option<Value>); 4] = [
                (Method::GET, user.clone(), None),
                (Method::PATCH, user.clone(), Some(json!({"enabled": true}))),
                (Method::POST, format!("{user}/password"), Some(password)),
                (Method::DELETE, user.clone(), None),
            ];
            for (method, path, body) in calls {
                let (status, answer) = registry.call(method.clone(), &path, body).await;
                assert_eq!(status, 404, "{method} {path}: {answer}");
            }
        }

        // a sign-in by the bot's row
        for upn in [format!("app:{bot}"), "workflows@apps.invalid".to_owned()] {
            let (status, answer) = server
                .password_grant(&upn, PASSWORD, &teams_auth::token::Scope::spaces())
                .await;
            assert_eq!(status, 400, "{answer}");
        }

        // a profile by MRI or address, and a team's people search
        let session = std::sync::Arc::new(server.session("ada@own.test").await);
        let client = Client::new(transport(&session, server, &ada).await);
        let lookup = ShortProfileLookup::new([
            Mri::new(format!("8:orgid:{bot}")),
            Mri::new(format!("28:{bot}")),
            Mri::new(format!("8:orgid:{ada}")),
        ])
        .unwrap();
        let answer = client.post_read(lookup.clone()).await.unwrap();
        let profiles = lookup.read_answer(answer.body()).unwrap();
        assert_eq!(
            profiles.iter().map(|profile| profile.mri.as_str()).collect::<Vec<_>>(),
            [format!("8:orgid:{ada}")]
        );
        let found = ngteams_server::people::repo::people_by_address(&server.app.db, &[format!("app:{bot}")])
            .await
            .unwrap();
        assert!(found.is_empty());
        let directory = ngteams_server::db::users::list(&server.app.db).await.unwrap();
        assert!(directory.iter().all(|user| user.id != bot && user.id != WORKFLOWS_BOT));
        assert!(
            ngteams_server::db::users::by_upn(&server.app.db, &format!("app:{bot}"))
                .await
                .unwrap()
                .is_none()
        );
        registry.server.stop().await;
    }
}
