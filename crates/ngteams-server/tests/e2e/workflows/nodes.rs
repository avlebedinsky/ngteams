//! Two nodes on one database: a workflow saved through one node takes webhook posts on either, and the other node's
//! account reads it and its runs.

use teams_proto::WorkflowRequest;

use super::flows::group_chat;
use super::hooks::{card_payload, post};
use super::{ORIGIN, Seat, Target, ask, environment, rows, save, server, webhook_link};
use crate::support::{TestServer, backends, redis_url};

#[tokio::test(flavor = "multi_thread")]
async fn a_webhook_link_works_on_either_node() {
    for backend in backends() {
        let first = server(backend).await;
        let url = first.app.config.database_url.clone();
        let second = TestServer::start_with(backend, |config| {
            config.database_url = url;
            config.public_origin = url::Url::parse(ORIGIN).unwrap();
            config.redis_url = redis_url();
        })
        .await;
        let ada = Seat::new(&first, "ada@own.test", "Ada").await;
        let bert_id = first.add_user("bert@own.test", "Bert").await;
        let bert = Seat::of(&second, "bert@own.test", bert_id).await;
        let environment = environment(&ada).await;
        let chat = group_chat(&ada, &[&bert], "Project room").await;
        let flow = save(&ada, &environment, &Target::Chat(&chat)).await;
        let id = flow["name"].as_str().unwrap().to_owned();
        let link = webhook_link(&ada, &environment, &id).await;
        let payload = card_payload("From either node").to_string();

        assert_eq!(post(&first, &link, payload.as_bytes()).await, 202);
        assert_eq!(post(&second, &link, payload.as_bytes()).await, 202);
        let history = ask(
            &ada,
            WorkflowRequest::History {
                environment: environment.clone(),
                flow: id.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(rows(&history).len(), 2);
        let page = teams_proto::chat::ChatReads::new(&*bert.client)
            .messages(&chat, 50)
            .await
            .unwrap();
        let posted = page
            .messages
            .iter()
            .filter(|message| message.display_name.as_deref() == Some("Workflows"))
            .count();
        assert_eq!(posted, 2);

        // turned off through the second node, the first refuses too
        let off = Seat::of(&second, "ada@own.test", ada.id.clone()).await;
        ask(
            &off,
            WorkflowRequest::SetEnabled {
                environment: environment.clone(),
                flow: id.clone(),
                enabled: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(post(&first, &link, payload.as_bytes()).await, 409);
        off.stop();
        ada.stop();
        bert.stop();
        second.stop().await;
        first.stop().await;
    }
}
