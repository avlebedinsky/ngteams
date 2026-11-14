//! A channel's activity: whom a reply, a mention and a new post tell, and what each account's own notification
//! settings of the channel (or of a chat) change about that.

use serde_json::json;
use teams_core::activity::{ActivityKind, Mention};
use teams_core::engine::Action;
use teams_proto::chat::MessageText;
use teams_proto::{ChannelOwnProperty, Mri, OwnTeamView, reply_chain_id};
use teams_store::OutboxState;

use super::{Seat, new_channel, server};
use crate::chatsvc::activity::{RawPost, feed, mentioning, react, send, sent};
use crate::support::backends;

fn plain(words: &str) -> MessageText {
    MessageText::plain(words).unwrap()
}

/// A post whose one mention names `mri` as a `kind` (`channel`, `team`): the composer writes no such mention.
fn broad_mention(conversation: &str, kind: &str, mri: &str, id: &str) -> RawPost {
    let mentions = json!([{"@type": "http://schema.skype.com/Mention", "itemid": "0", "mri": mri,
        "mentionType": kind, "displayName": "Launch"}]);
    RawPost {
        chat: conversation.into(),
        body: json!({
            "messagetype": "RichText/Html", "contenttype": "Text", "clientmessageid": id,
            "content": "<p><span itemtype=\"http://schema.skype.com/Mention\" itemscope itemid=\"0\">Launch</span> all hands</p>",
            "properties": {"mentions": mentions.to_string()},
        }),
    }
}

/// A reply from `seat` under `post`, which it reads first, as its client has when it replies.
async fn reply(seat: &Seat, channel: &str, post: &str, words: &str) -> String {
    seat.engine.sync_conversation(channel).await.unwrap();
    send(&seat.engine, &reply_chain_id(channel, post).unwrap(), plain(words)).await
}

