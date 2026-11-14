use serde_json::json;
use teams_core::TeamChange;
use teams_proto::{ChannelOwnProperty, ChannelRead, OwnTeamView};

use super::{Seat, server};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn an_account_hides_and_shows_a_team_and_its_channels_for_itself_alone() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;
        let shown = |listed: &teams_proto::Team| -> Vec<bool> { listed.channels.iter().map(|c| c.shown).collect() };

        let hide = vec![OwnTeamView::HideAll { team: team.clone() }];
        let change = TeamChange::Hidden {
            team: team.clone(),
            hidden: true,
        };
        bert.engine.own_team_view(hide, Some(change)).await.unwrap();
        let hidden = bert.team(&team).await;
        assert!(hidden.about.hidden);
        assert_eq!(shown(&hidden), [false, false]);
        let untouched = ada.team(&team).await;
        assert!(!untouched.about.hidden);
        assert_eq!(shown(&untouched), [true, true]);

        let show = vec![OwnTeamView::ShowAll { team: team.clone() }];
        bert.engine.own_team_view(show, None).await.unwrap();
        let back = bert.team(&team).await;
        assert!(!back.about.hidden);
        assert_eq!(
            shown(&back),
            [true, false],
            "a team shown again lists its general channel"
        );

        let set = |channel: &str, general: bool, shown: bool| {
            vec![
                OwnTeamView::ChannelInterest {
                    channel: channel.into(),
                    own: bert.mri.clone(),
                    shown,
                },
                OwnTeamView::ChannelFavorite {
                    channel: channel.into(),
                    general,
                    shown,
                },
            ]
        };
        bert.engine
            .own_team_view(set(&channel, false, true), None)
            .await
            .unwrap();
        assert_eq!(shown(&bert.team(&team).await), [true, true]);
        bert.engine
            .own_team_view(set(&channel, false, false), None)
            .await
            .unwrap();
        bert.engine.own_team_view(set(&team, true, false), None).await.unwrap();
        let none = bert.team(&team).await;
        assert_eq!(shown(&none), [false, false]);
        assert!(none.about.hidden, "a team with no channel listed is hidden");
        bert.engine
            .own_team_view(set(&channel, false, true), None)
            .await
            .unwrap();
        assert!(!bert.team(&team).await.about.hidden);

        // an account's interest is its own
        let other = vec![OwnTeamView::ChannelInterest {
            channel: channel.clone(),
            own: ada.mri.clone(),
            shown: false,
        }];
        assert!(bert.engine.own_team_view(other, None).await.is_err());
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channels_notification_settings_read_back_for_the_account_that_set_them() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;
        let preferences = |seat: &Seat, channel: &str| {
            seat.engine.channel_data(ChannelRead::Preferences {
                channel: channel.into(),
            })
        };
        let before = preferences(&bert, &channel).await.unwrap();
        assert_eq!(before.0["id"], channel.as_str());
        assert!(before.0["properties"].get("channelNotificationSettings").is_none());

        let settings = json!({"allNewPosts": "On", "dskNotif": "On", "includeReplies": true}).to_string();
        let property = |property, value: &str| OwnTeamView::ChannelProperty {
            channel: channel.clone(),
            property,
            value: value.into(),
        };
        let writes = vec![
            property(ChannelOwnProperty::FollowAll, "1760000060000"),
            property(ChannelOwnProperty::Notifications, &settings),
            property(ChannelOwnProperty::Alerts, "false"),
            property(ChannelOwnProperty::NotifyAll, ""),
        ];
        bert.engine.own_team_view(writes, None).await.unwrap();
        let after = preferences(&bert, &channel).await.unwrap();
        assert_eq!(after.0["properties"]["channelNotificationSettings"], settings.as_str());
        assert_eq!(after.0["properties"]["followAllRc"], "1760000060000");
        assert_eq!(after.0["properties"]["alerts"], "false");
        assert_eq!(after.0["properties"]["notifyAllRc"], "");
        assert_eq!(after.0["threadProperties"]["threadType"], "topic");
        let theirs = preferences(&ada, &channel).await.unwrap();
        assert!(theirs.0["properties"].get("channelNotificationSettings").is_none());
        assert!(theirs.0["properties"].get("followAllRc").is_none());

        // the general channel's too, and a value its dialog couldn't read back is refused
        let general = vec![OwnTeamView::ChannelProperty {
            channel: team.clone(),
            property: ChannelOwnProperty::Notifications,
            value: settings.clone(),
        }];
        bert.engine.own_team_view(general, None).await.unwrap();
        let read = preferences(&bert, &team).await.unwrap();
        assert_eq!(read.0["properties"]["channelNotificationSettings"], settings.as_str());
        let unreadable = vec![property(ChannelOwnProperty::Notifications, "[1]")];
        assert!(bert.engine.own_team_view(unreadable, None).await.is_err());
        let unreadable = vec![property(ChannelOwnProperty::Alerts, "sometimes")];
        assert!(bert.engine.own_team_view(unreadable, None).await.is_err());
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}
