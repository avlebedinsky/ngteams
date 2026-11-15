//! Rich messages as people on three ngteams clients meet them on the real server: mentions (of a person, of
//! everyone, many in one message, names in odd scripts) read back as Teams gives them, live and at a later sign-in;
//! edits that add, move and take away a mention and keep the compose time; quoted replies checked against the chat;
//! channel posts and replies with both; malformed mentions and quotes refused with nothing stored or pushed; a group
//! of sixty synthetic people with odd names. Every account, name and text is synthetic. JSON is compared as parsed
//! values.

use serde_json::{Value, json};
use teams_core::activity::{ActivityKind, FEED, Mention};
use teams_core::engine::{Action, MessageChange};
use teams_proto::chat::{ChatReads, ComposeError, Marks, MessageText, Node, Quote, RichText, parse_html};
use teams_proto::{Endpoint, Mri, NewChannelKind, Read, Service, reply_chain_id};

use super::channels::{channel, team};
use super::people::in_chat;
use super::support::{Rig, User, options};
use crate::chatsvc::activity::RawPost;
use crate::support::{PASSWORD, backends};

const MENTION_TYPE: &str = "http://schema.skype.com/Mention";
const MENTION: &str = include_str!("../../../../../../tests/fixtures/chatsvc/mention.json");

/// A piece of a paragraph: words, or a mention of an MRI shown as a name.
enum Part<'a> {
    Words(&'a str),
    Named(&'a str, &'a str),
}

use Part::{Named, Words};

fn words(text: &str) -> Node {
    Node::Text {
        text: text.into(),
        marks: Marks::NONE,
    }
}

/// What a reply quotes: the message, its sender's MRI and the text the replier's box previews.
struct Quoted<'a> {
    id: &'a str,
    sender: &'a str,
    preview: &'a str,
}

/// The rich text of the paragraph `parts`, after `quotes`, with its mention table, the mentions numbered from 0 as
/// the compose box numbers them.
fn rich(quotes: &[Quoted], parts: &[Part]) -> (RichText, Vec<(String, Mri)>) {
    let mut nodes: Vec<Node> = quotes
        .iter()
        .map(|quoted| {
            Node::Quote(Quote {
                message_id: Some(quoted.id.into()),
                sender: Some(quoted.sender.into()),
                sender_name: Some("QA Operator".into()),
                time: Some(quoted.id.into()),
                body: vec![Node::Paragraph(vec![words(quoted.preview)])],
                forward: false,
            })
        })
        .collect();
    let mut table = Vec::new();
    let mut paragraph = Vec::new();
    for part in parts {
        match part {
            Words(text) => paragraph.push(words(text)),
            Named(name, mri) => {
                let item = table.len().to_string();
                paragraph.push(Node::Mention {
                    item_id: item.clone(),
                    text: (*name).into(),
                });
                table.push((item, Mri::new(*mri)));
            }
        }
    }
    nodes.push(Node::Paragraph(paragraph));
    (
        RichText {
            nodes,
            truncated: false,
        },
        table,
    )
}

fn compose(quotes: &[Quoted], parts: &[Part]) -> MessageText {
    let (text, table) = rich(quotes, parts);
    MessageText::new(text, table).unwrap()
}

async fn post(user: &User, chat: &str, text: MessageText) -> String {
    let row = user
        .write(Action::Send {
            conversation: chat.into(),
            text,
        })
        .await;
    row.message_id.unwrap()
}

async fn edit(user: &User, chat: &str, id: &str, text: MessageText) {
    user.write(Action::Edit {
        conversation: chat.into(),
        message_id: id.into(),
        text,
    })
    .await;
}

/// The newest messages of a conversation, raw, as the engine's catch-up asks for them.
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

async fn page(user: &User, chat: &str) -> Vec<Value> {
    let response = user.client().read(&Page { chat: chat.into() }).await.unwrap();
    assert_eq!(response.status(), 200, "{}", user.upn);
    let page: Value = serde_json::from_slice(response.body()).unwrap();
    page["messages"].as_array().unwrap().clone()
}

/// Message `id` of `chat` as `user` reads it from the server, raw.
async fn read(user: &User, chat: &str, id: &str) -> Value {
    let messages = page(user, chat).await;
    let found = messages.iter().find(|message| message["id"] == id).cloned();
    found.unwrap_or_else(|| panic!("{}: {id} not in {chat}", user.upn))
}