async fn set(seat: &Seat, conversation: &str, property: ChannelOwnProperty, value: &str) {
    let write = OwnTeamView::ChannelProperty {
        channel: conversation.into(),
        property,
        value: value.into(),
    };
    seat.engine.own_team_view(vec![write], None).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channels_reply_tells_the_threads_people_and_a_mention_the_people_it_names_in_the_channel() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let dan = Seat::new(&server, "dan@own.test", "Dan").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert, &cora]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;

        let post = send(&ada.engine, &channel, plain("Fuel is loaded")).await;
        let reply = reply(&bert, &channel, &post, "Go for launch").await;
        let hers = feed(&ada.engine).await;
        assert_eq!(hers.items.len(), 1, "{hers:?}");
        let item = &hers.items[0];
        assert_eq!(item.kind, ActivityKind::Reply);
        assert_eq!(item.title(), "Bert replied");
        assert_eq!(item.thread.as_deref(), Some(channel.as_str()));
        assert_eq!(item.message.as_deref(), Some(reply.as_str()));
        assert_eq!(item.reply_chain.as_deref(), Some(post.as_str()));
        assert_eq!(item.topic.as_deref(), Some("Rocket"));
        assert!(item.in_channel());
        assert!(
            feed(&cora.engine).await.items.is_empty(),
            "cora isn't in the post's thread"
        );
        assert!(feed(&bert.engine).await.items.is_empty());

        self::reply(&cora, &channel, &post, "Counting down").await;
        assert_eq!(feed(&ada.engine).await.items.len(), 2);
        let his = feed(&bert.engine).await;
        assert_eq!(
            his.items.len(),
            1,
            "someone who replied in the thread is told of the next reply"
        );
        assert_eq!(his.items[0].kind, ActivityKind::Reply);

        send(&ada.engine, &channel, mentioning(&cora.mri, "Cora", "check the valves")).await;
        let hers = feed(&cora.engine).await;
        assert_eq!(hers.items.len(), 1, "{hers:?}");
        assert_eq!(hers.items[0].kind, ActivityKind::Mention(Mention::You));
        assert_eq!(hers.items[0].thread_type.as_deref(), Some("topic"));
        send(&ada.engine, &channel, mentioning(&dan.mri, "Dan", "not on the team")).await;
        assert!(
            feed(&dan.engine).await.items.is_empty(),
            "nobody outside the channel is told of its messages"
        );

        let wide = broad_mention(&channel, "channel", &channel, "501");
        assert_eq!(ada.client.write(wide).await.unwrap().status(), 201);
        let whole_team = broad_mention(&channel, "team", &team, "502");
        assert_eq!(ada.client.write(whole_team).await.unwrap().status(), 201);
        for seat in [&bert, &cora] {
            let feed = feed(&seat.engine).await;
            let kinds: Vec<_> = feed.items.iter().take(2).map(|item| item.kind.clone()).collect();
            assert_eq!(
                kinds,
                [
                    ActivityKind::Mention(Mention::Team),
                    ActivityKind::Mention(Mention::Channel)
                ]
            );
        }
        let elsewhere = broad_mention(&channel, "channel", "19:another@thread.tacv2", "503");
        assert_eq!(ada.client.write(elsewhere).await.unwrap().status(), 201);
        assert_eq!(
            feed(&bert.engine).await.items.len(),
            3,
            "another channel's mention is nobody's here"
        );

        // a private channel's messages tell its own people alone
        let made = ada
            .engine
            .create_channel(teams_core::ChannelDraft {
                channel: new_channel(&team, &group, "Crew", teams_proto::NewChannelKind::Private),
                show_by_default: false,
            })
            .await
            .unwrap();
        let failed = ada
            .engine
            .add_channel_members(group.as_str(), made.group.unwrap(), vec![bert.mri.clone()])
            .await
            .unwrap();
        assert!(failed.is_empty());
        let private = made.thread.unwrap();
        let before = feed(&cora.engine).await.items.len();
        send(&ada.engine, &private, mentioning(&cora.mri, "Cora", "crew only")).await;
        let wide = broad_mention(&private, "channel", &private, "504");
        assert_eq!(ada.client.write(wide).await.unwrap().status(), 201);
        assert_eq!(
            feed(&cora.engine).await.items.len(),
            before,
            "cora isn't in the private channel"
        );
        assert_eq!(feed(&bert.engine).await.items.len(), 4);
        for seat in [ada, bert, cora, dan] {
            seat.stop();
        }
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_accounts_channel_notification_settings_decide_what_its_feed_is_told_of() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert, &cora]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;
        let count = async |seat: &Seat| feed(&seat.engine).await.items.len();

        let first = send(&ada.engine, &channel, plain("Nobody asked to hear of posts")).await;
        assert_eq!(count(&bert).await, 0);

        // "Notify me for all new messages", without replies
        let posts = json!({"allNewPosts": "On", "includeReplies": false}).to_string();
        set(&bert, &channel, ChannelOwnProperty::Notifications, &posts).await;
        let post = send(&ada.engine, &channel, plain("Fuel is loaded")).await;
        let his = feed(&bert.engine).await;
        assert_eq!(his.items.len(), 1, "{his:?}");
        assert_eq!(his.items[0].kind, ActivityKind::Other);
        assert_eq!(his.items[0].message.as_deref(), Some(post.as_str()));
        assert_eq!(his.items[0].preview, "Fuel is loaded");
        assert_eq!(his.items[0].actor_name, "Ada");
        assert_eq!(count(&cora).await, 0, "the setting is bert's own");
        reply(&ada, &channel, &post, "Still loading").await;
        assert_eq!(count(&bert).await, 1, "replies aren't included");

        let with_replies = json!({"allNewPosts": "On", "dskNotif": "On", "includeReplies": true}).to_string();
        set(&bert, &channel, ChannelOwnProperty::Notifications, &with_replies).await;
        reply(&ada, &channel, &post, "Loaded").await;
        assert_eq!(count(&bert).await, 2);

        let off = json!({"allNewPosts": "Off", "includeReplies": false}).to_string();
        set(&bert, &channel, ChannelOwnProperty::Notifications, &off).await;
        send(&ada.engine, &channel, plain("Weather is clear")).await;
        assert_eq!(count(&bert).await, 2);

        // "Follow all new threads": replies in threads the account isn't in
        set(&cora, &channel, ChannelOwnProperty::FollowAll, "1760000060000").await;
        reply(&bert, &channel, &first, "I did ask").await;
        let hers = feed(&cora.engine).await;
        assert_eq!(hers.items.len(), 1, "{hers:?}");
        assert_eq!(hers.items[0].kind, ActivityKind::Reply);
        set(&cora, &channel, ChannelOwnProperty::FollowAll, "").await;
        reply(&bert, &channel, &first, "Twice").await;
        assert_eq!(count(&cora).await, 1);

        // a muted channel tells of nothing but a mention of the account itself
        let ada_items = count(&ada).await;
        set(&ada, &channel, ChannelOwnProperty::Alerts, "false").await;
        reply(&bert, &channel, &first, "Thrice").await;
        let wide = broad_mention(&channel, "channel", &channel, "601");
        assert_eq!(bert.client.write(wide).await.unwrap().status(), 201);
        assert_eq!(
            count(&ada).await,
            ada_items,
            "muted: no reply, no mention of the channel"
        );
        send(
            &bert.engine,
            &channel,
            mentioning(&ada.mri, "Ada", "this one is for you"),
        )
        .await;
        let hers = feed(&ada.engine).await;
        assert_eq!(hers.items.len(), ada_items + 1);
        assert_eq!(hers.items[0].kind, ActivityKind::Mention(Mention::You));
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_muted_chat_tells_its_account_of_a_mention_of_itself_alone() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let row = ada
            .engine
            .write(Action::CreateChat {
                others: vec![Mri::new(&bert.mri), Mri::new(&cora.mri)],
                topic: None,
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        let group = row.message_id.unwrap();
        let mine = send(&ada.engine, &group, plain("we shipped")).await;
        for seat in [&ada, &bert, &cora] {
            assert!(seat.engine.sync().await.unwrap().failed.is_empty());
        }
        set(&ada, &group, ChannelOwnProperty::Alerts, "false").await;
        sent(&bert.engine, react(&group, &mine, "like")).await;
        send(&bert.engine, &group, mentioning(&group, "Everyone", "standup")).await;
        assert!(
            feed(&ada.engine).await.items.is_empty(),
            "muted: no reaction, no mention of everyone"
        );
        assert_eq!(feed(&cora.engine).await.items.len(), 1, "cora didn't mute the chat");
        send(&bert.engine, &group, mentioning(&ada.mri, "Ada", "you in particular")).await;
        assert_eq!(feed(&ada.engine).await.items.len(), 1);

        set(&ada, &group, ChannelOwnProperty::Alerts, "true").await;
        sent(&cora.engine, react(&group, &mine, "heart")).await;
        assert_eq!(feed(&ada.engine).await.items.len(), 2);
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}
