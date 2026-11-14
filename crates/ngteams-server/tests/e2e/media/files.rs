//! Files sent in a chat: uploaded to the sender's drive, shared with the chat's people on Send, then read by them
//! through Graph's item, its thumbnail and its ranged download link.

use teams_core::engine::Action;
use teams_proto::chat::{ChatReads, MessageText};
use teams_proto::{FileDownload, FileThumbnails};
use teams_store::OutboxState;

use super::{Trio, cloud, png, status};
use crate::support::backends;

/// Uploads `bytes` as `name` from `trio`'s a and sends it to the chat; the sharing link b reads it by.
async fn send_file(trio: &Trio, name: &str, bytes: &[u8]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    let file = trio.a.upload_file(path).await.unwrap();
    assert_eq!(file.size(), bytes.len() as u64);
    let text = MessageText::plain("the file")
        .unwrap()
        .with_files(vec![file])
        .for_cloud(&cloud())
        .unwrap();
    let row = trio
        .a
        .write(Action::Send {
            conversation: trio.chat.clone(),
            text,
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    let reader = super::client(&trio.server, "b@own.test", &trio.ids[1]).await;
    let page = ChatReads::new(&reader).messages(&trio.chat, 1).await.unwrap();
    let files = page.messages[0].files_in(&cloud());
    assert!(files.unread.is_none(), "{:?}", files.unread);
    assert_eq!(files.files.len(), 1);
    files.files[0].share_url.clone().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_sends_a_file_and_the_other_member_sees_its_thumbnail_and_downloads_ranges() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let bytes = png(1200, 600);
        let share = send_file(&trio, "photo.png", &bytes).await;
        assert!(trio.b.sync().await.unwrap().failed.is_empty());

        let thumbnail = trio.b.file_thumbnail(share.clone()).await.unwrap().unwrap();
        let picture = image::load_from_memory(&thumbnail.bytes).unwrap();
        assert_eq!((picture.width(), picture.height()), (800, 400));

        assert_eq!(trio.b.file_size(share.clone()).await.unwrap(), bytes.len() as u64);
        let part = trio.b.file_range(share.clone(), 100, 50).await.unwrap();
        assert_eq!((part.offset, part.total), (100, bytes.len() as u64));
        assert_eq!(part.bytes.as_slice(), &bytes[100..150]);
        let tail = trio
            .b
            .file_range(share.clone(), bytes.len() as u64 - 10, 4096)
            .await
            .unwrap();
        assert_eq!(tail.bytes.as_slice(), &bytes[bytes.len() - 10..]);

        let outsider = trio.outsider().await;
        for refused in [
            status(&outsider.read(&FileDownload::of_in(&cloud(), &share).unwrap()).await.unwrap_err()),
            status(&outsider.read(&FileThumbnails::of_in(&cloud(), &share).unwrap()).await.unwrap_err()),
        ] {
            assert_eq!(refused, Some(403));
        }
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_file_of_the_same_name_is_kept_beside_the_first() {
    let trio = Trio::start(crate::support::Backend::Sqlite).await;
    let first = send_file(&trio, "notes.txt", b"first").await;
    let second = send_file(&trio, "notes.txt", b"second file").await;
    assert_ne!(first, second);
    assert_eq!(trio.b.file_size(first).await.unwrap(), 5);
    assert_eq!(trio.b.file_range(second, 0, 64).await.unwrap().bytes, b"second file");
    trio.stop().await;
}