/// The entries of a message's `properties.mentions`, a JSON string as Teams gives it.
fn mention_entries(message: &Value) -> Vec<Value> {
    let text = message["properties"]["mentions"].as_str();
    let text = text.unwrap_or_else(|| panic!("mentions not a string: {message}"));
    serde_json::from_str(text).unwrap()
}

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

fn numbered(named: &[(&str, &str)]) -> Vec<(String, String)> {
    named
        .iter()
        .enumerate()
        .map(|(index, (name, _))| (index.to_string(), (*name).to_owned()))
        .collect()
}

/// A message as Teams gives it back: no compose wrapper, each mention a span numbered in order, its entry numbered
/// the same and naming its MRI and name.
fn in_teams_form(message: &Value, named: &[(&str, &str)]) {
    let content = message["content"].as_str().unwrap();
    assert!(!content.contains("<readonly"), "{content}");
    let entries = mention_entries(message);
    assert_eq!(entries.len(), named.len(), "{entries:?}");
    for (index, ((name, mri), entry)) in named.iter().zip(&entries).enumerate() {
        assert_eq!(entry["@type"], MENTION_TYPE, "{entry}");
        assert_eq!(entry["itemid"], json!(index), "{entry}");
        assert_eq!(entry["mri"], *mri, "{entry}");
        assert_eq!(entry["displayName"], *name, "{entry}");
        let span = format!("<span itemtype=\"{MENTION_TYPE}\" itemscope=\"\" itemid=\"{index}\">");
        assert!(content.contains(&span), "{span} not in {content}");
    }
    assert_eq!(mentions_in(&parse_html(content).nodes), numbered(named));
}

/// The client stores message `id` with the mentions `named`, as its view parses them.
async fn stores_mentions(user: &User, chat: &str, id: &str, named: &[(&str, &str)]) {
    let stored = user.engine().message(chat, id).await.unwrap();
    let stored = stored.unwrap_or_else(|| panic!("{}: {id} not stored", user.upn));
    assert_eq!(
        mentions_in(&parse_html(&stored.content).nodes),
        numbered(named),
        "{}",
        user.upn
    );
}

/// The one item about message `id` that the client's feed gets.
async fn told_of(user: &mut User, id: &str) -> ActivityKind {
    let about = |feed: &teams_core::activity::ActivityFeed| {
        let items = feed.items.iter().filter(|item| item.message.as_deref() == Some(id));
        items.cloned().collect::<Vec<_>>()
    };
    let what = format!("an item about {id}");
    let feed = user.feed(&what, |feed| !about(feed).is_empty()).await;
    let items = about(&feed);
    assert_eq!(items.len(), 1, "{}: {items:#?}", user.upn);
    items[0].kind.clone()
}

/// After a quiet stretch the client's feed holds no item about message `id`.
async fn untold(user: &mut User, id: &str) {
    user.settle().await;
    let feed = user.engine().activity().await.unwrap();
    let found = feed.items.iter().any(|item| item.message.as_deref() == Some(id));
    assert!(!found, "{}: an item about {id}: {feed:#?}", user.upn);
}

fn ms(time: &Value) -> i64 {
    match time {
        Value::Number(number) => number.as_i64().unwrap(),
        Value::String(text) => chrono::DateTime::parse_from_rfc3339(text)
            .unwrap_or_else(|_| panic!("not a time: {text}"))
            .timestamp_millis(),
        other => panic!("not a time: {other}"),
    }
}

