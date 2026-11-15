//! Rich messages (mentions, mentions of everyone, quoted replies) as the real engine sends them, read back and
//! delivered in the forms the synthetic fixtures record (`tests/fixtures/chatsvc/{mention,mention-group,
//! mention-everyone,reply}.json`). JSON is compared as parsed values.

use std::time::Duration;

use serde_json::{Value, json};
use teams_core::engine::Action;
use teams_proto::chat::{MessageText, Node, Quote, RichText, parse_html};
use teams_proto::{Endpoint, Mri, Read, Service};
use teams_store::OutboxState;

use super::activity::{RawPost, Trio, send};
use super::{client, messages::Pair};
use crate::support::{Backend, TestServer, backends};

const MENTION: &str = include_str!("../../../../../tests/fixtures/chatsvc/mention.json");
const MENTION_GROUP: &str = include_str!("../../../../../tests/fixtures/chatsvc/mention-group.json");
const MENTION_EVERYONE: &str = include_str!("../../../../../tests/fixtures/chatsvc/mention-everyone.json");
const REPLY: &str = include_str!("../../../../../tests/fixtures/chatsvc/reply.json");

/// The chat service base the tests' public origin gives.
const BASE: &str = "https://own.example.test/api/chatsvc/own";

/// A fixture with its placeholders filled in: the lab's names and ids as the test's, Microsoft's bases as the server's.
pub(super) fn fixture(source: &str, swaps: &[(&str, &str)]) -> Value {
    let mut source = source
        .replace("https://teams.microsoft.com/api/chatsvc/<region>", BASE)
        .replace("https://notifications.skype.net", BASE);
    for (from, to) in swaps {
        source = source.replace(from, to);
    }
    serde_json::from_str(&source).unwrap()
}

/// A raw read of one conversation's newest messages, as the engine's catch-up asks for them.
struct Page {
    chat: String,
}

impl Read for Page {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "users", "ME", "conversations", &self.chat, "messages"])
            .with_query("view", "msnp24Equivalent|supportsMessageProperties")
            .with_query("pageSize", "200")
    }
}

/// Message `id` of `chat` as account `upn` reads it, raw.
pub(super) async fn read_back(server: &TestServer, upn: &str, user: &str, chat: &str, id: &str) -> Value {
    let reader = client(server, upn, user).await;
    let response = reader.read(&Page { chat: chat.into() }).await.unwrap();
    assert_eq!(response.status(), 200);
    let page: Value = serde_json::from_slice(response.body()).unwrap();
    page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["id"] == id)
        .unwrap_or_else(|| panic!("{id} not in {page}"))
        .clone()
}

/// `message` with its JSON-string `properties.mentions` parsed: compared as values, never as text (the key order of
/// the text is the sending client's `serde_json`'s).
fn parsed(mut message: Value) -> Value {
    if let Some(text) = message["properties"]["mentions"].as_str() {
        message["properties"]["mentions"] = serde_json::from_str(text).unwrap();
    }
    message
}

/// `golden` with what the server chooses itself (ids, times, its order number) taken from `actual`, and what it
/// doesn't make (Microsoft's language detection) left out.
pub(super) fn served(mut golden: Value, actual: &Value) -> Value {
    for key in [
        "id",
        "version",
        "sequenceId",
        "clientmessageid",
        "composetime",
        "originalarrivaltime",
    ] {
        golden[key] = actual[key].clone();
    }
    golden["properties"].as_object_mut().unwrap().remove("languageStamp");
    golden
}

/// `<mention> <rest>` in one paragraph, the mention `item_id` 0 naming `mri`: as the compose box writes one.
fn mention(name: &str, rest: &str, mri: &str) -> MessageText {
    let text = RichText {
        nodes: vec![Node::Paragraph(vec![
            Node::Mention {
                item_id: "0".into(),
                text: name.into(),
            },
            Node::Text {
                text: format!("\u{a0}{rest}"),
                marks: teams_proto::chat::Marks::NONE,
            },
        ])],
        truncated: false,
    };
    MessageText::new(text, [("0".to_owned(), Mri::new(mri))]).unwrap()
}

