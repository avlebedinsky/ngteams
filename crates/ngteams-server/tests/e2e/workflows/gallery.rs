//! The Workflows dialog's and app's reads: the environment, the channel catalog, the app's gallery and a template.

use teams_proto::{WorkflowFailure, WorkflowRequest};

use super::{CHANNEL_TEMPLATE, CHAT_TEMPLATE, Seat, ask, environment, rows, server, template};
use crate::support::backends;

fn names(answer: &serde_json::Value, key: &str) -> Vec<String> {
    rows(answer)
        .iter()
        .map(|row| row.pointer(key).and_then(serde_json::Value::as_str).unwrap().to_owned())
        .collect()
}

/// The Workflows app: Home and Templates hold both templates, each whole.
async fn app_gallery(ada: &Seat) {
    for category in ["neptune_teams_pa_home", "home_featured"] {
        let gallery = ask(
            ada,
            WorkflowRequest::Gallery {
                category: category.into(),
                search: None,
                top: 16,
            },
        )
        .await
        .unwrap();
        let mut found = names(&gallery, "/name");
        found.sort();
        assert_eq!(found, [CHAT_TEMPLATE, CHANNEL_TEMPLATE], "{category}");
        for row in rows(&gallery) {
            assert!(row["properties"]["displayName"].is_string());
            assert!(row["properties"]["categoryNames"].as_array().unwrap().len() > 1);
            assert_eq!(
                row["properties"]["connectionReferences"]["shared_teams"]["api"]["name"],
                "shared_teams"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_lists_the_gallery_and_opens_the_webhook_templates() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let tenant = ngteams_server::db::meta::tenant(&server.app.db).await.unwrap();

        assert_eq!(environment(&ada).await, format!("Default-{tenant}"));

        // the channel's Workflows dialog: the channel webhook template only, in the catalog's shape
        let catalog = ask(&ada, WorkflowRequest::Catalog).await.unwrap();
        assert_eq!(names(&catalog, "/id"), [CHANNEL_TEMPLATE]);
        let entry = &rows(&catalog)[0];
        assert_eq!(entry["name"], "Send webhook alerts to a channel");
        assert!(entry["description"].as_str().unwrap().contains("webhook"));

        // the Workflows app: Home and Templates hold both, a search finds by name
        app_gallery(&ada).await;
        let searched = ask(
            &ada,
            WorkflowRequest::Gallery {
                category: "home_featured".into(),
                search: Some("CHAT".into()),
                top: 10,
            },
        )
        .await
        .unwrap();
        assert_eq!(names(&searched, "/name"), [CHAT_TEMPLATE]);
        let none = ask(
            &ada,
            WorkflowRequest::Gallery {
                category: "schedule".into(),
                search: None,
                top: 10,
            },
        )
        .await
        .unwrap();
        assert!(rows(&none).is_empty());

        // a template opens in the card editor: its markup, definition and Teams connection
        let channel = template(&ada, CHANNEL_TEMPLATE).await;
        let properties = &channel["properties"];
        assert_eq!(properties["displayName"], "Send webhook alerts to a channel");
        assert_eq!(properties["definition"]["triggers"]["manual"]["kind"], "TeamsWebhook");
        let markup = &properties["metadata"]["userIntentMarkup"]["automation"]["template"];
        assert_eq!(markup["id"], CHANNEL_TEMPLATE);
        assert_eq!(markup["parameters"][0]["typeName"], "Teams.Group");
        assert_eq!(markup["parameters"][1]["typeName"], "Teams.Channel");
        assert_eq!(
            properties["connectionReferences"]["shared_teams"]["api"]["id"],
            "/providers/Microsoft.PowerApps/apis/shared_teams"
        );
        let chat = template(&ada, CHAT_TEMPLATE).await;
        assert_eq!(chat["properties"]["displayName"], "Send webhook alerts to a chat");
        assert_eq!(
            chat["properties"]["metadata"]["userIntentMarkup"]["automation"]["template"]["parameters"][0]["typeName"],
            "Teams.Chat"
        );
        assert_eq!(
            ask(
                &ada,
                WorkflowRequest::Template {
                    id: "805739fdad754e799502b6379900922e".into()
                }
            )
            .await,
            Err(WorkflowFailure::NotFound)
        );
        ada.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_makes_its_teams_connection_behind_the_scenes_and_reuses_it() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let environment = environment(&ada).await;
        let listed = async |seat: &Seat| {
            ask(
                seat,
                WorkflowRequest::Connections {
                    environment: environment.clone(),
                },
            )
            .await
        };
        assert!(rows(&listed(&ada).await.unwrap()).is_empty());
        let made = super::teams_connection(&ada, &environment).await;
        assert!(made.starts_with("shared-teams-"));
        let connections = listed(&ada).await.unwrap();
        assert_eq!(rows(&connections).len(), 1);
        let connection = &rows(&connections)[0];
        assert_eq!(connection["name"], made.as_str());
        assert_eq!(connection["properties"]["statuses"][0]["status"], "Connected");
        assert_eq!(connection["properties"]["environment"]["name"], environment.as_str());
        // opening the template again reuses it; another account neither sees nor takes it
        assert_eq!(super::teams_connection(&ada, &environment).await, made);
        assert!(rows(&listed(&bert).await.unwrap()).is_empty());
        let taken = ask(
            &bert,
            WorkflowRequest::AuthorizeConnection {
                environment: environment.clone(),
                connector: "shared_teams".into(),
                connection: made.clone(),
            },
        )
        .await;
        assert_eq!(taken, Err(WorkflowFailure::Forbidden));
        // a connector this server has no connection for is refused
        let other = ask(
            &ada,
            WorkflowRequest::CreateConnection {
                environment: environment.clone(),
                connector: "shared_planner".into(),
                connection: "shared-planner-1".into(),
            },
        )
        .await;
        assert_eq!(other, Err(WorkflowFailure::NotFound));
        // another environment is none of the account's
        let elsewhere = ask(
            &ada,
            WorkflowRequest::Connections {
                environment: "Default-elsewhere".into(),
            },
        )
        .await;
        assert_eq!(elsewhere, Err(WorkflowFailure::NotFound));
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}
