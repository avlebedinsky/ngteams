//! Two nodes on one database serve one calendar, and a meeting is told to the people it invites from either.

use std::time::Duration;

use serde_json::Value;
use teams_core::EventWrite;
use teams_proto::{EventResponse, RsvpAnswer};

use super::meetings::invited;
use super::{HOUR, MONDAY, Seat, draft, week};
use crate::support::{TestServer, backends, redis_url};
use crate::teams::ORIGIN;

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_serve_one_calendar_and_tell_the_invited_of_a_meeting() {
    for backend in backends() {
        let first = TestServer::start_with(backend, |config| {
            config.public_origin = url::Url::parse(ORIGIN).unwrap();
            config.redis_url = redis_url();
        })
        .await;
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
        // with Redis the two nodes share their fan-out: what one publishes the other's sockets get
        let listening = if redis_url().is_some() { &second } else { &first };
        let topic = format!("push:{}", bert.mri);
        let mut events = listening.app.fanout.subscribe(&topic).await.unwrap();

        let mut planning = draft("Planning", MONDAY + 9 * HOUR, 30);
        planning.online = true;
        planning.attendees = vec![invited("bert@own.test", "Bert", None, true)];
        let created = ada.engine.write_event(EventWrite::Create(planning)).await.unwrap();
        let created = created.created.unwrap();
        let mut told = Vec::new();
        while told.len() < 3 {
            let event = tokio::time::timeout(Duration::from_secs(5), events.recv()).await;
            let event: Value = serde_json::from_slice(&event.unwrap().unwrap()).unwrap();
            told.push(
                event["resource"]["conversationid"]
                    .as_str()
                    .unwrap_or("a thread")
                    .to_owned(),
            );
        }
        told.sort();
        let chat = created.meeting_chat.clone().unwrap();
        assert_eq!(told, [chat.as_str(), "48:notifications", "a thread"], "{backend:?}");

        // the other node has the event at once and takes the invited account's answer
        let theirs = week(&bert).await;
        assert_eq!(theirs.len(), 1, "{theirs:?}");
        assert_eq!(theirs[0].meeting_chat.as_deref(), Some(chat.as_str()));
        bert.engine
            .write_event(EventWrite::Respond {
                id: created.id.clone(),
                answer: RsvpAnswer::Accept,
                send: true,
                comment: None,
            })
            .await
            .unwrap();
        let whole = ada.engine.calendar_event(created.id).await.unwrap();
        assert_eq!(whole.event.attendees[1].response, Some(EventResponse::Accepted));
        ada.stop();
        bert.stop();
        first.stop().await;
        second.stop().await;
    }
}