/// a mentions b in their 1:1 chat, and c, who isn't in it; b's running client gets b's mention live. The chat, the
/// mention of b and the one of c.
async fn one_to_one(a: &User, b: &mut User, bm: &str, cm: &str) -> (String, String, String) {
    // a 1:1 chat: b's running client gets the mention live; c, not in the chat, is named but told nothing
    let chat = a.create(&[&*b], None).await;
    let one = post(
        a,
        &chat,
        compose(&[], &[Named("Bert", bm), Words("\u{a0}a made-up mention")]),
    )
    .await;
    let outside = post(a, &chat, compose(&[], &[Named("Cora", cm), Words(" behind her back")])).await;
    b.pushed(0, &chat, &one, MessageChange::New).await;
    assert_eq!(told_of(b, &one).await, ActivityKind::Mention(Mention::You));
    stores_mentions(b, &chat, &one, &[("Bert", bm)]).await;
    for user in [a, &*b] {
        in_teams_form(&read(user, &chat, &one).await, &[("Bert", bm)]);
    }
    // the client refuses a mention of everyone in a 1:1 chat before anything is sent
    let (text, table) = rich(&[], &[Named("Everyone", &chat), Words(" all two of us")]);
    assert!(matches!(
        MessageText::new(text, table),
        Err(ComposeError::NotAPerson(_))
    ));
    (chat, one, outside)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mentions_read_back_as_teams_gives_them_live_and_on_a_later_sign_in() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let mut a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        // c is signed in, but ngteams isn't running: it starts after the messages are written
        let mut c = User::signed_in(rig.other(), &rig.origin, "c@own.test", options()).await;
        let mris = (a.mri.to_string(), b.mri.to_string(), c.mri.to_string());
        let (bm, cm) = (mris.1.as_str(), mris.2.as_str());
        let (chat, one, outside) = one_to_one(&a, &mut b, bm, cm).await;

        // a group chat: a member's mention with markup characters and odd scripts, everyone, and many in one message
        let group = a.create(&[&b, &c], Some("Ünïcödé ✨ <group> & \"co\"")).await;
        let odd = "Бёрт \"B\" <b>bold</b> & 李 🚀";
        let named = post(&a, &group, compose(&[], &[Named(odd, bm), Words(" look")])).await;
        let all = post(
            &a,
            &group,
            compose(&[], &[Named("Everyone", group.as_str()), Words(" standup")]),
        )
        .await;
        let many_names: Vec<(String, &str)> = (0..12)
            .map(|index| match index % 3 {
                0 => (format!("كورا {index}"), cm),
                1 => (format!("बर्ट {index}"), bm),
                _ => (format!("Cora 👩🏽‍🚀 {index}"), cm),
            })
            .collect();
        let mut parts = Vec::new();
        for (name, mri) in &many_names {
            parts.push(Named(name, mri));
            parts.push(Words(", "));
        }
        parts.push(Words("all of you"));
        let many = post(&a, &group, compose(&[], &parts)).await;
        let crowd: Vec<(&str, &str)> = many_names.iter().map(|(name, mri)| (name.as_str(), *mri)).collect();

        for id in [&named, &all, &many] {
            b.pushed(0, &group, id, MessageChange::New).await;
        }
        assert_eq!(told_of(&mut b, &named).await, ActivityKind::Mention(Mention::You));
        assert_eq!(told_of(&mut b, &all).await, ActivityKind::Mention(Mention::Everyone));
        assert_eq!(told_of(&mut b, &many).await, ActivityKind::Mention(Mention::You));
        stores_mentions(&b, &group, &named, &[(odd, bm)]).await;
        stores_mentions(&b, &group, &many, &crowd).await;

        // c starts ngteams: its catch-up brings the messages and its feed the items that are hers
        c.start();
        c.online(0).await;
        c.caught_up(0).await;
        assert_eq!(told_of(&mut c, &all).await, ActivityKind::Mention(Mention::Everyone));
        assert_eq!(told_of(&mut c, &many).await, ActivityKind::Mention(Mention::You));
        untold(&mut c, &named).await;
        untold(&mut c, &outside).await;
        assert!(c.engine().message(&chat, &outside).await.unwrap().is_none());
        stores_mentions(&c, &group, &named, &[(odd, bm)]).await;
        stores_mentions(&c, &group, &all, &[("Everyone", group.as_str())]).await;
        stores_mentions(&c, &group, &many, &crowd).await;

        for user in [&a, &b, &c] {
            in_teams_form(&read(user, &group, &named).await, &[(odd, bm)]);
            let everyone = read(user, &group, &all).await;
            in_teams_form(&everyone, &[("Everyone", group.as_str())]);
            assert_eq!(mention_entries(&everyone)[0]["mentionType"], "everyone");
            in_teams_form(&read(user, &group, &many).await, &crowd);
        }
        // nobody is told of their own message
        for id in [&one, &named, &all, &many] {
            untold(&mut a, id).await;
        }
        assert!(c.engine().conversation(chat.clone()).await.unwrap().is_none());
        rig.stop().await;
    }
}

