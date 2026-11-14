//! Two nodes on one database serve one team, and a team's changes are published to its people.

use std::time::Duration;

use serde_json::Value;
use teams_core::engine::Action;
use teams_proto::TeamWrite;
use teams_proto::chat::{ChatReads, MessageText};
use teams_store::OutboxState;

use super::{ORIGIN, Seat, server};
use crate::support::{TestServer, backends, redis_url};

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_serve_one_team_and_tell_its_people_of_its_changes() {
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
        let mut events = first.app.fanout.subscribe(&format!("push:{}", bert.mri)).await.unwrap();

        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        let event: Value = serde_json::from_slice(&event).unwrap();
        assert_eq!(event["resourceType"], "ThreadUpdate");
        assert_eq!(event["resource"]["id"], team.as_str());
        assert_eq!(event["resource"]["properties"]["threadType"], "space");
        assert_eq!(event["resource"]["members"][0]["id"], bert.mri.as_str());

        // the other node lists the team at once and takes its people's writes
        let listed = bert.team(&team).await;
        assert_eq!(listed.about.people, 2);
        let channel = bert.make_channel(&team, &group, "Launch", false).await;
        let sent = bert
            .engine
            .write(Action::Send {
                conversation: channel.clone(),
                text: MessageText::plain("From the second node").unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(sent.state, OutboxState::Sent);
        let page = ChatReads::new(&*ada.client).messages(&channel, 200).await.unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(ada.team(&team).await.channels.len(), 2);

        // two nodes taking the same name at once make one channel
        let name = |seat: &Seat| {
            seat.engine.create_channel(teams_core::ChannelDraft {
                channel: super::new_channel(&team, &group, "Countdown", teams_proto::NewChannelKind::Standard),
                show_by_default: false,
            })
        };
        let (one, other) = tokio::join!(name(&ada), name(&bert));
        assert!(one.is_ok() != other.is_ok(), "{one:?} {other:?}");
        assert_eq!(ada.team(&team).await.channels.len(), 3);

        // and an owner leaving on one node while being demoted on the other never leaves the team ownerless
        let promote = TeamWrite::UpdateMember {
            team: team.clone(),
            group: group.clone(),
            member: teams_proto::Mri::new(&bert.mri),
            update: teams_proto::MemberUpdate::Promoted,
        };
        ada.engine.team_write(promote, None).await.unwrap();
        let leave = |seat: &Seat| {
            seat.engine.team_write(
                TeamWrite::UpdateMember {
                    team: team.clone(),
                    group: group.clone(),
                    member: teams_proto::Mri::new(&seat.mri),
                    update: teams_proto::MemberUpdate::DemotedAndLeft,
                },
                None,
            )
        };
        let (one, other) = tokio::join!(leave(&ada), leave(&bert));
        assert!(one.is_ok() != other.is_ok(), "{one:?} {other:?}");
        ada.stop();
        bert.stop();
        first.stop().await;
        second.stop().await;
    }
}
