//! A group chat's people: adding with as much history as the adder gives, removing and leaving, and what each change
//! lets the people it touches read and hear.

use std::time::Duration;

use serde_json::{Value, json};
use teams_core::engine::{Action, ChatHistory, EngineHandle};
use teams_proto::chat::{ChatEvent, ChatReads, Field, MessageText, chat_event};
use teams_proto::{AddChatMembers, Endpoint, Method, Mri, Request, Service, SharedHistory};
use teams_store::OutboxState;
use tempfile::TempDir;

use super::activity::{FEED, RawPost, Trio, feed, mentioning, mri, react, send};
use super::messages::Pair;
use super::{ChatClient, client, engine};
use crate::support::{Backend, TestServer, backends};

/// One more account with its engine and a typed client.
struct Guest {
    id: String,
    engine: EngineHandle,
    client: ChatClient,
    _dir: TempDir,
}

impl Guest {
    async fn new(server: &TestServer, name: &str) -> Self {
        let upn = format!("{}@own.test", name.to_lowercase());
        let id = server.add_user(&upn, name).await;
        let dir = tempfile::tempdir().unwrap();
        Self {
            engine: engine(server, &upn, &id, dir.path()).await,
            client: client(server, &upn, &id).await,
            id,
            _dir: dir,
        }
    }

    fn mri(&self) -> String {
        format!("8:orgid:{}", self.id)
    }

    /// The ids of the messages the account's client reads in `chat`, the newest first.
    async fn reads(&self, chat: &str) -> Vec<String> {
        let page = ChatReads::new(&self.client).messages(chat, 200).await.unwrap();
        page.messages.into_iter().map(|message| message.id).collect()
    }
}

pub(crate) struct RemoveMember {
    pub chat: String,
    pub member: String,
}

impl Request for RemoveMember {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn method(&self) -> Method {
        Method::Delete
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "threads", &self.chat, "members", &self.member])
    }
    fn body(&self) -> Option<Value> {
        None
    }
    fn is_write(&self) -> bool {
        true
    }
}

fn plain(words: &str) -> MessageText {
    MessageText::plain(words).unwrap()
}

