use serde_json::{Value, json};
use teams_core::{ChannelDraft, ChannelProblem, EngineError};
use teams_proto::{ChannelChange, ChannelRead, NewChannelKind, TeamWrite};

use super::{Seat, channel_write, new_channel, refusal, server};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn a_teams_settings_decide_what_its_members_may_do() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;

        let read = ada.engine.team_settings(team.as_str()).await.unwrap().unwrap();
        let mut settings: Value = serde_json::from_str(&read).unwrap();
        assert_eq!(settings["userSettings"]["createTopic"], true);
        assert_eq!(settings["userSettings"]["giphyRating"], 1);
        assert_eq!(bert.engine.team_settings(team.as_str()).await.unwrap(), Some(read));

        for key in ["createTopic", "updateTopic", "deleteTopic"] {
            settings["userSettings"][key] = json!(false);
        }
        settings["userSettings"]["giphyRating"] = json!(2);
        let write = TeamWrite::Settings {
            team: team.clone(),
            settings: settings.to_string(),
        };
        assert_eq!(
            refusal(bert.engine.team_write(write.clone(), None).await),
            None,
            "refused"
        );
        ada.engine.team_write(write, None).await.unwrap();
        let kept = ada.engine.team_settings(team.as_str()).await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&kept).unwrap(), settings);

        let theirs = bert.team(&team).await;
        assert!(!theirs.can_add_channel && !theirs.can_add_private_channel);
        assert!(!theirs.channels[1].can_edit && !theirs.channels[1].can_delete);
        assert!(ada.team(&team).await.can_add_channel, "an owner always may");
        let refused = bert
            .engine
            .create_channel(ChannelDraft {
                channel: new_channel(&team, &group, "Side door", NewChannelKind::Standard),
                show_by_default: false,
            })
            .await;
        assert!(
            matches!(
                refused,
                Err(EngineError::ChannelNotCreated {
                    why: ChannelProblem::NoPermission
                })
            ),
            "{refused:?}"
        );
        let refused = bert
            .engine
            .create_channel(ChannelDraft {
                channel: new_channel(&team, &group, "Back room", NewChannelKind::Private),
                show_by_default: false,
            })
            .await;
        assert!(
            matches!(
                refused,
                Err(EngineError::ChannelNotCreated {
                    why: ChannelProblem::PrivateNotAllowed
                })
            ),
            "{refused:?}"
        );
        let malformed = TeamWrite::Settings {
            team: team.clone(),
            settings: "not json".into(),
        };
        assert!(ada.engine.team_write(malformed, None).await.is_err());
        let _ = channel;
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owner_sets_a_channels_moderation_and_its_moderators() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;
        let write = |action| channel_write(&team, &group, &channel, action);

        let moderation = json!({"channelPostPermissions": 1, "channelReplyPermissions": 0, "kept": {"unknown": true}});
        ada.engine
            .team_write(write(ChannelChange::Settings(moderation.clone())), None)
            .await
            .unwrap();
        let thread = bert
            .engine
            .channel_data(ChannelRead::Thread {
                channel: channel.clone(),
            })
            .await
            .unwrap();
        let kept = thread.0["properties"]["channelSettings"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(kept).unwrap(), moderation);

        let moderator = |seat: &Seat, moderator| ChannelChange::Moderator {
            mri: seat.mri.clone(),
            moderator,
        };
        ada.engine
            .team_write(write(moderator(&bert, true)), None)
            .await
            .unwrap();
        let roster = ada.engine.team_roster(channel.as_str()).await.unwrap();
        let moderators: Vec<&str> = roster.iter().filter(|m| m.moderator).map(|m| m.mri.as_str()).collect();
        assert_eq!(moderators, [bert.mri.as_str()]);
        assert_eq!(
            refusal(ada.engine.team_write(write(moderator(&cora, true)), None).await).as_deref(),
            Some("MemberChangeRejected"),
            "a moderator is someone on the team"
        );
        ada.engine
            .team_write(write(moderator(&bert, false)), None)
            .await
            .unwrap();
        let roster = ada.engine.team_roster(channel.as_str()).await.unwrap();
        assert!(roster.iter().all(|member| !member.moderator));
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}
