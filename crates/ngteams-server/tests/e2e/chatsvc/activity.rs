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
pub(super) fn mentioning(target: &str, name: &str, words: &str) -> MessageText {
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

pub(super) async fn send(from: &EngineHandle, conversation: &str, text: MessageText) -> String {
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
pub(super) async fn feed(of: &EngineHandle) -> ActivityFeed {
    assert!(of.sync().await.unwrap().failed.is_empty());
    of.activity().await.unwrap()
}

/// A post as a client that skips the composer's own checks would send it.
struct RawPost {
    chat: String,
    body: Value,
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
