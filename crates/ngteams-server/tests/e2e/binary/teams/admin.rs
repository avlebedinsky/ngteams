use serde_json::{Value, json};
use teams_core::{ChannelDraft, ChannelProblem, EngineError};
use teams_proto::chat::ChatReads;
use teams_proto::{ChannelChange, ChannelTarget, ChannelWrite, MemberUpdate, Mri, NewChannelKind, TeamWrite};
use teams_store::OutboxState;

use super::support::{Rig, Seat};
use crate::support::backends;
use crate::teams::{channel_write, new_channel, refusal};

fn member(team: &str, group: &str, who: &Seat, update: MemberUpdate) -> TeamWrite {
    TeamWrite::UpdateMember {
        team: team.into(),
        group: group.into(),
        member: Mri::new(&who.mri),
        update,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_renames_a_team_and_sets_what_its_members_may_do() {
    for backend in backends() {
        let rig = Rig::start(backend, "manage").await;
        let [a, b, c] = &rig.seats;
        let (team, group) = a.make_team("Rocket", false).await;
        a.add(&team, &group, &[b, c]).await;
        let channel = a.make_channel(&team, &group, "Launch", true).await;

        let edit = |name: Option<&str>, description: &str| TeamWrite::Edit {
            team: team.clone(),
            name: name.map(str::to_owned),
            description: description.into(),
            public: None,
        };
        let refused = b.engine().team_write(edit(Some("Mine"), "mine"), None).await;
        assert_eq!(refusal(refused).as_deref(), Some("Forbidden"));
        a.engine()
            .team_write(edit(Some("Apollo"), "To the moon"), None)
            .await
            .unwrap();
        a.engine()
            .team_write(edit(None, "To the moon and back"), None)
            .await
            .unwrap();
        for seat in [a, b, c] {
            let listed = seat.team(&team).await;
            assert_eq!(
                (listed.name.as_str(), listed.about.description.as_str()),
                ("Apollo", "To the moon and back")
            );
        }

        {
            let team: &str = &team;
            let group: &str = &group;
            let read = a.engine().team_settings(team).await.unwrap().unwrap();
            let mut settings: Value = serde_json::from_str(&read).unwrap();
            assert_eq!(settings["userSettings"]["createTopic"], true);
            for key in ["createTopic", "updateTopic", "deleteTopic"] {
                settings["userSettings"][key] = json!(false);
            }
            let write = TeamWrite::Settings {
                team: team.into(),
                settings: settings.to_string(),
            };
            assert!(
                b.engine().team_write(write.clone(), None).await.is_err(),
                "a member's write"
            );
            assert_eq!(a.engine().team_settings(team).await.unwrap(), Some(read));
            a.engine().team_write(write, None).await.unwrap();
            let kept = b.engine().team_settings(team).await.unwrap().unwrap();
            assert_eq!(serde_json::from_str::<Value>(&kept).unwrap(), settings);

            let theirs = b.team(team).await;
            assert!(!theirs.can_add_channel && !theirs.channels[1].can_edit && !theirs.channels[1].can_delete);
            let draft = || ChannelDraft {
                channel: new_channel(team, group, "Side door", NewChannelKind::Standard),
                show_by_default: false,
            };
            let refused = b.engine().create_channel(draft()).await;
            let why = match refused {
                Err(EngineError::ChannelNotCreated { why }) => why,
                other => panic!("made, or refused otherwise: {other:?}"),
            };
            assert_eq!(why, ChannelProblem::NoPermission);
            assert!(a.engine().create_channel(draft()).await.is_ok(), "an owner always may");
        }
        let _ = channel;
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owners_give_and_take_roles_and_name_a_channels_moderators() {
    for backend in backends() {
        let rig = Rig::start(backend, "roles").await;
        let [a, b, c] = &rig.seats;
        let (team, group) = a.make_team("Rocket", false).await;
        a.add(&team, &group, &[b, c]).await;
        let channel = a.make_channel(&team, &group, "Launch", true).await;
        {
            let team: &str = &team;
            let group: &str = &group;
            let write = |who: &Seat, update| member(team, group, who, update);
            // a member manages nobody, itself neither
            for change in [write(c, MemberUpdate::Removed), write(b, MemberUpdate::Promoted)] {
                assert_eq!(
                    refusal(b.engine().team_write(change, None).await).as_deref(),
                    Some("Forbidden")
                );
            }
            a.engine()
                .team_write(write(b, MemberUpdate::Promoted), None)
                .await
                .unwrap();
            let roster = a.engine().team_roster(team).await.unwrap();
            let owners: Vec<&str> = roster.iter().filter(|m| m.owner).map(|m| m.mri.as_str()).collect();
            assert_eq!(owners, [a.mri.as_str(), b.mri.as_str()]);
            assert_eq!(roster.len(), 3);
            assert!(b.team(team).await.owner && b.team(team).await.can_add_channel);
            // an owner made so demotes the first one and promotes it back
            b.engine()
                .team_write(write(a, MemberUpdate::Demoted), None)
                .await
                .unwrap();
            assert!(!a.team(team).await.owner);
            let refused = a.engine().team_write(write(b, MemberUpdate::Demoted), None).await;
            assert_eq!(refusal(refused).as_deref(), Some("Forbidden"));
            b.engine()
                .team_write(write(a, MemberUpdate::Promoted), None)
                .await
                .unwrap();
            a.engine()
                .team_write(write(b, MemberUpdate::Demoted), None)
                .await
                .unwrap();
            assert!(a.team(team).await.owner && !b.team(team).await.owner);
        }
        moderators(a, b, &team, &group, &channel).await;
        rig.stop().await;
    }
}

async fn moderators(a: &Seat, b: &Seat, team: &str, group: &str, channel: &str) {
    let moderator = |who: &Seat, moderator| {
        let change = ChannelChange::Moderator {
            mri: who.mri.clone(),
            moderator,
        };
        channel_write(team, group, channel, change)
    };
    let moderation = json!({"channelPostPermissions": 1, "channelReplyPermissions": 0});
    let set = channel_write(team, group, channel, ChannelChange::Settings(moderation.clone()));
    assert!(
        b.engine().team_write(set.clone(), None).await.is_err(),
        "a member's write"
    );
    assert!(
        b.engine().team_write(moderator(b, true), None).await.is_err(),
        "a member's write"
    );
    a.engine().team_write(set, None).await.unwrap();
    a.engine().team_write(moderator(b, true), None).await.unwrap();
    let read = teams_proto::ChannelRead::Thread {
        channel: channel.into(),
    };
    let thread = b.engine().channel_data(read).await.unwrap();
    let kept = thread.0["properties"]["channelSettings"].as_str().unwrap();
    assert_eq!(serde_json::from_str::<Value>(kept).unwrap(), moderation);
    let moderators = async |seat: &Seat| {
        let roster = seat.engine().team_roster(channel).await.unwrap();
        let named = roster.into_iter().filter(|member| member.moderator);
        named.map(|member| member.mri).collect::<Vec<_>>()
    };
    assert_eq!(moderators(b).await, std::slice::from_ref(&b.mri));
    a.engine().team_write(moderator(b, false), None).await.unwrap();
    assert!(moderators(a).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn someone_removed_loses_the_team_and_a_team_and_a_private_channel_keep_an_owner() {
    for backend in backends() {
        let mut rig = Rig::start(backend, "removal").await;
        let [a, b, _] = &rig.seats;
        let (team, group) = a.make_team("Rocket", false).await;
        a.add(&team, &group, &[b]).await;
        let channel = a.make_channel(&team, &group, "Launch", true).await;
        let (secret, secret_group) = a.make_private(&team, &group, "Secret").await;
        let people = vec![b.mri.clone()];
        let failed = a
            .engine()
            .add_channel_members(group.as_str(), secret_group.as_str(), people)
            .await;
        assert!(failed.unwrap().is_empty());
        for conversation in [&team, &channel, &secret] {
            a.send(conversation, "Before").await;
            b.send(conversation, "I am here").await;
        }
        b.sync().await;
        b.stored_teams(|teams| teams.iter().any(|t| t.channels.len() == 3))
            .await;

        last_owner_stays(a, b, &team, &group, &secret, &secret_group).await;

        a.engine()
            .team_write(member(&team, &group, b, MemberUpdate::Removed), None)
            .await
            .unwrap();
        assert_eq!(a.team(&team).await.about.people, 1);
        assert!(b.listed().await.is_empty());
        let reads = ChatReads::new(&*b.client);
        for conversation in [&team, &channel, &secret] {
            assert_eq!(
                reads.thread(conversation).await.unwrap_err().status(),
                Some(403),
                "{conversation}"
            );
            assert_eq!(reads.messages(conversation, 10).await.unwrap_err().status(), Some(403));
            assert_ne!(b.post(conversation, "Still here?").await.state, OutboxState::Sent);
            assert!(b.engine().sync_conversation(conversation.as_str()).await.is_err());
        }
        assert!(reads.conversations().await.unwrap().conversations.is_empty());
        assert!(b.engine().team_channels(group.as_str()).await.unwrap().is_empty());
        // its client, started again, reads no team
        rig.seats[1].restart();
        let [a, b, _] = &rig.seats;
        b.sync().await;
        assert!(b.listed().await.is_empty());
        let page = ChatReads::new(&*a.client).messages(&channel, 200).await.unwrap();
        assert_eq!(page.messages.len(), 2, "what b wrote while on the team stays");
        rig.stop().await;
    }
}

async fn last_owner_stays(a: &Seat, b: &Seat, team: &str, group: &str, secret: &str, secret_group: &str) {
    for change in [
        MemberUpdate::Demoted,
        MemberUpdate::DemotedAndLeft,
        MemberUpdate::Removed,
    ] {
        let refused = a.engine().team_write(member(team, group, a, change), None).await;
        assert_eq!(
            refusal(refused).as_deref(),
            Some("LastOwnerCannotBeRemoved"),
            "{change:?}"
        );
    }
    let private = |action| {
        TeamWrite::Channel(ChannelWrite {
            target: ChannelTarget {
                team: team.into(),
                team_group: group.into(),
                channel: secret.into(),
                channel_group: Some(secret_group.into()),
                mailbox: None,
            },
            action,
        })
    };
    // the engine words a demotion the service refused for one member as its own code
    let own = a.mri.clone();
    for (change, code) in [
        (
            ChannelChange::RemoveMember { mri: own.clone() },
            "LastOwnerCannotBeRemoved",
        ),
        (
            ChannelChange::UpdateMember { mri: own, owner: false },
            "MemberChangeRejected",
        ),
    ] {
        let refused = a.engine().team_write(private(change), None).await;
        assert_eq!(refusal(refused).as_deref(), Some(code));
    }
    // a member of either removes nobody
    let out = ChannelChange::RemoveMember { mri: a.mri.clone() };
    assert!(b.engine().team_write(private(out), None).await.is_err());
    let listed = a.team(team).await;
    assert!(
        listed.owner
            && listed
                .channels
                .iter()
                .any(|channel| channel.id == secret && channel.owner)
    );
    assert_eq!(listed.about.people, 2);
}