/// The first `NewMessage` of message `id` on a fan-out subscription.
async fn delivered(events: &mut ngteams_server::fanout::Subscription, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event: Value = serde_json::from_slice(&events.recv().await.unwrap()).unwrap();
            if event["resourceType"] == "NewMessage" && event["resource"]["id"] == id {
                return event["resource"].clone();
            }
        }
    })
    .await
    .unwrap()
}

/// The mentions of a stored message's content, as the reader's client parses them.
fn mentions_in(nodes: &[Node]) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for node in nodes {
        match node {
            Node::Mention { item_id, text } => found.push((item_id.clone(), text.clone())),
            Node::Paragraph(children) => found.extend(mentions_in(children)),
            _ => {}
        }
    }
    found
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mention_reads_back_and_is_delivered_without_its_compose_wrapper_and_with_a_numbered_entry() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let (a, b) = (pair.a_id.as_str(), pair.b_id.as_str());
        let golden = fixture(
            MENTION,
            &[
                ("19:<guid:a>_<guid:b>@unq.gbl.spaces", &pair.chat),
                ("<guid:a>", a),
                ("<guid:b>", b),
                ("Lab A", "Ada"),
                ("Lab B", "Bert"),
            ],
        );
        let text = mention("Bert", "a made-up mention", &format!("8:orgid:{b}"));
        // the engine sends the captured form, wrapper and all
        assert_eq!(text.html(), golden["request"]["body"]["content"]);
        let mut events = pair
            .server
            .app
            .fanout
            .subscribe(&format!("push:8:orgid:{b}"))
            .await
            .unwrap();

        let id = send(&pair.a, &pair.chat, text).await;

        let read = read_back(&pair.server, "b@own.test", b, &pair.chat, &id).await;
        assert_eq!(parsed(read.clone()), parsed(served(golden["read_back"].clone(), &read)));
        assert!(read["properties"]["mentions"].is_string());
        let resource = delivered(&mut events, &id).await;
        assert_eq!(resource["content"], read["content"]);
        assert_eq!(resource["properties"]["mentions"], read["properties"]["mentions"]);

        // the other member's engine stores it and its client reads the mention back
        assert!(pair.b.sync().await.unwrap().failed.is_empty());
        let stored = pair.b.message(&pair.chat, &id).await.unwrap().unwrap();
        assert_eq!(
            mentions_in(&parse_html(&stored.content).nodes),
            [("0".to_owned(), "Bert".to_owned())]
        );
        pair.stop().await;
    }
}

/// The group part of a group chat's id (`19:<group>@thread.v2`), as the fixtures name it.
fn group_of(chat: &str) -> &str {
    chat.trim_start_matches("19:").trim_end_matches("@thread.v2")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_group_chats_mentions_of_a_member_and_of_everyone_read_back_numbered_and_reach_the_readers_client() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let (a, b, c) = (&trio.pair.a_id, &trio.pair.b_id, &trio.c_id);
        let swaps = [
            ("<guid:a>", a.as_str()),
            ("<guid:b>", b.as_str()),
            ("<group>", group_of(&trio.group)),
            ("Lab A", "Ada"),
            ("Lab B", "Bert"),
        ];

        let member = fixture(MENTION_GROUP, &swaps);
        let text = mention("Bert", "a made-up group mention", &format!("8:orgid:{b}"));
        assert_eq!(text.html(), member["request"]["body"]["content"]);
        let id = send(&trio.pair.a, &trio.group, text).await;
        let read = read_back(&trio.pair.server, "c@own.test", c, &trio.group, &id).await;
        assert_eq!(read["content"], member["read_back"]["content"]);
        assert_eq!(
            parsed(read.clone())["properties"]["mentions"],
            parsed(member["read_back"].clone())["properties"]["mentions"]
        );

        let everyone = fixture(MENTION_EVERYONE, &swaps);
        let text = mention("Everyone", "a made-up mention of everyone", &trio.group);
        assert_eq!(text.html(), everyone["request"]["body"]["content"]);
        let id = send(&trio.pair.a, &trio.group, text).await;
        let read = read_back(&trio.pair.server, "c@own.test", c, &trio.group, &id).await;
        assert_eq!(
            read["content"],
            "<p><span itemtype=\"http://schema.skype.com/Mention\" itemscope=\"\" itemid=\"0\">Everyone</span>&nbsp;\
             a made-up mention of everyone</p>"
        );
        let entries: Value = serde_json::from_str(read["properties"]["mentions"].as_str().unwrap()).unwrap();
        assert_eq!(
            entries,
            json!([{"@type": "http://schema.skype.com/Mention", "itemid": 0, "mri": trio.group,
                "mentionType": "everyone", "displayName": "Everyone"}])
        );

        // Cora's client: the mention of everyone is a mention, of kind everyone
        assert!(trio.c.sync().await.unwrap().failed.is_empty());
        let stored = trio.c.message(&trio.group, &id).await.unwrap().unwrap();
        assert_eq!(
            mentions_in(&parse_html(&stored.content).nodes),
            [("0".to_owned(), "Everyone".to_owned())]
        );
        trio.stop().await;
    }
}

