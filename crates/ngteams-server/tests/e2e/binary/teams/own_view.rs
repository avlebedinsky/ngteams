use serde_json::json;
use teams_core::TeamChange;
use teams_proto::{ChannelOwnProperty, ChannelRead, ChannelSectionAction, ChannelSections, OwnTeamView};

use super::support::{Rig, Seat};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_accounts_own_view_of_a_team_is_private_to_it_and_survives_restarts() {
    for backend in backends() {
        let mut rig = Rig::start(backend, "own-view").await;
        let [a, b, _] = &rig.seats;
        let (team, group) = a.make_team("Rocket", false).await;
        a.add(&team, &group, &[b]).await;
        let channel = a.make_channel(&team, &group, "Launch", true).await;
        let quiet = a.make_channel(&team, &group, "Budget", false).await;
        assert_eq!(view(b, &team).await, (false, vec![true, true, false]));

        // b shows the channel nobody was shown, hides the one everyone was, and sets its notifications
        let show = |channel: &str, shown: bool| {
            vec![
                OwnTeamView::ChannelInterest {
                    channel: channel.into(),
                    own: b.mri.clone(),
                    shown,
                },
                OwnTeamView::ChannelFavorite {
                    channel: channel.into(),
                    general: false,
                    shown,
                },
            ]
        };
        b.engine().own_team_view(show(&quiet, true), None).await.unwrap();
        b.engine().own_team_view(show(&channel, false), None).await.unwrap();
        let settings = json!({"allNewPosts": "On", "dskNotif": "On", "includeReplies": true}).to_string();
        let notify = vec![OwnTeamView::ChannelProperty {
            channel: quiet.clone(),
            property: ChannelOwnProperty::Notifications,
            value: settings.clone(),
        }];
        b.engine().own_team_view(notify, None).await.unwrap();
        let create = ChannelSectionAction::Create {
            channel: quiet.clone(),
            name: "Money".into(),
        };
        b.engine().channel_section(create).await.unwrap();
        // one account's interest is not another's to set
        let other = vec![OwnTeamView::ChannelInterest {
            channel: channel.clone(),
            own: a.mri.clone(),
            shown: false,
        }];
        assert!(b.engine().own_team_view(other, None).await.is_err());

        let check = async |a: &Seat, b: &Seat| {
            assert_eq!(view(b, &team).await, (false, vec![true, false, true]));
            assert_eq!(
                preferences(b, &quiet).await["properties"]["channelNotificationSettings"],
                settings.as_str()
            );
            let own = sections(b).await;
            assert_eq!(own.sections.len(), 2);
            assert_eq!(
                (own.sections[1].name.as_str(), &own.sections[1].items),
                ("Money", &vec![quiet.clone()])
            );
            // a's own view is as a left it
            assert_eq!(view(a, &team).await, (false, vec![true, true, true]));
            assert!(
                preferences(a, &quiet).await["properties"]
                    .get("channelNotificationSettings")
                    .is_none()
            );
            let theirs = sections(a).await;
            assert_eq!(theirs.sections.len(), 1);
            assert!(theirs.sections[0].items.is_empty());
        };
        check(a, b).await;

        // the same after the clients start again, and after the server's nodes do
        for seat in &mut rig.seats {
            seat.restart();
        }
        check(&rig.seats[0], &rig.seats[1]).await;
        rig.restart_nodes().await;
        let [a, b, _] = &rig.seats;
        check(a, b).await;
        b.sync().await;
        let stored = b
            .stored_teams(|teams| teams.iter().any(|t| t.channels.len() == 3))
            .await;
        let shown: Vec<bool> = stored[0].channels.iter().map(|c| c.shown).collect();
        assert_eq!(shown, [true, false, true], "the engine's list");

        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_hides_a_team_for_itself_alone_and_it_stays_hidden_after_restarts() {
    for backend in backends() {
        let mut rig = Rig::start(backend, "hidden").await;
        let [a, b, _] = &rig.seats;
        let (team, group) = a.make_team("Rocket", false).await;
        a.add(&team, &group, &[b]).await;
        a.make_channel(&team, &group, "Launch", true).await;
        b.sync().await;
        b.stored_teams(|teams| teams.iter().any(|t| t.id == team)).await;

        let hide = vec![OwnTeamView::HideAll { team: team.clone() }];
        let change = TeamChange::Hidden {
            team: team.clone(),
            hidden: true,
        };
        b.engine().own_team_view(hide, Some(change)).await.unwrap();
        assert_eq!(view(b, &team).await, (true, vec![false, false]));
        assert_eq!(view(a, &team).await, (false, vec![true, true]));
        let stored = b.stored_teams(|teams| teams.iter().any(|t| t.about.hidden)).await;
        assert!(
            stored[0].channels.iter().all(|channel| !channel.shown),
            "the engine's list"
        );

        rig.restart_nodes().await;
        for seat in &mut rig.seats {
            seat.restart();
        }
        let [a, b, _] = &rig.seats;
        assert_eq!(view(b, &team).await, (true, vec![false, false]));
        assert_eq!(view(a, &team).await, (false, vec![true, true]));
        assert!(b.stored_teams(|_| true).await[0].about.hidden, "kept in b's store");
        // a hidden team still takes its people's posts
        b.send(&team, "Hidden, not gone").await;

        let show = vec![OwnTeamView::ShowAll { team: team.clone() }];
        b.engine().own_team_view(show, None).await.unwrap();
        assert_eq!(view(b, &team).await, (false, vec![true, false]));
        assert_eq!(view(a, &team).await, (false, vec![true, true]));
        rig.stop().await;
    }
}

async fn sections(seat: &Seat) -> ChannelSections {
    let data = seat.engine().channel_data(ChannelRead::Sections).await.unwrap();
    ChannelSections::read(&data.0).unwrap()
}

async fn preferences(seat: &Seat, channel: &str) -> serde_json::Value {
    let read = ChannelRead::Preferences {
        channel: channel.into(),
    };
    seat.engine().channel_data(read).await.unwrap().0
}

/// What an account's teams read shows of the team: hidden, and each channel shown.
async fn view(seat: &Seat, team: &str) -> (bool, Vec<bool>) {
    let listed = seat.team(team).await;
    (listed.about.hidden, listed.channels.iter().map(|c| c.shown).collect())
}
