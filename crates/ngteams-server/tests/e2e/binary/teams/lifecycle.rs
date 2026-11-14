use teams_core::{ChannelDraft, ChannelProblem, EngineError};
use teams_proto::chat::ChatReads;
use teams_proto::{ChannelChange, NewChannelKind, TeamWrite};
use teams_store::OutboxState;

use super::support::{Names, Rig, Seat};
use crate::support::backends;
use crate::teams::{channel_write, new_channel, refusal};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_team_is_gone_for_everyone_on_it() {
    for backend in backends() {
        let mut rig = Rig::start(backend, "deletion").await;
        let [a, b, c] = &rig.seats;
        let (team, group) = a.make_team("Rocket", false).await;
        let (other, other_group) = a.make_team("Keeper", false).await;
        a.add(&team, &group, &[b, c]).await;
        a.add(&other, &other_group, &[b]).await;
        let channel = a.make_channel(&team, &group, "Launch", true).await;
        let (secret, _) = c.make_private(&team, &group, "Den").await;
        for (seat, conversation) in [(a, &team), (b, &channel), (c, &secret), (a, &other)] {
            seat.send(conversation, "Before the end").await;
        }
        b.sync().await;
        b.stored_teams(|teams| teams.len() == 2).await;

        let delete = TeamWrite::Delete { team: team.clone() };
        let refused = b.engine().team_write(delete.clone(), None).await;
        assert_eq!(refusal(refused).as_deref(), Some("Forbidden"));
        assert_eq!(a.team(&team).await.about.people, 3);
        a.engine().team_write(delete, None).await.unwrap();

        for seat in [a, b, c] {
            let listed = seat.listed().await;
            assert!(listed.iter().all(|listed| listed.id != team), "still listed");
            let reads = ChatReads::new(&*seat.client);
            for thread in [&team, &channel, &secret] {
                assert_eq!(reads.thread(thread).await.unwrap_err().status(), Some(404), "{thread}");
                assert_ne!(seat.post(thread, "Anyone?").await.state, OutboxState::Sent, "{thread}");
            }
            let list = reads.conversations().await.unwrap();
            assert!(list.conversations.iter().all(|entry| entry.id != team));
        }
        assert!(a.engine().team_channels(group.as_str()).await.unwrap().is_empty());
        // the other team is untouched, also after the nodes and b's client start again
        rig.restart_nodes().await;
        rig.seats[1].restart();
        let [a, b, _] = &rig.seats;
        b.sync().await;
        let listed = b.listed().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, other);
        b.send(&other, "Still here").await;
        assert_eq!(a.listed().await.len(), 1);
        let name = TeamWrite::Create(teams_proto::NewTeam {
            name: "Rocket".into(),
            description: String::new(),
            first_channel: "General".into(),
            public: false,
        });
        assert!(
            a.engine().team_write(name, None).await.is_ok(),
            "its name is free again"
        );
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_team_takes_as_many_channels_as_teams_allows() {
    for backend in backends() {
        let rig = Rig::start(backend, "limits").await;
        let [a, b, _] = &rig.seats;
        let (team, group) = a.make_team("Rocket", false).await;
        a.add(&team, &group, &[b]).await;
        let names = Names::default().with("team", &team).with("group", &group);

        // 200 standard channels, the general one among them
        let first = a.make_channel(&team, &group, "Room 1", false).await;
        for n in 2..200 {
            let body = format!(r#"{{"displayName":"Room {n}","groupId":"{{group}}","channelType":"Standard"}}"#);
            let (status, answer) = a.call("POST", "{MT}/teams/{team}/channels", &body, &names).await;
            assert_eq!(status, 201, "{n}: {answer}");
        }
        assert_eq!(a.team(&team).await.channels.len(), 200);
        for seat in [a, b] {
            let refused = problem(seat, &team, &group, "One too many", NewChannelKind::Standard).await;
            assert_eq!(refused, Some(ChannelProblem::TooManyChannels));
        }
        let body = r#"{"displayName":"One too many","groupId":"{group}","channelType":"Standard"}"#;
        let (status, answer) = a.call("POST", "{MT}/teams/{team}/channels", body, &names).await;
        assert_eq!(
            (status, answer["errorCode"].as_str()),
            (403, Some("MaxChannelLimitExceeded"))
        );

        // 30 private ones, counted apart
        for n in 0..30 {
            let body = format!(r#"{{"spaceThreadId":"{{team}}","displayName":"Den {n}","channelType":"Private"}}"#);
            let (status, answer) = a.call("POST", "{PROV}/teams/{group}/channels", &body, &names).await;
            assert_eq!(status, 200, "{n}: {answer}");
        }
        for seat in [a, b] {
            let refused = problem(seat, &team, &group, "Den 30", NewChannelKind::Private).await;
            assert_eq!(refused, Some(ChannelProblem::TooManyPrivateChannels));
        }
        assert_eq!(a.team(&team).await.channels.len(), 230);
        assert_eq!(
            b.team(&team).await.channels.len(),
            200,
            "b is in none of the private ones"
        );

        // a deleted channel makes room for one
        let delete = channel_write(&team, &group, &first, ChannelChange::Delete);
        a.engine().team_write(delete, None).await.unwrap();
        assert_eq!(
            problem(b, &team, &group, "Room 1", NewChannelKind::Standard).await,
            None
        );
        assert_eq!(
            problem(a, &team, &group, "One too many", NewChannelKind::Standard).await,
            Some(ChannelProblem::TooManyChannels)
        );
        rig.stop().await;
    }
}

async fn problem(seat: &Seat, team: &str, group: &str, name: &str, kind: NewChannelKind) -> Option<ChannelProblem> {
    let draft = ChannelDraft {
        channel: new_channel(team, group, name, kind),
        show_by_default: false,
    };
    match seat.engine().create_channel(draft).await {
        Ok(_) => None,
        Err(EngineError::ChannelNotCreated { why }) => Some(why),
        Err(other) => panic!("refused otherwise: {other:?}"),
    }
}