fn plain(text: &str) -> Node {
    Node::Text {
        text: text.into(),
        marks: teams_proto::chat::Marks::NONE,
    }
}

/// A reply quoting message `quoted` of `sender` (named `name`), then `reply`: as the compose box's Reply writes one.
fn reply(quoted: &str, sender: &str, name: &str, preview: &str, reply: &str) -> MessageText {
    let quote = Quote {
        message_id: Some(quoted.into()),
        sender: Some(format!("8:orgid:{sender}")),
        sender_name: Some(name.into()),
        time: Some(quoted.into()),
        body: vec![Node::Paragraph(vec![plain(preview)])],
        forward: false,
    };
    let text = RichText {
        nodes: vec![Node::Quote(quote), Node::Paragraph(vec![plain(reply)])],
        truncated: false,
    };
    MessageText::new(text, []).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_quoted_reply_reads_back_with_its_quote_checked_against_the_chat() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let (a, b) = (pair.a_id.as_str(), pair.b_id.as_str());
        let quoted = pair.send("a made-up message, edited").await;
        let golden = fixture(
            REPLY,
            &[
                ("19:<guid:a>_<guid:b>@unq.gbl.spaces", &pair.chat),
                ("<guid:a>", a),
                ("<guid:b>", b),
                ("Lab A", "Ada"),
                ("1760000010000", &quoted),
            ],
        );
        let text = reply(&quoted, a, "Ada", "a made-up message, edited", "a made-up reply");
        assert_eq!(text.html(), golden["request"]["body"]["content"]);
        let mut events = pair
            .server
            .app
            .fanout
            .subscribe(&format!("push:8:orgid:{b}"))
            .await
            .unwrap();

        let id = send(&pair.a, &pair.chat, text).await;

        let read = read_back(&pair.server, "b@own.test", b, &pair.chat, &id).await;
        let mut expected = served(golden["read_back"].clone(), &read);
        // Teams puts a line feed between the quote and the reply; the markup's model is the same either way
        assert_eq!(
            parse_html(read["content"].as_str().unwrap()),
            parse_html(expected["content"].as_str().unwrap())
        );
        expected["content"] = read["content"].clone();
        assert_eq!(read, expected);
        assert_eq!(delivered(&mut events, &id).await["properties"], read["properties"]);
        assert!(pair.b.sync().await.unwrap().failed.is_empty());
        let stored = pair.b.message(&pair.chat, &id).await.unwrap().unwrap();
        let quote = parse_html(&stored.content)
            .nodes
            .into_iter()
            .find_map(|node| match node {
                Node::Quote(quote) => Some(quote),
                _ => None,
            });
        assert_eq!(quote.unwrap().message_id.as_deref(), Some(quoted.as_str()));

        // a quote of a message that isn't the chat's is posted, its quote marked as not checked out
        let elsewhere = server_message_elsewhere(&pair).await;
        let text = reply(&elsewhere, a, "Ada", "not of this chat", "a reply to it");
        let id = send(&pair.a, &pair.chat, text).await;
        let read = read_back(&pair.server, "b@own.test", b, &pair.chat, &id).await;
        assert_eq!(read["properties"]["qtdMsgs"][0]["validationResult"], "Invalid");
        assert_eq!(
            read["properties"]["qtdMsgs"][0]["messageId"],
            json!(elsewhere.parse::<u64>().unwrap())
        );
        assert_eq!(read["properties"]["hasValidMsgReferences"], false);
        pair.stop().await;
    }
}

