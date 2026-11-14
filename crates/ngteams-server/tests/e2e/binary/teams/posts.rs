use teams_core::engine::Action;
use teams_proto::chat::{ChatReads, MessageText};
use teams_proto::{Author, ChannelChange, EditMessage, Mri, Original, SendMessage, TeamWrite, reply_chain_id};
use teams_store::OutboxState;

use super::support::{Rig, Seat};
use crate::support::backends;
use crate::teams::channel_write;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channels_posts_and_replies_read_in_order_after_sync_and_a_client_restart() {
    for backend in backends() {
        let mut rig = Rig::start(backend, "threads").await;
        let [a, b, _] = &rig.seats;
        let (team, group) = a.make_team("Rocket", false).await;
        a.add(&team, &group, &[b]).await;
        let shown = a.make_channel(&team, &group, "Launch", true).await;
        let quiet = a.make_channel(&team, &group, "Budget", false).await;

        // the channel shown to everyone is in b's list; the other is listed hidden; its maker has both
        b.sync().await;
        let stored = b
            .stored_teams(|teams| teams.iter().any(|t| t.channels.len() == 3))
            .await;
        let listed: Vec<(&str, bool)> = stored[0].channels.iter().map(|c| (c.name.as_str(), c.shown)).collect();
        assert_eq!(listed, [("General", true), ("Launch", true), ("Budget", false)]);
        let own = a
            .stored_teams(|teams| teams.iter().any(|t| t.channels.len() == 3))
            .await;
        assert!(own[0].channels.iter().all(|channel| channel.shown));
        assert_eq!(stored[0].channels[2].id, quiet);

        let post = a.send(&shown, "Fuel is loaded").await;
        let second = a.send(&shown, "Weather is clear").await;
        assert_eq!(b.engine().sync_conversation(shown.as_str()).await.unwrap().inserted, 2);
        let chain = reply_chain_id(&shown, &post).unwrap();
        let reply = b.send(&chain, "Go for launch").await;
        a.engine().sync_conversation(shown.as_str()).await.unwrap();
        let answer = a.send(&chain, "Counting down").await;

        let order = [answer.clone(), reply.clone(), second.clone(), post.clone()];
        let mut expected = vec![
            (answer.clone(), post.clone()),
            (reply.clone(), post.clone()),
            (second.clone(), second.clone()),
            (post.clone(), post.clone()),
        ];
        expected.sort();
        for seat in [a, b] {
            seat.engine().sync_conversation(shown.as_str()).await.unwrap();
            assert_eq!(seat.stored_ids(&shown).await, order);
            assert_eq!(roots(seat, &shown).await, expected);
            let stored = seat
                .engine()
                .message(shown.as_str(), reply.as_str())
                .await
                .unwrap()
                .unwrap();
            assert!(stored.content.contains("Go for launch"));
            assert_eq!(stored.sender.as_deref(), Some(b.mri.as_str()));
            // the post's own chain, as the service pages it: the newest first, the post last
            let page = ChatReads::new(&*seat.client).messages(&chain, 200).await.unwrap();
            let ids: Vec<&str> = page.messages.iter().map(|message| message.id.as_str()).collect();
            assert_eq!(ids, [answer.as_str(), reply.as_str(), post.as_str()]);
        }

        for seat in &mut rig.seats[..2] {
            seat.restart();
            assert_eq!(seat.stored_ids(&shown).await, order, "kept in the store");
            assert_eq!(roots(seat, &shown).await, expected);
            assert_eq!(
                seat.engine().sync_conversation(shown.as_str()).await.unwrap().inserted,
                0
            );
            assert_eq!(seat.stored_ids(&shown).await, order);
        }
        rig.stop().await;
    }
}

