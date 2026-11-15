//! The activity feed as users of three ngteams clients meet it: what other people's messages put there live, what
//! takes it out again, what each account's own settings keep out, and the account's own marks on its items, which
//! follow it to its other client and outlast client and server restarts.

use serde_json::json;
use teams_core::activity::{ActivityFeed, ActivityItem, ActivityKind, FEED, Mention};
use teams_core::engine::{Action, MessageChange};
use teams_proto::{ChannelOwnProperty, DeleteMessage, NewChannelKind, OwnTeamView, SetActivityRead, reply_chain_id};

use super::channels::{channel, team};
use super::people::in_chat;
use super::support::{Rig, User};
use crate::chatsvc::activity::{mentioning, react};
use crate::support::backends;

/// `from` posts "<name> <words>" in `chat`, `name` a mention of `target`; the message's id.
async fn mention(from: &User, chat: &str, target: &str, name: &str, words: &str) -> String {
    let action = Action::Send {
        conversation: chat.into(),
        text: mentioning(target, name, words),
    };
    from.write(action).await.message_id.unwrap()
}

/// After a quiet stretch the client's feed has `items` items and was told of none since `mark`.
async fn feed_stays(user: &mut User, mark: usize, items: usize) {
    user.settle().await;
    assert_eq!(user.count(mark, |event| in_chat(event, FEED)), 0, "{}", user.upn);
    let feed = user.engine().activity().await.unwrap();
    assert_eq!(feed.items.len(), items, "{}: {feed:#?}", user.upn);
}

/// Where the client's log stands once what was already on its way has arrived.
async fn quiet(user: &mut User) -> usize {
    user.settle().await;
    user.mark()
}

/// A group chat "Launch" of the three whose first message, a's, the others' clients show.
async fn launch(a: &User, b: &mut User, c: &mut User) -> (String, String) {
    let group = a.create(&[&*b, &*c], Some("Launch")).await;
    let first = a.send(&group, "we shipped").await;
    for user in [b, c] {
        user.pushed(0, &group, &first, MessageChange::New).await;
    }
    (group, first)
}

/// An item of `kind` about `message` in `chat`, from `actor`, with `preview`.
fn tells(item: &ActivityItem, kind: &ActivityKind, actor: &User, place: (&str, &str), preview: &str) {
    let (chat, message) = place;
    assert_eq!(item.kind, *kind);
    assert_eq!(item.actor.as_deref(), Some(actor.mri.as_str()));
    assert_eq!(item.actor_name, "QA Operator");
    assert_eq!(item.thread.as_deref(), Some(chat));
    assert_eq!(item.message.as_deref(), Some(message));
    assert_eq!(item.preview, preview);
}

/// a mentions b, then everyone: each item reaches the people it names, with its sender, preview and place.
async fn mentions_reach_the_people_they_name(a: &mut User, b: &mut User, c: &mut User, group: &str) -> String {
    let (on_a, on_c) = (a.mark(), c.mark());
    let named = mention(a, group, b.mri.as_str(), "Bert", "look at this").await;
    b.since(0, "the mention's feed item", |event| in_chat(event, FEED))
        .await;
    let his = b.feed("the mention", |feed| feed.items.len() == 1).await;
    let item = &his.items[0];
    let you = ActivityKind::Mention(Mention::You);
    tells(item, &you, a, (group, &named), "Bert look at this");
    assert_eq!(item.title(), "QA Operator mentioned you");
    assert_eq!(item.topic.as_deref(), Some("Launch"));
    assert!(!his.is_read(item));
    assert_eq!(his.unseen(), 1);
    c.pushed(on_c, group, &named, MessageChange::New).await;
    feed_stays(c, on_c, 0).await;
    feed_stays(a, on_a, 0).await;
    everyone_is_told(a, b, c, group).await;
    feed_stays(a, on_a, 0).await;
    named
}

/// a mentions everyone in the chat: b and c each get an item.
async fn everyone_is_told(a: &User, b: &mut User, c: &mut User, group: &str) {
    let all = mention(a, group, group, "Everyone", "standup now").await;
    let everyone = ActivityKind::Mention(Mention::Everyone);
    for (user, items) in [(b, 2), (c, 1)] {
        let feed = user.feed("everyone's mention", |feed| feed.items.len() == items);
        let feed = feed.await;
        tells(&feed.items[0], &everyone, a, (group, &all), "Everyone standup now");
    }
}

