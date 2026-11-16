use teams_core::engine::Action;
use teams_proto::chat::{ChatReads, ConversationKind, MessageText};
use teams_proto::{Author, ChannelChange, Mri, SendMessage, reply_chain_id};
use teams_store::OutboxState;

use super::{Seat, channel_write, server};
use crate::support::backends;

async fn send(seat: &Seat, conversation: &str, text: &str) -> teams_store::OutboxEntry {
    seat.engine
        .write(Action::Send {
            conversation: conversation.into(),
            text: MessageText::plain(text).unwrap(),
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn engines_post_in_a_channel_and_reply_in_a_posts_thread() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;

        let post = send(&ada, &channel, "Fuel is loaded").await;
        assert_eq!(post.state, OutboxState::Sent, "{post:?}");
        let post = post.message_id.unwrap();
        let second = send(&ada, &channel, "Weather is clear").await.message_id.unwrap();

        let synced = bert.engine.sync_conversation(channel.as_str()).await.unwrap();
        assert_eq!(synced.inserted, 2, "{synced:?}");
        let chain = reply_chain_id(&channel, &post).unwrap();
        let reply = send(&bert, &chain, "Go for launch").await;
        assert_eq!(reply.state, OutboxState::Sent, "{reply:?}");
        let reply = reply.message_id.unwrap();

        ada.engine.sync_conversation(channel.as_str()).await.unwrap();
        let roots = ada.engine.reply_roots(channel.as_str()).await.unwrap();
        let root_of = |id: &str| {
            roots
                .iter()
                .find(|(message, _)| message == id)
                .map(|(_, root)| root.as_str())
        };
        assert_eq!(root_of(&reply), Some(post.as_str()));
        assert_eq!(root_of(&post), Some(post.as_str()));
        assert_eq!(root_of(&second), Some(second.as_str()));
        let stored = ada
            .engine
            .message(channel.as_str(), reply.as_str())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.conversation_id, channel);

        // the channel's page holds posts and replies; a post's chain only its own
        let reads = ChatReads::new(&*bert.client);
        let page = reads.messages(&channel, 200).await.unwrap();
        assert_eq!(page.messages.len(), 3);
        let link = |id: &str| {
            let message = page.messages.iter().find(|message| message.id == id).unwrap();
            message.extra["conversationLink"].as_str().unwrap().to_owned()
        };
        assert!(link(&reply).ends_with(&format!("{channel};messageid={post}")));
        assert!(link(&second).ends_with(&format!("{channel};messageid={second}")));
        let chain_page = reads.messages(&chain, 200).await.unwrap();
        let ids: Vec<&str> = chain_page.messages.iter().map(|message| message.id.as_str()).collect();
        assert_eq!(ids, [reply.as_str(), post.as_str()]);

        // reactions and edits go to the message in the channel, a reply's as a post's
        let reacted = ada
            .engine
            .write(Action::React {
                conversation: channel.clone(),
                message_id: reply.clone(),
                key: "like".into(),
            })
            .await
            .unwrap();
        assert_eq!(reacted.state, OutboxState::Sent, "{reacted:?}");
        let edited = bert
            .engine
            .write(Action::Edit {
                conversation: channel.clone(),
                message_id: reply.clone(),
                text: MessageText::plain("Go!").unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(edited.state, OutboxState::Sent, "{edited:?}");
        let page = reads.messages(&channel, 200).await.unwrap();
        let after = page.messages.iter().find(|message| message.id == reply).unwrap();
        assert!(
            after.extra["conversationLink"]
                .as_str()
                .unwrap()
                .ends_with(&format!(";messageid={post}"))
        );

        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_teams_general_channel_is_in_the_conversation_list_and_its_other_channels_are_read_by_id() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;
        send(&ada, &team, "Welcome aboard").await;
        send(&ada, &channel, "Fuel is loaded").await;

        let reads = ChatReads::new(&*bert.client);
        let list = reads.conversations().await.unwrap();
        let ids: Vec<&str> = list
            .conversations
            .iter()
            .map(|entry| entry.id.as_str())
            .filter(|id| !crate::support::own_stream(id))
            .collect();
        assert_eq!(ids, [team.as_str()]);
        let general = list
            .conversations
            .iter()
            .find(|entry| entry.id == team.as_str())
            .unwrap();
        assert_eq!(general.thread_properties["threadType"], "space");
        assert_eq!(general.thread_properties["groupId"], group.as_str());
        assert_eq!(general.thread_properties["spaceThreadTopic"], "Rocket");

        let space = reads.thread(&team).await.unwrap();
        assert_eq!(space.kind(), ConversationKind::Channel);
        assert_eq!(space.properties["threadType"], "space");
        assert_eq!(space.properties["groupId"], group.as_str());
        let topic = reads.thread(&channel).await.unwrap();
        assert_eq!(topic.kind(), ConversationKind::Channel);
        assert_eq!(topic.properties["threadType"], "topic");
        assert_eq!(topic.properties["spaceId"], team.as_str());
        assert_eq!(topic.topic(), Some("Launch"));

        // a catch-up brings the general channel's messages; the other channel's come when it is opened
        assert!(bert.engine.sync().await.unwrap().failed.is_empty());
        assert_eq!(
            bert.engine
                .messages(team.as_str(), None, 10)
                .await
                .unwrap()
                .messages
                .len(),
            1
        );
        assert_eq!(
            bert.engine.sync_conversation(channel.as_str()).await.unwrap().inserted,
            1
        );

        // an archived channel takes no new post
        let archive = ChannelChange::Archive {
            archived: true,
            site_read_only: false,
        };
        ada.engine
            .team_write(channel_write(&team, &group, &channel, archive), None)
            .await
            .unwrap();
        assert_ne!(send(&bert, &channel, "Too late").await.state, OutboxState::Sent);
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channel_is_read_by_its_people_and_a_reply_needs_a_post_of_it() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;
        let post = send(&ada, &channel, "Fuel is loaded").await.message_id.unwrap();
        bert.engine.sync_conversation(channel.as_str()).await.unwrap();
        let chain = reply_chain_id(&channel, &post).unwrap();
        let reply = send(&bert, &chain, "Go for launch").await.message_id.unwrap();

        let outsider = ChatReads::new(&*cora.client);
        assert_eq!(outsider.messages(&channel, 200).await.unwrap_err().status(), Some(403));
        assert_eq!(outsider.thread(&channel).await.unwrap_err().status(), Some(403));
        for under in ["1", reply.as_str()] {
            let chain = reply_chain_id(&channel, under).unwrap();
            let author = Author::new(Mri::new(&ada.mri), "Ada");
            let write = SendMessage::new(chain, author, MessageText::plain("Lost").unwrap());
            let refused = ada.client.write(write).await.unwrap();
            assert_eq!(refused.status(), 400);
        }
        // a deleted post takes no more replies
        let deleted = ada
            .engine
            .write(Action::Delete {
                conversation: channel.clone(),
                message_id: post.clone(),
            })
            .await
            .unwrap();
        assert_eq!(deleted.state, OutboxState::Sent, "{deleted:?}");
        let author = Author::new(Mri::new(&bert.mri), "Bert");
        let late = SendMessage::new(chain, author, MessageText::plain("Late").unwrap());
        assert_eq!(bert.client.write(late).await.unwrap().status(), 400);
        // and a chat has no reply chains
        let chat = ada
            .engine
            .write(Action::CreateChat {
                others: vec![Mri::new(&bert.mri)],
                topic: None,
            })
            .await
            .unwrap()
            .message_id
            .unwrap();
        let author = Author::new(Mri::new(&ada.mri), "Ada");
        let write = SendMessage::new(
            format!("{chat};messageid={post}"),
            author,
            MessageText::plain("No").unwrap(),
        );
        let status = ada.client.write(write).await.map_or(400, |answer| answer.status());
        assert_ne!(status, 201);
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}
