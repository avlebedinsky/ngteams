use teams_core::ChannelDraft;
use teams_proto::chat::ChatReads;
use teams_proto::{ChannelChange, ChannelTarget, ChannelType, ChannelWrite, NewChannelKind, TeamWrite};

use super::{Seat, new_channel, refusal, server};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn a_private_channel_is_for_the_people_its_owner_adds() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let dave = Seat::new(&server, "dave@own.test", "Dave").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert, &cora]).await;
        let tenant = ngteams_server::db::meta::tenant(&server.app.db).await.unwrap();

        let made = ada
            .engine
            .create_channel(ChannelDraft {
                channel: new_channel(&team, &group, "Secret", NewChannelKind::Private),
                show_by_default: false,
            })
            .await
            .unwrap();
        let channel_group = made.group.unwrap();
        assert!(channel_group.starts_with("OID:") && channel_group.ends_with(&format!("@{tenant}")));
        let thread = ada
            .engine
            .channel_thread(team.as_str(), group.as_str(), channel_group.as_str(), "Secret")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(made.thread.as_deref(), Some(thread.as_str()));

        // someone who isn't on the team can't be in its private channel
        let failed = ada
            .engine
            .add_channel_members(
                group.as_str(),
                channel_group.as_str(),
                vec![bert.mri.clone(), dave.mri.clone()],
            )
            .await
            .unwrap();
        assert_eq!(failed, [dave.mri.clone()]);

        let own = ada.team(&team).await;
        let listed = own.channels.iter().find(|channel| channel.id == thread).unwrap();
        assert_eq!(listed.kind, ChannelType::Private);
        assert!(listed.owner && listed.shown);
        assert_eq!(
            listed.group.as_deref().map(|group| format!("OID:{group}@{tenant}")),
            Some(channel_group.clone())
        );
        let theirs = bert.team(&team).await;
        assert!(
            !theirs
                .channels
                .iter()
                .find(|channel| channel.id == thread)
                .unwrap()
                .owner
        );
        assert!(
            cora.team(&team)
                .await
                .channels
                .iter()
                .all(|channel| channel.id != thread)
        );
        let outside = ChatReads::new(&*cora.client);
        assert_eq!(outside.thread(&thread).await.unwrap_err().status(), Some(403));
        assert_eq!(outside.messages(&thread, 200).await.unwrap_err().status(), Some(403));
        let inside = ChatReads::new(&*bert.client);
        assert_eq!(
            inside.thread(&thread).await.unwrap().properties["productThreadType"],
            "TeamsPrivateChannel"
        );
        assert_eq!(inside.members(&thread).await.unwrap().members.len(), 2);

        let write = |action| {
            TeamWrite::Channel(ChannelWrite {
                target: ChannelTarget {
                    team: team.clone(),
                    team_group: group.clone(),
                    channel: thread.clone(),
                    channel_group: Some(channel_group.clone()),
                    mailbox: None,
                },
                action,
            })
        };
        let member = |seat: &Seat, owner| ChannelChange::UpdateMember {
            mri: seat.mri.clone(),
            owner,
        };
        let remove = |seat: &Seat| ChannelChange::RemoveMember { mri: seat.mri.clone() };
        // its only owner stays one
        assert_eq!(
            refusal(ada.engine.team_write(write(remove(&ada)), None).await).as_deref(),
            Some("LastOwnerCannotBeRemoved")
        );
        assert_eq!(
            refusal(ada.engine.team_write(write(member(&cora, true)), None).await).as_deref(),
            Some("MemberChangeRejected"),
            "someone outside the channel has no role in it"
        );
        ada.engine.team_write(write(member(&bert, true)), None).await.unwrap();
        assert!(
            bert.team(&team)
                .await
                .channels
                .iter()
                .any(|channel| channel.id == thread && channel.owner)
        );
        let add = ChannelChange::AddMember {
            mri: cora.mri.clone(),
            owner: false,
        };
        bert.engine.team_write(write(add), None).await.unwrap();
        assert!(
            cora.team(&team)
                .await
                .channels
                .iter()
                .any(|channel| channel.id == thread)
        );
        bert.engine.team_write(write(remove(&cora)), None).await.unwrap();
        assert!(
            cora.team(&team)
                .await
                .channels
                .iter()
                .all(|channel| channel.id != thread)
        );

        let edit = ChannelChange::Edit {
            name: "Hush".into(),
            description: "Need to know".into(),
        };
        bert.engine.team_write(write(edit), None).await.unwrap();
        let archive = ChannelChange::Archive {
            archived: true,
            site_read_only: false,
        };
        ada.engine.team_write(write(archive), None).await.unwrap();
        let listed = ada.team(&team).await;
        let hush = listed.channels.iter().find(|channel| channel.id == thread).unwrap();
        assert_eq!(hush.name, "Hush");
        assert!(hush.archived);

        // leaving the team takes someone out of its private channels too
        let leave = TeamWrite::UpdateMember {
            team: team.clone(),
            group: group.clone(),
            member: teams_proto::Mri::new(&bert.mri),
            update: teams_proto::MemberUpdate::Left,
        };
        bert.engine.team_write(leave, None).await.unwrap();
        assert_eq!(inside.thread(&thread).await.unwrap_err().status(), Some(403));

        ada.engine.team_write(write(ChannelChange::Delete), None).await.unwrap();
        assert!(
            ada.team(&team)
                .await
                .channels
                .iter()
                .all(|channel| channel.id != thread)
        );
        for seat in [ada, bert, cora, dave] {
            seat.stop();
        }
        server.stop().await;
    }
}
