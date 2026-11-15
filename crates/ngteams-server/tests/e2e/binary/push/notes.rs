//! The account's own notes as a user of several ngteams clients meets them: written, edited and deleted on one
//! client, live on the account's other client, kept over client and server restarts, and nobody else's.

use teams_core::engine::{Action, Event, MessageChange};
use teams_core::sync::NOTES;
use teams_proto::DeleteMessage;
use teams_proto::chat::{ChatReads, MessageText};

use super::support::{Rig, User};
use crate::support::backends;

fn note(text: &str) -> Action {
    Action::Send {
        conversation: NOTES.into(),
        text: MessageText::plain(text).unwrap(),
    }
}

fn about_notes(event: &Event) -> bool {
    matches!(event, Event::Message { conversation, .. } if conversation == NOTES)
}

/// The notes the server gives `user`'s client, the newest first: each one's id and text.
async fn served(user: &User) -> Vec<(String, String)> {
    let client = user.client();
    let page = ChatReads::new(&client).messages(NOTES, 200).await.unwrap();
    page.messages
        .into_iter()
        .map(|message| (message.id, message.content))
        .collect()
}

/// The client holds a's two notes as a left them: the first edited, the second deleted.
async fn holds_the_notes(user: &User, kept: &str, dropped: &str) {
    user.shows(NOTES, kept, "buy oat milk").await;
    let gone = user.engine().message(NOTES, dropped).await.unwrap().unwrap();
    assert!(gone.delete_time.is_some(), "{}: {gone:?}", user.upn);
    assert!(gone.content.is_empty(), "{}: {gone:?}", user.upn);
}

/// `other` was told nothing of a's notes, holds none of them, and reads none by any name or id.
async fn reads_none_of_them(other: &mut User, owner: &User, ids: &[&str]) {
    other.settle().await;
    assert_eq!(other.count(0, about_notes), 0, "{}", other.upn);
    let names = [
        format!("{NOTES}:{}", owner.oid()),
        format!("{NOTES}:{}", owner.mri.as_str()),
        format!("{NOTES}:{}", owner.upn),
    ];
    reads_none_by_name(other, &names).await;
    for id in ids {
        assert!(other.engine().message(NOTES, *id).await.unwrap().is_none());
        reaches_none_by_id(other, &names, id).await;
    }
}

/// Its own notes are none yet, a made-up name of the owner's stream is nothing, and no list has a stream.
async fn reads_none_by_name(other: &User, names: &[String]) {
    let client = other.client();
    let reads = ChatReads::new(&client);
    assert_eq!(reads.messages(NOTES, 200).await.unwrap_err().status(), Some(404));
    for name in names {
        let page = reads.messages(name, 200).await;
        assert_eq!(page.unwrap_err().status(), Some(404), "{}: {name}", other.upn);
        let thread = reads.thread(name).await;
        assert_eq!(thread.unwrap_err().status(), Some(404), "{}: {name}", other.upn);
    }
    let listed = reads.conversations().await.unwrap();
    assert!(listed.conversations.iter().all(|entry| !entry.id.starts_with("48:")));
}

/// A note's id reads and deletes nothing from another account, under its own notes' name or a made-up one.
async fn reaches_none_by_id(other: &User, names: &[String], id: &str) {
    let client = other.client();
    let reads = ChatReads::new(&client);
    let around = reads.messages_around(NOTES, id.parse().unwrap(), 200).await;
    assert_eq!(around.unwrap_err().status(), Some(404), "{}: around {id}", other.upn);
    for name in names.iter().map(String::as_str).chain([NOTES]) {
        let delete = DeleteMessage::new(name, id);
        let status = client.write(delete).await.unwrap().status();
        assert_eq!(status, 404, "{}: delete {id} in {name}", other.upn);
    }
}

