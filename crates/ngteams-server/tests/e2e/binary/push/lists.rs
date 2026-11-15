//! A member's teams and channels as a running ngteams client lists them: a team or channel made, renamed or removed,
//! and someone added to or taken off a team or a private channel, show in the lists of the clients of the people it
//! concerns while they run, through the push channel alone (no restart, no reconnect, no sync asked).

use std::time::Duration;

use teams_proto::{ChannelChange, ChannelTarget, ChannelWrite, MemberUpdate, NewChannelKind, TeamWrite};
use teams_store::Team;

use super::channels::{channel, team};
use super::support::{Rig, User};
use crate::support::backends;

/// How long a change may take to reach a running client's list.
const LIVE: Duration = Duration::from_secs(15);

/// The client's stored teams once its first read settled them.
async fn listed(user: &mut User) -> Vec<Team> {
    let until = tokio::time::Instant::now() + LIVE;
    loop {
        if let Some(teams) = user.engine().teams().await.unwrap() {
            return teams;
        }
        assert!(
            tokio::time::Instant::now() < until,
            "{}: the teams were never read",
            user.upn
        );
        user.settle().await;
    }
}

/// Waits until the running client's own list of teams shows `what`, which the push channel alone brings.
async fn lists(user: &mut User, what: &str, pick: impl Fn(&[Team]) -> bool) {
    let until = tokio::time::Instant::now() + LIVE;
    loop {
        let teams = listed(user).await;
        if pick(&teams) {
            return;
        }
        let names: Vec<(&str, Vec<&str>)> = teams
            .iter()
            .map(|team| {
                (
                    team.name.as_str(),
                    team.channels.iter().map(|c| c.name.as_str()).collect(),
                )
            })
            .collect();
        assert!(
            tokio::time::Instant::now() < until,
            "{}: the list never showed {what}: {names:?}",
            user.upn
        );
        user.settle().await;
    }
}

fn team_of<'a>(teams: &'a [Team], id: &str) -> Option<&'a Team> {
    teams.iter().find(|team| team.id == id)
}

/// The team `id`'s channel names, as the list shows them.
fn channels_of(teams: &[Team], id: &str) -> Vec<String> {
    team_of(teams, id)
        .map(|team| team.channels.iter().map(|channel| channel.name.clone()).collect())
        .unwrap_or_default()
}

fn target(team: &str, group: &str, channel: &str, channel_group: Option<&str>) -> ChannelTarget {
    ChannelTarget {
        team: team.into(),
        team_group: group.into(),
        channel: channel.into(),
        channel_group: channel_group.map(str::to_owned),
        mailbox: None,
    }
}

async fn change_channel(owner: &User, target: ChannelTarget, action: ChannelChange) {
    let write = TeamWrite::Channel(ChannelWrite { target, action });
    owner.engine().team_write(write, None).await.unwrap();
}

/// a makes a channel in its team, renames it and renames the team: b's running client lists each; the channel.
async fn makes_and_renames(a: &User, b: &mut User, rocket: &str, group: &str) -> String {
    let (launch, _) = channel(a, rocket, group, "Launch", NewChannelKind::Standard).await;
    lists(b, "the new channel", |teams| {
        channels_of(teams, rocket).contains(&"Launch".to_owned())
    })
    .await;
    let rename = ChannelChange::Edit {
        name: "Liftoff".into(),
        description: "Up we go".into(),
    };
    change_channel(a, target(rocket, group, &launch, None), rename).await;
    lists(b, "the channel's new name", |teams| {
        let names = channels_of(teams, rocket);
        names.contains(&"Liftoff".to_owned()) && !names.contains(&"Launch".to_owned())
    })
    .await;
    let edit = TeamWrite::Edit {
        team: rocket.to_owned(),
        name: Some("Rocket Lab".into()),
        description: "Rocket's people".into(),
        public: None,
    };
    a.engine().team_write(edit, None).await.unwrap();
    lists(b, "the team's new name", |teams| {
        team_of(teams, rocket).is_some_and(|team| team.name == "Rocket Lab")
    })
    .await;
    launch
}

/// a puts b into a private channel of its team and takes it out again: b's running client lists it, then not.
async fn in_and_out_of_a_private_channel(a: &User, b: &mut User, rocket: &str, group: &str) {
    let (secret, secret_group) = channel(a, rocket, group, "Secret", NewChannelKind::Private).await;
    let secret_group = secret_group.unwrap();
    let people = vec![b.mri.as_str().to_owned()];
    let failed = a.engine().add_channel_members(group, secret_group.as_str(), people);
    assert!(failed.await.unwrap().is_empty());
    lists(b, "the private channel it was added to", |teams| {
        channels_of(teams, rocket).contains(&"Secret".to_owned())
    })
    .await;
    let out = ChannelChange::RemoveMember {
        mri: b.mri.as_str().to_owned(),
    };
    change_channel(a, target(rocket, group, &secret, Some(&secret_group)), out).await;
    lists(b, "the private channel gone", |teams| {
        let names = channels_of(teams, rocket);
        names.contains(&"Liftoff".to_owned()) && !names.contains(&"Secret".to_owned())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_members_running_client_lists_each_team_and_channel_change_live() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let a = rig.team_user(&rig.first, "a@own.test").await;
        let mut b = rig.team_user(rig.other(), "b@own.test").await;
        let mut c = rig.team_user(rig.other(), "c@own.test").await;
        for user in [&mut b, &mut c] {
            assert!(listed(user).await.is_empty(), "{}", user.upn);
        }

        // added to a team: it shows
        let (rocket, group) = team(&a, &[&b, &c]).await;
        for user in [&mut b, &mut c] {
            lists(user, "the team it was added to", |teams| {
                team_of(teams, &rocket).is_some_and(|team| team.name == "Rocket")
            })
            .await;
        }

        let launch = makes_and_renames(&a, &mut b, &rocket, &group).await;

        in_and_out_of_a_private_channel(&a, &mut b, &rocket, &group).await;

        // a channel deleted
        change_channel(&a, target(&rocket, &group, &launch, None), ChannelChange::Delete).await;
        lists(&mut b, "the deleted channel gone", |teams| {
            let names = channels_of(teams, &rocket);
            !names.is_empty() && !names.contains(&"Liftoff".to_owned())
        })
        .await;

        // taken off the team: it goes from that one's list alone
        let removed = TeamWrite::UpdateMember {
            team: rocket.clone(),
            group: group.clone(),
            member: c.mri.clone(),
            update: MemberUpdate::Removed,
        };
        a.engine().team_write(removed, None).await.unwrap();
        lists(&mut c, "the team it was taken off gone", |teams| {
            team_of(teams, &rocket).is_none()
        })
        .await;
        assert!(team_of(&listed(&mut b).await, &rocket).is_some());

        // the team deleted
        a.engine()
            .team_write(TeamWrite::Delete { team: rocket.clone() }, None)
            .await
            .unwrap();
        lists(&mut b, "the deleted team gone", |teams| {
            team_of(teams, &rocket).is_none()
        })
        .await;
        drop((a, b, c));
        rig.stop().await;
    }
}