/// A message of a chat of Ada's that Bert isn't in: its id is real, but not of the pair's chat.
async fn server_message_elsewhere(pair: &Pair) -> String {
    let cora = pair.server.add_user("c@own.test", "Cora").await;
    let row = pair
        .a
        .write(Action::CreateChat {
            others: vec![Mri::new(format!("8:orgid:{cora}"))],
            topic: None,
        })
        .await
        .unwrap();
    let chat = row.message_id.unwrap();
    send(&pair.a, &chat, MessageText::plain("only for Cora").unwrap()).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_that_adds_a_mention_reads_back_as_a_post_of_it_does() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let b = pair.b_id.clone();
        let id = pair.send("about to name someone").await;
        // an odd name: not Latin, with an emoji and markup characters
        let edited = pair
            .a
            .write(Action::Edit {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
                text: mention("Бёрт 李 🚀 <&>", "now named", &format!("8:orgid:{b}")),
            })
            .await
            .unwrap();
        assert_eq!(edited.state, OutboxState::Sent, "{edited:?}");
        let read = read_back(&pair.server, "b@own.test", &b, &pair.chat, &id).await;
        assert_eq!(
            read["content"],
            "<p><span itemtype=\"http://schema.skype.com/Mention\" itemscope=\"\" itemid=\"0\">Бёрт 李 🚀 &lt;&amp;&gt;</span>\
             &nbsp;now named</p>"
        );
        let entries: Value = serde_json::from_str(read["properties"]["mentions"].as_str().unwrap()).unwrap();
        assert_eq!(entries[0]["itemid"], 0);
        assert_eq!(entries[0]["displayName"], "Бёрт 李 🚀 <&>");
        assert!(read["properties"].get("hasValidMsgReferences").is_none());
        assert!(pair.b.sync().await.unwrap().failed.is_empty());
        let stored = pair.b.message(&pair.chat, &id).await.unwrap().unwrap();
        assert_eq!(
            mentions_in(&parse_html(&stored.content).nodes),
            [("0".to_owned(), "Бёрт 李 🚀 <&>".to_owned())]
        );
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mentions_or_quotes_that_arent_a_list_of_entries_are_refused_and_plain_text_is_kept_as_sent() {
    let pair = Pair::start(Backend::Sqlite).await;
    let sender = client(&pair.server, "a@own.test", &pair.a_id).await;
    let golden = fixture(MENTION, &[("<guid:a>", &pair.a_id), ("<guid:b>", &pair.b_id)]);
    let body = || {
        let mut body = golden["request"]["body"].clone();
        body["from"] = format!("8:orgid:{}", pair.a_id).into();
        body
    };
    for (key, value) in [
        ("mentions", json!("not json")),
        ("mentions", json!("{\"itemid\":0}")),
        ("mentions", json!(["a string"])),
        ("qtdMsgs", json!("[7]")),
        ("qtdMsgs", json!(7)),
    ] {
        let mut sent = body();
        sent["properties"][key] = value;
        let response = sender
            .write(RawPost {
                chat: pair.chat.clone(),
                body: sent,
            })
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{key}");
    }
    let mut sent = body();
    sent["messagetype"] = "Text".into();
    sent["content"] = "<readonly itemtype=\"http://schema.skype.com/Mention\">kept</readonly>".into();
    sent["properties"]["mentions"] = json!([]);
    let response = sender
        .write(RawPost {
            chat: pair.chat.clone(),
            body: sent.clone(),
        })
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let id: Value = serde_json::from_slice(response.body()).unwrap();
    let read = read_back(
        &pair.server,
        "b@own.test",
        &pair.b_id,
        &pair.chat,
        &id["OriginalArrivalTime"].to_string(),
    )
    .await;
    assert_eq!(read["content"], sent["content"]);
    assert_eq!(read["properties"]["mentions"], "[]");
    pair.stop().await;
}
