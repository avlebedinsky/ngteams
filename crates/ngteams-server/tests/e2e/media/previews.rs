//! Link previews: the server fetches a page's title and picture for an account, and refuses every address that
//! isn't on the public internet.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::http::header::{CONTENT_TYPE, LOCATION};
use axum::http::StatusCode;
use axum::routing::get;
use serde_json::Value;
use teams_proto::LinkPreviewPicture;

use super::{ORIGIN, Trio, cloud, png};
use crate::support::{Backend, TestServer};

const PAGE: &str = r#"<!doctype html><html><head><title>Fallback title</title>
<meta property="og:title" content="A made-up page">
<meta property="og:description" content="What the page says about itself.">
<meta property="og:image" content="/picture.png">
<link rel="icon" href="/favicon.ico"></head><body>text</body></html>"#;

/// A site on loopback: a page, its picture, a redirect to the page, and how many requests it got.
async fn site() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let router = Router::new()
        .route("/page", get(|| async { ([(CONTENT_TYPE, "text/html; charset=utf-8")], PAGE) }))
        .route("/picture.png", get(|| async { ([(CONTENT_TYPE, "image/png")], png(1600, 400)) }))
        .route("/moved", get(|| async { (StatusCode::FOUND, [(LOCATION, "/page")]) }))
        .route("/text", get(|| async { "not a picture" }))
        .layer(axum::middleware::from_fn(move |request, next: axum::middleware::Next| {
            counter.fetch_add(1, Ordering::SeqCst);
            next.run(request)
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (origin, hits)
}

async fn token(server: &TestServer, upn: &str) -> String {
    let session = server.session(upn).await;
    let token = session.access_token("https://ic3.teams.office.com").await.unwrap();
    token.token.expose().to_owned()
}

async fn ask(server: &TestServer, token: Option<&str>, path: &str, url: &str) -> (u16, Vec<u8>) {
    let mut asked = url::Url::parse(&server.url(&format!("/ams/urlp/v1/url/{path}"))).unwrap();
    asked.query_pairs_mut().append_pair("url", url);
    let mut request = reqwest::Client::new().get(asked);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let answer = request.send().await.unwrap();
    (answer.status().as_u16(), answer.bytes().await.unwrap().to_vec())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_links_preview_is_read_and_its_picture_reaches_the_engine_as_a_thumbnail() {
    let trio = Trio::start_with(Backend::Sqlite, |config| {
        config.media.preview_allow_private_addresses = true;
    })
    .await;
    let (site, _) = site().await;
    let token = token(&trio.server, "a@own.test").await;
    for page in ["/page", "/moved"] {
        let (status, body) = ask(&trio.server, Some(&token), "info", &format!("{site}{page}")).await;
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        let info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(info["title"], "A made-up page");
        assert_eq!(info["preview"]["description"], "What the page says about itself.");
        assert_eq!(info["favicon"], format!("{site}/favicon.ico"));
        let preview = info["preview"]["previewurl"].as_str().unwrap();
        assert!(preview.starts_with(&format!("{ORIGIN}/ams/urlp/v1/url/image/Thumbnail?url=")));
        let read = LinkPreviewPicture::of_in(&cloud(), preview).unwrap();
        assert_eq!(read.picture(), format!("{site}/picture.png"));

        let picture = trio.b.message_picture(preview).await.unwrap().unwrap();
        let picture = image::load_from_memory(&picture.bytes).unwrap();
        assert_eq!((picture.width(), picture.height()), (640, 160));
    }
    let (status, _) = ask(&trio.server, Some(&token), "image/Thumbnail", &format!("{site}/text")).await;
    assert_eq!(status, 415);
    let (status, _) = ask(&trio.server, Some(&token), "info", &format!("{site}/missing")).await;
    assert_eq!(status, 502);
    let (status, _) = ask(&trio.server, None, "info", &format!("{site}/page")).await;
    assert_eq!(status, 401);
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn previews_refuse_private_loopback_and_link_local_addresses() {
    let trio = Trio::start(Backend::Sqlite).await;
    let (site, hits) = site().await;
    let port = site.rsplit(':').next().unwrap();
    let token = token(&trio.server, "a@own.test").await;
    for url in [
        format!("{site}/page"),
        format!("http://localhost:{port}/page"),
        format!("http://[::1]:{port}/page"),
        format!("http://[::ffff:127.0.0.1]:{port}/page"),
        format!("http://2130706433:{port}/page"),
        "http://10.0.0.1/".to_owned(),
        "http://192.168.1.1/".to_owned(),
        "http://172.16.0.1/".to_owned(),
        "http://169.254.169.254/latest/meta-data/".to_owned(),
        "http://100.64.0.1/".to_owned(),
        "http://[fd00::1]/".to_owned(),
        "http://[fe80::1]/".to_owned(),
        "http://0.0.0.0/".to_owned(),
        "ftp://example.com/file".to_owned(),
        "file:///etc/passwd".to_owned(),
        "http://user:secret@example.com/".to_owned(),
    ] {
        for path in ["info", "image/Thumbnail"] {
            let (status, _) = ask(&trio.server, Some(&token), path, &url).await;
            assert!(matches!(status, 400 | 403), "{path} {url}: {status}");
        }
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn previews_can_be_turned_off() {
    let trio = Trio::start_with(Backend::Sqlite, |config| config.media.link_previews = false).await;
    let token = token(&trio.server, "a@own.test").await;
    let (status, _) = ask(&trio.server, Some(&token), "info", "https://example.com/").await;
    assert_eq!(status, 404);
    trio.stop().await;
}
