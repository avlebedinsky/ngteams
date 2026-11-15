//! A group chat's people as users of three ngteams clients meet them: someone added reads what the adder chose,
//! someone removed or gone keeps the old history and gets and writes nothing more, everyone sees the roster and its
//! lines, and a post racing a removal never reaches the one removed.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use teams_core::activity::FEED;
use teams_core::engine::{Action, ChatHistory, Event, MessageChange};
use teams_proto::chat::{ChatReads, MessageText};
use teams_proto::{AddChatMembers, DeleteMessage, SharedHistory};
use teams_store::OutboxState;

use super::people::{in_chat, knows_people};
use super::support::{Rig, User};
use crate::chatsvc::activity::{mentioning, react};
use crate::chatsvc::members::RemoveMember;
use crate::support::backends;

const ADDED: &str = "ThreadActivity/AddMember";
const REMOVED: &str = "ThreadActivity/DeleteMember";

/// Waits until the push channel brought the chat's own line of `kind` since `mark`.
async fn line(user: &mut User, mark: usize, chat: &str, kind: &str) {
    user.since(mark, kind, |event| {
        matches!(event, Event::Message { conversation, message: Some(message), .. }
            if conversation == chat && message.message_type == kind)
    })
    .await;
}

/// The ids of the chat's messages the server gives `user`'s client, the newest first, with each one's type.
async fn served(user: &User, chat: &str) -> Vec<(String, String)> {
    let client = user.client();
    let page = ChatReads::new(&client).messages(chat, 200).await.unwrap();
    let typed = |message: teams_proto::chat::Message| (message.id, message.message_type.as_str().to_owned());
    page.messages.into_iter().map(typed).collect()
}

async fn served_ids(user: &User, chat: &str) -> Vec<String> {
    let served = served(user, chat).await;
    served.into_iter().map(|(id, _)| id).collect()
}

/// How many of the chat's lines of `kind` the client's store holds.
async fn lines(user: &User, chat: &str, kind: &str) -> usize {
    let page = user.engine().messages(chat, None, 200).await.unwrap();
    let of_kind = page.messages.iter().filter(|message| message.message_type == kind);
    of_kind.count()
}

/// The chat's people as the server gives them to `user`'s client now.
async fn roster(user: &User, chat: &str) -> Vec<String> {
    let client = user.client();
    let thread = ChatReads::new(&client).thread(chat).await.unwrap();
    thread.members.into_iter().map(|member| member.id).collect()
}

async fn remove(by: &User, chat: &str, who: &User) -> u16 {
    let request = RemoveMember {
        chat: chat.into(),
        member: who.mri.as_str().to_owned(),
    };
    by.client().write(request).await.unwrap().status()
}

async fn add(by: &User, chat: &str, who: &User, history: SharedHistory) -> u16 {
    let request = AddChatMembers::new(chat, vec![who.mri.clone()], history).unwrap();
    by.client().write(request).await.unwrap().status()
}

/// A group chat of a, b and d with two messages of a's that b's client shows; the chat and the two.
async fn with_two_messages(a: &mut User, b: &mut User, d: &User, topic: &str) -> (String, [String; 2]) {
    let chat = a.create(&[&*b, d], Some(topic)).await;
    let old = [a.send(&chat, "the oldest").await, a.send(&chat, "a later one").await];
    b.pushed(0, &chat, &old[1], MessageChange::New).await;
    knows_people(a, &chat, 3).await;
    (chat, old)
}

/// a adds c to `chat` with `history`: everyone's client hears the line and reads the four people from the server;
/// the adder's and the added one's clients show them at once.
async fn adds_cora(people: [&mut User; 4], chat: &str, history: ChatHistory) {
    let [a, b, c, d] = people;
    a.write(Action::AddMembers {
        conversation: chat.into(),
        members: vec![c.mri.clone()],
        history,
    })
    .await;
    let added = c.mri.as_str().to_owned();
    for user in [&mut *a, &mut *b, &mut *c, &mut *d] {
        line(user, 0, chat, ADDED).await;
        assert_eq!(lines(user, chat, ADDED).await, 1, "{}", user.upn);
        let people = roster(user, chat).await;
        assert!(people.len() == 4 && people.contains(&added), "{}: {people:?}", user.upn);
    }
    for user in [a, c] {
        shows_cora(user, chat, &added).await;
    }
}