async fn add(engine: &EngineHandle, chat: &str, who: &Guest, history: ChatHistory) {
    let row = engine
        .write(Action::AddMembers {
            conversation: chat.into(),
            members: vec![mri(&who.id)],
            history,
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
}

async fn roster(client: &ChatClient, chat: &str) -> (Vec<String>, u64) {
    let thread = ChatReads::new(client).thread(chat).await.unwrap();
    let Field::Known(version) = thread.roster_version else {
        panic!("no roster version: {thread:?}");
    };
    (thread.members.into_iter().map(|member| member.id).collect(), version)
}

#[tokio::test(flavor = "multi_thread")]
async fn someone_added_without_history_reads_from_their_arrival_on_and_nothing_older_by_any_read() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let server = &trio.pair.server;
        let a = &trio.pair.a;
        let group = trio.group.as_str();
        let own = client(server, "a@own.test", &trio.pair.a_id).await;
        let dan = Guest::new(server, "Dan").await;
        let first = send(a, group, plain("the oldest")).await;
        let second = send(a, group, plain("a later one")).await;
        assert!(a.sync().await.unwrap().failed.is_empty());
        let (before, roster_before) = roster(&own, group).await;
        assert_eq!(before.len(), 3);

        // "Don't include chat history"
        add(a, group, &dan, ChatHistory::None).await;
        let (people, roster_after) = roster(&own, group).await;
        assert_eq!(people.len(), 4);
        assert!(people.contains(&dan.mri()));
        assert!(roster_after > roster_before);
        let stored = a.members(group).await.unwrap();
        assert!(stored.iter().any(|member| member.mri == dan.mri()), "{stored:?}");
        let third = send(a, group, plain("after dan came")).await;

        assert!(dan.engine.sync().await.unwrap().failed.is_empty());
        assert!(dan.engine.message(group, &first).await.unwrap().is_none());
        assert!(dan.engine.message(group, &second).await.unwrap().is_none());
        assert!(dan.engine.message(group, &third).await.unwrap().is_some());
        let reads = ChatReads::new(&dan.client);
        let page = reads.messages(group, 200).await.unwrap();
        assert_eq!(page.messages.len(), 2, "the event that added dan and what came after");
        assert_eq!(page.messages[0].id, third);
        let event = page.messages[1].thread_activity().unwrap();
        assert_eq!(event.kind, "addmember");
        let initiator = format!("8:orgid:{}", trio.pair.a_id);
        assert_eq!(event.initiator.as_deref(), Some(initiator.as_str()));
        assert_eq!(event.targets, [dan.mri()]);
        assert_eq!(event.roster_version, Field::Known(roster_after));

        // no other way round to what dan wasn't given
        let around = reads.messages_around(group, first.parse().unwrap(), 200).await.unwrap();
        let old = |id: &String| *id == first || *id == second;
        assert!(!around.messages.iter().any(|message| old(&message.id)));
        let liked = dan.engine.write(react(group, &first, "like")).await;
        assert!(liked.is_err() || liked.unwrap().state != OutboxState::Sent);
        let listed = reads.conversations().await.unwrap();
        let entry = listed.conversations.iter().find(|entry| entry.id == group).unwrap();
        assert_eq!(entry.last_message().unwrap().id, third);
        dan.engine.shutdown();
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn someone_added_with_all_history_or_from_a_time_reads_that_much_and_the_others_hear_of_each_arrival() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let server = &trio.pair.server;
        let (a, b) = (&trio.pair.a, &trio.pair.b);
        let group = trio.group.as_str();
        let own = client(server, "a@own.test", &trio.pair.a_id).await;
        let all = Guest::new(server, "Eve").await;
        let since = Guest::new(server, "Fay").await;
        let first = send(a, group, plain("the oldest")).await;
        let second = send(a, group, plain("a later one")).await;
        assert!(a.sync().await.unwrap().failed.is_empty());

        // "Include all chat history"
        add(a, group, &all, ChatHistory::All).await;
        assert!(all.engine.sync().await.unwrap().failed.is_empty());
        assert!(all.engine.message(group, &first).await.unwrap().is_some());
        let read = all.reads(group).await;
        assert!(read.contains(&first) && read.contains(&second));
        let page = ChatReads::new(&all.client).messages(group, 1).await.unwrap();
        let event = page.messages[0].thread_activity().unwrap();
        assert_eq!(event.share_history_time, Field::Known(0));

        // history from a time on: the second message's
        let from = second.parse().unwrap();
        let request = AddChatMembers::new(group, vec![mri(&since.id)], SharedHistory::Since(from)).unwrap();
        assert_eq!(own.write(request).await.unwrap().status(), 200);
        let read = since.reads(group).await;
        assert!(!read.contains(&first), "{read:?}");
        assert!(read.contains(&second));

        // the people already there hear of each new member
        assert!(b.sync().await.unwrap().failed.is_empty());
        let stored = b.messages(group, None, 50).await.unwrap();
        let events = stored.messages.iter();
        let added = events.filter(|message| message.message_type == "ThreadActivity/AddMember");
        assert_eq!(added.count(), 2);
        assert_eq!(roster(&own, group).await.0.len(), 5);
        all.engine.shutdown();
        since.engine.shutdown();
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_group_chats_own_people_add_to_it_and_only_accounts_that_can_sign_in_are_added() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let server = &trio.pair.server;
        let group = trio.group.as_str();
        let own = client(server, "a@own.test", &trio.pair.a_id).await;
        let outsider = Guest::new(server, "Dan").await;
        let wanted = Guest::new(server, "Eve").await;
        let adding =
            |chat: &str, who: &str| AddChatMembers::new(chat, vec![Mri::new(who)], SharedHistory::All).unwrap();
        let status = async |client: &ChatClient, request: AddChatMembers| client.write(request).await.unwrap().status();

        assert_eq!(status(&outsider.client, adding(group, &outsider.mri())).await, 403);
        assert_eq!(status(&outsider.client, adding(group, &wanted.mri())).await, 403);
        assert_eq!(
            status(&own, adding(&trio.pair.chat, &wanted.mri())).await,
            400,
            "a 1:1 chat takes nobody"
        );
        assert_eq!(status(&own, adding("48:notes", &wanted.mri())).await, 404);
        let nobody = format!("8:orgid:{}", uuid::Uuid::new_v4());
        assert_eq!(status(&own, adding(group, &nobody)).await, 400);
        assert_eq!(status(&own, adding(group, "8:live:someone")).await, 400);
        ngteams_server::admin::set_disabled(&server.app.db, server.app.fanout.as_ref(), "eve@own.test", true)
            .await
            .unwrap();
        assert_eq!(
            status(&own, adding(group, &wanted.mri())).await,
            400,
            "a disabled account"
        );
        let (people, version) = roster(&own, group).await;
        assert_eq!(people.len(), 3);
        // someone already there is no change
        assert_eq!(
            status(&own, adding(group, &format!("8:orgid:{}", trio.c_id))).await,
            200
        );
        assert_eq!(roster(&own, group).await, (people, version));
        assert_eq!(
            ChatReads::new(&own).messages(group, 200).await.unwrap().messages.len(),
            0
        );
        outsider.engine.shutdown();
        wanted.engine.shutdown();
        trio.stop().await;
    }
}

