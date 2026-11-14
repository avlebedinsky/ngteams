use teams_core::engine::Action;
use teams_proto::chat::MessageText;
use teams_proto::{AnalyticsPeriod, ChannelChange, RawBody, TeamCopy, TeamPicture, TeamWrite, reply_chain_id};

use super::{Seat, channel_write, server};
use crate::support::backends;

const PNG: [u8; 12] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];

async fn send(seat: &Seat, conversation: &str, text: &str) -> String {
    let row = seat
        .engine
        .write(Action::Send {
            conversation: conversation.into(),
            text: MessageText::plain(text).unwrap(),
        })
        .await
        .unwrap();
    row.message_id.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn manage_team_lists_channels_and_counts_what_was_written() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;
        let archived = ada.make_channel(&team, &group, "Old plans", false).await;
        let archive = ChannelChange::Archive {
            archived: true,
            site_read_only: false,
        };
        ada.engine
            .team_write(channel_write(&team, &group, &archived, archive), None)
            .await
            .unwrap();

        let listed = bert.engine.team_channels(group.as_str()).await.unwrap();
        let rows: Vec<(&str, &str, bool)> = listed
            .iter()
            .map(|row| (row.name.as_str(), row.kind.as_str(), row.archived))
            .collect();
        assert_eq!(
            rows,
            [
                ("General", "Standard", false),
                ("Launch", "Standard", false),
                ("Old plans", "Standard", true)
            ]
        );
        assert_eq!(listed[1].id, channel);
        assert_eq!(listed[1].description, "About Launch");
        assert!(cora.engine.team_channels(group.as_str()).await.unwrap().is_empty());
        assert!(ada.engine.pending_invites().await.unwrap().is_empty());
        assert_eq!(ada.engine.owned_groups().await.unwrap(), Some(0));
        assert!(ada.engine.team_templates().await.unwrap().is_empty());

        let post = send(&ada, &channel, "Fuel is loaded").await;
        send(&ada, &team, "Welcome aboard").await;
        bert.engine.sync_conversation(channel.as_str()).await.unwrap();
        let reply = send(&bert, &reply_chain_id(&channel, &post).unwrap(), "Go for launch").await;
        ada.engine.sync_conversation(channel.as_str()).await.unwrap();
        ada.engine
            .write(Action::React {
                conversation: channel.clone(),
                message_id: reply,
                key: "like".into(),
            })
            .await
            .unwrap();

        let week = AnalyticsPeriod::LastSevenDays;
        let numbers = bert.engine.team_analytics(group.as_str(), week).await.unwrap().unwrap();
        assert_eq!(
            (numbers.team.as_str(), numbers.name.as_str()),
            (group.as_str(), "Rocket")
        );
        assert_eq!((numbers.posts, numbers.replies, numbers.reactions), (2, 1, 1));
        assert_eq!(numbers.active_users, 2);
        assert_eq!(numbers.active_users_by_day.len(), 7);
        assert_eq!(numbers.active_users_by_day.last().unwrap().1, 2);
        let month = AnalyticsPeriod::LastThirtyDays;
        let of_channel = ada
            .engine
            .channel_analytics(group.as_str(), channel.as_str(), month)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((of_channel.posts, of_channel.replies), (1, 1));
        assert_eq!(of_channel.active_users_by_day.len(), 30);
        let all = ada.engine.all_teams_analytics(week).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].posts, 2);
        assert_eq!(cora.engine.team_analytics(group.as_str(), week).await.unwrap(), None);
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owner_sets_the_teams_picture_and_copies_the_team() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        ada.make_channel(&team, &group, "Launch", true).await;

        let picture = |seat: &Seat, etag: &str| TeamPicture {
            own: seat.mri.clone(),
            group: group.clone(),
            name: "Rocket".into(),
            etag: etag.into(),
        };
        assert_eq!(ada.team(&team).await.about.picture_etag, None);
        assert!(ada.engine.team_picture(picture(&ada, "")).await.unwrap().is_none());
        let set = |bytes: &[u8]| TeamWrite::SetPicture {
            team: team.clone(),
            group: group.clone(),
            picture: RawBody::from(bytes.to_vec()),
        };
        assert!(bert.engine.team_write(set(&PNG), None).await.is_err());
        assert!(ada.engine.team_write(set(b"not a picture"), None).await.is_err());
        ada.engine.team_write(set(&PNG), None).await.unwrap();
        let etag = bert.team(&team).await.about.picture_etag.unwrap();
        let read = bert.engine.team_picture(picture(&bert, &etag)).await.unwrap().unwrap();
        assert_eq!(read.bytes, PNG);
        assert!(cora.engine.team_picture(picture(&cora, &etag)).await.unwrap().is_none());

        let copy = |seat: &Seat, roster| {
            TeamWrite::Copy(TeamCopy {
                group: group.clone(),
                name: "Rocket [copy]".into(),
                description: "Again".into(),
                public: true,
                owner: seat.id.clone(),
                roster,
                channels: true,
                apps: true,
                settings: true,
                tabs: true,
            })
        };
        assert!(bert.engine.team_write(copy(&bert, false), None).await.is_err());
        ada.engine.team_write(copy(&ada, true), None).await.unwrap();
        let teams = ada.listed().await;
        assert_eq!(teams.len(), 2);
        let copied = &teams[1];
        assert_eq!(copied.name, "Rocket [copy]");
        assert!(copied.about.public && copied.owner);
        assert_eq!(copied.about.people, 2);
        let names: Vec<&str> = copied.channels.iter().map(|channel| channel.name.as_str()).collect();
        assert_eq!(names, ["General", "Launch"]);
        assert_eq!(bert.listed().await.len(), 2, "its people came along");
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}