/// The client's own people list of the chat has the four, c among them.
async fn shows_cora(user: &mut User, chat: &str, added: &str) {
    knows_people(user, chat, 4).await;
    let people = user.engine().members(chat).await.unwrap();
    assert!(people.iter().any(|member| member.mri == added), "{}", user.upn);
}

/// What c reads of the two chats it was added to: the one without history from its arrival on, the other all of it.
async fn reads_what_the_choice_allows(c: &User, bare: &(String, [String; 2]), full: &(String, [String; 2])) {
    for (chat, _) in [bare, full] {
        // c opens the chat
        c.engine().sync_conversation(chat.as_str()).await.unwrap();
    }
    for old in &bare.1 {
        assert!(
            c.engine()
                .message(bare.0.as_str(), old.as_str())
                .await
                .unwrap()
                .is_none()
        );
        assert!(!served_ids(c, &bare.0).await.contains(old));
        let client = c.client();
        let reads = ChatReads::new(&client);
        let around = reads.messages_around(&bare.0, old.parse().unwrap(), 200).await.unwrap();
        assert!(!around.messages.iter().any(|message| message.id == *old));
    }
    for old in &full.1 {
        c.shows(&full.0, old, "").await;
        assert!(served_ids(c, &full.0).await.contains(old));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn someone_added_reads_what_the_adder_chose_and_everyone_sees_the_roster_and_its_line() {
    for backend in backends() {
        let mut rig = Rig::start(backend).await;
        let mut a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let mut c = rig.user(rig.other(), "c@own.test").await;
        let mut d = rig.user(&rig.first, "d@own.test").await;
        let bare = with_two_messages(&mut a, &mut b, &d, "Without history").await;
        let full = with_two_messages(&mut a, &mut b, &d, "With history").await;
        assert_ne!(bare.0, full.0);
        adds_cora([&mut a, &mut b, &mut c, &mut d], &bare.0, ChatHistory::None).await;
        adds_cora([&mut a, &mut b, &mut c, &mut d], &full.0, ChatHistory::All).await;

        // what follows is c's to hear in both; what was before only where the adder gave it
        let mut welcomes = Vec::new();
        for chat in [&bare.0, &full.0] {
            let welcome = a.send(chat, "welcome cora").await;
            c.pushed(0, chat, &welcome, MessageChange::New).await;
            c.shows(chat, &welcome, "welcome cora").await;
            welcomes.push(welcome);
        }
        reads_what_the_choice_allows(&c, &bare, &full).await;
        let page = served(&c, &bare.0).await;
        assert_eq!(page.len(), 2, "the line that added c and what came after: {page:?}");
        assert_eq!((page[0].0.as_str(), page[1].1.as_str()), (welcomes[0].as_str(), ADDED));

        // the same after c's client and the servers restarted, and on a new device of c; a client that was running
        // when c came shows the four people once it caught up
        c.restart().await;
        reads_what_the_choice_allows(&c, &bare, &full).await;
        for user in [&mut b, &mut d] {
            user.restart().await;
            for chat in [&bare.0, &full.0] {
                shows_cora(user, chat, c.mri.as_str()).await;
            }
        }
        for user in [&mut a, &mut b, &mut c, &mut d] {
            user.close();
        }
        rig.restart().await;
        let mut third = rig.user(&rig.first, "c@own.test").await;
        third.caught_up(0).await;
        reads_what_the_choice_allows(&third, &bare, &full).await;
        assert_eq!(served(&third, &bare.0).await, page);
        knows_people(&mut third, &full.0, 4).await;
        drop((a, b, c, d, third));
        rig.stop().await;
    }
}

/// The write never reaches the chat: the client's outbox reports it failed, or the engine refuses it itself.
async fn refused(user: &User, what: &str, action: Action) {
    match user.engine().write(action).await {
        Ok(row) => {
            assert_eq!(row.state, OutboxState::Failed, "{}: {what}: {row:?}", user.upn);
            assert_eq!(row.last_error.as_deref(), Some("HTTP 403"), "{}: {what}", user.upn);
        }
        Err(error) => eprintln!("{}: {what} refused by its own client: {error}", user.upn),
    }
}

/// Everything b's client can write to the chat it was removed from is refused, and the chat is as it was.
async fn writes_nothing_more(b: &User, others: [&User; 2], chat: &str, before: &str, own: &str) {
    let [a, d] = others;
    its_client_writes_nothing(b, d, chat, before, own).await;
    the_server_refuses_it(b, [a, d], chat, own).await;
    let kept = a.engine().message(chat, own).await.unwrap().unwrap();
    assert!(kept.delete_time.is_none() && kept.edit_time.is_none(), "{kept:?}");
    assert!(a.engine().reactions(chat, before).await.unwrap().is_empty());
}

/// A post, an edit, a delete, a reaction and an add from b's client: each is refused.
async fn its_client_writes_nothing(b: &User, d: &User, chat: &str, before: &str, own: &str) {
    let conversation = || chat.to_owned();
    let text = MessageText::plain("am I still here?").unwrap();
    let post = Action::Send {
        conversation: conversation(),
        text: text.clone(),
    };
    let row = b.engine().write(post).await.unwrap();
    assert_eq!(row.state, OutboxState::Failed, "{row:?}");
    assert_eq!(row.last_error.as_deref(), Some("HTTP 403"));
    let edit = Action::Edit {
        conversation: conversation(),
        message_id: own.into(),
        text,
    };
    refused(b, "an edit of its own message", edit).await;
    let delete = Action::Delete {
        conversation: conversation(),
        message_id: own.into(),
    };
    refused(b, "a delete of its own message", delete).await;
    refused(b, "a reaction", react(chat, before, "like")).await;
    let add_d = Action::AddMembers {
        conversation: conversation(),
        members: vec![d.mri.clone()],
        history: ChatHistory::All,
    };
    refused(b, "adding someone", add_d).await;
}

/// The same requests as a client without the engine's own checks sends them: the server answers each 403.
async fn the_server_refuses_it(b: &User, others: [&User; 2], chat: &str, own: &str) {
    let [a, d] = others;
    let statuses = [
        add(b, chat, d, SharedHistory::All).await,
        add(b, chat, b, SharedHistory::All).await,
        remove(b, chat, a).await,
        b.client().write(DeleteMessage::new(chat, own)).await.unwrap().status(),
    ];
    assert_eq!(statuses, [403; 4], "an add, adding itself back, a removal, a delete");
    let client = b.client();
    let reads = ChatReads::new(&client);
    assert_eq!(reads.thread(chat).await.unwrap_err().status(), Some(403));
    assert_eq!(reads.members(chat).await.unwrap_err().status(), Some(403));
}

/// b, removed, holds and reads the chat as it was when it left, and nothing a wrote or changed afterwards.
async fn keeps_the_old_history(b: &User, chat: &str, old: &[&str], newer: &[String]) {
    for id in old {
        assert!(b.engine().message(chat, *id).await.unwrap().is_some(), "{id}");
    }
    assert_eq!(lines(b, chat, REMOVED).await, 1);
    assert!(b.engine().activity().await.unwrap().items.is_empty());
    assert!(b.engine().conversation(chat.to_owned()).await.unwrap().is_some());
    reads_nothing_newer(b, chat, newer).await;
}

async fn reads_nothing_newer(b: &User, chat: &str, newer: &[String]) {
    let page = served(b, chat).await;
    assert_eq!(page[0].1, REMOVED, "the line of its going is the last it reads");
    for id in newer {
        assert!(b.engine().message(chat, id.as_str()).await.unwrap().is_none(), "{id}");
        assert!(!page.iter().any(|(served, _)| served == id), "{id}");
    }
}

/// a removes b, then goes on writing: a mention of b, a post, a reaction to b's old message. The newer messages.
async fn removes_bert_and_goes_on(a: &mut User, b: &mut User, c: &mut User, chat: &str, own: &str) -> Vec<String> {
    assert_eq!(remove(a, chat, b).await, 200);
    for user in [&mut *a, &mut *b, &mut *c] {
        line(user, 0, chat, REMOVED).await;
    }
    for user in [&*a, &*c] {
        assert_eq!(roster(user, chat).await.len(), 2, "{}", user.upn);
    }
    b.settle().await;
    let on_b = b.mark();
    let newer = goes_on(a, c, chat, b.mri.as_str(), own).await;
    b.settle().await;
    let told = b.count(on_b, |event| in_chat(event, chat) || in_chat(event, FEED));
    assert_eq!(told, 0, "b hears nothing after its removal");
    newer
}

/// a mentions the one removed, posts, and reacts to their old message and takes it back; c's client follows.
async fn goes_on(a: &User, c: &mut User, chat: &str, gone: &str, own: &str) -> Vec<String> {
    let behind = Action::Send {
        conversation: chat.into(),
        text: mentioning(gone, "Bert", "said behind his back"),
    };
    let newer = vec![
        a.write(behind).await.message_id.unwrap(),
        a.send(chat, "more he never sees").await,
    ];
    c.pushed(0, chat, &newer[1], MessageChange::New).await;
    a.write(react(chat, own, "like")).await;
    c.pushed(0, chat, own, MessageChange::Reaction).await;
    a.write(Action::Unreact {
        conversation: chat.into(),
        message_id: own.into(),
        key: "like".into(),
    })
    .await;
    newer
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn someone_removed_keeps_the_old_history_and_gets_and_writes_nothing_more_and_someone_leaves_by_themself() {
    for backend in backends() {
        let mut rig = Rig::start(backend).await;
        let mut a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let mut c = rig.user(rig.other(), "c@own.test").await;
        let d = rig.user(&rig.first, "d@own.test").await;
        let chat = a.create(&[&b, &c], Some("Launch")).await;
        let before = a.send(&chat, "while bert was here").await;
        b.pushed(0, &chat, &before, MessageChange::New).await;
        let own = b.send(&chat, "bert's own").await;
        b.eventually_stored(&chat, &own).await;
        for user in [&mut a, &mut c] {
            user.pushed(0, &chat, &own, MessageChange::New).await;
        }
        knows_people(&mut b, &chat, 3).await;

        let newer = removes_bert_and_goes_on(&mut a, &mut b, &mut c, &chat, &own).await;
        keeps_the_old_history(&b, &chat, &[&before, &own], &newer).await;
        writes_nothing_more(&b, [&a, &d], &chat, &before, &own).await;
        b.restart().await;
        keeps_the_old_history(&b, &chat, &[&before, &own], &newer).await;

        // c leaves by itself: a hears the line, and c nothing after it
        let on_a = a.mark();
        assert_eq!(remove(&c, &chat, &c).await, 200);
        line(&mut a, on_a, &chat, REMOVED).await;
        assert_eq!(roster(&a, &chat).await, [a.mri.as_str()]);
        assert_eq!(lines(&a, &chat, REMOVED).await, 2);
        c.settle().await;
        let on_c = c.mark();
        let alone = a.send(&chat, "alone now").await;
        c.settle().await;
        assert_eq!(c.count(on_c, |event| in_chat(event, &chat)), 0);
        assert!(
            c.engine()
                .message(chat.as_str(), alone.as_str())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(remove(&c, &chat, &a).await, 403);

        // the servers restart: a new device of b gets the chat as b left it, and still writes nothing
        for user in [&mut a, &mut b, &mut c] {
            user.close();
        }
        rig.restart().await;
        let mut third = rig.user(rig.other(), "b@own.test").await;
        third.eventually_stored(&chat, &before).await;
        third.settle().await;
        let mut all = newer.clone();
        all.push(alone);
        keeps_the_old_history(&third, &chat, &[&before], &all).await;
        a.restart().await;
        knows_people(&mut a, &chat, 1).await;
        writes_nothing_more(&third, [&a, &d], &chat, &before, &own).await;
        drop((a, b, c, d, third));
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_group_started_from_a_1_1_chat_is_a_new_chat_and_the_1_1_chat_stays_its_two_peoples() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let mut a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let mut c = rig.user(rig.other(), "c@own.test").await;
        let private = a.create(&[&b], None).await;
        let secret = a.send(&private, "between ada and bert").await;
        b.pushed(0, &private, &secret, MessageChange::New).await;

        // "Start a group chat" from the 1:1 chat
        let group = a.create(&[&b, &c], None).await;
        assert_ne!(group, private);
        let hello = a.send(&group, "now with cora").await;
        for user in [&mut b, &mut c] {
            user.pushed(0, &group, &hello, MessageChange::New).await;
        }
        assert_eq!(served_ids(&c, &group).await, [hello]);
        let answer = b.send(&private, "still ours").await;
        a.pushed(0, &private, &answer, MessageChange::New).await;
        assert_eq!(served_ids(&a, &private).await, [answer, secret.clone()]);

        // c has no part in the 1:1 chat, and it takes and loses nobody
        c.settle().await;
        assert_eq!(c.count(0, |event| in_chat(event, &private)), 0);
        assert!(c.engine().conversation(private.clone()).await.unwrap().is_none());
        assert!(
            c.engine()
                .message(private.as_str(), secret.as_str())
                .await
                .unwrap()
                .is_none()
        );
        let client = c.client();
        let reads = ChatReads::new(&client);
        assert_eq!(reads.messages(&private, 200).await.unwrap_err().status(), Some(403));
        assert_eq!(add(&a, &private, &c, SharedHistory::All).await, 400);
        assert_eq!(remove(&a, &private, &b).await, 400);
        assert_eq!(remove(&a, &private, &a).await, 400);
        let client = a.client();
        let thread = ChatReads::new(&client).thread(&private).await.unwrap();
        assert_eq!(thread.members.len(), 2);
        drop((a, b, c));
        rig.stop().await;
    }
}

/// How many posts a makes before the removal starts, and after it answered.
const AROUND: usize = 8;
const ROUNDS: usize = 3;

/// a posts in a loop while c removes b; the posts' ids, the last [`AROUND`] of them sent after the removal answered.
async fn posts_while_bert_is_removed(a: &User, b: &User, c: &User, chat: &str, round: usize) -> Vec<String> {
    let (posted, removed) = (AtomicUsize::new(0), AtomicBool::new(false));
    let posting = async {
        let (mut ids, mut tail) = (Vec::new(), 0);
        while tail < AROUND {
            if removed.load(Ordering::SeqCst) {
                tail += 1;
            }
            ids.push(a.send(chat, &format!("race {round}, post {}", ids.len())).await);
            posted.fetch_add(1, Ordering::SeqCst);
        }
        ids
    };
    let removing = async {
        while posted.load(Ordering::SeqCst) < AROUND {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(remove(c, chat, b).await, 200);
        removed.store(true, Ordering::SeqCst);
    };
    tokio::join!(posting, removing).0
}

/// Whether b's client was told of the chat's message `id` since `mark`, or holds it.
async fn got(b: &User, mark: usize, chat: &str, id: &str) -> bool {
    let told = b.count(
        mark,
        |event| matches!(event, Event::Message { conversation, id: of, .. } if conversation == chat && of == id),
    );
    told > 0 || b.engine().message(chat, id).await.unwrap().is_some()
}

/// One round; how many posts were made before the removal and how many after it.
async fn race(a: &User, b: &mut User, c: &mut User, chat: &str, round: usize) -> (usize, usize) {
    let on_b = b.mark();
    let ids = posts_while_bert_is_removed(a, b, c, chat, round).await;
    line(b, on_b, chat, REMOVED).await;
    c.pushed(0, chat, &ids[ids.len() - 1], MessageChange::New).await;
    b.settle().await;
    let made = nothing_made_after_the_removal_is_berts(b, on_b, chat, &ids).await;
    assert_eq!(add(a, chat, b, SharedHistory::All).await, 200);
    line(b, on_b, chat, ADDED).await;
    made
}

/// What b reads now is all it ever got: every post made before the line of its going, and none made after it.
async fn nothing_made_after_the_removal_is_berts(b: &User, on_b: usize, chat: &str, ids: &[String]) -> (usize, usize) {
    let page = served(b, chat).await;
    assert_eq!(page[0].1, REMOVED, "b reads nothing after the line of its going");
    let gone: u64 = page[0].0.parse().unwrap();
    let reads = |id: &String| page.iter().any(|(served, _)| served == id);
    let (mut before, mut after) = (0, 0);
    for (n, id) in ids.iter().enumerate() {
        let made_after = id.parse::<u64>().unwrap() > gone;
        assert!(
            made_after || n < ids.len() - AROUND,
            "post {n} was sent after the removal"
        );
        if made_after {
            after += 1;
            assert!(!got(b, on_b, chat, id).await, "b got post {n}, made after its removal");
        } else {
            before += 1;
        }
        assert_eq!(reads(id), !made_after, "post {n}");
    }
    (before, after)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn someone_removed_while_another_posts_in_a_loop_never_receives_a_post_made_after_the_removal() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let mut c = rig.user(rig.other(), "c@own.test").await;
        let chat = a.create(&[&b, &c], Some("Launch")).await;
        let first = a.send(&chat, "before the race").await;
        for user in [&mut b, &mut c] {
            user.pushed(0, &chat, &first, MessageChange::New).await;
        }
        let (mut before, mut after) = (0, 0);
        for round in 0..ROUNDS {
            let (made_before, made_after) = race(&a, &mut b, &mut c, &chat, round).await;
            before += made_before;
            after += made_after;
        }
        eprintln!("{backend:?}: {before} posts made before b's removal, {after} after it");
        assert!(before >= ROUNDS * AROUND && after >= ROUNDS * AROUND);
        drop((a, b, c));
        rig.stop().await;
    }
}
