use teams_core::ChannelDraft;
use teams_proto::chat::ChatReads;
use teams_proto::{ChannelChange, ChannelTarget, ChannelType, ChannelWrite, NewChannelKind, TeamWrite};

use super::{Seat, new_channel, refusal, server};
use crate::support::{Backend, TestServer, backends};

/// Ada's team with Bert and Cora on it, and its private channel with Ada (its owner) and Bert in it.
struct Secret {
    server: TestServer,
    ada: Seat,
    bert: Seat,
    cora: Seat,
    team: String,
    group: String,
    thread: String,
    channel_group: String,
}

impl Secret {
    async fn new(backend: Backend) -> Self {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let dave = Seat::new(&server, "dave@own.test", "Dave").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert, &cora]).await;
        let draft = ChannelDraft {
            channel: new_channel(&team, &group, "Secret", NewChannelKind::Private),
            show_by_default: false,
        };
        let made = ada.engine.create_channel(draft).await.unwrap();
        let channel_group = made.group.unwrap();
        let thread = ada
            .engine
            .channel_thread(team.as_str(), group.as_str(), channel_group.as_str(), "Secret")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(made.thread.as_deref(), Some(thread.as_str()));
        // someone who isn't on the team can't be in its private channel
        let people = vec![bert.mri.clone(), dave.mri.clone()];
        let failed = ada
            .engine
            .add_channel_members(group.as_str(), channel_group.as_str(), people)
            .await
            .unwrap();
        assert_eq!(failed, std::slice::from_ref(&dave.mri));
        dave.stop();
        Self {
            server,
            ada,
            bert,
            cora,
            team,
            group,
            thread,
            channel_group,
        }
    }

    fn write(&self, action: ChannelChange) -> TeamWrite {
        TeamWrite::Channel(ChannelWrite {
            target: ChannelTarget {
                team: self.team.clone(),
                team_group: self.group.clone(),
                channel: self.thread.clone(),
                channel_group: Some(self.channel_group.clone()),
                mailbox: None,
            },
            action,
        })
    }

    /// Whether `seat`'s teams read lists the channel, and as its owner.
    async fn listed(&self, seat: &Seat) -> Option<bool> {
        let team = seat.team(&self.team).await;
        team.channels
            .iter()
            .find(|channel| channel.id == self.thread)
            .map(|channel| channel.owner)
    }

    async fn stop(self) {
        for seat in [self.ada, self.bert, self.cora] {
            seat.stop();
        }
        self.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_private_channel_is_for_the_people_its_owner_adds() {
    for backend in backends() {
        let secret = Secret::new(backend).await;
        let tenant = ngteams_server::db::meta::tenant(&secret.server.app.db).await.unwrap();
        assert!(secret.channel_group.starts_with("OID:") && secret.channel_group.ends_with(&format!("@{tenant}")));

        let own = secret.ada.team(&secret.team).await;
        let listed = own.channels.iter().find(|channel| channel.id == secret.thread).unwrap();
        assert_eq!(listed.kind, ChannelType::Private);
        assert!(listed.owner && listed.shown);
        let named = listed.group.as_deref().map(|group| format!("OID:{group}@{tenant}"));
        assert_eq!(named.as_deref(), Some(secret.channel_group.as_str()));
        assert_eq!(secret.listed(&secret.bert).await, Some(false));
        assert_eq!(secret.listed(&secret.cora).await, None);

        let outside = ChatReads::new(&*secret.cora.client);
        assert_eq!(outside.thread(&secret.thread).await.unwrap_err().status(), Some(403));
        assert_eq!(
            outside.messages(&secret.thread, 200).await.unwrap_err().status(),
            Some(403)
        );
        let inside = ChatReads::new(&*secret.bert.client);
        let thread = inside.thread(&secret.thread).await.unwrap();
        assert_eq!(thread.properties["productThreadType"], "TeamsPrivateChannel");
        assert_eq!(inside.members(&secret.thread).await.unwrap().members.len(), 2);
        let channels = secret.cora.engine.team_channels(secret.group.as_str()).await.unwrap();
        assert!(channels.iter().all(|row| row.id != secret.thread));
        let channels = secret.bert.engine.team_channels(secret.group.as_str()).await.unwrap();
        assert!(
            channels
                .iter()
                .any(|row| row.id == secret.thread && row.kind == "Private")
        );
        secret.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_private_channels_owners_manage_its_people_and_the_channel() {
    for backend in backends() {
        let secret = Secret::new(backend).await;
        let (ada, bert, cora) = (&secret.ada, &secret.bert, &secret.cora);
        let member = |seat: &Seat, owner| ChannelChange::UpdateMember {
            mri: seat.mri.clone(),
            owner,
        };
        let remove = |seat: &Seat| ChannelChange::RemoveMember { mri: seat.mri.clone() };
        let write = |seat: &Seat, action| seat.engine.team_write(secret.write(action), None);

        // its only owner stays one, and someone outside the channel has no role in it
        assert_eq!(
            refusal(write(ada, remove(ada)).await).as_deref(),
            Some("LastOwnerCannotBeRemoved")
        );
        assert_eq!(
            refusal(write(ada, member(cora, true)).await).as_deref(),
            Some("MemberChangeRejected")
        );
        write(ada, member(bert, true)).await.unwrap();
        assert_eq!(secret.listed(bert).await, Some(true));
        let add = ChannelChange::AddMember {
            mri: cora.mri.clone(),
            owner: false,
        };
        write(bert, add).await.unwrap();
        assert_eq!(secret.listed(cora).await, Some(false));
        write(bert, remove(cora)).await.unwrap();
        assert_eq!(secret.listed(cora).await, None);

        let edit = ChannelChange::Edit {
            name: "Hush".into(),
            description: "Need to know".into(),
        };
        write(bert, edit).await.unwrap();
        let archive = ChannelChange::Archive {
            archived: true,
            site_read_only: false,
        };
        write(ada, archive).await.unwrap();
        let listed = ada.team(&secret.team).await;
        let hush = listed
            .channels
            .iter()
            .find(|channel| channel.id == secret.thread)
            .unwrap();
        assert_eq!(hush.name, "Hush");
        assert!(hush.archived);

        // leaving the team takes someone out of its private channels too
        let leave = TeamWrite::UpdateMember {
            team: secret.team.clone(),
            group: secret.group.clone(),
            member: teams_proto::Mri::new(&bert.mri),
            update: teams_proto::MemberUpdate::Left,
        };
        bert.engine.team_write(leave, None).await.unwrap();
        let gone = ChatReads::new(&*bert.client).thread(&secret.thread).await.unwrap_err();
        assert_eq!(gone.status(), Some(403));

        write(ada, ChannelChange::Delete).await.unwrap();
        assert_eq!(secret.listed(ada).await, None);
        secret.stop().await;
    }
}
