//! The activity feed (`48:notifications`) and the mentions stream (`48:mentions`): what other people's messages put
//! there, for whom, and what the account does with its own items.

use serde_json::{Value, json};
use teams_core::activity::{ActivityFeed, ActivityKind, Mention};
use teams_core::engine::{Action, EngineHandle};
use teams_proto::chat::{ChatReads, Marks, MessageText, Node, RichText};
use teams_proto::{Endpoint, Method, Mri, Request, Service};
use teams_store::OutboxState;
use tempfile::TempDir;

use super::messages::Pair;
use super::{client, engine};
use crate::support::{Backend, backends};

pub(super) const FEED: &str = "48:notifications";
pub(super) const MENTIONS: &str = "48:mentions";

/// a's and b's 1:1 chat ([`Pair`]) with a third account c and a group chat of the three.
pub(super) struct Trio {
    pub pair: Pair,
    pub c_id: String,
    pub c: EngineHandle,
    pub group: String,
    _dir: TempDir,
}

impl Trio {
    pub async fn start(backend: Backend) -> Self {
        let pair = Pair::start(backend).await;
        let c_id = pair.server.add_user("c@own.test", "Cora").await;
        let dir = tempfile::tempdir().unwrap();
        let c = engine(&pair.server, "c@own.test", &c_id, dir.path()).await;
        let row = pair
            .a
            .write(Action::CreateChat {
                others: vec![mri(&pair.b_id), mri(&c_id)],
                topic: Some("Launch".into()),
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        let group = row.message_id.unwrap();
        Self {
            pair,
            c_id,
            c,
            group,
            _dir: dir,
        }
    }

    pub async fn stop(self) {
        self.c.shutdown();
        self.pair.stop().await;
    }
}

pub(super) fn mri(id: &str) -> Mri {
    Mri::new(format!("8:orgid:{id}"))
}

/// "<name> <words>", `name` a mention of `target` (a person's MRI, or a group chat's id for everyone in it).
pub(crate) fn mentioning(target: &str, name: &str, words: &str) -> MessageText {
    let text = RichText {
        nodes: vec![Node::Paragraph(vec![
            Node::Mention {
                item_id: "m".into(),
                text: name.into(),
            },
            Node::Text {
                text: format!(" {words}"),
                marks: Marks::NONE,
            },
        ])],
        truncated: false,
    };
    MessageText::new(text, [("m".to_owned(), Mri::new(target))]).unwrap()
}

pub(crate) async fn send(from: &EngineHandle, conversation: &str, text: MessageText) -> String {
    let row = from
        .write(Action::Send {
            conversation: conversation.into(),
            text,
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    row.message_id.unwrap()
}

/// The account's feed after a catch-up.
pub(crate) async fn feed(of: &EngineHandle) -> ActivityFeed {
    assert!(of.sync().await.unwrap().failed.is_empty());
    of.activity().await.unwrap()
}

/// A post as a client that skips the composer's own checks would send it.
pub(crate) struct RawPost {
    pub chat: String,
    pub body: Value,
}

impl Request for RawPost {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn method(&self) -> Method {
        Method::Post
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "users", "ME", "conversations", &self.chat, "messages"])
    }
    fn body(&self) -> Option<Value> {
        Some(self.body.clone())
    }
    fn is_write(&self) -> bool {
        true
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mention_in_a_1_1_chat_is_in_the_mentioned_accounts_feed_and_mentions_stream_alone() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let b = format!("8:orgid:{}", pair.b_id);
        let id = send(&pair.a, &pair.chat, mentioning(&b, "Bert", "look at this")).await;

        let theirs = feed(&pair.b).await;
        assert_eq!(theirs.items.len(), 1, "{theirs:?}");
        let item = &theirs.items[0];
        assert_eq!(item.kind, ActivityKind::Mention(Mention::You));
        assert_eq!(item.subtype.as_deref(), Some("mention"));
        assert_eq!(item.actor.as_deref(), Some(format!("8:orgid:{}", pair.a_id).as_str()));
        assert_eq!(item.actor_name, "Ada");
        assert_eq!(item.title(), "Ada mentioned you");
        assert_eq!(item.preview, "Bert look at this");
        assert_eq!(item.thread.as_deref(), Some(pair.chat.as_str()));
        assert_eq!(item.message.as_deref(), Some(id.as_str()));
        assert_eq!(item.thread_type.as_deref(), Some("chat"));
        assert!(!theirs.is_read(item));
        assert_eq!(theirs.unseen(), 1);

        let reader = client(&pair.server, "b@own.test", &pair.b_id).await;
        let reads = ChatReads::new(&reader);
        let mentions = reads.messages(MENTIONS, 200).await.unwrap();
        assert_eq!(mentions.messages.len(), 1);
        let activity = &mentions.messages[0].properties["activity"];
        assert_eq!(activity["activityType"], "mentions");
        assert_eq!(activity["sourceMessageId"].to_string(), id);
        assert_eq!(activity["targetUserId"], b);
        let listed = reads.conversations().await.unwrap();
        for stream in [FEED, MENTIONS] {
            assert!(listed.conversations.iter().any(|entry| entry.id == stream), "{stream}");
        }

        assert!(feed(&pair.a).await.items.is_empty(), "the sender gets no item");
        let sender = client(&pair.server, "a@own.test", &pair.a_id).await;
        let sender_reads = ChatReads::new(&sender);
        assert_eq!(sender_reads.messages(FEED, 200).await.unwrap_err().status(), Some(404));
        let listed = sender_reads.conversations().await.unwrap();
        assert!(listed.conversations.iter().all(|entry| !entry.id.starts_with("48:")));
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_group_chats_mention_of_a_person_or_of_everyone_reaches_just_the_people_it_names() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let (a, b, c) = (&trio.pair.a, &trio.pair.b, &trio.c);
        let person = send(
            a,
            &trio.group,
            mentioning(&format!("8:orgid:{}", trio.c_id), "Cora", "your turn"),
        )
        .await;

        let hers = feed(c).await;
        assert_eq!(hers.items.len(), 1, "{hers:?}");
        assert_eq!(hers.items[0].kind, ActivityKind::Mention(Mention::You));
        assert_eq!(hers.items[0].subtype.as_deref(), Some("person"));
        assert_eq!(hers.items[0].message.as_deref(), Some(person.as_str()));
        assert_eq!(hers.items[0].topic.as_deref(), Some("Launch"));
        assert!(
            feed(b).await.items.is_empty(),
            "a group member nobody mentioned gets no item"
        );

        let everyone = send(b, &trio.group, mentioning(&trio.group, "Everyone", "standup now")).await;
        for (engine, count) in [(a, 1), (c, 2)] {
            let feed = feed(engine).await;
            assert_eq!(feed.items.len(), count, "{feed:?}");
            let item = &feed.items[0];
            assert_eq!(item.kind, ActivityKind::Mention(Mention::Everyone));
            assert_eq!(item.message.as_deref(), Some(everyone.as_str()));
            assert_eq!(item.actor_name, "Bert");
            assert_eq!(item.title(), "Bert mentioned Everyone");
        }
        assert!(
            feed(b).await.items.is_empty(),
            "the sender is not told of its own mention of everyone"
        );
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mention_reaches_nobody_outside_the_chat_and_names_no_other_chats_everyone() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let (a, b, c) = (&trio.pair.a, &trio.pair.b, &trio.c);
        let chat = &trio.pair.chat;
        send(
            a,
            chat,
            mentioning(&format!("8:orgid:{}", trio.c_id), "Cora", "a secret of a and b"),
        )
        .await;
        assert!(feed(c).await.items.is_empty(), "c is not in a's and b's chat");
        let outsider = client(&trio.pair.server, "c@own.test", &trio.c_id).await;
        assert_eq!(
            ChatReads::new(&outsider)
                .messages(FEED, 200)
                .await
                .unwrap_err()
                .status(),
            Some(404)
        );

        send(
            a,
            chat,
            mentioning(&format!("8:orgid:{}", trio.pair.a_id), "Ada", "a note to self"),
        )
        .await;
        assert!(feed(a).await.items.is_empty(), "nobody is told of mentioning themself");

        let sender = client(&trio.pair.server, "a@own.test", &trio.pair.a_id).await;
        let mentions = json!([{"@type": "http://schema.skype.com/Mention", "itemid": "0", "mri": trio.group,
            "mentionType": "everyone", "displayName": "Everyone"}]);
        let post = RawPost {
            chat: chat.clone(),
            body: json!({
                "messagetype": "RichText/Html", "contenttype": "Text", "clientmessageid": "424242",
                "content": "<p><span itemtype=\"http://schema.skype.com/Mention\" itemscope itemid=\"0\">Everyone</span> hi</p>",
                "properties": {"mentions": mentions.to_string()},
            }),
        };
        assert_eq!(sender.write(post).await.unwrap().status(), 201);
        assert!(
            feed(c).await.items.is_empty(),
            "everyone of another chat is nobody here"
        );
        assert!(feed(b).await.items.is_empty(), "and nobody of this chat either");
        trio.stop().await;
    }
}

pub(crate) fn react(conversation: &str, message: &str, key: &str) -> Action {
    Action::React {
        conversation: conversation.into(),
        message_id: message.into(),
        key: key.into(),
    }
}

pub(crate) async fn sent(engine: &EngineHandle, action: Action) {
    let row = engine.write(action).await.unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reaction_tells_the_messages_sender_until_it_is_taken_back() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let (a, b, c) = (&trio.pair.a, &trio.pair.b, &trio.c);
        let id = send(a, &trio.group, MessageText::plain("we shipped").unwrap()).await;
        for engine in [a, b, c] {
            assert!(engine.sync().await.unwrap().failed.is_empty());
        }
        sent(a, react(&trio.group, &id, "like")).await;
        assert!(
            feed(a).await.items.is_empty(),
            "a reaction to one's own message tells nobody"
        );

        sent(b, react(&trio.group, &id, "like")).await;
        let mine = feed(a).await;
        assert_eq!(mine.items.len(), 1, "{mine:?}");
        let item = &mine.items[0];
        assert_eq!(item.kind, ActivityKind::Reaction("like".into()));
        assert_eq!(
            item.actor.as_deref(),
            Some(format!("8:orgid:{}", trio.pair.b_id).as_str())
        );
        assert_eq!(item.title(), "Bert reacted to your message");
        assert_eq!(item.message.as_deref(), Some(id.as_str()));
        assert_eq!(item.thread.as_deref(), Some(trio.group.as_str()));
        assert_eq!(item.preview, "we shipped");

        sent(c, react(&trio.group, &id, "heart")).await;
        let mine = feed(a).await;
        assert_eq!(mine.items.len(), 1, "reactions to one message are one item: {mine:?}");
        assert_eq!(mine.items[0].others, 1);
        assert_eq!(mine.items[0].ids.len(), 2);

        sent(
            b,
            Action::Unreact {
                conversation: trio.group.clone(),
                message_id: id.clone(),
                key: "like".into(),
            },
        )
        .await;
        let mine = feed(a).await;
        assert_eq!(mine.items.len(), 1, "{mine:?}");
        assert_eq!(mine.items[0].others, 0);
        assert_eq!(
            mine.items[0].actor.as_deref(),
            Some(format!("8:orgid:{}", trio.c_id).as_str())
        );
        assert_eq!(mine.items[0].kind, ActivityKind::Reaction("heart".into()));

        sent(b, react(&trio.group, &id, "laugh")).await;
        assert_eq!(feed(a).await.items[0].others, 1, "a reaction given again is told again");
        for engine in [b, c] {
            assert!(feed(engine).await.items.is_empty(), "only the message's sender is told");
        }
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_message_takes_its_items_out_of_the_feeds_and_an_edit_tells_the_newly_mentioned() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let (a, b, c) = (&trio.pair.a, &trio.pair.b, &trio.c);
        let b_mri = format!("8:orgid:{}", trio.pair.b_id);
        let c_mri = format!("8:orgid:{}", trio.c_id);
        let id = send(a, &trio.group, mentioning(&b_mri, "Bert", "first wording")).await;
        for engine in [a, b, c] {
            assert!(engine.sync().await.unwrap().failed.is_empty());
        }
        sent(c, react(&trio.group, &id, "like")).await;
        assert_eq!(feed(b).await.items.len(), 1);
        assert_eq!(feed(a).await.items.len(), 1);

        sent(
            a,
            Action::Edit {
                conversation: trio.group.clone(),
                message_id: id.clone(),
                text: mentioning(&c_mri, "Cora", "second wording"),
            },
        )
        .await;
        let hers = feed(c).await;
        assert_eq!(hers.items.len(), 1, "{hers:?}");
        assert_eq!(hers.items[0].preview, "Cora second wording");
        assert_eq!(
            feed(b).await.items.len(),
            1,
            "an edit writes no second item for the same message"
        );

        sent(
            a,
            Action::Delete {
                conversation: trio.group.clone(),
                message_id: id.clone(),
            },
        )
        .await;
        for engine in [a, b, c] {
            let feed = feed(engine).await;
            assert!(
                feed.items.is_empty(),
                "a deleted message's preview stays in no feed: {feed:?}"
            );
        }
        trio.stop().await;
    }
}

struct RemoveItem(String);

impl Request for RemoveItem {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn method(&self) -> Method {
        Method::Delete
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "users", "ME", "conversations", FEED, "messages", &self.0])
            .with_query("behavior", "softDelete")
    }
    fn body(&self) -> Option<Value> {
        None
    }
    fn is_write(&self) -> bool {
        true
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_marks_its_feed_items_read_or_unread_sees_the_feed_marks_all_read_and_removes_an_item() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let b_mri = format!("8:orgid:{}", pair.b_id);
        send(&pair.a, &pair.chat, mentioning(&b_mri, "Bert", "one")).await;
        send(&pair.a, &pair.chat, mentioning(&b_mri, "Bert", "two")).await;
        let before = feed(&pair.b).await;
        assert_eq!(before.unseen(), 2);
        let (newer, older) = (before.items[0].id.clone(), before.items[1].id.clone());

        // what the server keeps is what a second engine of the account, with an empty store, reads back
        let dir = tempfile::tempdir().unwrap();
        let other_device = engine(&pair.server, "b@own.test", &pair.b_id, dir.path()).await;
        let read = |feed: &ActivityFeed, id: &str| feed.is_read(feed.item(id).unwrap());

        sent(
            &pair.b,
            Action::ActivityRead {
                item: older.clone(),
                read: true,
            },
        )
        .await;
        let kept = feed(&other_device).await;
        assert!(read(&kept, &older));
        assert!(!read(&kept, &newer));
        assert_eq!(kept.unseen(), 1);

        sent(
            &pair.b,
            Action::ActivityRead {
                item: older.clone(),
                read: false,
            },
        )
        .await;
        assert!(!read(&feed(&other_device).await, &older));

        sent(&pair.b, Action::ActivitySeen).await;
        let kept = feed(&other_device).await;
        assert_eq!(kept.seen_ms, Some(kept.items[0].at_ms));
        assert_eq!(kept.unseen(), 0, "nothing is newer than the feed's own horizon");
        assert!(!read(&kept, &newer), "seeing the feed reads no item");

        sent(&pair.b, Action::ActivityAllRead).await;
        let kept = feed(&other_device).await;
        assert!(kept.bookmark_ms.is_some());
        assert!(read(&kept, &older) && read(&kept, &newer), "{kept:?}");

        send(&pair.a, &pair.chat, mentioning(&b_mri, "Bert", "three")).await;
        let kept = feed(&other_device).await;
        assert_eq!(kept.items.len(), 3);
        assert!(!kept.is_read(&kept.items[0]), "an item after the bookmark is unread");
        assert_eq!(kept.unseen(), 1);

        let own = client(&pair.server, "b@own.test", &pair.b_id).await;
        assert_eq!(own.write(RemoveItem(newer.clone())).await.unwrap().status(), 200);
        let kept = feed(&other_device).await;
        assert_eq!(kept.items.len(), 2);
        assert!(kept.item(&newer).is_none());

        // the sender has no feed, and b's item ids name nothing in anyone else's
        let other = client(&pair.server, "a@own.test", &pair.a_id).await;
        assert_eq!(other.write(RemoveItem(older.clone())).await.unwrap().status(), 404);
        let mark = teams_proto::SetActivityRead::new(&older, true).unwrap();
        assert_eq!(other.write(mark).await.unwrap().status(), 404);
        assert!(!read(&feed(&other_device).await, &older) || kept.bookmark_ms.is_some());
        assert!(feed(&other_device).await.item(&older).is_some());
        other_device.shutdown();
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn nobody_posts_into_a_feed_and_a_stream_is_no_thread() {
    let pair = Pair::start(Backend::Sqlite).await;
    let b_mri = format!("8:orgid:{}", pair.b_id);
    send(&pair.a, &pair.chat, mentioning(&b_mri, "Bert", "one")).await;
    let own = client(&pair.server, "b@own.test", &pair.b_id).await;
    for stream in [FEED, MENTIONS] {
        let post = RawPost {
            chat: stream.into(),
            body: json!({"messagetype": "Text", "content": "made up", "clientmessageid": "77"}),
        };
        assert_eq!(own.write(post).await.unwrap().status(), 403, "{stream}");
        let reads = ChatReads::new(&own);
        assert_eq!(reads.thread(stream).await.unwrap_err().status(), Some(404));
        assert_eq!(reads.members(stream).await.unwrap_err().status(), Some(404));
        assert_eq!(reads.messages(stream, 200).await.unwrap().messages.len(), 1);
    }
    let made_up = format!("{FEED}:{}", pair.b_id);
    let reads = ChatReads::new(&own);
    assert_eq!(
        reads.messages(&made_up, 200).await.unwrap_err().status(),
        Some(404),
        "the thread a stream is kept as has no name a request can use"
    );
    assert_eq!(reads.thread(&made_up).await.unwrap_err().status(), Some(404));
    pair.stop().await;
}
