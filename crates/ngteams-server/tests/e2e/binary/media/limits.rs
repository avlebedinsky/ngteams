//! The configured limits: a picture's and a file's size and what one account may store. What is refused, or
//! given up half-way, leaves nothing in the store.

use teams_core::engine::Action;
use teams_proto::{CreatePicture, PastedFormat, PastedPicture, UploadPicture};

use super::super::push::support::User;
use super::support::{
    MIB, Stage, abandon_upload, card, noise, paste, send_file, streamed_upload, token, try_upload, upload_path,
};
use crate::media::png;
use crate::support::{Backend, backends};

fn picture(bytes: Vec<u8>) -> PastedPicture {
    PastedPicture {
        format: PastedFormat::Png,
        bytes,
    }
}

/// The status the engine's upload of a file was refused with.
async fn refused(user: &User, name: &str, bytes: &[u8]) -> String {
    try_upload(user, name, bytes).await.map(|_| ()).unwrap_err()
}

async fn pasting_fails_with(user: &User, chat: &str, bytes: Vec<u8>, status: &str) {
    let failed = user.engine().upload_picture(chat.to_owned(), picture(bytes)).await;
    let failed = format!("{:?}", failed.unwrap_err());
    assert!(failed.contains(status), "{failed}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_picture_or_file_over_its_size_and_an_upload_given_up_leave_nothing_in_the_store() {
    let large = png(120, 120);
    let config = format!(
        "[media]\nmax_picture_bytes = {}\nmax_file_bytes = {}\n",
        large.len() - 1,
        8 * MIB
    );
    for backend in backends() {
        let stage = Stage::start(backend, &config).await;
        let (a, b, c, chat) = stage.people().await;
        let node = &stage.rig.first;
        assert_eq!(stage.blobs.keys().await, Vec::<String>::new());

        pasting_fails_with(&a, &chat, large.clone(), "413").await;
        let writer = a.client();
        let made = writer.write(CreatePicture::new(&chat, PastedFormat::Png).unwrap());
        let object = CreatePicture::read_answer(made.await.unwrap().body()).unwrap();
        let text = UploadPicture::new(&object, &picture(b"<html>not a picture</html>".to_vec())).unwrap();
        assert_eq!(writer.write(text).await.unwrap().status(), 415);
        assert_eq!(
            stage.blobs.keys().await,
            Vec::<String>::new(),
            "a refused picture was kept"
        );

        let over = noise(8 * MIB + 1, 1);
        let said = refused(&a, "over.bin", &over).await;
        assert!(said.contains("413"), "{said}");
        let own = token(&a, &stage.rig.origin).await;
        let status = streamed_upload(node, &upload_path(&a, "streamed.bin"), &own, &over).await;
        assert_eq!(status, 413);
        abandon_upload(node, &upload_path(&a, "given-up.bin"), &own, 7 * MIB, &over[..6 * MIB]).await;
        assert_eq!(
            stage.blobs.keys().await,
            Vec::<String>::new(),
            "a refused file was kept"
        );

        let fits = try_upload(&a, "fits.bin", &over[..8 * MIB]).await.unwrap();
        assert_eq!(fits.size(), 8 * MIB as u64);
        let kept = stage.blobs.keys().await;
        assert!(kept.len() == 1 && kept[0].starts_with("drive/"), "{kept:?}");
        drop((a, b, c));
        stage.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_at_its_quota_is_refused_the_next_upload_and_the_others_keep_theirs() {
    let small = png(8, 8);
    let config = format!("[media]\nuser_quota_bytes = {}\n", small.len() + 3000);
    for backend in backends() {
        let stage = Stage::start(backend, &config).await;
        let (a, b, c, chat) = stage.people().await;
        let node = &stage.rig.first;
        paste(&a, &stage.cloud, &chat, &small, (8, 8)).await;

        let said = refused(&a, "large.bin", &[7; 3001]).await;
        assert!(said.contains("507"), "{said}");
        let own = token(&a, &stage.rig.origin).await;
        let status = streamed_upload(node, &upload_path(&a, "streamed.bin"), &own, &[7; 3001]).await;
        assert_eq!(status, 507);
        abandon_upload(node, &upload_path(&a, "given-up.bin"), &own, 2500, &[7; 1000]).await;
        assert_eq!(stage.blobs.keys().await.len(), 1, "a refused upload was kept");

        assert_eq!(try_upload(&a, "fits.bin", &[7; 2000]).await.unwrap().size(), 2000);
        pasting_fails_with(&a, &chat, png(64, 64), "507").await;
        assert_eq!(try_upload(&a, "rest.bin", &[7; 1000]).await.unwrap().size(), 1000);
        let said = refused(&a, "one-more.bin", b"x").await;
        assert!(said.contains("507"), "{said}");

        assert_eq!(try_upload(&b, "theirs.bin", &[7; 2000]).await.unwrap().size(), 2000);
        let kept = stage.blobs.keys().await;
        let (pictures, files) = kept.iter().partition::<Vec<_>, _>(|key| key.starts_with("ams/"));
        assert_eq!((pictures.len(), files.len()), (1, 3), "{kept:?}");
        drop((a, b, c));
        stage.stop().await;
    }
}

/// Q1 of QA round 1 for task 182: nothing an account does gives its stored bytes back.
#[ignore = "an account has no way to free its quota: no route deletes a file or a picture"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_at_its_quota_uploads_again_after_it_deleted_what_it_sent() {
    let small = png(8, 8);
    let config = format!("[media]\nuser_quota_bytes = {}\n", small.len() + 3000);
    let stage = Stage::start(Backend::Sqlite, &config).await;
    let (a, mut b, c, chat) = stage.people().await;
    let (_, pasted) = paste(&a, &stage.cloud, &chat, &small, (8, 8)).await;
    let sent = send_file(&a, &stage.cloud, &chat, "full.bin", &[7; 3000]).await;
    card(&mut b, &stage.cloud, &chat, &sent).await;
    let said = refused(&a, "one-more.bin", b"x").await;
    assert!(said.contains("507"), "{said}");

    for message_id in [pasted, sent] {
        a.write(Action::Delete {
            conversation: chat.clone(),
            message_id,
        })
        .await;
    }
    let again = try_upload(&a, "after.bin", &[7; 3000]).await;
    assert!(
        again.is_ok(),
        "still over the quota after deleting both messages: {again:?}"
    );
    drop((a, b, c));
    stage.stop().await;
}