/// b reacts to a's message and takes it back: a's item comes and goes; nobody else is told.
async fn a_reaction_comes_and_goes(a: &mut User, b: &mut User, c: &mut User, group: &str, first: &str) {
    let (on_b, on_c) = (quiet(b).await, quiet(c).await);
    b.write(react(group, first, "like")).await;
    let mine = a.feed("the reaction", |feed| feed.items.len() == 1).await;
    let like = ActivityKind::Reaction("like".into());
    tells(&mine.items[0], &like, b, (group, first), "we shipped");
    assert_eq!(mine.items[0].title(), "QA Operator reacted to your message");
    c.pushed(on_c, group, first, MessageChange::Reaction).await;
    feed_stays(c, on_c, 1).await;
    feed_stays(b, on_b, 2).await;
    b.write(Action::Unreact {
        conversation: group.into(),
        message_id: first.into(),
        key: "like".into(),
    })
    .await;
    a.feed("the reaction taken back", |feed| feed.items.is_empty()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mentions_reactions_edits_and_deletes_put_items_in_the_right_feeds_live_and_take_them_out_again() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let mut a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let mut c = rig.user(rig.other(), "c@own.test").await;
        let (group, first) = launch(&a, &mut b, &mut c).await;
        let named = mentions_reach_the_people_they_name(&mut a, &mut b, &mut c, &group).await;
        a_reaction_comes_and_goes(&mut a, &mut b, &mut c, &group, &first).await;

        // an edit that adds a mention tells the newly mentioned, and nobody else
        let on_b = quiet(&mut b).await;
        let plain = a.send(&group, "who checks the valves?").await;
        a.eventually_stored(&group, &plain).await;
        c.pushed(0, &group, &plain, MessageChange::New).await;
        a.write(Action::Edit {
            conversation: group.clone(),
            message_id: plain.clone(),
            text: mentioning(c.mri.as_str(), "Cora", "checks the valves"),
        })
        .await;
        let hers = c.feed("the edit's mention", |feed| feed.items.len() == 2).await;
        assert_eq!(hers.items[0].kind, ActivityKind::Mention(Mention::You));
        assert_eq!(hers.items[0].message.as_deref(), Some(plain.as_str()));
        assert_eq!(hers.items[0].preview, "Cora checks the valves");
        b.pushed(on_b, &group, &plain, MessageChange::Edit).await;
        feed_stays(&mut b, on_b, 2).await;

        // a deleted message takes its items out of the feeds
        for (message, user) in [(&named, &mut b), (&plain, &mut c)] {
            a.write(Action::Delete {
                conversation: group.clone(),
                message_id: message.clone(),
            })
            .await;
            let feed = user.feed("the deleted message's item gone", |feed| feed.items.len() == 1);
            let feed = feed.await;
            assert_eq!(feed.items[0].kind, ActivityKind::Mention(Mention::Everyone));
        }
        drop((a, b, c));
        rig.stop().await;
    }
}

async fn set(user: &User, conversation: &str, property: ChannelOwnProperty, value: &str) {
    let write = OwnTeamView::ChannelProperty {
        channel: conversation.into(),
        property,
        value: value.into(),
    };
    user.engine().own_team_view(vec![write], None).await.unwrap();
}

/// b mutes the group chat: it is told of a mention of itself and nothing else, until it unmutes.
async fn a_muted_chat_tells_of_a_direct_mention_alone(a: &mut User, b: &mut User, c: &mut User) {
    let group = a.create(&[&*b, &*c], Some("Launch")).await;
    let mine = b.send(&group, "my part is done").await;
    for user in [&mut *a, &mut *c] {
        user.pushed(0, &group, &mine, MessageChange::New).await;
    }
    set(b, &group, ChannelOwnProperty::Alerts, "false").await;
    muted(a, b, c, (&group, &mine)).await;
    set(b, &group, ChannelOwnProperty::Alerts, "true").await;
    c.write(react(&group, &mine, "heart")).await;
    let his = b.feed("a reaction once unmuted", |feed| feed.items.len() == 2).await;
    assert_eq!(his.items[0].kind, ActivityKind::Reaction("heart".into()));
}

/// While b has the chat muted: a reaction to its message and a mention of everyone tell it nothing, its own name does.
async fn muted(a: &User, b: &mut User, c: &mut User, chat: (&str, &str)) {
    let (group, mine) = chat;
    let on_b = b.mark();
    a.write(react(group, mine, "like")).await;
    mention(a, group, group, "Everyone", "standup").await;
    c.feed("everyone's mention", |feed| feed.items.len() == 1).await;
    feed_stays(b, on_b, 0).await;
    let direct = mention(a, group, b.mri.as_str(), "Bert", "you in particular").await;
    let his = b.feed("the direct mention", |feed| feed.items.len() == 1).await;
    let you = ActivityKind::Mention(Mention::You);
    tells(&his.items[0], &you, a, (group, &direct), "Bert you in particular");
}

