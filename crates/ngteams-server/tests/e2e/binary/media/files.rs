//! Files sent in a chat: a small one, one over the store's multipart part size and a picture. The other member's
//! client gets each message over its push channel, shows the file card, downloads the file whole and from the
//! middle, and loses it when the sender ends the share; someone outside the chat never reads it.

use reqwest::Method;
use teams_proto::{FileDownload, FileThumbnails};

use super::super::push::support::User;
use super::support::{MIB, Stage, card, download, drive, get, item, noise, raw, same, send_file, token};
use crate::media::{png, status};
use crate::support::backends;

/// What `b` does with a file `a` sent: sees its card, downloads it whole, resumes it; the sharing link.
async fn the_member_downloads(stage: &Stage, a: &User, b: &mut User, chat: &str, name: &str, bytes: &[u8]) -> String {
    let id = send_file(a, &stage.cloud, chat, name, bytes).await;
    let shown = card(b, &stage.cloud, chat, &id).await;
    assert_eq!(shown.name, name);
    let share = shown.share_url.unwrap();
    assert_eq!(b.engine().file_size(share.clone()).await.unwrap(), bytes.len() as u64);
    assert!(
        same(&download(b, &share, 0).await, bytes),
        "{name}: the download differs"
    );
    let half = bytes.len() / 2;
    let rest = download(b, &share, half as u64).await;
    assert!(same(&rest, &bytes[half..]), "{name}: the resumed download differs");

    the_link_resumes(stage, b, &share, name, bytes).await;
    share
}

/// The download link the reader was handed answers a `Range` from the middle with the rest.
async fn the_link_resumes(stage: &Stage, b: &User, share: &str, name: &str, bytes: &[u8]) {
    let (half, last) = (bytes.len() / 2, bytes.len() - 1);
    let (_, link) = item(stage.rig.other(), b, stage, share).await.unwrap();
    let range = format!("bytes={half}-");
    let (status, headers, body) = raw(stage.rig.other(), Method::GET, &link, None, &[("range", &range)], None).await;
    assert_eq!(status, 206, "{name}");
    assert_eq!(headers["content-range"], format!("bytes {half}-{last}/{}", bytes.len()));
    assert!(same(&body, &bytes[half..]), "{name}: the ranged answer differs");
}

async fn the_outsider_is_refused(stage: &Stage, c: &User, share: &str) {
    let download = FileDownload::of_in(&stage.cloud, share).unwrap();
    assert_eq!(status(&c.client().read(&download).await.unwrap_err()), Some(403));
    let thumbnails = FileThumbnails::of_in(&stage.cloud, share).unwrap();
    assert_eq!(status(&c.client().read(&thumbnails).await.unwrap_err()), Some(403));
    assert!(c.engine().file_size(share.to_owned()).await.is_err());
    assert!(c.engine().file_range(share.to_owned(), 0, 16).await.is_err());
    assert!(!matches!(
        c.engine().file_thumbnail(share.to_owned()).await,
        Ok(Some(_))
    ));
    assert_eq!(item(stage.rig.other(), c, stage, share).await.unwrap_err(), 403);
}

/// `a` ends `b`'s share of the file: `b`'s client, a new Graph read and the link it held all stop.
async fn the_sender_ends_the_share(stage: &Stage, a: &User, b: &User, share: &str) {
    let rig = &stage.rig;
    let (id, link) = item(rig.other(), b, stage, share).await.unwrap();
    assert_eq!(get(rig.other(), &link, None).await, 200);
    let own = token(a, &rig.origin).await;
    let revoke = format!("{}/sites/root/items/{id}/driveItem/permissions/{}", drive(a), b.oid());
    assert_eq!(
        raw(&rig.first, Method::DELETE, &revoke, Some(&own), &[], None).await.0,
        204
    );
    the_share_is_over(stage, a, b, share, &link).await;
}

async fn the_share_is_over(stage: &Stage, a: &User, b: &User, share: &str, link: &str) {
    let rig = &stage.rig;
    assert_eq!(get(rig.other(), link, None).await, 403, "the link outlived the share");
    assert_eq!(get(&rig.first, link, None).await, 403);
    assert_eq!(item(rig.other(), b, stage, share).await.unwrap_err(), 403);
    assert!(b.engine().file_range(share.to_owned(), 0, 16).await.is_err());
    assert!(
        a.engine().file_size(share.to_owned()).await.is_ok(),
        "the sender lost its own file"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sent_files_are_downloaded_whole_and_resumed_by_the_chats_other_member_alone_until_the_share_ends() {
    for backend in backends() {
        let stage = Stage::start(backend, "").await;
        let (a, mut b, c, chat) = stage.people().await;
        let small = b"a few plain words\n".repeat(64);
        let large = noise(12 * MIB + 123, 7);
        let picture = png(1200, 600);

        let note = the_member_downloads(&stage, &a, &mut b, &chat, "notes.txt", &small).await;
        let film = the_member_downloads(&stage, &a, &mut b, &chat, "film.bin", &large).await;
        let photo = the_member_downloads(&stage, &a, &mut b, &chat, "photo.png", &picture).await;
        let thumbnail = b.engine().file_thumbnail(photo.clone()).await.unwrap().unwrap();
        let thumbnail = image::load_from_memory(&thumbnail.bytes).unwrap();
        assert_eq!((thumbnail.width(), thumbnail.height()), (800, 400));
        assert!(b.engine().file_thumbnail(note.clone()).await.unwrap().is_none());

        let kept = stage.blobs.keys().await;
        let files = kept.iter().filter(|key| key.starts_with("drive/")).count();
        assert_eq!(files, 3, "{kept:?}");
        for share in [&note, &film, &photo] {
            the_outsider_is_refused(&stage, &c, share).await;
        }
        the_sender_ends_the_share(&stage, &a, &b, &film).await;
        assert!(
            same(&download(&b, &note, 0).await, &small),
            "another file's share ended too"
        );
        drop((a, b, c));
        stage.stop().await;
    }
}