/// Message `id` as `user` reads it after an edit: mentioning `named`, its compose time, arrival and client message
/// id those of the message `before` the edits.
async fn edited(user: &User, chat: &str, id: &str, before: &Value, named: &[(&str, &str)]) {
    let after = read(user, chat, id).await;
    in_teams_form(&after, named);
    let composed = ms(&before["composetime"]);
    assert_eq!(ms(&after["composetime"]), composed, "{after}");
    assert_eq!(ms(&after["properties"]["composetime"]), composed, "{after}");
    assert_eq!(after["originalarrivaltime"], before["originalarrivaltime"]);
    assert_eq!(after["clientmessageid"], before["clientmessageid"]);
    assert!(ms(&after["properties"]["edittime"]) >= composed, "{after}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_adds_moves_and_takes_away_a_mention_and_keeps_the_compose_time() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let mut c = rig.user(rig.other(), "c@own.test").await;
        let mris = (b.mri.to_string(), c.mri.to_string());
        let (bm, cm) = (mris.0.as_str(), mris.1.as_str());
        let group = a.create(&[&b, &c], Some("Valves")).await;
        let id = a.send(&group, "who checks the valves?").await;
        b.pushed(0, &group, &id, MessageChange::New).await;
        let before = read(&b, &group, &id).await;
        assert_eq!(mention_entries(&before), Vec::<Value>::new());
        let composed = ms(&before["composetime"]);

        // the edit adds a mention of b: b is told, c isn't; everyone reads the mention
        let (on_b, on_c) = (b.mark(), c.mark());
        let to_b = [("Бёрт 🚰", bm)];
        edit(
            &a,
            &group,
            &id,
            compose(&[], &[Named(to_b[0].0, bm), Words(" checks the valves")]),
        )
        .await;
        b.pushed(on_b, &group, &id, MessageChange::Edit).await;
        c.pushed(on_c, &group, &id, MessageChange::Edit).await;
        assert_eq!(told_of(&mut b, &id).await, ActivityKind::Mention(Mention::You));
        untold(&mut c, &id).await;
        for user in [&a, &b, &c] {
            edited(user, &group, &id, &before, &to_b).await;
        }
        stores_mentions(&b, &group, &id, &to_b).await;
        stores_mentions(&c, &group, &id, &to_b).await;

        // an edit moves it to c: c is told and b's item goes
        let (on_b, on_c) = (b.mark(), c.mark());
        let to_c = [("Кора", cm)];
        edit(
            &a,
            &group,
            &id,
            compose(&[], &[Named(to_c[0].0, cm), Words(" checks them now")]),
        )
        .await;
        b.pushed(on_b, &group, &id, MessageChange::Edit).await;
        c.pushed(on_c, &group, &id, MessageChange::Edit).await;
        assert_eq!(told_of(&mut c, &id).await, ActivityKind::Mention(Mention::You));
        b.feed("the moved mention's item gone", |feed| {
            !feed
                .items
                .iter()
                .any(|item| item.message.as_deref() == Some(id.as_str()))
        })
        .await;
        for user in [&a, &b, &c] {
            edited(user, &group, &id, &before, &to_c).await;
        }

        // and an edit takes it away: nobody is mentioned, no item stays
        let on_c = c.mark();
        edit(&a, &group, &id, MessageText::plain("nobody checks the valves").unwrap()).await;
        c.pushed(on_c, &group, &id, MessageChange::Edit).await;
        c.feed("the taken mention's item gone", |feed| {
            !feed
                .items
                .iter()
                .any(|item| item.message.as_deref() == Some(id.as_str()))
        })
        .await;
        untold(&mut b, &id).await;
        for user in [&a, &b, &c] {
            edited(user, &group, &id, &before, &[]).await;
        }
        stores_mentions(&c, &group, &id, &[]).await;
        let stored = c.engine().message(&group, &id).await.unwrap().unwrap();
        assert_eq!(stored.compose_time, Some(composed));
        rig.stop().await;
    }
}

/// The client's stored message `id` holds a quote of `quoted` in its text, and its properties' quote list.
async fn stores_quote(user: &User, chat: &str, id: &str, quoted: &[&str], valid: &[&str]) {
    let stored = user.engine().message(chat, id).await.unwrap();
    let stored = stored.unwrap_or_else(|| panic!("{}: {id} not stored", user.upn));
    let in_text: Vec<String> = parse_html(&stored.content)
        .nodes
        .into_iter()
        .filter_map(|node| match node {
            Node::Quote(quote) => quote.message_id,
            _ => None,
        })
        .collect();
    assert_eq!(in_text, quoted, "{}", user.upn);
    let properties: Value = serde_json::from_str(stored.properties.as_deref().unwrap()).unwrap();
    let results: Vec<&Value> = properties["qtdMsgs"]
        .as_array()
        .unwrap_or_else(|| panic!("{}: {properties}", user.upn))
        .iter()
        .map(|entry| &entry["validationResult"])
        .collect();
    assert_eq!(results, valid, "{}", user.upn);
}