/// The trio's three typed clients and MRIs, a's first.
async fn seats(trio: &Trio) -> ([ChatClient; 3], [String; 3]) {
    let server = &trio.pair.server;
    let ids = [&trio.pair.a_id, &trio.pair.b_id, &trio.c_id];
    let clients = [
        client(server, "a@own.test", ids[0]).await,
        client(server, "b@own.test", ids[1]).await,
        client(server, "c@own.test", ids[2]).await,
    ];
    (clients, ids.map(|id| format!("8:orgid:{id}")))
}

fn removal(chat: &str, member: &str) -> RemoveMember {
    RemoveMember {
        chat: chat.into(),
        member: member.into(),
    }
}

/// The removal a thread's event tells: who did it and whom it took out.
fn removed(event: &teams_proto::chat::Message) -> teams_proto::chat::Removed {
    assert_eq!(event.message_type.as_str(), "ThreadActivity/DeleteMember");
    match chat_event(event.message_type.as_str(), &event.content) {
        Some(ChatEvent::MembersRemoved(removed)) => removed,
        other => panic!("not a removal: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn someone_removed_from_a_group_chat_keeps_what_they_could_read_and_writes_and_hears_nothing_more() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let (a, c) = (&trio.pair.a, &trio.c);
        let group = trio.group.as_str();
        let ([own, _, cora], [a_mri, b_mri, c_mri]) = seats(&trio).await;
        let before = send(a, group, plain("while cora was here")).await;
        assert!(c.sync().await.unwrap().failed.is_empty());
        let fanout = &trio.pair.server.app.fanout;
        let mut heard = fanout.subscribe(&format!("push:{c_mri}")).await.unwrap();
        let (_, roster_before) = roster(&own, group).await;

        let bad = removal(group, "8:orgid:not-an-id");
        assert_eq!(cora.write(bad).await.unwrap().status(), 400);
        assert_eq!(own.write(removal(group, &c_mri)).await.unwrap().status(), 200);
        let (people, roster_after) = roster(&own, group).await;
        assert_eq!(people, [a_mri.clone(), b_mri]);
        assert!(roster_after > roster_before);
        let after = send(a, group, mentioning(&c_mri, "Cora", "said behind her back")).await;

        // cora reads what she could, and the event that took her out; nothing newer, and writes nothing
        let reads = ChatReads::new(&cora);
        let page = reads.messages(group, 200).await.unwrap();
        assert_eq!(page.messages.len(), 2);
        assert_eq!(page.messages[1].id, before);
        let gone = &page.messages[0];
        let event = removed(gone);
        assert_eq!(event.initiator, a_mri);
        assert_eq!(event.targets, std::slice::from_ref(&c_mri));
        assert_eq!(reads.thread(group).await.unwrap_err().status(), Some(403));
        assert_eq!(reads.members(group).await.unwrap_err().status(), Some(403));
        let post = Action::Send {
            conversation: group.into(),
            text: plain("am I still here?"),
        };
        let post = c.write(post).await.unwrap();
        assert_eq!(post.state, OutboxState::Failed, "{post:?}");
        assert_eq!(post.last_error.as_deref(), Some("HTTP 403"));
        assert_eq!(cora.write(removal(group, &a_mri)).await.unwrap().status(), 403);

        // her client's catch-up brings the chat once more, as it was when she left, and no item of the later mention
        let report = c.sync().await.unwrap();
        assert!(report.failed.is_empty(), "{report:?}");
        assert!(c.message(group, &gone.id).await.unwrap().is_some());
        assert!(c.message(group, &after).await.unwrap().is_none());
        assert!(feed(c).await.items.is_empty());
        let listed = reads.conversations().await.unwrap();
        let entry = listed.conversations.iter().find(|entry| entry.id == group).unwrap();
        assert_eq!(entry.last_message().unwrap().id, gone.id);
        send(a, group, plain("more she never sees")).await;
        let later = reads.conversations_from(listed.sync_state().unwrap()).await.unwrap();
        assert!(
            later.conversations.is_empty(),
            "a chat she left never moves for her again"
        );

        // what her push channel was told: her removal, and nothing after it
        let mut told = Vec::new();
        while let Ok(Ok(event)) = tokio::time::timeout(Duration::from_millis(300), heard.recv()).await {
            let event: Value = serde_json::from_slice(&event).unwrap();
            told.push(event["resource"]["id"].as_str().unwrap_or_default().to_owned());
        }
        assert_eq!(told, std::slice::from_ref(&gone.id));
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_later_change_is_not_a_leavers_to_read_and_someone_added_back_reads_from_then_on() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let (a, b, c) = (&trio.pair.a, &trio.pair.b, &trio.c);
        let group = trio.group.as_str();
        let ([own, bert, cora], [a_mri, b_mri, c_mri]) = seats(&trio).await;
        let before = send(a, group, plain("while cora was here")).await;
        assert!(c.sync().await.unwrap().failed.is_empty());
        assert_eq!(own.write(removal(group, &c_mri)).await.unwrap().status(), 200);
        let reads = ChatReads::new(&cora);
        assert_eq!(reads.messages(group, 200).await.unwrap().messages.len(), 2);

        // a message changed after she left is no longer hers to read: she gets no later wording or reaction
        sent_reaction(b, group, &before).await;
        let page = reads.messages(group, 200).await.unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(removed(&page.messages[0]).targets, std::slice::from_ref(&c_mri));
        assert!(c.sync().await.unwrap().failed.is_empty());
        assert!(c.reactions(group, &before).await.unwrap().is_empty());

        // bert leaves: the same for him, and the chat goes on for ada
        assert_eq!(bert.write(removal(group, &b_mri)).await.unwrap().status(), 200);
        assert_eq!(roster(&own, group).await.0, std::slice::from_ref(&a_mri));
        let page = ChatReads::new(&bert).messages(group, 1).await.unwrap();
        let left = removed(&page.messages[0]);
        assert_eq!(left.initiator, b_mri);
        assert!(left.targets.is_empty(), "someone who leaves removes nobody else");
        let again = own.write(removal(group, &b_mri)).await.unwrap();
        assert_eq!(again.status(), 404, "he is gone already");

        // added back without history, cora reads from then on
        let back = AddChatMembers::new(group, vec![Mri::new(&c_mri)], SharedHistory::None).unwrap();
        assert_eq!(own.write(back).await.unwrap().status(), 200);
        let welcome = send(a, group, plain("welcome back")).await;
        let page = reads.messages(group, 200).await.unwrap();
        assert_eq!(page.messages.len(), 2);
        assert_eq!(page.messages[0].id, welcome);
        assert!(reads.thread(group).await.is_ok());
        trio.stop().await;
    }
}

async fn sent_reaction(engine: &EngineHandle, chat: &str, message: &str) {
    assert!(engine.sync().await.unwrap().failed.is_empty());
    let row = engine.write(react(chat, message, "like")).await.unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_1_1_chat_keeps_its_two_people_and_a_group_started_from_it_is_a_new_chat_without_its_history() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let cora = Guest::new(&pair.server, "Cora").await;
        let own = client(&pair.server, "a@own.test", &pair.a_id).await;
        let private = pair.send("between ada and bert").await;
        let b_mri = format!("8:orgid:{}", pair.b_id);

        let remove = RemoveMember {
            chat: pair.chat.clone(),
            member: b_mri.clone(),
        };
        assert_eq!(own.write(remove).await.unwrap().status(), 400);
        let leave = RemoveMember {
            chat: pair.chat.clone(),
            member: format!("8:orgid:{}", pair.a_id),
        };
        assert_eq!(own.write(leave).await.unwrap().status(), 400);
        assert_eq!(roster(&own, &pair.chat).await.0.len(), 2);

        // "Start a group chat" from the 1:1 chat: a chat of the three, made with its first message
        let row = pair
            .a
            .write(Action::CreateChat {
                others: vec![mri(&pair.b_id), mri(&cora.id)],
                topic: None,
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        let group = row.message_id.unwrap();
        assert_ne!(group, pair.chat);
        let hello = send(&pair.a, &group, plain("now with cora")).await;
        assert_eq!(cora.reads(&group).await, [hello]);
        assert_eq!(
            ChatReads::new(&cora.client)
                .messages(&pair.chat, 200)
                .await
                .unwrap_err()
                .status(),
            Some(403),
            "the 1:1 chat stays ada's and bert's"
        );
        let reader = client(&pair.server, "b@own.test", &pair.b_id).await;
        let kept = ChatReads::new(&reader).messages(&pair.chat, 200).await.unwrap();
        assert_eq!(kept.messages.len(), 1);
        assert_eq!(kept.messages[0].id, private);
        cora.engine.shutdown();
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_teams_channel_takes_and_loses_people_through_its_team_alone() {
    let server = crate::teams::server(Backend::Sqlite).await;
    let ada = crate::teams::Seat::new(&server, "ada@own.test", "Ada").await;
    let bert = crate::teams::Seat::new(&server, "bert@own.test", "Bert").await;
    let cora = crate::teams::Seat::new(&server, "cora@own.test", "Cora").await;
    let (team, group) = ada.make_team("Rocket", false).await;
    ada.add(&team, &group, &[&bert]).await;
    let add = AddChatMembers::new(team.as_str(), vec![Mri::new(&cora.mri)], SharedHistory::All).unwrap();
    assert_eq!(ada.client.write(add).await.unwrap().status(), 400);
    let remove = RemoveMember {
        chat: team.clone(),
        member: bert.mri.clone(),
    };
    assert_eq!(ada.client.write(remove).await.unwrap().status(), 400);
    assert_eq!(
        ChatReads::new(&*ada.client).members(&team).await.unwrap().members.len(),
        2
    );
    for seat in [ada, bert, cora] {
        seat.stop();
    }
    server.stop().await;
}

/// How many rounds the race below runs on each database.
const ROUNDS: usize = 20;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_racing_a_removal_tells_nobody_who_was_out_of_the_chat_when_it_was_written() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let server = &trio.pair.server;
        let group = trio.group.as_str();
        let c_mri = format!("8:orgid:{}", trio.c_id);
        let own = client(server, "a@own.test", &trio.pair.a_id).await;
        let bert = client(server, "b@own.test", &trio.pair.b_id).await;
        let cora = client(server, "c@own.test", &trio.c_id).await;
        let mentions = json!([{"@type": "http://schema.skype.com/Mention", "itemid": "0", "mri": c_mri,
            "mentionType": "person", "displayName": "Cora"}])
        .to_string();
        let (mut before, mut after) = (0, 0);
        for round in 0..ROUNDS {
            let mut heard = server.app.fanout.subscribe(&format!("push:{c_mri}")).await.unwrap();
            let post = RawPost {
                chat: group.into(),
                body: json!({
                    "messagetype": "RichText/Html", "contenttype": "Text", "clientmessageid": format!("9{round}"),
                    "content": "<p><span itemtype=\"http://schema.skype.com/Mention\" itemscope itemid=\"0\">Cora</span> now</p>",
                    "properties": {"mentions": mentions},
                }),
            };
            let remove = RemoveMember {
                chat: group.into(),
                member: c_mri.clone(),
            };
            let (posted, removed) = tokio::join!(bert.write(post), own.write(remove));
            let posted = posted.unwrap();
            assert_eq!(posted.status(), 201);
            assert_eq!(removed.unwrap().status(), 200);
            let answer: Value = serde_json::from_slice(posted.body()).unwrap();
            let id = answer["OriginalArrivalTime"].to_string();

            // whether the message was written while cora was in the chat is what she may read of it now
            let page = ChatReads::new(&cora).messages(group, 200).await.unwrap();
            let hers = page.messages.iter().any(|message| message.id == id);
            let mut told = false;
            while let Ok(Ok(event)) = tokio::time::timeout(Duration::from_millis(150), heard.recv()).await {
                let event: Value = serde_json::from_slice(&event).unwrap();
                let about = &event["resource"]["properties"]["activity"]["sourceMessageId"];
                told |= event["resource"]["id"] == id.as_str()
                    || about.as_i64().is_some_and(|about| about.to_string() == id);
            }
            let items = ChatReads::new(&cora).messages(FEED, 200).await;
            let in_feed = items.is_ok_and(|page| {
                let about =
                    |item: &teams_proto::chat::Message| item.properties["activity"]["sourceMessageId"].to_string();
                page.messages.iter().any(|item| about(item) == id)
            });
            assert_eq!(told, hers, "round {round}: the push follows what she may read");
            assert_eq!(in_feed, hers, "round {round}: so does her feed");
            if hers {
                before += 1;
            } else {
                after += 1;
            }
            let back = AddChatMembers::new(group, vec![Mri::new(&c_mri)], SharedHistory::All).unwrap();
            assert_eq!(own.write(back).await.unwrap().status(), 200);
        }
        eprintln!("{backend:?}: written before the removal {before} times, after it {after} times");
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_racing_a_removal_gives_nothing_written_after_it() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let group = trio.group.as_str();
        let ([own, bert, cora], [_, _, c_mri]) = seats(&trio).await;
        let reads = ChatReads::new(&cora);
        let ids = |page: teams_proto::chat::MessagePage| -> Vec<String> {
            page.messages.into_iter().map(|message| message.id).collect()
        };
        for round in 0..ROUNDS {
            let post = RawPost {
                chat: group.into(),
                body: json!({"messagetype": "Text", "content": "behind her back",
                    "clientmessageid": format!("8{round}")}),
            };
            let go = async {
                assert_eq!(own.write(removal(group, &c_mri)).await.unwrap().status(), 200);
                assert_eq!(bert.write(post).await.unwrap().status(), 201);
            };
            let (page, around, ()) = tokio::join!(reads.messages(group, 200), reads.messages_around(group, 1, 200), go);

            // what she may read now is all she could at any moment of the race
            let hers = ids(reads.messages(group, 200).await.unwrap());
            for (read, racing) in [("page", ids(page.unwrap())), ("around", ids(around.unwrap()))] {
                let leaked: Vec<_> = racing.iter().filter(|id| !hers.contains(id)).collect();
                assert!(leaked.is_empty(), "round {round}, {read}: {leaked:?}");
            }
            let back = AddChatMembers::new(group, vec![Mri::new(&c_mri)], SharedHistory::All).unwrap();
            assert_eq!(own.write(back).await.unwrap().status(), 200);
        }
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reaction_taken_back_after_its_messages_sender_left_changes_and_tells_them_nothing() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let (a, b, c) = (&trio.pair.a, &trio.pair.b, &trio.c);
        let group = trio.group.as_str();
        let ([_, bert, cora], [_, _, c_mri]) = seats(&trio).await;
        let id = send(c, group, plain("before I go")).await;
        sent_reaction(b, group, &id).await;
        let before = ChatReads::new(&cora).messages(FEED, 200).await.unwrap();
        assert_eq!(before.messages.len(), 1);
        assert_eq!(bert.write(removal(group, &c_mri)).await.unwrap().status(), 200);
        let fanout = &trio.pair.server.app.fanout;
        let mut heard = fanout.subscribe(&format!("push:{c_mri}")).await.unwrap();

        let back = Action::Unreact {
            conversation: group.into(),
            message_id: id.clone(),
            key: "like".into(),
        };
        let row = b.write(back).await.unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        assert!(a.sync().await.unwrap().failed.is_empty());
        assert!(a.reactions(group, &id).await.unwrap().is_empty());
        let told = tokio::time::timeout(Duration::from_millis(300), heard.recv()).await;
        assert!(told.is_err(), "{told:?}");
        let after = ChatReads::new(&cora).messages(FEED, 200).await.unwrap();
        assert_eq!(after.messages, before.messages, "her feed is as it was when she left");
        trio.stop().await;
    }
}

struct RawAdd {
    chat: String,
    body: Value,
}

impl Request for RawAdd {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn method(&self) -> Method {
        Method::Put
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "threads", &self.chat, "members"])
    }
    fn body(&self) -> Option<Value> {
        Some(self.body.clone())
    }
    fn is_write(&self) -> bool {
        true
    }
}

