//! A team's and a channel's changes on the push socket: each is a `ThreadUpdate` to the sockets of the people in
//! that thread, and of nobody else.

use std::time::Duration;

use teams_core::ChannelDraft;
use teams_proto::{ChannelChange, ChannelTarget, ChannelWrite, MemberUpdate, Mri, NewChannelKind, TeamWrite};

use super::refusals::{EPID, IC3, Raw, query, register, registration, token};
use crate::support::{TestServer, backends};
use crate::teams::{Seat, channel_write, new_channel, server};

/// The account's messaging socket, connected and its endpoint registered.
async fn listening(server: &TestServer, upn: &str) -> Raw {
    let ic3 = token(server, upn, IC3).await;
    let mut socket = Raw::open(server, &query(EPID, "TeamsCDL")).await;
    let info = socket.connect(&ic3).await;
    let body = registration("TeamsCDLWebWorker", EPID, info["surl"].as_str().unwrap(), 3600);
    assert_eq!(register(server, Some(&ic3), &body).await, 202);
    socket
}

/// The threads of `threads` a `ThreadUpdate` came for until the socket was quiet.
async fn updated<'a>(socket: &mut Raw, threads: &[&'a str]) -> Vec<&'a str> {
    let mut told = Vec::new();
    while let Ok(Some(frame)) = socket.next_within(Duration::from_millis(700)).await {
        if !frame.contains("ThreadUpdate") {
            continue;
        }
        for thread in threads {
            if frame.contains(&format!("/v1/threads/{thread}")) && !told.contains(thread) {
                told.push(*thread);
            }
        }
    }
    told.sort_unstable();
    told
}

/// The `ThreadUpdate` frames of `thread` that came until the socket was quiet.
async fn updates_of(socket: &mut Raw, thread: &str) -> Vec<String> {
    let mut told = Vec::new();
    while let Ok(Some(frame)) = socket.next_within(Duration::from_millis(700)).await {
        if frame.contains("ThreadUpdate") && frame.contains(&format!("/v1/threads/{thread}")) {
            told.push(frame);
        }
    }
    told
}

/// After someone's removal from `team`: the one removed is told it as someone no longer in it, a member as one of its
/// people.
async fn told_of_removal(on_gone: &mut Raw, gone: &Seat, on_member: &mut Raw, member: &Seat, team: &str) {
    let told = updates_of(on_gone, team).await;
    assert!(!told.is_empty());
    assert!(told.iter().all(|frame| !frame.contains(&gone.mri)), "{told:?}");
    let told = updates_of(on_member, team).await;
    assert!(told.iter().any(|frame| frame.contains(&member.mri)), "{told:?}");
}

fn sorted<'a>(threads: &[&'a str]) -> Vec<&'a str> {
    let mut threads = threads.to_vec();
    threads.sort_unstable();
    threads
}

fn rename(name: &str) -> ChannelChange {
    ChannelChange::Edit {
        name: name.into(),
        description: format!("About {name}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_teams_and_a_channels_changes_reach_the_sockets_of_the_people_in_them_alone() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        Seat::new(&server, "dan@own.test", "Dan").await.stop();
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert, &cora]).await;
        let mut on_bert = listening(&server, "bert@own.test").await;
        let mut on_cora = listening(&server, "cora@own.test").await;
        let mut on_dan = listening(&server, "dan@own.test").await;

        // a standard channel is every team member's: its making and its new name
        let launch = ada.make_channel(&team, &group, "Launch", true).await;
        let all = [team.as_str(), launch.as_str()];
        assert!(
            updated(&mut on_bert, &all).await.contains(&launch.as_str()),
            "{backend:?}"
        );
        assert!(
            updated(&mut on_cora, &all).await.contains(&launch.as_str()),
            "{backend:?}"
        );
        let write = channel_write(&team, &group, &launch, rename("Lift-off"));
        ada.engine.team_write(write, None).await.unwrap();
        assert_eq!(updated(&mut on_bert, &all).await, sorted(&[&launch]), "{backend:?}");
        assert_eq!(updated(&mut on_cora, &all).await, sorted(&[&launch]), "{backend:?}");

        // a private channel is its own people's: bert, on the team, hears nothing of it
        let draft = ChannelDraft {
            channel: new_channel(&team, &group, "Secret", NewChannelKind::Private),
            show_by_default: false,
        };
        let made = ada.engine.create_channel(draft).await.unwrap();
        let (secret, secret_group) = (made.thread.unwrap(), made.group.unwrap());
        let people = vec![cora.mri.clone()];
        let failed = ada
            .engine
            .add_channel_members(group.as_str(), secret_group.as_str(), people);
        assert!(failed.await.unwrap().is_empty());
        let hush = TeamWrite::Channel(ChannelWrite {
            target: ChannelTarget {
                team: team.clone(),
                team_group: group.clone(),
                channel: secret.clone(),
                channel_group: Some(secret_group),
                mailbox: None,
            },
            action: rename("Hush"),
        });
        ada.engine.team_write(hush.clone(), None).await.unwrap();
        let all = [team.as_str(), launch.as_str(), secret.as_str()];
        assert!(
            updated(&mut on_cora, &all).await.contains(&secret.as_str()),
            "{backend:?}"
        );
        assert!(
            !updated(&mut on_bert, &all).await.contains(&secret.as_str()),
            "{backend:?}"
        );

        // off the team, cora is out of its channels: no change of them reaches its socket
        let removed = TeamWrite::UpdateMember {
            team: team.clone(),
            group: group.clone(),
            member: Mri::new(&cora.mri),
            update: MemberUpdate::Removed,
        };
        ada.engine.team_write(removed, None).await.unwrap();
        told_of_removal(&mut on_cora, &cora, &mut on_bert, &bert, &team).await;
        let write = channel_write(&team, &group, &launch, rename("Orbit"));
        ada.engine.team_write(write, None).await.unwrap();
        ada.engine.team_write(hush, None).await.unwrap();
        assert_eq!(updated(&mut on_bert, &all).await, sorted(&[&launch]), "{backend:?}");
        assert_eq!(updated(&mut on_cora, &all).await, Vec::<&str>::new(), "{backend:?}");

        // and someone who never was on the team heard nothing at all
        assert!(
            on_dan.next_within(Duration::from_millis(300)).await.is_err(),
            "{backend:?}"
        );
        ada.stop();
        bert.stop();
        cora.stop();
        server.stop().await;
    }
}