/// A reply in a channel post's thread tells the post's writer; a channel's new posts tell who asked to hear of them.
async fn a_channels_reply_and_followed_posts(a: &mut User, b: &mut User, c: &mut User, d: &mut User) {
    let (team, group) = team(a, &[&*b, &*c]).await;
    let (launch, _) = channel(a, &team, &group, "Launch", NewChannelKind::Standard).await;
    let post = a.send(&launch, "Fuel is loaded").await;
    for user in [&mut *b, &mut *c] {
        user.pushed(0, &launch, &post, MessageChange::New).await;
    }
    a_reply_tells_the_posts_writer(a, b, c, (&launch, &post)).await;
    new_posts_tell_who_asked(a, b, c, &launch).await;
    feed_stays(d, 0, 0).await;
}

async fn a_reply_tells_the_posts_writer(a: &mut User, b: &mut User, c: &mut User, place: (&str, &str)) {
    let (launch, post) = place;
    let on_c = quiet(c).await;
    let chain = reply_chain_id(launch, post).unwrap();
    let reply = b.send(&chain, "Go for launch").await;
    let hers = a.feed("the reply", |feed| feed.items.len() == 1).await;
    let item = &hers.items[0];
    tells(item, &ActivityKind::Reply, b, (launch, &reply), "Go for launch");
    assert_eq!(item.title(), "QA Operator replied");
    assert_eq!(item.reply_chain.as_deref(), Some(post));
    assert!(item.in_channel());
    c.pushed(on_c, launch, &reply, MessageChange::New).await;
    feed_stays(c, on_c, 1).await;
}

/// "Notify me for all new messages" is c's own setting: on, a's post tells c alone; off again, nobody.
async fn new_posts_tell_who_asked(a: &User, b: &mut User, c: &mut User, launch: &str) {
    let on_b = quiet(b).await;
    let posts = json!({"allNewPosts": "On", "includeReplies": false}).to_string();
    set(c, launch, ChannelOwnProperty::Notifications, &posts).await;
    let second = a.send(launch, "Weather is clear").await;
    let hers = c.feed("the followed post", |feed| feed.items.len() == 2).await;
    tells(
        &hers.items[0],
        &ActivityKind::Other,
        a,
        (launch, &second),
        "Weather is clear",
    );
    b.pushed(on_b, launch, &second, MessageChange::New).await;
    feed_stays(b, on_b, 2).await;
    let off = json!({"allNewPosts": "Off", "includeReplies": false}).to_string();
    set(c, launch, ChannelOwnProperty::Notifications, &off).await;
    let on_c = quiet(c).await;
    let third = a.send(launch, "Counting down").await;
    c.pushed(on_c, launch, &third, MessageChange::New).await;
    feed_stays(c, on_c, 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_muted_chat_a_channels_reply_and_each_accounts_channel_settings_decide_what_its_feed_gets_live() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let mut a = rig.team_user(&rig.first, "a@own.test").await;
        let mut b = rig.team_user(rig.other(), "b@own.test").await;
        let mut c = rig.team_user(rig.other(), "c@own.test").await;
        let mut d = rig.team_user(rig.other(), "d@own.test").await;
        a_muted_chat_tells_of_a_direct_mention_alone(&mut a, &mut b, &mut c).await;
        a_channels_reply_and_followed_posts(&mut a, &mut b, &mut c, &mut d).await;
        drop((a, b, c, d));
        rig.stop().await;
    }
}

/// What a client shows of the feed: each item with whether it reads as read, the feed's horizon, its all-read
/// bookmark and the rail's count.
type Shown = (Vec<(String, bool)>, Option<i64>, Option<i64>, usize);

fn shown(feed: &ActivityFeed) -> Shown {
    let items = feed.items.iter();
    (
        items.map(|item| (item.id.clone(), feed.is_read(item))).collect(),
        feed.seen_ms,
        feed.bookmark_ms,
        feed.unseen(),
    )
}

fn read(feed: &ActivityFeed, id: &str) -> bool {
    feed.item(id).is_some_and(|item| feed.is_read(item))
}

