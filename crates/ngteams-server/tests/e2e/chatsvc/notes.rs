//! The account's own notes (`48:notes`): none until something is written there, read as any chat's messages, and
//! nobody else's.

use teams_core::engine::Action;
use teams_core::sync::NOTES;
use teams_proto::chat::{ChatReads, MessageText};
use teams_store::OutboxState;

use super::messages::Pair;
use super::{client, engine};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn an_accounts_notes_are_none_until_written_then_sync_to_its_other_engine_and_stay_its_own() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let own = client(&pair.server, "a@own.test", &pair.a_id).await;
        let reads = ChatReads::new(&own);
        assert_eq!(reads.messages(NOTES, 200).await.unwrap_err().status(), Some(404));

        let note = |text: &str| Action::Send {
            conversation: NOTES.into(),
            text: MessageText::plain(text).unwrap(),
        };
        let first = pair.a.write(note("a note to myself")).await.unwrap();
        assert_eq!(first.state, OutboxState::Sent, "{first:?}");
        let first = first.message_id.unwrap();
        let second = pair.a.write(note("and another")).await.unwrap();
        assert_eq!(second.state, OutboxState::Sent, "{second:?}");
        let second = second.message_id.unwrap();

        let dir = tempfile::tempdir().unwrap();
        let other_device = engine(&pair.server, "a@own.test", &pair.a_id, dir.path()).await;
        other_device.sync_conversation(NOTES).await.unwrap();
        let stored = other_device.message(NOTES, &first).await.unwrap().unwrap();
        assert!(stored.content.contains("a note to myself"));
        assert_eq!(
            stored.sender.as_deref(),
            Some(format!("8:orgid:{}", pair.a_id).as_str())
        );

        let page = reads.messages(NOTES, 1).await.unwrap();
        assert_eq!(page.messages[0].id, second);
        assert_eq!(page.messages[0].conversation_id.as_deref(), Some(NOTES));
        let older = reads.messages_from(NOTES, page.backward_link().unwrap()).await.unwrap();
        assert_eq!(older.messages[0].id, first);

        let theirs = client(&pair.server, "b@own.test", &pair.b_id).await;
        let their_reads = ChatReads::new(&theirs);
        assert_eq!(
            their_reads.messages(NOTES, 200).await.unwrap_err().status(),
            Some(404),
            "another account's notes are its own, and it has none"
        );
        let wrote = pair.b.write(note("b's own note")).await.unwrap();
        assert_eq!(wrote.state, OutboxState::Sent, "{wrote:?}");
        let theirs_page = their_reads.messages(NOTES, 200).await.unwrap();
        assert_eq!(theirs_page.messages.len(), 1);
        assert!(theirs_page.messages[0].content.contains("b's own note"));
        assert_eq!(reads.messages(NOTES, 200).await.unwrap().messages.len(), 2);

        for reads in [&reads, &their_reads] {
            let listed = reads.conversations().await.unwrap();
            assert!(
                listed.conversations.iter().all(|entry| entry.id != NOTES),
                "the notes are in no conversation list"
            );
        }
        other_device.shutdown();
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_edits_and_deletes_its_notes_and_its_other_engine_syncs_the_change() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let wrote = |text: &str| Action::Send {
            conversation: NOTES.into(),
            text: MessageText::plain(text).unwrap(),
        };
        let kept = pair.a.write(wrote("first wording")).await.unwrap().message_id.unwrap();
        let dropped = pair.a.write(wrote("to be deleted")).await.unwrap().message_id.unwrap();
        pair.a.sync_conversation(NOTES).await.unwrap();
        let edit = pair
            .a
            .write(Action::Edit {
                conversation: NOTES.into(),
                message_id: kept.clone(),
                text: MessageText::plain("second wording").unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(edit.state, OutboxState::Sent, "{edit:?}");
        let delete = pair
            .a
            .write(Action::Delete {
                conversation: NOTES.into(),
                message_id: dropped.clone(),
            })
            .await
            .unwrap();
        assert_eq!(delete.state, OutboxState::Sent, "{delete:?}");

        let dir = tempfile::tempdir().unwrap();
        let other_device = engine(&pair.server, "a@own.test", &pair.a_id, dir.path()).await;
        other_device.sync_conversation(NOTES).await.unwrap();
        let edited = other_device.message(NOTES, &kept).await.unwrap().unwrap();
        assert!(edited.content.contains("second wording"));
        assert!(edited.edit_time.is_some());
        let deleted = other_device.message(NOTES, &dropped).await.unwrap().unwrap();
        assert!(deleted.delete_time.is_some());
        assert!(deleted.content.is_empty());
        other_device.shutdown();
        pair.stop().await;
    }
}
