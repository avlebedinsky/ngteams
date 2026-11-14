use teams_proto::chat::ChatReads;
use teams_proto::reply_chain_id;

use super::support::{Rig, Seat};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_team_is_served_by_every_node_and_survives_their_restarts() {
    for backend in backends() {
        let mut rig = Rig::start(backend, "nodes").await;
        let [a, b, _] = &rig.seats;
        // a works through the first node; with two nodes b works through the second
        let (team, group) = a.make_team("Rocket", false).await;
        a.add(&team, &group, &[b]).await;
        let channel = a.make_channel(&team, &group, "Launch", true).await;
        let post = a.send(&channel, "Fuel is loaded").await;
        b.sync().await;
        b.stored_teams(|teams| teams.iter().any(|t| t.channels.len() == 2))
            .await;
        assert_eq!(
            b.engine().sync_conversation(channel.as_str()).await.unwrap().inserted,
            1
        );
        let chain = reply_chain_id(&channel, &post).unwrap();
        let reply = b.send(&chain, "Go for launch").await;
        assert_state(&[a, b], &team, &channel, &["General", "Launch"], &[&reply, &post]).await;

        // the first node goes down: the second one still serves the team
        rig.stop_first().await;
        let mut channels = vec!["General", "Launch"];
        let mut messages = vec![reply.clone(), post.clone()];
        if rig.second.is_some() {
            let b = &rig.seats[1];
            messages.insert(0, b.send(&channel, "While the first node is down").await);
            b.make_channel(&team, &group, "Recovery", false).await;
            channels.push("Recovery");
        }
        rig.start_first().await;
        let [a, b, _] = &rig.seats;
        let ids: Vec<&str> = messages.iter().map(String::as_str).collect();
        assert_state(&[a, b], &team, &channel, &channels, &ids).await;

        // every node and every client starts again: nothing is lost, and work goes on
        rig.restart_nodes().await;
        for seat in &mut rig.seats {
            seat.restart();
        }
        let [a, b, _] = &rig.seats;
        assert_eq!(b.stored_ids(&channel).await, ids, "the client's own store");
        assert_state(&[a, b], &team, &channel, &channels, &ids).await;
        let after = a.send(&chain, "Lift-off").await;
        let mut ids = ids;
        ids.insert(0, &after);
        assert_state(&[a, b], &team, &channel, &channels, &ids).await;
        let roots = b.engine().reply_roots(channel.as_str()).await.unwrap();
        assert!(roots.contains(&(after.clone(), post.clone())) && roots.contains(&(reply.clone(), post.clone())));
        rig.stop().await;
    }
}

/// What both accounts read of the team: its name, people, channels and the channel's messages, the newest first.
async fn assert_state(seats: &[&Seat], team: &str, channel: &str, channels: &[&str], messages: &[&str]) {
    for seat in seats {
        let listed = seat.team(team).await;
        assert_eq!(listed.name, "Rocket");
        assert_eq!(listed.about.people, 2);
        let names: Vec<&str> = listed.channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, channels);
        seat.engine().sync_conversation(channel).await.unwrap();
        assert_eq!(seat.stored_ids(channel).await, messages);
        let page = ChatReads::new(&*seat.client).messages(channel, 200).await.unwrap();
        let ids: Vec<&str> = page.messages.iter().map(|message| message.id.as_str()).collect();
        assert_eq!(ids, messages);
        assert_eq!(seat.engine().team_roster(team).await.unwrap().len(), 2);
    }
}
