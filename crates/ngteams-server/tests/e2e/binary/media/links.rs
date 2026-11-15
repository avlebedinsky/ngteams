//! A file's signed download link: it opens without a token on any node while it lasts, for its own file and
//! purpose alone, and stops with its lifetime, when its holder is disabled and when the holder's password is reset.

use std::time::{Duration, Instant};

use reqwest::Method;

use super::support::{
    GRAPH, Stage, card, download, get, grant, item, item_as, noise, raw, same, send_file, try_upload,
};
use crate::support::{PASSWORD, backends};

const LIFETIME: u64 = 6;
const NEW_PASSWORD: &str = "another horse battery";

/// The link is good for nothing but its own file's bytes; an account outside the share gets none.
async fn the_link_opens_its_own_file_alone(
    stage: &Stage,
    c_graph: &str,
    share: &str,
    id: &str,
    link: &str,
    other: &str,
) {
    let node = stage.rig.other();
    let credential = link.split_once("tempauth=").unwrap().1;
    assert_eq!(get(node, &format!("/sp/download/{id}"), None).await, 401);
    assert_eq!(
        get(node, &format!("/sp/download/{id}?tempauth=made-up"), None).await,
        401
    );
    assert_eq!(
        get(node, &format!("/sp/download/{other}?tempauth={credential}"), None).await,
        401
    );
    let thumbnail = format!("/sp/thumbnail/{id}/large?tempauth={credential}");
    assert_eq!(get(node, &thumbnail, None).await, 401);
    assert_eq!(item_as(node, c_graph, stage, share).await.unwrap_err(), 403);
    assert_eq!(get(node, &format!("/sp/download/{id}"), Some(c_graph)).await, 401);
}

/// The refusals just seen came while the link handed out at `handed` still had time left.
fn in_its_lifetime(handed: Instant) {
    assert!(
        handed.elapsed() < Duration::from_secs(LIFETIME - 1),
        "too slow to tell a refusal from the link's own end"
    );
}

/// The operator disables and then resets `b@own.test`: each ends the links it held; a new sign-in gets a new one.
async fn the_operators_commands_end_the_holders_links(stage: &Stage, share: &str, senders: &str) {
    let (first, node) = (&stage.rig.first, stage.rig.other());
    let graph = grant(node, "b@own.test", PASSWORD, GRAPH).await.unwrap();
    let (_, link) = item_as(node, &graph, stage, share).await.unwrap();
    let handed = Instant::now();
    assert_eq!(get(node, &link, None).await, 200);
    first.cli(&["user", "disable", "b@own.test"], "");
    assert_eq!(get(node, &link, None).await, 401, "a disabled account's link opened");
    assert_eq!(get(first, &link, None).await, 401);
    assert_eq!(get(node, senders, None).await, 200, "the sender's own link ended too");
    in_its_lifetime(handed);
    first.cli(&["user", "enable", "b@own.test"], "");
    a_password_reset_ends_the_holders_links(stage, share).await;
}

async fn a_password_reset_ends_the_holders_links(stage: &Stage, share: &str) {
    let (first, node) = (&stage.rig.first, stage.rig.other());
    let graph = grant(node, "b@own.test", PASSWORD, GRAPH).await.unwrap();
    let (_, link) = item_as(node, &graph, stage, share).await.unwrap();
    let handed = Instant::now();
    assert_eq!(get(node, &link, None).await, 200);
    first.cli(&["user", "reset-password", "b@own.test"], &format!("{NEW_PASSWORD}\n"));
    assert_eq!(
        get(node, &link, None).await,
        401,
        "a link from before the password reset opened"
    );
    assert_eq!(get(first, &link, None).await, 401);
    in_its_lifetime(handed);
    assert_eq!(item_as(node, &graph, stage, share).await.unwrap_err(), 401);
    a_new_sign_in_gets_a_new_link(stage, share).await;
}

async fn a_new_sign_in_gets_a_new_link(stage: &Stage, share: &str) {
    let node = stage.rig.other();
    assert!(grant(node, "b@own.test", PASSWORD, GRAPH).await.is_none());
    let graph = grant(node, "b@own.test", NEW_PASSWORD, GRAPH).await.unwrap();
    let (_, link) = item_as(node, &graph, stage, share).await.unwrap();
    assert_eq!(get(node, &link, None).await, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_download_link_opens_without_a_token_until_it_runs_out_or_its_holder_is_disabled_or_reset() {
    let config = format!("[media]\nlink_lifetime = {LIFETIME}\n");
    for backend in backends() {
        let stage = Stage::start(backend, &config).await;
        let rig = &stage.rig;
        let (a, mut b, c, chat) = stage.people().await;
        let bytes = noise(70_000, 3);
        let sent = send_file(&a, &stage.cloud, &chat, "report.bin", &bytes).await;
        let share = card(&mut b, &stage.cloud, &chat, &sent).await.share_url.unwrap();
        let private = try_upload(&c, "private.bin", b"c's own").await.unwrap();
        let private = private.entry()["objectUrl"].as_str().unwrap().to_owned();
        let (others, _) = item(rig.other(), &c, &stage, &private).await.unwrap();

        let (id, link) = item(rig.other(), &b, &stage, &share).await.unwrap();
        let handed = tokio::time::Instant::now();
        for node in [&rig.first, rig.other()] {
            let (status, _, body) = raw(node, Method::GET, &link, None, &[], None).await;
            assert!(status == 200 && same(&body, &bytes), "the link answered {status}");
        }
        let c_graph = grant(rig.other(), "c@own.test", PASSWORD, GRAPH).await.unwrap();
        the_link_opens_its_own_file_alone(&stage, &c_graph, &share, &id, &link, &others).await;

        tokio::time::sleep_until(handed + Duration::from_millis(LIFETIME * 1000 + 1200)).await;
        assert_eq!(
            get(rig.other(), &link, None).await,
            401,
            "the link outlived its lifetime"
        );
        assert_eq!(get(&rig.first, &link, None).await, 401);
        assert!(
            same(&download(&b, &share, 0).await, &bytes),
            "the client got no new link"
        );

        let (_, senders) = item(&rig.first, &a, &stage, &share).await.unwrap();
        the_operators_commands_end_the_holders_links(&stage, &share, &senders).await;
        drop((a, b, c));
        stage.stop().await;
    }
}
