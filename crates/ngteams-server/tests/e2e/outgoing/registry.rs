//! Registering a team's outgoing webhooks on the middle tier: who may, what is checked, and that the token shows once.

use base64::Engine as _;
use reqwest::Method;
use serde_json::{Value, json};

use super::{Crew, hooks_path, key_of};
use crate::bots::{ROOT, png};
use crate::support::backends;

const ADA: &str = "ada@own.test";
const BERT: &str = "bert@own.test";
const CORA: &str = "cora@own.test";

fn hook_path(team: &str, id: &str) -> String {
    format!("{}/{id}", hooks_path(team))
}

/// The listed webhooks' names.
fn names(list: &Value) -> Vec<&str> {
    list["value"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hook| hook["name"].as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owner_registers_a_webhook_and_sees_its_token_once_and_the_team_sees_it_listed() {
    for backend in backends() {
        let crew = Crew::open(backend).await;
        let path = hooks_path(&crew.team);
        let picture = png(96, 96);
        let body = json!({
            "name": "Echo hook (synthetic)",
            "callbackUrl": "https://hooks.example.test/teams?x=1",
            "description": "Says it back",
            "picture": base64::engine::general_purpose::STANDARD.encode(&picture),
        });
        let (status, made) = crew.mt(ADA, Method::POST, &path, Some(body)).await;
        assert_eq!(status, 201, "{made}");
        let made = &made["value"];
        let id = made["id"].as_str().unwrap();
        assert!(uuid::Uuid::parse_str(id).is_ok(), "{made}");
        assert_eq!(made["mri"], format!("28:{id}"));
        assert_eq!(made["name"], "Echo hook (synthetic)");
        assert_eq!(made["description"], "Says it back");
        assert_eq!(made["callbackUrl"], "https://hooks.example.test/teams?x=1");
        assert_eq!(made["teamId"], crew.team.as_str());
        assert_eq!(key_of(made["securityToken"].as_str().unwrap()).len(), 32);

        // the team's people see it listed, never its token; only those who may change it see its address
        let (status, mine) = crew.mt(ADA, Method::GET, &path, None).await;
        assert_eq!(status, 200, "{mine}");
        assert_eq!(names(&mine), ["Echo hook (synthetic)"]);
        let listed = &mine["value"][0];
        assert!(listed.get("securityToken").is_none(), "{listed}");
        assert_eq!(listed["callbackUrl"], "https://hooks.example.test/teams?x=1");
        assert_eq!(listed["canManage"], true);
        crew.close_to_members().await;
        let (status, theirs) = crew.mt(BERT, Method::GET, &path, None).await;
        assert_eq!(status, 200, "{theirs}");
        let seen = &theirs["value"][0];
        assert!(
            seen.get("securityToken").is_none() && seen.get("callbackUrl").is_none(),
            "{seen}"
        );
        assert_eq!(seen["canManage"], false);
        assert_eq!(seen["mri"], format!("28:{id}"));

        // its picture, for the team's people
        let picture_path = format!("{}/picture", hook_path(&crew.team, id));
        let (status, bytes) = crew.mt_raw(BERT, Method::GET, &picture_path, None).await;
        assert_eq!(status, 200);
        assert_eq!(bytes, picture);
        let (status, _) = crew.mt_raw(CORA, Method::GET, &picture_path, None).await;
        assert_eq!(status, 404);

        // someone not on the team learns nothing
        let (status, _) = crew.mt(CORA, Method::GET, &path, None).await;
        assert_eq!(status, 404);
        crew.stop().await;
    }
}

async fn refuse_invalid_forms(crew: &Crew) {
    let path = hooks_path(&crew.team);
    // the form: a name, a callback address the server sends to, no second webhook of one name in a team
    let refused = [
        json!({"name": "", "callbackUrl": "https://hooks.example.test/a"}),
        json!({"name": "x".repeat(101), "callbackUrl": "https://hooks.example.test/a"}),
        json!({"name": "Plain", "callbackUrl": "ftp://hooks.example.test/a"}),
        json!({"name": "Plain", "callbackUrl": "not a url"}),
        json!({"name": "Plain", "callbackUrl": "https://user:pw@hooks.example.test/a"}),
        json!({"name": "Plain", "callbackUrl": "https://169.254.169.254/latest"}),
        json!({"name": "Plain", "callbackUrl": "https://hooks.example.test/a", "picture": "bm90IGEgcGljdHVyZQ=="}),
    ];
    for body in refused {
        let (status, answer) = crew.mt(ADA, Method::POST, &path, Some(body.clone())).await;
        assert_eq!(status, 400, "{body} {answer}");
    }
    let twin = json!({"name": "status (SYNTHETIC)", "callbackUrl": "https://hooks.example.test/b"});
    let (status, answer) = crew.mt(ADA, Method::POST, &path, Some(twin)).await;
    assert_eq!(status, 409, "{answer}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_webhook_is_changed_and_removed_by_those_who_may_and_its_form_is_checked() {
    for backend in backends() {
        let crew = Crew::open(backend).await;
        let path = hooks_path(&crew.team);
        let made = crew
            .create("Status (synthetic)", "https://hooks.example.test/status")
            .await;
        let id = made["id"].as_str().unwrap().to_owned();

        refuse_invalid_forms(&crew).await;

        // a member while the team's settings allow it; not once they don't; never someone off the team
        let bert_hook = json!({"name": "Bert's (synthetic)", "callbackUrl": "https://hooks.example.test/c"});
        let (status, answer) = crew.mt(BERT, Method::POST, &path, Some(bert_hook)).await;
        assert_eq!(status, 201, "{answer}");
        let bert_id = answer["value"]["id"].as_str().unwrap().to_owned();
        crew.close_to_members().await;
        let late = json!({"name": "Late (synthetic)", "callbackUrl": "https://hooks.example.test/d"});
        let (status, _) = crew.mt(BERT, Method::POST, &path, Some(late.clone())).await;
        assert_eq!(status, 403);
        let (status, _) = crew.mt(CORA, Method::POST, &path, Some(late)).await;
        assert_eq!(status, 404);
        let rename = json!({"name": "Renamed by Bert"});
        let (status, _) = crew
            .mt(BERT, Method::PUT, &hook_path(&crew.team, &id), Some(rename))
            .await;
        assert_eq!(status, 403);
        let (status, _) = crew.mt(BERT, Method::DELETE, &hook_path(&crew.team, &id), None).await;
        assert_eq!(status, 403);

        // An owner changes it. The answer does not show its token again.
        let change = json!({"name": "Status v2 (synthetic)", "callbackUrl": "https://hooks.example.test/v2",
            "description": "Newer"});
        let (status, changed) = crew
            .mt(ADA, Method::PUT, &hook_path(&crew.team, &id), Some(change))
            .await;
        assert_eq!(status, 200, "{changed}");
        assert_eq!(changed["value"]["name"], "Status v2 (synthetic)");
        assert_eq!(changed["value"]["callbackUrl"], "https://hooks.example.test/v2");
        assert!(changed["value"].get("securityToken").is_none());
        let (status, _) = crew
            .mt(
                ADA,
                Method::PUT,
                &hook_path(&crew.team, &id),
                Some(json!({"callbackUrl": "ftp://x"})),
            )
            .await;
        assert_eq!(status, 400);
        let (status, _) = crew
            .mt(
                ADA,
                Method::PUT,
                &hook_path(&crew.team, "4f1c6a3e-0000-4000-8000-000000000000"),
                Some(json!({"name": "Ghost"})),
            )
            .await;
        assert_eq!(status, 404);

        // the server's administrator manages any team's webhooks without a seat on it
        let (status, listed) = crew.mt(ROOT, Method::GET, &path, None).await;
        assert_eq!(status, 200, "{listed}");
        assert_eq!(names(&listed), ["Bert's (synthetic)", "Status v2 (synthetic)"]);
        let (status, _) = crew
            .mt(ROOT, Method::DELETE, &hook_path(&crew.team, &bert_id), None)
            .await;
        assert_eq!(status, 204);
        let (status, _) = crew.mt(ADA, Method::DELETE, &hook_path(&crew.team, &id), None).await;
        assert_eq!(status, 204);
        let (status, _) = crew.mt(ADA, Method::DELETE, &hook_path(&crew.team, &id), None).await;
        assert_eq!(status, 404);
        let (_, left) = crew.mt(ADA, Method::GET, &path, None).await;
        assert!(names(&left).is_empty());
        crew.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn authz_announces_outgoing_webhooks_only_while_the_server_serves_them() {
    for backend in backends() {
        let on = Crew::open(backend).await;
        let (_, answer) = on.mt(ADA, Method::GET, &hooks_path(&on.team), None).await;
        assert!(answer["value"].is_array());
        let session = on.registry.server.session(ADA).await;
        session.skypetoken().await.unwrap();
        assert!(session.service_base("outgoingWebhookService").is_some());
        on.stop().await;

        let off = Crew::open_with(backend, |config| config.outgoing_webhooks.enabled = false).await;
        let session = off.registry.server.session(ADA).await;
        session.skypetoken().await.unwrap();
        assert!(session.service_base("outgoingWebhookService").is_none());
        let body = json!({"name": "Off (synthetic)", "callbackUrl": "https://hooks.example.test/off"});
        let (status, _) = off.mt(ADA, Method::POST, &hooks_path(&off.team), Some(body)).await;
        assert_eq!(status, 404);
        off.stop().await;
    }
}

async fn picture_bytes(crew: &Crew, id: &str) -> (u16, Vec<u8>) {
    crew.mt_raw(
        BERT,
        Method::GET,
        &format!("{}/picture", hook_path(&crew.team, id)),
        None,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_name_change_keeps_the_picture_and_its_url() {
    for backend in backends() {
        let crew = Crew::open(backend).await;
        let path = hooks_path(&crew.team);
        let picture = png(48, 48);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&picture);
        let (status, made) = crew
            .mt(
                ADA,
                Method::POST,
                &path,
                Some(json!({
                    "name": "First (synthetic)", "callbackUrl": "https://hooks.example.test/a", "picture": encoded
                })),
            )
            .await;
        assert_eq!(status, 201);
        let id = made["value"]["id"].as_str().unwrap();
        let original_url = &made["value"]["pictureUrl"];
        crew.create("Second (synthetic)", "https://hooks.example.test/b").await;
        let replacement = png(64, 64);
        for picture_change in [
            Value::Null,
            json!(base64::engine::general_purpose::STANDARD.encode(&replacement)),
        ] {
            let (status, _) = crew
                .mt(
                    ADA,
                    Method::PUT,
                    &hook_path(&crew.team, id),
                    Some(json!({"name": "Second (synthetic)", "picture": picture_change})),
                )
                .await;
            assert_eq!(status, 409);
            let (_, listed) = crew.mt(ADA, Method::GET, &path, None).await;
            let listed = listed["value"]
                .as_array()
                .unwrap()
                .iter()
                .find(|hook| hook["id"] == id)
                .unwrap();
            assert_eq!(&listed["pictureUrl"], original_url);
            let (status, bytes) = picture_bytes(&crew, id).await;
            assert_eq!(status, 200);
            assert_eq!(bytes, picture);
        }
        let (status, changed) = crew
            .mt(
                ADA,
                Method::PUT,
                &hook_path(&crew.team, id),
                Some(json!({"picture": base64::engine::general_purpose::STANDARD.encode(&replacement)})),
            )
            .await;
        assert_eq!(status, 200);
        assert_ne!(&changed["value"]["pictureUrl"], original_url);
        let (status, bytes) = picture_bytes(&crew, id).await;
        assert_eq!(status, 200);
        assert_eq!(bytes, replacement);
        let (status, changed) = crew
            .mt(
                ADA,
                Method::PUT,
                &hook_path(&crew.team, id),
                Some(json!({"picture": null})),
            )
            .await;
        assert_eq!(status, 200);
        assert!(changed["value"].get("pictureUrl").is_none());
        let (status, _) = picture_bytes(&crew, id).await;
        assert_eq!(status, 404);
        crew.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picture_from_the_old_file_layout_can_be_read_and_replaced() -> Result<(), Box<dyn std::error::Error>> {
    use diesel::sql_types::Text;
    use diesel_async::RunQueryDsl as _;
    use ngteams_server::db::with_conn;

    for backend in backends() {
        let dir = tempfile::tempdir().unwrap();
        let crew = Crew::open_with(backend, |config| {
            config.storage_url = url::Url::from_directory_path(dir.path()).unwrap().to_string();
        })
        .await;
        let made = crew
            .create("Old picture (synthetic)", "https://hooks.example.test/old")
            .await;
        let id = made["id"].as_str().unwrap();
        let app = &crew.registry.server.app;
        let old_key = format!("outgoing-webhooks/{id}/picture");
        let original = png(48, 48);
        app.storage.put(&old_key, original.clone().into()).await.unwrap();
        with_conn!(&app.db, |conn| diesel::sql_query(
            "UPDATE outgoing_webhooks SET picture_etag = 'legacy' WHERE id = $1"
        )
        .bind::<Text, _>(id)
        .execute(conn)
        .await?);
        let (status, bytes) = picture_bytes(&crew, id).await;
        assert_eq!(status, 200);
        assert_eq!(bytes, original);
        let replacement = png(64, 64);
        let (status, _) = crew
            .mt(
                ADA,
                Method::PUT,
                &hook_path(&crew.team, id),
                Some(json!({"picture": base64::engine::general_purpose::STANDARD.encode(&replacement)})),
            )
            .await;
        assert_eq!(status, 200);
        let (status, bytes) = picture_bytes(&crew, id).await;
        assert_eq!(status, 200);
        assert_eq!(bytes, replacement);
        assert!(app.storage.get(&old_key).await.unwrap().is_none());
        crew.stop().await;
    }
    Ok(())
}
