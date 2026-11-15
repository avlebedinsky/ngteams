//! Several nodes and restarts: what one node stored another serves, a link one node signed opens on the other,
//! and pictures and files are still there after every node was restarted.

use teams_core::engine::MessageChange;

use super::super::push::support::User;
use super::support::{MIB, Stage, card, download, get, item, noise, paste, same, send_file};
use crate::media::png;
use crate::support::backends;

struct Sent {
    src: String,
    picture: Vec<u8>,
    film: String,
    large: Vec<u8>,
    photo: String,
}

async fn the_reader_has_it_all(reader: &User, sent: &Sent) {
    let read = reader
        .engine()
        .message_picture(sent.src.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(same(&read.bytes, &sent.picture), "{}: the picture differs", reader.upn);
    assert!(
        same(&download(reader, &sent.film, 0).await, &sent.large),
        "{}: the file differs",
        reader.upn
    );
    let thumbnail = reader
        .engine()
        .file_thumbnail(sent.photo.clone())
        .await
        .unwrap()
        .unwrap();
    let thumbnail = image::load_from_memory(&thumbnail.bytes).unwrap();
    assert_eq!((thumbnail.width(), thumbnail.height()), (800, 400));
}

/// Closes the client and starts it again on its store, connected.
async fn restart(user: &mut User) {
    user.close();
    let mark = user.mark();
    user.start();
    user.online(mark).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_one_node_stored_the_other_serves_and_it_is_all_there_after_the_nodes_restart() {
    for backend in backends() {
        let mut stage = Stage::start(backend, "").await;
        let (mut a, mut b, c, chat) = stage.people().await;
        let picture = png(320, 200);
        let large = noise(6 * MIB + 17, 11);
        let (src, pasted) = paste(&a, &stage.cloud, &chat, &picture, (320, 200)).await;
        b.pushed(0, &chat, &pasted, MessageChange::New).await;
        let id = send_file(&a, &stage.cloud, &chat, "film.bin", &large).await;
        let film = card(&mut b, &stage.cloud, &chat, &id).await.share_url.unwrap();
        let id = send_file(&a, &stage.cloud, &chat, "photo.png", &png(1200, 600)).await;
        let photo = card(&mut b, &stage.cloud, &chat, &id).await.share_url.unwrap();
        let sent = Sent {
            src,
            picture,
            film,
            large,
            photo,
        };

        the_reader_has_it_all(&b, &sent).await;
        let (_, signed_by_other) = item(stage.rig.other(), &b, &stage, &sent.film).await.unwrap();
        let (_, signed_by_first) = item(&stage.rig.first, &a, &stage, &sent.film).await.unwrap();
        assert_eq!(get(&stage.rig.first, &signed_by_other, None).await, 200);
        assert_eq!(get(stage.rig.other(), &signed_by_first, None).await, 200);
        let kept = stage.blobs.keys().await;

        if let Some(node) = &mut stage.rig.second {
            node.stop().await;
            node.start().await;
        }
        stage.rig.first.stop().await;
        stage.rig.first.start().await;
        assert_eq!(stage.blobs.keys().await, kept);
        assert_eq!(
            get(stage.rig.other(), &signed_by_first, None).await,
            200,
            "a restart ended a link"
        );
        for user in [&mut a, &mut b] {
            restart(user).await;
            user.shows(&chat, &pasted, &sent.src).await;
            the_reader_has_it_all(user, &sent).await;
        }
        drop((a, b, c));
        stage.stop().await;
    }
}