struct RawMark {
    item: String,
    body: Value,
}

impl Request for RawMark {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn method(&self) -> Method {
        Method::Put
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new([
            "v1",
            "users",
            "ME",
            "conversations",
            FEED,
            "messages",
            &self.item,
            "properties",
        ])
        .with_query("name", "isread")
    }
    fn body(&self) -> Option<Value> {
        Some(self.body.clone())
    }
    fn is_write(&self) -> bool {
        true
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_roster_and_feed_requests_are_refused_and_a_threads_own_event_is_nobodys_to_change() {
    let trio = Trio::start(Backend::Sqlite).await;
    let server = &trio.pair.server;
    let group = trio.group.as_str();
    let own = client(server, "a@own.test", &trio.pair.a_id).await;
    let guest = Guest::new(server, "Dan").await;
    let member = |role: &str| json!({"id": guest.mri(), "role": role, "shareHistoryTime": 0});
    for body in [
        json!({}),
        json!({"members": []}),
        json!({"members": [{"id": guest.mri()}]}),
        json!({"members": [member("User")]}),
        json!({"members": [member("Admin"), member("Admin")]}),
        json!([member("Admin")]),
    ] {
        let request = RawAdd {
            chat: group.into(),
            body: body.clone(),
        };
        assert_eq!(own.write(request).await.unwrap().status(), 400, "{body}");
    }
    assert_eq!(roster(&own, group).await.0.len(), 3);

    // the thread's own event: no edit, no delete, no reaction, by the one who caused it either
    add(&trio.pair.a, group, &guest, ChatHistory::All).await;
    assert!(trio.pair.a.sync().await.unwrap().failed.is_empty());
    let page = ChatReads::new(&own).messages(group, 1).await.unwrap();
    let event = page.messages[0].id.clone();
    let liked = trio.pair.a.write(react(group, &event, "like")).await;
    assert!(liked.is_err() || liked.unwrap().state != OutboxState::Sent);
    let delete = teams_proto::DeleteMessage::new(group, event.as_str());
    assert_eq!(own.write(delete).await.unwrap().status(), 403);
    let after = ChatReads::new(&own).messages(group, 1).await.unwrap();
    assert_eq!(after.messages[0].content, page.messages[0].content);

    // a feed item's mark takes `true` or `false`; a removed item takes none
    send(&trio.pair.a, group, mentioning(&guest.mri(), "Dan", "hello")).await;
    let item = feed(&guest.engine).await.items[0].id.clone();
    let mark = |body: Value| RawMark {
        item: item.clone(),
        body,
    };
    for body in [json!({}), json!({"isread": "maybe"}), json!({"isread": 1})] {
        assert_eq!(guest.client.write(mark(body)).await.unwrap().status(), 400);
    }
    assert_eq!(
        guest
            .client
            .write(mark(json!({"isread": true})))
            .await
            .unwrap()
            .status(),
        200
    );
    let kept = feed(&guest.engine).await;
    assert!(kept.is_read(&kept.items[0]));
    let missing = RawMark {
        item: "12345".into(),
        body: json!({"isread": "true"}),
    };
    assert_eq!(guest.client.write(missing).await.unwrap().status(), 404);
    let remove = teams_proto::DeleteMessage::new(FEED, item.as_str());
    assert_eq!(guest.client.write(remove).await.unwrap().status(), 200);
    assert_eq!(
        guest
            .client
            .write(mark(json!({"isread": "true"})))
            .await
            .unwrap()
            .status(),
        400
    );
    guest.engine.shutdown();
    trio.stop().await;
}