/// A reply's quote list as read back, each entry `messageId` a number and `message` null, judged as `valid`.
fn quotes_read_back(message: &Value, quoted: &[(&str, &str)], valid: &[&str]) {
    let entries = message["properties"]["qtdMsgs"].as_array();
    let entries = entries.unwrap_or_else(|| panic!("qtdMsgs not a list: {message}"));
    assert_eq!(entries.len(), quoted.len(), "{message}");
    for ((entry, (id, sender)), result) in entries.iter().zip(quoted).zip(valid) {
        let id: u64 = id.parse().unwrap();
        assert_eq!(entry["messageId"], json!(id), "{entry}");
        assert_eq!(entry["sender"], *sender, "{entry}");
        assert!(!entry["time"].is_null(), "{entry}");
        assert_eq!(entry["validationResult"], *result, "{entry}");
        for empty in ["message", "sharedRefId", "replyChainId"] {
            assert_eq!(entry[empty], Value::Null, "{entry}");
        }
    }
    let all = !valid.is_empty() && valid.iter().all(|result| *result == "Valid");
    assert_eq!(message["properties"]["hasValidMsgReferences"], all, "{message}");
}

/// b quotes a's message `quoted` of the group: every client stores the quote and reads it back as valid.
async fn quote_of_the_chat(a: &mut User, b: &User, c: &mut User, group: &str, quoted: &str) {
    let am = &a.mri.to_string();
    let of_a = Quoted {
        id: quoted,
        sender: am,
        preview: "the fuel is loaded",
    };
    let reply = post(b, group, compose(&[of_a], &[Words("great, go")])).await;
    for user in [&mut *a, &mut *c] {
        user.pushed(0, group, &reply, MessageChange::New).await;
    }
    for user in [&*a, b, &*c] {
        quotes_read_back(&read(user, group, &reply).await, &[(quoted, am)], &["Valid"]);
    }
    for user in [&*a, &*c] {
        stores_quote(user, group, &reply, &[quoted], &["Valid"]).await;
    }
}

/// a deletes a message of the group, then b posts a quote of it: the deleted message and b's reply.
async fn quote_of_a_deleted_message(a: &User, b: &mut User, group: &str) -> (String, String) {
    let gone = a.send(group, "a message about to go").await;
    b.pushed(0, group, &gone, MessageChange::New).await;
    a.write(Action::Delete {
        conversation: group.into(),
        message_id: gone.clone(),
    })
    .await;
    b.pushed(0, group, &gone, MessageChange::Delete).await;
    let deleted = Quoted {
        id: &gone,
        sender: a.mri.as_str(),
        preview: "a message about to go",
    };
    let late = post(b, group, compose(&[deleted], &[Words("too late")])).await;
    (gone, late)
}

/// The quote of message `gone` (its id and sender) in reply `late` as `user` reads it, whichever way it is judged.
async fn quote_judged(user: &User, group: &str, late: &str, gone: (&str, &str)) -> Value {
    let message = read(user, group, late).await;
    let entry = message["properties"]["qtdMsgs"][0].clone();
    let judged = entry["validationResult"].as_str().unwrap();
    quotes_read_back(&message, &[gone], &[judged]);
    entry
}

