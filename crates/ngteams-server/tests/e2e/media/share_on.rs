//! A file shared on to another chat, as Teams' "Share this file" does it: the post names the file's entry again, and
//! the server gives the people of that chat access to it when the sender has access. The sharing link's permission
//! is read on the file's site by anyone with access.

use teams_core::engine::Action;
use teams_proto::chat::{ChatReads, MessageText, SharedFile, share_entry};
use teams_proto::{FileDownload, Mri};
use teams_store::OutboxState;

use super::files::send_file;
use super::{Trio, cloud, engine, status};
use crate::support::Backend;

/// The file of the newest message of `chat` as `reader` reads it.
async fn newest_file(reader: &super::MediaClient, chat: &str) -> SharedFile {
    let page = ChatReads::new(reader).messages(chat, 1).await.unwrap();
    let files = page.messages[0].files_in(&cloud());
    assert!(files.unread.is_none(), "{:?}", files.unread);
    assert_eq!(files.files.len(), 1);
    files.files[0].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_shared_on_to_another_chat_reaches_its_people() {
    let trio = Trio::start(Backend::Sqlite).await;
    let share = send_file(&trio, "plan.txt", b"made-up plan").await;
    let b_reads = super::client(&trio.server, "b@own.test", &trio.ids[1]).await;
    let file = newest_file(&b_reads, &trio.chat).await;
    assert_eq!(file.share_url.as_deref(), Some(share.as_str()));

    let dir = tempfile::tempdir().unwrap();
    let c = engine(&trio.server, "c@own.test", &trio.ids[2], dir.path()).await;
    let c_reads = trio.outsider().await;
    let made = trio
        .b
        .write(Action::CreateChat {
            others: vec![Mri::new(format!("8:orgid:{}", trio.ids[2]))],
            topic: None,
        })
        .await
        .unwrap();
    let b_and_c = made.message_id.unwrap();
    let download = FileDownload::of_in(&cloud(), &share).unwrap();
    assert_eq!(status(&c_reads.read(&download).await.unwrap_err()), Some(403));

    // c has no access: its post of the entry gives nobody any
    let entry = share_entry(&cloud(), &file, None).unwrap();
    assert!(c.sync().await.unwrap().failed.is_empty());
    let text = MessageText::plain("not mine")
        .unwrap()
        .with_shared(vec![entry.clone()])
        .for_cloud(&cloud())
        .unwrap();
    let row = c
        .write(Action::Send {
            conversation: b_and_c.clone(),
            text,
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    assert_eq!(status(&c_reads.read(&download).await.unwrap_err()), Some(403));

    // b has access: what its link lets people do, then its share with a message
    let permission = trio
        .b
        .link_permission(file.object_url.clone().unwrap(), share.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(permission.scope, "users");
    assert_eq!(permission.description.as_deref(), Some("People you choose can edit"));
    let entry = share_entry(&cloud(), &file, Some(&permission.scope)).unwrap();
    let text = MessageText::plain("have a look")
        .unwrap()
        .with_shared(vec![entry])
        .for_cloud(&cloud())
        .unwrap();
    let row = trio
        .b
        .write(Action::Send {
            conversation: b_and_c.clone(),
            text,
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");

    let shared = newest_file(&c_reads, &b_and_c).await;
    assert_eq!(shared.share_url.as_deref(), Some(share.as_str()));
    assert_eq!(c.file_size(share.clone()).await.unwrap(), 12);
    assert_eq!(c.file_range(share.clone(), 0, 64).await.unwrap().bytes, b"made-up plan");
    // c now has access too: it may read the link's permission
    assert!(
        c.link_permission(file.object_url.unwrap(), share)
            .await
            .unwrap()
            .is_some()
    );
    c.shutdown();
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_links_permission_is_refused_to_someone_without_access() {
    let trio = Trio::start(Backend::Sqlite).await;
    let share = send_file(&trio, "plan.txt", b"made-up plan").await;
    let b_reads = super::client(&trio.server, "b@own.test", &trio.ids[1]).await;
    let file = newest_file(&b_reads, &trio.chat).await;
    let dir = tempfile::tempdir().unwrap();
    let c = engine(&trio.server, "c@own.test", &trio.ids[2], dir.path()).await;
    assert!(
        c.link_permission(file.object_url.clone().unwrap(), share.clone())
            .await
            .is_err()
    );
    // the owner reads it as anyone with access does
    let own = trio.a.link_permission(file.object_url.unwrap(), share).await.unwrap();
    assert_eq!(own.map(|permission| permission.scope).as_deref(), Some("users"));
    c.shutdown();
    trio.stop().await;
}