/// The post each stored message of the channel belongs to, by message, sorted.
async fn roots(seat: &Seat, channel: &str) -> Vec<(String, String)> {
    let mut roots = seat.engine().reply_roots(channel).await.unwrap();
    roots.sort();
    roots
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn posts_and_replies_are_edited_deleted_and_reacted_to() {
    for backend in backends() {
        let rig = Rig::start(backend, "changes").await;
        let [a, b, _] = &rig.seats;
        let (team, group) = a.make_team("Rocket", false).await;
        a.add(&team, &group, &[b]).await;
        let channel = a.make_channel(&team, &group, "Launch", true).await;
        let post = a.send(&channel, "Fuel is loaded").await;
        b.engine().sync_conversation(channel.as_str()).await.unwrap();
        let chain = reply_chain_id(&channel, &post).unwrap();
        let reply = b.send(&chain, "Go for launch").await;
        a.engine().sync_conversation(channel.as_str()).await.unwrap();

        {
            let channel: &str = &channel;
            let post: &str = &post;
            let reply: &str = &reply;
            let edit = |id: &str, text: &str| Action::Edit {
                conversation: channel.into(),
                message_id: id.into(),
                text: MessageText::plain(text).unwrap(),
            };
            let react = |id: &str, key: &str| Action::React {
                conversation: channel.into(),
                message_id: id.into(),
                key: key.into(),
            };
            // a message is its author's to edit
            let before = b.engine().message(channel, post).await.unwrap().unwrap();
            let forged = EditMessage::new(
                channel,
                post,
                Author::new(Mri::new(&b.mri), "QA Operator"),
                MessageText::plain("forbidden").unwrap(),
                Original {
                    client_message_id: before.client_message_id.clone().unwrap(),
                    compose_time_ms: before.compose_time.unwrap(),
                    arrival_time_ms: before.arrival_time,
                },
            );
            assert_eq!(b.client.write(forged).await.unwrap().status(), 403);

            a.write(edit(post, "Fuel is topped up")).await;
            b.write(edit(reply, "Go!")).await;
            a.write(react(reply, "like")).await;
            b.write(react(reply, "like")).await;
            b.write(react(post, "heart")).await;
            for seat in [a, b] {
                seat.engine().sync_conversation(channel).await.unwrap();
                let stored = seat.engine().message(channel, post).await.unwrap().unwrap();
                assert!(stored.content.contains("Fuel is topped up"), "the post's edit");
                assert!(stored.edit_time.is_some());
                assert_eq!(stored.arrival_time, before.arrival_time);
                let stored = seat.engine().message(channel, reply).await.unwrap().unwrap();
                assert!(stored.content.contains("Go!"), "the reply's edit");
                assert_eq!(seat.engine().reactions(channel, reply).await.unwrap().len(), 2);
                let hearts = seat.engine().reactions(channel, post).await.unwrap();
                assert_eq!(hearts.len(), 1);
                assert_eq!(
                    (hearts[0].key.as_str(), hearts[0].user.as_str()),
                    ("heart", b.mri.as_str())
                );
                // an edited reply is still under its post
                let roots = seat.engine().reply_roots(channel).await.unwrap();
                assert!(roots.contains(&(reply.to_owned(), post.to_owned())));
            }
            let unreact = Action::Unreact {
                conversation: channel.into(),
                message_id: reply.into(),
                key: "like".into(),
            };
            a.write(unreact).await;
            b.engine().sync_conversation(channel).await.unwrap();
            let left = b.engine().reactions(channel, reply).await.unwrap();
            assert_eq!(left.len(), 1);
            assert_eq!(left[0].user, b.mri);
        }
        deletes(a, b, &channel, &post, &reply).await;
        rig.stop().await;
    }
}

async fn deletes(a: &Seat, b: &Seat, channel: &str, post: &str, reply: &str) {
    let delete = |id: &str| Action::Delete {
        conversation: channel.into(),
        message_id: id.into(),
    };
    assert!(
        a.engine().write(delete(reply)).await.is_err(),
        "a reply is its author's to delete"
    );
    b.write(delete(reply)).await;
    a.engine().sync_conversation(channel).await.unwrap();
    let gone = a.engine().message(channel, reply).await.unwrap().unwrap();
    assert!(gone.content.is_empty() && gone.delete_time.is_some());
    let kept = a.engine().message(channel, post).await.unwrap().unwrap();
    assert!(kept.delete_time.is_none() && kept.content.contains("Fuel is topped up"));

    a.write(delete(post)).await;
    b.engine().sync_conversation(channel).await.unwrap();
    let gone = b.engine().message(channel, post).await.unwrap().unwrap();
    assert!(gone.content.is_empty() && gone.delete_time.is_some());
    // a deleted post takes no more replies
    let chain = reply_chain_id(channel, post).unwrap();
    let author = Author::new(Mri::new(&b.mri), "QA Operator");
    let late = SendMessage::new(chain, author, MessageText::plain("Late").unwrap());
    assert_eq!(b.client.write(late).await.unwrap().status(), 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archived_channel_or_team_takes_no_message() {
    for backend in backends() {
        let rig = Rig::start(backend, "archives").await;
        let [a, b, _] = &rig.seats;
        let (team, group) = a.make_team("Rocket", false).await;
        a.add(&team, &group, &[b]).await;
        let channel = a.make_channel(&team, &group, "Launch", true).await;
        {
            let team: &str = &team;
            let group: &str = &group;
            let channel: &str = &channel;
            let archive = |archived| {
                let change = ChannelChange::Archive {
                    archived,
                    site_read_only: false,
                };
                channel_write(team, group, channel, change)
            };
            let post = b.send(channel, "Still open").await;
            let chain = reply_chain_id(channel, &post).unwrap();
            a.engine().sync_conversation(channel).await.unwrap();
            a.engine().team_write(archive(true), None).await.unwrap();
            for seat in [a, b] {
                assert_ne!(seat.post(channel, "Too late").await.state, OutboxState::Sent);
                assert_ne!(seat.post(&chain, "Too late").await.state, OutboxState::Sent);
            }
            b.send(team, "The general channel is open").await;
            a.engine().team_write(archive(false), None).await.unwrap();
            b.send(&chain, "Open again").await;

            let whole = TeamWrite::Archive {
                group: group.into(),
                site_read_only: false,
            };
            a.engine().team_write(whole, None).await.unwrap();
            for conversation in [team, channel, chain.as_str()] {
                assert_ne!(
                    b.post(conversation, "Anyone?").await.state,
                    OutboxState::Sent,
                    "{conversation}"
                );
                assert_ne!(
                    a.post(conversation, "Anyone?").await.state,
                    OutboxState::Sent,
                    "{conversation}"
                );
            }
            let page = ChatReads::new(&*a.client).messages(channel, 200).await.unwrap();
            assert!(
                page.messages
                    .iter()
                    .all(|m| !m.content.contains("Too late") && !m.content.contains("Anyone?"))
            );
            a.engine()
                .team_write(TeamWrite::Restore { group: group.into() }, None)
                .await
                .unwrap();
            b.send(channel, "We are back").await;
            b.send(team, "We are back").await;
        }
        rig.stop().await;
    }
}