/// The client's stored messages `ids` hold `secret` neither in their text nor in their properties.
async fn stores_nothing_of(user: &User, chat: &str, ids: &[&str], secret: &str) {
    for id in ids {
        let stored = user.engine().message(chat, *id).await.unwrap().unwrap();
        assert!(!stored.content.contains(secret), "{}", user.upn);
        let properties = stored.properties.unwrap_or_default();
        assert!(!properties.contains(secret), "{}", user.upn);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quoted_reply_reads_back_on_every_client_and_a_quote_from_elsewhere_or_of_a_deleted_message_is_not_valid() {
    let mut of_deleted = Vec::new();
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let mut a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let mut c = rig.user(rig.other(), "c@own.test").await;
        let mri = a.mri.to_string();
        let am = mri.as_str();
        let group = a.create(&[&b, &c], Some("Quotes")).await;
        let quoted = a.send(&group, "the fuel is loaded").await;
        b.pushed(0, &group, &quoted, MessageChange::New).await;

        quote_of_the_chat(&mut a, &b, &mut c, &group, &quoted).await;

        // a message of a chat b isn't in: b posts a quote of its id, which reads back invalid and tells nothing of it
        let secret = "the made-up launch code is 0000-SYNTHETIC";
        let private = a.create(&[&c], None).await;
        let hidden = a.send(&private, secret).await;
        let elsewhere = Quoted {
            id: &hidden,
            sender: am,
            preview: "a guess",
        };
        let forged = post(&b, &group, compose(&[elsewhere], &[Words("what is this?")])).await;
        let (gone, late) = quote_of_a_deleted_message(&a, &mut b, &group).await;
        // and both kinds beside a valid quote
        let mixed = [
            Quoted {
                id: &quoted,
                sender: am,
                preview: "the fuel is loaded",
            },
            Quoted {
                id: &hidden,
                sender: am,
                preview: "a guess",
            },
        ];
        let both = post(&b, &group, compose(&mixed, &[Words("both")])).await;

        for id in [&forged, &late, &both] {
            for user in [&mut a, &mut c] {
                user.pushed(0, &group, id, MessageChange::New).await;
            }
        }
        for user in [&a, &b, &c] {
            let read_forged = read(user, &group, &forged).await;
            quotes_read_back(&read_forged, &[(hidden.as_str(), am)], &["Invalid"]);
            let read_both = read(user, &group, &both).await;
            quotes_read_back(
                &read_both,
                &[(quoted.as_str(), am), (hidden.as_str(), am)],
                &["Valid", "Invalid"],
            );
            for message in [&read_forged, &read_both] {
                assert!(
                    !message.to_string().contains("0000-SYNTHETIC"),
                    "{}: {message}",
                    user.upn
                );
            }
        }
        for user in [&a, &c] {
            stores_nothing_of(user, &group, &[&forged, &both], "0000-SYNTHETIC").await;
        }
        assert!(b.engine().message(&private, &hidden).await.unwrap().is_none());
        // the deleted message's quote is checked after every backend ran: its failure is reported apart
        for user in [&a, &b, &c] {
            let entry = quote_judged(user, &group, &late, (&gone, am)).await;
            of_deleted.push((backend, user.upn.clone(), entry));
        }
        rig.stop().await;
    }
    for (backend, upn, entry) in &of_deleted {
        eprintln!("{backend:?} {upn}: a quote of a deleted message: {entry}");
    }
    for (backend, upn, entry) in of_deleted {
        assert_eq!(
            entry["validationResult"], "Invalid",
            "{backend:?} {upn}: a quote of a deleted message: {entry}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channels_posts_and_replies_carry_their_mentions_and_quotes_to_the_teams_people() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let mut a = rig.team_user(&rig.first, "a@own.test").await;
        let mut b = rig.team_user(rig.other(), "b@own.test").await;
        let mut c = rig.team_user(rig.other(), "c@own.test").await;
        let mris = (a.mri.to_string(), b.mri.to_string());
        let (am, bm) = (mris.0.as_str(), mris.1.as_str());
        let (team, group) = team(&a, &[&b, &c]).await;
        let (launch, _) = channel(&a, &team, &group, "Launch 🚀", NewChannelKind::Standard).await;

        // a's post names b
        let named = [("Bert Ünïcode", bm)];
        let root = post(
            &a,
            &launch,
            compose(&[], &[Named(named[0].0, bm), Words(" fuel check, please")]),
        )
        .await;
        for user in [&mut b, &mut c] {
            user.pushed(0, &launch, &root, MessageChange::New).await;
        }
        assert_eq!(told_of(&mut b, &root).await, ActivityKind::Mention(Mention::You));
        untold(&mut c, &root).await;
        for user in [&a, &b, &c] {
            in_teams_form(&read(user, &launch, &root).await, &named);
        }
        stores_mentions(&c, &launch, &root, &named).await;

        // c's reply in its chain quotes the post and names a
        let chain = reply_chain_id(&launch, &root).unwrap();
        let of_post = Quoted {
            id: &root,
            sender: am,
            preview: "Bert Ünïcode fuel check, please",
        };
        let back = [("Ада", am)];
        let reply = post(
            &c,
            &chain,
            compose(&[of_post], &[Named(back[0].0, am), Words(" fuel checked")]),
        )
        .await;
        for user in [&mut a, &mut b] {
            user.pushed(0, &launch, &reply, MessageChange::New).await;
        }
        let kind = told_of(&mut a, &reply).await;
        assert_eq!(kind, ActivityKind::Mention(Mention::You));
        for user in [&a, &b, &c] {
            let message = read(user, &launch, &reply).await;
            in_teams_form(&message, &back);
            quotes_read_back(&message, &[(root.as_str(), am)], &["Valid"]);
        }
        for user in [&a, &b] {
            stores_mentions(user, &launch, &reply, &back).await;
            stores_quote(user, &launch, &reply, &[root.as_str()], &["Valid"]).await;
        }
        rig.stop().await;
    }
}

/// A post by `from` with the captured mention body (`mention.json`), `change` made to it.
fn raw_post(from: &User, to: &User, chat: &str, change: impl FnOnce(&mut Value)) -> RawPost {
    let source = MENTION
        .replace("19:<guid:a>_<guid:b>@unq.gbl.spaces", chat)
        .replace("<guid:a>", from.oid())
        .replace("<guid:b>", to.oid());
    let fixture: Value = serde_json::from_str(&source).unwrap();
    let mut body = fixture["request"]["body"].clone();
    body["from"] = from.mri.as_str().into();
    body["conversationid"] = chat.into();
    body["clientmessageid"] = uuid::Uuid::new_v4().as_u128().to_string()[..19].into();
    change(&mut body);
    RawPost {
        chat: chat.into(),
        body,
    }
}

/// Mention and quote lists of the right form but naming nobody and no message are posted: nobody is told, the
/// quotes are invalid.
async fn wrong_ids_are_posted_and_tell_nobody(a: &User, b: &mut User, c: &mut User, group: &str) {
    // well-formed lists with wrong ids are posted: a mention of nobody tells nobody, quotes of no message are
    // invalid
    let nobody = "8:orgid:00000000-0000-4000-8000-000000000000";
    let request = raw_post(a, b, group, |body| {
        body["content"] = "<p>a made-up post with wrong ids</p>".into();
        body["properties"]["mentions"] = json!([{"@type": MENTION_TYPE, "itemid": "41", "mri": nobody,
            "mentionType": "person", "displayName": "Nobody"}])
        .to_string()
        .into();
        body["properties"]["qtdMsgs"] = json!([{"messageId": "abc"}, {"messageId": "999999"}])
            .to_string()
            .into();
    });
    let response = a.client().write(request).await.unwrap();
    eprintln!("wrong ids: {}", response.status());
    assert_eq!(response.status(), 201);
    let posted: Value = serde_json::from_slice(response.body()).unwrap();
    let wrong = posted["OriginalArrivalTime"].to_string();
    for user in [&mut *b, &mut *c] {
        user.pushed(0, group, &wrong, MessageChange::New).await;
        untold(user, &wrong).await;
    }
    let message = read(c, group, &wrong).await;
    assert_eq!(mention_entries(&message)[0]["itemid"], 41);
    let results: Vec<&Value> = message["properties"]["qtdMsgs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| &entry["validationResult"])
        .collect();
    assert_eq!(results, ["Invalid", "Invalid"], "{message}");
    assert_eq!(message["properties"]["hasValidMsgReferences"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_mentions_and_quotes_are_refused_nothing_is_stored_and_nobody_is_told() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let mut c = rig.user(rig.other(), "c@own.test").await;
        let group = a.create(&[&b, &c], Some("Malformed")).await;
        let first = a.send(&group, "the last good message").await;
        for user in [&mut b, &mut c] {
            user.pushed(0, &group, &first, MessageChange::New).await;
            user.settle().await;
        }
        let (on_b, on_c) = (b.mark(), c.mark());
        let stored_before = page(&b, &group).await.len();
        let mention = |count: usize| {
            let entry = json!({"@type": MENTION_TYPE, "itemid": "0", "mri": b.mri.as_str(), "mentionType": "person",
                "displayName": "Bert"});
            Value::Array(vec![entry; count])
        };
        let huge = mention(2000);
        let cases: Vec<(&str, Value)> = vec![
            ("mentions", json!("not json")),
            ("mentions", json!("{\"itemid\":0}")),
            ("mentions", json!({"itemid": 0})),
            ("mentions", json!(["a string"])),
            ("mentions", json!([[]])),
            ("mentions", json!("[1,2]")),
            ("mentions", json!(7)),
            ("mentions", json!(true)),
            ("mentions", Value::String(huge.to_string())),
            ("mentions", huge),
            ("qtdMsgs", json!("[7]")),
            ("qtdMsgs", json!(7)),
            ("qtdMsgs", json!("not json")),
            ("qtdMsgs", json!({"messageId": first})),
            ("qtdMsgs", json!(["x"])),
        ];
        let sender = a.client();
        for (key, value) in cases {
            let text = value.to_string();
            let (shown, oversized) = (text.chars().take(60).collect::<String>(), text.len() > 64 * 1024);
            let request = raw_post(&a, &b, &group, |body| body["properties"][key] = value);
            let response = sender.write(request).await.unwrap();
            let status = response.status();
            eprintln!("malformed {key} = {shown} ({} bytes): {status}", text.len());
            assert!(
                status == 400 || (oversized && status == 413),
                "{key} = {shown}: {status}"
            );
        }
        for (user, mark) in [(&mut b, on_b), (&mut c, on_c)] {
            user.settle().await;
            assert_eq!(user.count(mark, |event| in_chat(event, &group)), 0, "{}", user.upn);
            assert_eq!(user.count(mark, |event| in_chat(event, FEED)), 0, "{}", user.upn);
        }
        assert_eq!(page(&b, &group).await.len(), stored_before);

        wrong_ids_are_posted_and_tell_nobody(&a, &mut b, &mut c, &group).await;
        rig.stop().await;
    }
}

/// Names that test more than a Latin one: scripts, direction marks, emoji, markup and quoting characters.
const ODD_NAMES: [&str; 14] = [
    "سارة الحربي",
    "Zoë O'Brien-Ñúñez",
    "李小龍",
    "Ἀλέξανδρος",
    "👩🏽‍🚀 Astro Naut",
    "<script>alert(1)</script>",
    "Tom \"Quotes\" & Jerry",
    "Ｆｕｌｌｗｉｄｔｈ",
    "Z̷a̷l̷g̷o̷",
    "\u{202e}desrever",
    "zero\u{200b}width",
    "हिन्दी नाम",
    "<readonly itemtype=\"http://schema.skype.com/Mention\">x</readonly>",
    "a very long synthetic display name that keeps going on and on past what any card would show, and then some more",
];

fn odd_name(index: usize) -> String {
    ODD_NAMES.get(index).map_or_else(
        || {
            let scripts = ["Ølaf", "Ŋoro", "Ýmir", "Çağla", "Ħalil", "Ægir", "Đorđe"];
            format!("Synthetic {index} {}", scripts[index % scripts.len()])
        },
        |name| (*name).to_owned(),
    )
}

/// Sixty synthetic people with odd names, added with the operator's command line; their UPNs and MRIs.
fn sixty(rig: &Rig) -> Vec<(String, String, String)> {
    (0..60)
        .map(|index| {
            let upn = format!("p{index:02}@own.test");
            let name = odd_name(index);
            let output = rig
                .first
                .cli_output(&["user", "add", &upn, "--name", &name], &format!("{PASSWORD}\n"));
            let said = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "user add {name:?}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            let id = said.trim().strip_prefix(&format!("added {upn} (")).unwrap();
            (upn, name, format!("8:orgid:{}", id.trim_end_matches(')')))
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_group_of_sixty_people_with_odd_names_reads_its_many_mentions_back() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let people = sixty(&rig);
        let a = rig.user(&rig.first, "a@own.test").await;
        // three of them run ngteams: two are named, the last one is only in everyone
        let mut readers = Vec::new();
        for index in [0, 13, 59] {
            let user = rig.user(rig.other(), &people[index].0).await;
            assert_eq!(user.mri.as_str(), people[index].2);
            readers.push(user);
        }
        let row = a
            .write(Action::CreateChat {
                others: people.iter().map(|(_, _, mri)| Mri::new(mri)).collect(),
                topic: Some("Sixty 🎉 <odd> & \"names\"".into()),
            })
            .await;
        let group = row.message_id.unwrap();
        let members = ChatReads::new(&a.client()).members(&group).await.unwrap().members;
        assert_eq!(members.len(), 61);

        let named: Vec<(&str, &str)> = people[..59]
            .iter()
            .map(|(_, name, mri)| (name.as_str(), mri.as_str()))
            .collect();
        let mut parts = vec![Words("roll call: ")];
        for (name, mri) in &named {
            parts.push(Named(name, mri));
            parts.push(Words(" "));
        }
        let call = post(&a, &group, compose(&[], &parts)).await;
        let all = post(
            &a,
            &group,
            compose(&[], &[Named("Everyone", group.as_str()), Words(" thanks, all sixty")]),
        )
        .await;

        for (index, reader) in readers.iter_mut().enumerate() {
            reader.pushed(0, &group, &call, MessageChange::New).await;
            reader.pushed(0, &group, &all, MessageChange::New).await;
            in_teams_form(&read(reader, &group, &call).await, &named);
            in_teams_form(&read(reader, &group, &all).await, &[("Everyone", group.as_str())]);
            stores_mentions(reader, &group, &call, &named).await;
            assert_eq!(told_of(reader, &all).await, ActivityKind::Mention(Mention::Everyone));
            if index < 2 {
                assert_eq!(told_of(reader, &call).await, ActivityKind::Mention(Mention::You));
            } else {
                untold(reader, &call).await;
            }
        }
        rig.stop().await;
    }
}
