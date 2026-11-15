//! Stored bytes come back: a picture's when the last message that shows it is deleted, a file's when its owner
//! deletes it from the drive, either one when the operator deletes it. The account's count stays right when
//! uploads and deletes run at once.

use std::fmt::Write as _;

use clap::Parser as _;
use ngteams_server::cli::{Cli, CliError, run};
use reqwest::Method;
use teams_core::engine::Action;
use teams_proto::chat::{MessageText, Node, RichText};
use teams_proto::{PastedFormat, PastedPicture, sent_picture_in};
use teams_store::OutboxState;

use super::{IC3, ORIGIN, Trio, cloud, png, raw, token};
use crate::support::{Backend, TestServer, backends};

/// a sends a message that shows the picture `object`; the message.
async fn show(trio: &Trio, object: &str) -> String {
    let image = sent_picture_in(&cloud(), object, (8, 8)).unwrap();
    let text = RichText {
        nodes: vec![Node::Paragraph(vec![Node::Image(image)])],
        truncated: false,
    };
    let row = trio
        .a
        .write(Action::Send {
            conversation: trio.chat.clone(),
            text: MessageText::new_in(&cloud(), text, []).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    row.message_id.unwrap()
}

async fn delete_message(trio: &Trio, message_id: String) {
    let row = trio
        .a
        .write(Action::Delete {
            conversation: trio.chat.clone(),
            message_id,
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
}

fn drive(trio: &Trio) -> String {
    format!("/sp/personal/{}/_api/v2.0", trio.ids[0].replace('-', ""))
}

/// a's raw upload of `size` bytes as `name`: the status and the file's id.
async fn put(trio: &Trio, own: &str, name: &str, size: usize) -> (u16, String) {
    let path = format!(
        "{}/drive/root:/Microsoft%20Teams%20Chat%20Files/{name}:/content",
        drive(trio)
    );
    let (status, _, body) = raw(&trio.server, Method::PUT, &path, Some(own), &[], Some(vec![7; size])).await;
    let item: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let id = item["sharepointIds"]["listItemUniqueId"].as_str().unwrap_or_default();
    (status, id.to_owned())
}

async fn delete(trio: &Trio, token: &str, path: &str) -> u16 {
    raw(&trio.server, Method::DELETE, path, Some(token), &[], None).await.0
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picture_two_messages_show_stays_until_the_last_of_them_is_deleted() {
    let small = png(8, 8);
    let quota = small.len() as u64;
    let trio = Trio::start_with(Backend::Sqlite, |config| config.media.user_quota_bytes = quota).await;
    let picture = || PastedPicture {
        format: PastedFormat::Png,
        bytes: small.clone(),
    };
    let object = trio.a.upload_picture(trio.chat.clone(), picture()).await.unwrap();
    let (first, second) = (show(&trio, &object).await, show(&trio, &object).await);
    let ic3 = token(&trio.server, "a@own.test", IC3).await;
    let view = format!("/ams/v1/objects/{object}/views/imgo");
    let read = async || raw(&trio.server, Method::GET, &view, Some(&ic3), &[], None).await.0;

    delete_message(&trio, first).await;
    assert_eq!(read().await, 200, "a picture another message shows was freed");
    let full = trio.a.upload_picture(trio.chat.clone(), picture()).await.unwrap_err();
    assert!(format!("{full:?}").contains("507"), "{full:?}");

    delete_message(&trio, second).await;
    assert_eq!(read().await, 404);
    // the object the refused paste made holds nothing; the freed bytes fit a new picture
    trio.a.upload_picture(trio.chat.clone(), picture()).await.unwrap();
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_frees_no_picture_of_another_account() {
    let trio = Trio::start(Backend::Sqlite).await;
    let picture = PastedPicture {
        format: PastedFormat::Png,
        bytes: png(8, 8),
    };
    let object = trio.b.upload_picture(trio.chat.clone(), picture).await.unwrap();
    let message = show(&trio, &object).await;
    delete_message(&trio, message).await;
    let ic3 = token(&trio.server, "b@own.test", IC3).await;
    let view = format!("/ams/v1/objects/{object}/views/imgo");
    assert_eq!(
        raw(&trio.server, Method::GET, &view, Some(&ic3), &[], None).await.0,
        200
    );
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_and_deletes_at_once_leave_the_accounts_count_right() {
    const QUOTA: usize = 8000;
    for backend in backends() {
        let trio = Trio::start_with(backend, |config| config.media.user_quota_bytes = QUOTA as u64).await;
        let own = token(&trio.server, "a@own.test", ORIGIN).await;
        let mut files = Vec::new();
        for number in 0..8 {
            let (status, id) = put(&trio, &own, &format!("old-{number}.bin"), 1000).await;
            assert_eq!(status, 201);
            files.push(format!("{}/drive/items/{id}", drive(&trio)));
        }
        assert_eq!(put(&trio, &own, "one-more.bin", 1).await.0, 507);

        let repeated = files.iter().map(|path| async {
            let mut twice = [delete(&trio, &own, path).await, 0];
            twice[1] = delete(&trio, &own, path).await;
            twice
        });
        let racing = files.iter().map(|path| delete(&trio, &own, path));
        let uploads = (0..8).map(|number| {
            let own = &own;
            let trio = &trio;
            async move { put(trio, own, &format!("new-{number}.bin"), 500).await.0 }
        });
        let (twice, raced, uploaded) = tokio::join!(
            futures_util::future::join_all(repeated),
            futures_util::future::join_all(racing),
            futures_util::future::join_all(uploads)
        );
        for (twice, raced) in twice.iter().zip(&raced) {
            let mut answers = [twice[0], twice[1], *raced];
            answers.sort_unstable();
            assert_eq!(answers, [204, 404, 404], "one file was deleted more or less than once");
        }
        assert!(
            uploaded.iter().all(|status| [201, 507].contains(status)),
            "{uploaded:?}"
        );
        let kept = uploaded.iter().filter(|status| **status == 201).count();

        let listed = format!(
            "{}/drive/root:/Microsoft%20Teams%20Chat%20Files:/children",
            drive(&trio)
        );
        let (status, _, body) = raw(&trio.server, Method::GET, &listed, Some(&own), &[], None).await;
        let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let sizes: Vec<u64> = listed["value"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["size"].as_u64().unwrap())
            .collect();
        assert_eq!((status, sizes), (200, vec![500; kept]));

        let rest = QUOTA - 500 * kept;
        assert_eq!(put(&trio, &own, "rest.bin", rest).await.0, 201, "the count is too high");
        assert_eq!(put(&trio, &own, "over.bin", 1).await.0, 507, "the count is too low");
        trio.stop().await;
    }
}

async fn cli(server: &TestServer, args: &[&str]) -> Result<String, CliError> {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("server.toml");
    let config = &server.app.config;
    let mut text = format!(
        "database_url = {:?}\npublic_origin = {:?}\nstorage_url = {:?}\n[storage_options]\n",
        config.database_url, server.origin, config.storage_url
    );
    for (key, value) in &config.storage_options {
        writeln!(text, "{key} = {value:?}").unwrap();
    }
    std::fs::write(&file, text).unwrap();
    let mut command = vec!["ngteams-server", "--config", file.to_str().unwrap()];
    command.extend_from_slice(args);
    let mut output = Vec::new();
    run(
        Cli::parse_from(command),
        &mut "".as_bytes(),
        &mut output,
        &mut Vec::new(),
        std::future::pending(),
    )
    .await?;
    Ok(String::from_utf8(output).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_operator_lists_and_deletes_what_an_account_keeps() {
    let small = png(8, 8);
    let quota = small.len() + 3000;
    for backend in backends() {
        let trio = Trio::start_with(backend, |config| config.media.user_quota_bytes = quota as u64).await;
        let own = token(&trio.server, "a@own.test", ORIGIN).await;
        let picture = PastedPicture {
            format: PastedFormat::Png,
            bytes: small.clone(),
        };
        let object = trio.a.upload_picture(trio.chat.clone(), picture).await.unwrap();
        let share = super::files::send_file(&trio, "photo.png", &png(40, 30)).await;
        assert!(trio.b.file_thumbnail(share.clone()).await.unwrap().is_some());
        let (status, file) = put(&trio, &own, "rest.bin", 3000 - png(40, 30).len()).await;
        assert_eq!(status, 201);

        let listed = cli(&trio.server, &["media", "list", "A@own.test"]).await.unwrap();
        let rows: Vec<Vec<&str>> = listed.lines().map(|line| line.split('\t').collect()).collect();
        let named: Vec<(&str, &str, &str)> = rows.iter().map(|row| (row[0], row[2], row[4])).collect();
        let (photo, rest, pasted) = (
            png(40, 30).len().to_string(),
            (3000 - png(40, 30).len()).to_string(),
            small.len().to_string(),
        );
        assert_eq!(rows.len(), 3, "{listed}");
        assert!(named.contains(&("file", photo.as_str(), "photo.png")), "{listed}");
        assert!(named.contains(&("file", rest.as_str(), "rest.bin")), "{listed}");
        assert_eq!((named[2].0, named[2].1), ("picture", pasted.as_str()), "{listed}");
        assert_eq!(rows[2][1], object);
        let photo = rows.iter().find(|row| row[4] == "photo.png").unwrap()[1];

        let nobody = cli(&trio.server, &["media", "list", "nobody@own.test"]).await;
        assert!(matches!(nobody, Err(CliError::Admin(_))), "{nobody:?}");
        let other = cli(&trio.server, &["media", "delete", "b@own.test", photo]).await;
        assert!(matches!(other, Err(CliError::NotStored(_))), "{other:?}");
        assert_eq!(put(&trio, &own, "one-more.bin", 1).await.0, 507);

        let deleted = cli(&trio.server, &["media", "delete", "a@own.test", photo, &object, &file]).await;
        assert_eq!(deleted.unwrap().lines().count(), 3);
        assert_eq!(cli(&trio.server, &["media", "list", "a@own.test"]).await.unwrap(), "");
        assert!(trio.b.file_size(share).await.is_err(), "a deleted file is still read");
        assert_eq!(put(&trio, &own, "whole.bin", quota).await.0, 201);
        trio.stop().await;
    }
}