/// a writes two notes, edits the first and deletes the second; its other client follows live. The two ids.
async fn writes_edits_and_deletes(a: &mut User, a2: &mut User) -> (String, String) {
    let client = a.client();
    let none = ChatReads::new(&client).messages(NOTES, 200).await;
    assert_eq!(none.unwrap_err().status(), Some(404), "no notes until the first one");

    let milk = a.write(note("buy milk")).await.message_id.unwrap();
    let bank = a.write(note("call the bank")).await.message_id.unwrap();
    for id in [&milk, &bank] {
        a2.pushed(0, NOTES, id, MessageChange::New).await;
        a.eventually_stored(NOTES, id).await;
    }
    a2.shows(NOTES, &milk, "buy milk").await;
    a2.shows(NOTES, &bank, "call the bank").await;

    a.write(Action::Edit {
        conversation: NOTES.into(),
        message_id: milk.clone(),
        text: MessageText::plain("buy oat milk").unwrap(),
    })
    .await;
    a2.pushed(0, NOTES, &milk, MessageChange::Edit).await;
    a.write(Action::Delete {
        conversation: NOTES.into(),
        message_id: bank.clone(),
    })
    .await;
    a2.pushed(0, NOTES, &bank, MessageChange::Delete).await;
    holds_the_notes(a2, &milk, &bank).await;
    (milk, bank)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_accounts_notes_follow_it_to_its_other_client_survive_restarts_and_are_never_anyone_elses() {
    for backend in backends() {
        let mut rig = Rig::start(backend).await;
        let mut a = rig.user(&rig.first, "a@own.test").await;
        let mut a2 = rig.user(rig.other(), "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let mut c = rig.user(rig.other(), "c@own.test").await;
        let (milk, bank) = writes_edits_and_deletes(&mut a, &mut a2).await;

        // b and c never see them, whatever they name; b's own notes are b's alone
        for other in [&mut b, &mut c] {
            reads_none_of_them(other, &a, &[&milk, &bank]).await;
        }
        let on_a2 = a2.mark();
        let theirs = b.write(note("b's own note")).await.message_id.unwrap();
        let listed = served(&b).await;
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert!(listed[0].0 == theirs && listed[0].1.contains("b's own note"));
        let delete = DeleteMessage::new(NOTES, milk.as_str());
        assert_eq!(b.client().write(delete).await.unwrap().status(), 404);
        a2.settle().await;
        assert_eq!(a2.count(on_a2, about_notes), 0, "a hears nothing of b's notes");
        let mine = served(&a).await;
        assert_eq!(mine.len(), 2, "{mine:?}");
        assert!(mine.iter().all(|(id, _)| *id != theirs));

        // a's client quits and starts again; then every server node does
        a.restart().await;
        holds_the_notes(&a, &milk, &bank).await;
        for user in [&mut a, &mut a2, &mut b, &mut c] {
            user.close();
        }
        rig.restart().await;
        a2.restart().await;
        holds_the_notes(&a2, &milk, &bank).await;
        let mine = served(&a2).await;
        assert_eq!(mine.len(), 2, "{mine:?}");
        assert!(mine[1].0 == milk && mine[1].1.contains("buy oat milk"), "{mine:?}");
        assert!(mine[0].0 == bank && mine[0].1.is_empty(), "{mine:?}");

        // a new device of a opens its notes; what it writes there is live on the other client
        let third = rig.user(&rig.first, "a@own.test").await;
        third.engine().sync_conversation(NOTES).await.unwrap();
        holds_the_notes(&third, &milk, &bank).await;
        let on_a2 = a2.mark();
        let later = third.write(note("after the restart")).await.message_id.unwrap();
        a2.pushed(on_a2, NOTES, &later, MessageChange::New).await;
        b.restart().await;
        assert_eq!(served(&b).await.len(), 1);
        assert!(b.engine().message(NOTES, later.as_str()).await.unwrap().is_none());
        drop((a, a2, b, c, third));
        rig.stop().await;
    }
}