/// One client of b marks an item read, the other marks it unread; the feed is seen, then all of it read: each state
/// shows on both clients.
async fn marks_follow_the_account(b: &mut User, b2: &mut User, older: &str, newer: &str) {
    b.write(Action::ActivityRead {
        item: older.into(),
        read: true,
    })
    .await;
    for user in [&mut *b, &mut *b2] {
        let feed = user.feed("one item read", |feed| read(feed, older)).await;
        assert!(!read(&feed, newer));
        assert_eq!(feed.unseen(), 2);
    }
    b2.write(Action::ActivityRead {
        item: older.into(),
        read: false,
    })
    .await;
    for user in [&mut *b, &mut *b2] {
        user.feed("the item unread again", |feed| !read(feed, older)).await;
    }
    seen_then_all_read(b, b2, newer).await;
}

async fn seen_then_all_read(b: &mut User, b2: &mut User, newer: &str) {
    b.write(Action::ActivitySeen).await;
    for user in [&mut *b, &mut *b2] {
        let feed = user.feed("the feed seen", |feed| feed.unseen() == 0).await;
        assert_eq!(feed.seen_ms, Some(feed.items[0].at_ms));
        assert!(!read(&feed, newer), "seeing the feed reads no item");
    }
    b.write(Action::ActivityAllRead).await;
    for user in [b, b2] {
        let all = |feed: &ActivityFeed| feed.items.iter().all(|item| feed.is_read(item));
        let feed = user.feed("all read", all).await;
        assert!(feed.bookmark_ms.is_some());
    }
}

/// c's client names b's items: it marks and removes nothing of b's.
async fn nobody_else_marks_them(c: &User, b2: &mut User, item: &str) {
    let before = shown(&b2.engine().activity().await.unwrap());
    let client = c.client();
    let mark = SetActivityRead::new(item, true).unwrap();
    assert_eq!(client.write(mark).await.unwrap().status(), 404);
    let remove = DeleteMessage::new(FEED, item);
    assert_eq!(client.write(remove).await.unwrap().status(), 404);
    b2.settle().await;
    assert_eq!(shown(&b2.engine().activity().await.unwrap()), before);
    assert!(c.engine().activity().await.unwrap().items.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_accounts_marks_on_its_feed_show_on_its_other_client_and_survive_client_and_server_restarts() {
    for backend in backends() {
        let mut rig = Rig::start(backend).await;
        let mut a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let mut b2 = rig.user(&rig.first, "b@own.test").await;
        let mut c = rig.user(rig.other(), "c@own.test").await;
        let (group, _) = launch(&a, &mut b, &mut c).await;
        for words in ["one", "two", "three"] {
            mention(&a, &group, b.mri.as_str(), "Bert", words).await;
        }
        for user in [&mut b, &mut b2] {
            let three = |feed: &ActivityFeed| feed.items.len() == 3 && feed.unseen() == 3;
            user.feed("three unseen items", three).await;
        }
        let feed = b.engine().activity().await.unwrap();
        let ids: Vec<String> = feed.items.iter().map(|item| item.id.clone()).collect();
        let (newer, middle, older) = (&ids[0], &ids[1], &ids[2]);
        marks_follow_the_account(&mut b, &mut b2, older, newer).await;

        // an item after "mark all as read" is new; a removed item is gone on both clients
        mention(&a, &group, b.mri.as_str(), "Bert", "four").await;
        let remove = DeleteMessage::new(FEED, middle.as_str());
        assert_eq!(b.client().write(remove).await.unwrap().status(), 200);
        for user in [&mut b, &mut b2] {
            let pick = |feed: &ActivityFeed| feed.items.len() == 3 && feed.item(middle).is_none();
            let feed = user.feed("a new item and one removed", pick).await;
            assert!(!feed.is_read(&feed.items[0]), "an item after the bookmark is unread");
            assert_eq!(feed.items[0].preview, "Bert four");
            assert_eq!(feed.unseen(), 1);
            assert!(read(&feed, older) && read(&feed, newer));
        }
        nobody_else_marks_them(&c, &mut b2, older).await;
        let kept = shown(&b2.engine().activity().await.unwrap());
        assert_eq!(shown(&b.engine().activity().await.unwrap()), kept);

        // b's client quits and starts again; every node restarts; a new device of b signs in
        b.restart().await;
        assert_eq!(shown(&b.engine().activity().await.unwrap()), kept);
        for user in [&mut a, &mut b, &mut b2, &mut c] {
            user.close();
        }
        rig.restart().await;
        let mut third = rig.user(rig.other(), "b@own.test").await;
        third.feed("what the server kept", |feed| shown(feed) == kept).await;
        for user in [&mut b, &mut b2] {
            user.restart().await;
            assert_eq!(shown(&user.engine().activity().await.unwrap()), kept, "{}", user.upn);
        }
        drop((a, b, b2, c, third));
        rig.stop().await;
    }
}
