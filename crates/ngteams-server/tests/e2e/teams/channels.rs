use teams_core::{ChannelDraft, ChannelProblem, EngineError};
use teams_proto::{ChannelChange, ChannelType, NewChannelKind};

use super::{Seat, channel_write, new_channel, refusal, server};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_adds_a_standard_channel_and_the_team_lists_it_for_its_people() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;

        let shown = ada.make_channel(&team, &group, "Launch", true).await;
        let quiet = ada.make_channel(&team, &group, "Budget", false).await;
        assert!(shown.ends_with("@thread.tacv2") && shown != team && shown != quiet);

        let own = ada.team(&team).await;
        let names: Vec<&str> = own.channels.iter().map(|channel| channel.name.as_str()).collect();
        assert_eq!(names, ["General", "Launch", "Budget"]);
        let launch = &own.channels[1];
        assert_eq!(launch.kind, ChannelType::Standard);
        assert_eq!(launch.description, "About Launch");
        assert!(launch.shown && launch.can_edit && launch.can_delete && !launch.general && !launch.archived);
        assert!(own.channels[2].shown, "its maker has a new channel in the list");

        let theirs = bert.team(&team).await;
        assert!(theirs.channels[1].shown, "shown to everyone");
        assert!(
            !theirs.channels[2].shown,
            "a channel not shown by default is listed hidden"
        );
        assert!(!theirs.owner && theirs.can_add_channel);

        for taken in ["launch", "General"] {
            let again = ada
                .engine
                .create_channel(ChannelDraft {
                    channel: new_channel(&team, &group, taken, NewChannelKind::Standard),
                    show_by_default: false,
                })
                .await;
            assert!(
                matches!(
                    again,
                    Err(EngineError::ChannelNotCreated {
                        why: ChannelProblem::NameTaken
                    })
                ),
                "{again:?}"
            );
        }
        let other_group = ada
            .engine
            .create_channel(ChannelDraft {
                channel: new_channel(
                    &team,
                    &uuid::Uuid::new_v4().to_string(),
                    "Elsewhere",
                    NewChannelKind::Standard,
                ),
                show_by_default: false,
            })
            .await;
        assert!(other_group.is_err());
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_renames_archives_restores_and_deletes_a_standard_channel() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;
        ada.make_channel(&team, &group, "Budget", true).await;
        let write = |action| channel_write(&team, &group, &channel, action);

        let edit = ChannelChange::Edit {
            name: "Lift-off".into(),
            description: "Countdown".into(),
        };
        ada.engine.team_write(write(edit), None).await.unwrap();
        let listed = bert.team(&team).await;
        assert_eq!(listed.channels[1].name, "Lift-off");
        assert_eq!(listed.channels[1].description, "Countdown");

        let taken = ChannelChange::Edit {
            name: "budget".into(),
            description: String::new(),
        };
        assert_eq!(
            refusal(ada.engine.team_write(write(taken), None).await).as_deref(),
            Some("ChannelNameAlreadyExist")
        );

        let archive = |archived| ChannelChange::Archive {
            archived,
            site_read_only: false,
        };
        // a member's own check of the fresh teams read refuses it before anything is sent
        assert_eq!(
            refusal(bert.engine.team_write(write(archive(true)), None).await).as_deref(),
            Some("ChannelPermissionChanged")
        );
        ada.engine.team_write(write(archive(true)), None).await.unwrap();
        assert!(bert.team(&team).await.channels[1].archived);
        ada.engine.team_write(write(archive(false)), None).await.unwrap();
        assert!(!bert.team(&team).await.channels[1].archived);

        // a member may delete a channel while the team's settings allow it
        bert.engine
            .team_write(write(ChannelChange::Delete), None)
            .await
            .unwrap();
        let left: Vec<String> = ada.team(&team).await.channels.into_iter().map(|c| c.name).collect();
        assert_eq!(left, ["General", "Budget"]);
        ada.engine.team_write(write(ChannelChange::Delete), None).await.unwrap();
        let general = channel_write(&team, &group, &team, ChannelChange::Delete);
        assert!(ada.engine.team_write(general, None).await.is_err());
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}
