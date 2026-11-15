//! A picture pasted into a chat: the other member's client gets the message over its push channel and reads
//! every view of the picture, someone outside the chat reads none, and both still read it after a restart.

use reqwest::Method;
use teams_core::engine::MessageChange;
use teams_proto::MessagePicture;

use super::super::push::support::User;
use super::super::support::Node;
use super::support::{IC3, Stage, get, paste, raw, same, token};
use crate::media::{png, status};
use crate::support::backends;

const VIEWS: [&str; 3] = ["imgo", "imgpsh_fullsize", "imgpsh_fullsize_anim"];

/// The client reads every view of the picture a message can name, each the bytes as sent.
async fn reads_every_view(reader: &User, src: &str, bytes: &[u8]) {
    for view in VIEWS {
        let read = reader.engine().message_picture(src.replace("imgo", view)).await;
        let read = read.unwrap().unwrap_or_else(|| panic!("{}: no {view}", reader.upn));
        assert!(same(&read.bytes, bytes), "{}: {view} is other bytes", reader.upn);
    }
}

async fn the_outsider_reads_no_view(stage: &Stage, c: &User, src: &str) {
    let node = stage.rig.other();
    let object = src.split('/').rev().nth(2).unwrap();
    let ic3 = token(c, IC3).await;
    for view in VIEWS {
        let src = src.replace("imgo", view);
        let shown = c.engine().message_picture(src.clone()).await;
        assert!(!matches!(shown, Ok(Some(_))), "the outsider's client shows {view}");
        let read = MessagePicture::of_in(&stage.cloud, &src).unwrap();
        assert_eq!(status(&c.client().read(&read).await.unwrap_err()), Some(403), "{view}");
        let path = format!("/ams/v1/objects/{object}/views/{view}");
        assert_eq!(get(node, &path, Some(&ic3)).await, 403, "{view}");
        assert_eq!(get(node, &path, None).await, 401, "{view} without a token");
    }
    the_outsider_neither_describes_nor_fills(node, &ic3, object).await;
}

async fn the_outsider_neither_describes_nor_fills(node: &Node, ic3: &str, object: &str) {
    let described = format!("/ams/v1/objects/{object}");
    assert_eq!(get(node, &described, Some(ic3)).await, 403);
    let fill = format!("{described}/content/imgpsh");
    let filled = raw(node, Method::PUT, &fill, Some(ic3), &[], Some(png(4, 4))).await;
    assert_eq!(filled.0, 403, "the outsider replaced the picture");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pasted_picture_comes_live_is_read_by_the_chats_people_alone_and_again_after_a_restart() {
    for backend in backends() {
        let stage = Stage::start(backend, "").await;
        let (mut a, mut b, c, chat) = stage.people().await;
        let bytes = png(640, 480);
        let (src, id) = paste(&a, &stage.cloud, &chat, &bytes, (640, 480)).await;

        b.pushed(0, &chat, &id, MessageChange::New).await;
        b.shows(&chat, &id, &src).await;
        reads_every_view(&b, &src, &bytes).await;
        reads_every_view(&a, &src, &bytes).await;
        the_outsider_reads_no_view(&stage, &c, &src).await;

        for user in [&mut a, &mut b] {
            user.close();
            let mark = user.mark();
            user.start();
            user.online(mark).await;
            user.shows(&chat, &id, &src).await;
            reads_every_view(user, &src, &bytes).await;
        }
        the_outsider_reads_no_view(&stage, &c, &src).await;
        drop((a, b, c));
        stage.stop().await;
    }
}
