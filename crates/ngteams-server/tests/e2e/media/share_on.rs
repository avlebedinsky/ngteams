//! A file shared on to another chat, as Teams' "Share this file" does it: the post names the file's entry again, and
//! the server gives the people of that chat access to it when the sender has access. The sharing link's permission
//! is read on the file's site by anyone with access.

use teams_core::engine::Action;
use teams_proto::chat::{ChatReads, MessageText, SharedFile, share_entry};
use teams_proto::{FileDownload, Mri};
use teams_store::OutboxState;

use super::files::send_file;
use super::{Trio, cloud, engine, status};
use crate::chatsvc::activity::RawPost;
use crate::support::Backend;

async fn sql(trio: &Trio, statement: &str) {
    use diesel_async::SimpleAsyncConnection as _;
    let ngteams_server::db::Pool::Sqlite(pool) = trio.server.app.db.pool() else {
        panic!("this test uses SQLite");
    };
    pool.get().await.unwrap().batch_execute(statement).await.unwrap();
}

async fn destination(trio: &Trio) -> String {
    trio.b
        .write(Action::CreateChat {
            others: vec![Mri::new(format!("8:orgid:{}", trio.ids[2]))],
            topic: None,
        })
        .await
        .unwrap()
        .message_id
        .unwrap()
}

fn post(chat: &str, files: serde_json::Value, id: &str) -> RawPost {
    let mut body = serde_json::json!({"messagetype": "RichText/Html", "content": "<p>test</p>",
        "clientmessageid": id, "properties": {}});
    body["properties"]["files"] = files;
    RawPost {
        chat: chat.to_owned(),
        body,
    }
}

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

#[tokio::test(flavor = "multi_thread")]
async fn a_grant_error_rolls_the_message_back_and_the_same_request_can_finish() {
    let trio = Trio::start(Backend::Sqlite).await;
    let share = send_file(&trio, "plan.txt", b"made-up plan").await;
    let b = super::client(&trio.server, "b@own.test", &trio.ids[1]).await;
    let c = trio.outsider().await;
    let file = newest_file(&b, &trio.chat).await;
    let chat = destination(&trio).await;
    let entry = share_entry(&cloud(), &file, None).unwrap();
    let mut events = trio
        .server
        .app
        .fanout
        .subscribe(&format!("push:8:orgid:{}", trio.ids[2]))
        .await
        .unwrap();
    sql(
        &trio,
        "CREATE TRIGGER refuse_grant BEFORE INSERT ON drive_grants BEGIN SELECT RAISE(FAIL, 'test grant error'); END;",
    )
    .await;
    let request = || post(&chat, serde_json::json!([entry]), "5430001");
    assert_eq!(b.write(request()).await.unwrap().status(), 503);
    assert!(
        ChatReads::new(&c)
            .messages(&chat, 10)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    let download = FileDownload::of_in(&cloud(), &share).unwrap();
    assert_eq!(status(&c.read(&download).await.unwrap_err()), Some(403));
    sql(&trio, "DROP TRIGGER refuse_grant;").await;
    let first = b.write(request()).await.unwrap();
    assert_eq!(first.status(), 201);
    let pushed = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    let pushed: serde_json::Value = serde_json::from_slice(&pushed).unwrap();
    assert_eq!(pushed["resourceType"], "NewMessage");
    assert!(c.read(&download).await.is_ok());
    let duplicate = b.write(request()).await.unwrap();
    assert_eq!(duplicate.body(), first.body());
    assert_eq!(ChatReads::new(&c).messages(&chat, 10).await.unwrap().messages.len(), 1);
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_links_and_entries_after_the_limit_cannot_grant_access() {
    let trio = Trio::start(Backend::Sqlite).await;
    let share = send_file(&trio, "secret.txt", b"made-up secret").await;
    let b = super::client(&trio.server, "b@own.test", &trio.ids[1]).await;
    let c = trio.outsider().await;
    let file = newest_file(&b, &trio.chat).await;
    let chat = destination(&trio).await;
    let good = share_entry(&cloud(), &file, None).unwrap();
    let download = FileDownload::of_in(&cloud(), &share).unwrap();
    for (index, link) in [
        share.replacen("https://", "https://x@", 1),
        share.replacen("https://", "https://x:pass@", 1),
        share.replacen(".test/", ".test:8443/", 1),
    ]
    .into_iter()
    .enumerate()
    {
        let mut entry = good.clone();
        entry["fileInfo"]["shareUrl"] = link.into();
        assert_eq!(
            b.write(post(&chat, serde_json::json!([entry]), &format!("54301{index}")))
                .await
                .unwrap()
                .status(),
            201
        );
        assert_eq!(status(&c.read(&download).await.unwrap_err()), Some(403));
    }
    let mut entries = vec![serde_json::Value::Null; 10];
    entries.push(good);
    assert_eq!(
        b.write(post(&chat, entries.into(), "5430111")).await.unwrap().status(),
        201
    );
    assert_eq!(status(&c.read(&download).await.unwrap_err()), Some(403));
    trio.stop().await;
}
