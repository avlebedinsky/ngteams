//! Who reaches a drive, an object and a file's links; and two nodes over one database and one store.

use reqwest::Method;
use serde_json::{Value, json};
use teams_proto::{CreatePicture, FileDownload, PastedFormat, PastedPicture, UploadPicture};

use super::files::send_file;
use super::{GRAPH, IC3, ORIGIN, Trio, client, cloud, engine, png, raw, status, token};
use crate::support::{Backend, TestServer, backends};

fn site(id: &str) -> String {
    format!("/sp/personal/{}", id.replace('-', ""))
}

/// The file's item as Graph answers `upn`: its id and the path and query of its download link.
async fn item(server: &TestServer, upn: &str, share: &str) -> (String, String) {
    let read = FileDownload::of_in(&cloud(), share).unwrap();
    let graph = token(server, upn, GRAPH).await;
    let path = format!("/graph/v1.0/shares/{}/driveItem", read.share());
    let (status, _, body) = raw(server, Method::GET, &path, Some(&graph), &[], None).await;
    assert_eq!(status, 200);
    let item: Value = serde_json::from_slice(&body).unwrap();
    let link = item["@microsoft.graph.downloadUrl"].as_str().unwrap();
    (
        item["sharepointIds"]["listItemUniqueId"].as_str().unwrap().to_owned(),
        link.strip_prefix(ORIGIN).unwrap().to_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_drive_and_an_object_answer_only_their_owner_and_the_people_shared_with() {
    let trio = Trio::start(Backend::Sqlite).await;
    let server = &trio.server;
    let [a, b, _] = &trio.ids;
    let share = send_file(&trio, "report.txt", b"0123456789").await;
    let (id, _) = item(server, "b@own.test", &share).await;
    let drive = format!("{}/_api/v2.0/drive", site(a));
    let own = token(server, "a@own.test", ORIGIN).await;
    let others = token(server, "b@own.test", ORIGIN).await;
    let get = |path: String, token: String| async move { raw(server, Method::GET, &path, Some(&token), &[], None).await.0 };
    assert_eq!(get(drive.clone(), own.clone()).await, 200);
    assert_eq!(get(drive.clone(), others.clone()).await, 403);
    assert_eq!(get(drive.clone(), token(server, "a@own.test", GRAPH).await).await, 401);
    assert_eq!(get(format!("{}/_api/v2.0/drive", site(b)), others.clone()).await, 200);
    assert_eq!(get(format!("{}/_api/v2.0/drive/items", site(a)), own.clone()).await, 404);
    let permissions = format!("{}/_api/v2.0/sites/root/items/{id}/driveItem/permissions", site(a));
    assert_eq!(get(permissions.clone(), own.clone()).await, 200);
    assert_eq!(get(permissions.replace(&site(a), &site(b)), others.clone()).await, 404);

    let invite = |body: Value| {
        let path = format!("{}/_api/v2.0/sites/root/items/{id}/driveItem/invite", site(a));
        let own = own.clone();
        async move {
            let headers = [("content-type", "application/json")];
            raw(server, Method::POST, &path, Some(&own), &headers, Some(body.to_string().into_bytes()))
                .await
                .0
        }
    };
    let to = |email: &str, sign_in: bool| {
        json!({"recipients": [{"email": email}], "roles": ["write"], "requireSignIn": sign_in, "sendInvitation": false})
    };
    assert_eq!(invite(to("nobody@own.test", true)).await, 400);
    assert_eq!(invite(to("c@own.test", false)).await, 400);
    assert_eq!(invite(json!({"recipients": [], "requireSignIn": true, "sendInvitation": false})).await, 400);
    assert_eq!(invite(to("C@own.test", true)).await, 200);
    let outsider = trio.outsider().await;
    let read = FileDownload::of_in(&cloud(), &share).unwrap();
    assert!(outsider.read(&read).await.is_ok());

    let revoke = format!("{permissions}/{}", trio.ids[2]);
    assert_eq!(raw(server, Method::DELETE, &revoke, Some(&own), &[], None).await.0, 204);
    assert_eq!(raw(server, Method::DELETE, &revoke, Some(&own), &[], None).await.0, 404);
    assert_eq!(status(&outsider.read(&read).await.unwrap_err()), Some(403));
    let owner = format!("{permissions}/{a}");
    assert_eq!(raw(server, Method::DELETE, &owner, Some(&own), &[], None).await.0, 404);

    let graph = token(server, "b@own.test", GRAPH).await;
    for (share, expected) in [("u!bm90IGEgbGluaw", 404), ("not-a-token", 400)] {
        let path = format!("/graph/v1.0/shares/{share}/driveItem");
        assert_eq!(raw(server, Method::GET, &path, Some(&graph), &[], None).await.0, expected);
    }
    let elsewhere = FileDownload::of_in(&cloud(), &share.replace("report.txt", "other.txt")).unwrap();
    let path = format!("/graph/v1.0/shares/{}/driveItem", elsewhere.share());
    assert_eq!(raw(server, Method::GET, &path, Some(&graph), &[], None).await.0, 404);

    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_object_is_made_for_the_makers_chats_filled_by_its_maker_and_read_once_filled() {
    let trio = Trio::start(Backend::Sqlite).await;
    let server = &trio.server;
    let [a, b, _] = &trio.ids;
    let outsider = trio.outsider().await;
    let writer = client(server, "a@own.test", a).await;
    let made = writer
        .write(CreatePicture::new(&trio.chat, PastedFormat::Png).unwrap())
        .await
        .unwrap();
    let object = CreatePicture::read_answer(made.body()).unwrap();
    let fill = || {
        let picture = PastedPicture {
            format: PastedFormat::Png,
            bytes: png(4, 4),
        };
        UploadPicture::new(&object, &picture).unwrap()
    };
    let member = client(server, "b@own.test", b).await;
    assert_eq!(member.write(fill()).await.unwrap().status(), 403);
    let made = outsider
        .write(CreatePicture::new(&trio.chat, PastedFormat::Png).unwrap())
        .await
        .unwrap();
    assert_eq!(made.status(), 403);
    let ic3 = token(server, "b@own.test", IC3).await;
    let view = format!("/ams/v1/objects/{object}/views/imgo");
    assert_eq!(raw(server, Method::GET, &view, Some(&ic3), &[], None).await.0, 404);
    assert_eq!(writer.write(fill()).await.unwrap().status(), 201);
    assert_eq!(raw(server, Method::GET, &view, Some(&ic3), &[], None).await.0, 200);
    let unknown = view.replace("imgo", "imgt1");
    assert_eq!(raw(server, Method::GET, &unknown, Some(&ic3), &[], None).await.0, 404);
    let (status, _, body) = raw(server, Method::GET, &format!("/ams/v1/objects/{object}"), Some(&ic3), &[], None).await;
    let read: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!((status, read["type"].as_str()), (200, Some("pish/image")));
    assert!(read["contents"]["imgpsh"]["length"].as_u64().unwrap() > 0);
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_link_serves_ranges_and_only_with_its_own_credential() {
    let trio = Trio::start(Backend::Sqlite).await;
    let server = &trio.server;
    let bytes = b"0123456789".to_vec();
    let share = send_file(&trio, "digits.html", &bytes).await;
    let other = send_file(&trio, "other.txt", b"other").await;
    let (id, link) = item(server, "b@own.test", &share).await;
    let (other_id, other_link) = item(server, "b@own.test", &other).await;
    let get = |path: String, range: Option<&'static str>| async move {
        let headers: Vec<(&str, &str)> = range.map(|range| ("range", range)).into_iter().collect();
        raw(server, Method::GET, &path, None, &headers, None).await
    };

    let (status, headers, body) = get(link.clone(), None).await;
    assert_eq!((status, body.as_slice()), (200, bytes.as_slice()));
    assert!(headers["content-disposition"].to_str().unwrap().starts_with("attachment"));
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["accept-ranges"], "bytes");
    for (range, expected, content_range) in [
        ("bytes=2-5", &b"2345"[..], "bytes 2-5/10"),
        ("bytes=7-", b"789", "bytes 7-9/10"),
        ("bytes=-3", b"789", "bytes 7-9/10"),
        ("bytes=8-200", b"89", "bytes 8-9/10"),
    ] {
        let (status, headers, body) = get(link.clone(), Some(range)).await;
        assert_eq!((status, body.as_slice()), (206, expected), "{range}");
        assert_eq!(headers["content-range"], content_range);
    }
    let (status, headers, _) = get(link.clone(), Some("bytes=10-")).await;
    assert_eq!((status, headers["content-range"].to_str().unwrap()), (416, "bytes */10"));
    assert_eq!(get(link.clone(), Some("lines=1-2")).await.0, 416);
    let (status, _, body) = get(link.clone(), Some("bytes=0-1,4-5")).await;
    assert_eq!((status, body.len()), (200, 10));

    let bare = format!("/sp/download/{id}");
    assert_eq!(get(bare.clone(), None).await.0, 401);
    assert_eq!(get(format!("{bare}?tempauth=made-up"), None).await.0, 401);
    let swapped = other_link.replace(&other_id, &id);
    assert_eq!(get(swapped, None).await.0, 401);
    let credential = link.split_once("tempauth=").unwrap().1;
    assert_eq!(get(format!("/sp/thumbnail/{id}/large?tempauth={credential}"), None).await.0, 401);
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_link_ends_with_its_lifetime_and_the_engine_asks_for_another() {
    let trio = Trio::start_with(Backend::Sqlite, |config| config.media.link_lifetime = 1).await;
    let share = send_file(&trio, "short.txt", b"short lived").await;
    let (_, link) = item(&trio.server, "b@own.test", &share).await;
    assert_eq!(trio.b.file_size(share.clone()).await.unwrap(), 11);
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    assert_eq!(raw(&trio.server, Method::GET, &link, None, &[], None).await.0, 401);
    assert_eq!(trio.b.file_range(share, 0, 5).await.unwrap().bytes, b"short");
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_share_files_pictures_and_link_credentials() {
    for backend in backends() {
        let (first, blobs) = super::server(backend, |_| {}).await;
        let shared = first.app.config.clone();
        let second = TestServer::start_with(backend, |config| {
            config.public_origin = shared.public_origin.clone();
            config.partition = shared.partition.clone();
            config.database_url = shared.database_url.clone();
            config.storage_url = shared.storage_url.clone();
            config.storage_options = shared.storage_options.clone();
        })
        .await;
        let a = first.add_user("a@own.test", "Ada").await;
        let b = first.add_user("b@own.test", "Bert").await;
        let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let sender = engine(&first, "a@own.test", &a, dirs[0].path()).await;
        let reader = engine(&second, "b@own.test", &b, dirs[1].path()).await;
        let row = sender
            .write(teams_core::engine::Action::CreateChat {
                others: vec![teams_proto::Mri::new(format!("8:orgid:{b}"))],
                topic: None,
            })
            .await
            .unwrap();
        let chat = row.message_id.unwrap();
        let picture = PastedPicture {
            format: PastedFormat::Png,
            bytes: png(20, 10),
        };
        let object = sender.upload_picture(chat.clone(), picture.clone()).await.unwrap();
        let src = teams_proto::sent_picture_in(&cloud(), &object, (20, 10)).unwrap().src;
        assert_eq!(reader.message_picture(src).await.unwrap().unwrap().bytes, picture.bytes);

        let path = dirs[0].path().join("shared.png");
        std::fs::write(&path, png(300, 200)).unwrap();
        let file = sender.upload_file(path).await.unwrap();
        let entry = file.entry();
        let share = entry["objectUrl"].as_str().unwrap().to_owned();
        let text = teams_proto::chat::MessageText::plain("on two nodes")
            .unwrap()
            .with_files(vec![file])
            .for_cloud(&cloud())
            .unwrap();
        let sent = sender
            .write(teams_core::engine::Action::Send {
                conversation: chat,
                text,
            })
            .await
            .unwrap();
        assert_eq!(sent.state, teams_store::OutboxState::Sent, "{sent:?}");
        assert!(reader.file_thumbnail(share.clone()).await.unwrap().is_some());
        assert_eq!(reader.file_range(share.clone(), 1, 3).await.unwrap().bytes, b"PNG");
        // a link the second node signed opens on the first
        let (_, link) = item(&second, "b@own.test", &share).await;
        assert_eq!(raw(&first, Method::GET, &link, None, &[], None).await.0, 200);
        sender.shutdown();
        reader.shutdown();
        second.stop().await;
        first.stop().await;
        drop(blobs);
    }
}
