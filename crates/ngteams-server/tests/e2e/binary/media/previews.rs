//! Link previews: one account has the server read a page, sends the link with what came back, and the other
//! member's client shows the card and its picture without reaching the page's site. The site is a web server of
//! the test's own on loopback, which a server reaches only when its configuration lets previews read private
//! addresses.

use std::net::ToSocketAddrs as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, LOCATION};
use axum::routing::get as route;
use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::authz::CHAT_BASE_GTM;
use teams_core::engine::MessageChange;
use teams_proto::chat::{HYPERLINK_TYPE, link_previews};

use super::super::push::support::User;
use super::super::support::Node;
use super::support::{IC3, Stage, noise, raw, token};
use crate::media::png;
use crate::support::backends;

const PAGE: &str = r#"<!doctype html><html><head><title>Fallback title</title>
<meta property="og:title" content="A made-up page">
<meta property="og:description" content="What the page says about itself.">
<meta property="og:image" content="/picture.png">
<link rel="icon" href="/favicon.ico"></head><body>text</body></html>"#;
const PAGE_CAP: usize = 4096;

fn html(page: String) -> impl axum::response::IntoResponse {
    ([(CONTENT_TYPE, "text/html; charset=utf-8")], page)
}

fn moved(to: &'static str) -> axum::routing::MethodRouter {
    route(move || async move { (StatusCode::FOUND, [(LOCATION, to)]) })
}

/// The largest picture the server is configured to fetch: the site's own.
fn picture_cap() -> usize {
    png(1600, 400).len()
}

/// A picture too noisy to compress: over [`picture_cap`].
fn huge_picture() -> Vec<u8> {
    let picture = image::RgbImage::from_raw(1000, 800, noise(1000 * 800 * 3, 5)).unwrap();
    let mut bytes = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    assert!(bytes.get_ref().len() > picture_cap());
    bytes.into_inner()
}

/// The outside web: a site on loopback and how many requests reached it.
async fn site() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let late = format!(
        "<html><head>{}<title>A late title</title></head></html>",
        " ".repeat(2 * PAGE_CAP)
    );
    // made before anything is served: encoding them takes longer than a fetch may
    let (picture, huge) = (png(1600, 400), huge_picture());
    let router = Router::new()
        .route("/page", route(|| async { html(PAGE.to_owned()) }))
        .route(
            "/long",
            route(|| async { html(format!("{PAGE}{}", "x".repeat(64 * PAGE_CAP))) }),
        )
        .route("/late", route(move || async move { html(late) }))
        .route(
            "/picture.png",
            route(move || async move { ([(CONTENT_TYPE, "image/png")], picture) }),
        )
        .route(
            "/huge.png",
            route(move || async move { ([(CONTENT_TYPE, "image/png")], huge) }),
        )
        .route("/moved", moved("/page"))
        .route("/around", moved("/around"))
        .route(
            "/slow",
            route(|| async {
                tokio::time::sleep(Duration::from_secs(6)).await;
                html(PAGE.to_owned())
            }),
        )
        .layer(axum::middleware::from_fn(
            move |request, next: axum::middleware::Next| {
                counter.fetch_add(1, Ordering::SeqCst);
                next.run(request)
            },
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (origin, hits)
}

/// What the preview service answers `token`'s holder about `url`: `info` or `image/Thumbnail`.
async fn ask(node: &Node, token: Option<&str>, what: &str, url: &str) -> (u16, Vec<u8>) {
    let mut asked = url::Url::parse(&format!("{}/ams/urlp/v1/url/{what}", node.endpoint)).unwrap();
    asked.query_pairs_mut().append_pair("url", url);
    let path = asked.as_str().strip_prefix(&node.endpoint).unwrap().to_owned();
    let (status, _, body) = raw(node, Method::GET, &path, token, &[], None).await;
    (status, body)
}

/// `a` sends `url` with the preview the server read for it, as a client that previews links does; the message.
async fn send_with_preview(stage: &Stage, a: &User, chat: &str, url: &str, preview: &Value) -> String {
    let links = json!([{
        "@type": HYPERLINK_TYPE, "itemid": "1", "url": url, "previewenabled": true, "preview": preview,
    }]);
    let body = json!({
        "messagetype": "RichText/Html", "contenttype": "Text",
        "content": format!("<p><a href=\"{url}\" itemid=\"1\">{url}</a></p>"),
        "clientmessageid": (7_182_000_000_000_000_000_u64 + u64::try_from(url.len()).unwrap()).to_string(),
        "properties": {"links": links.to_string()},
    });
    let base = a.session().service_base(CHAT_BASE_GTM).unwrap();
    let base = base.strip_prefix(&stage.rig.origin).unwrap();
    let path = format!("{base}/v1/users/ME/conversations/{chat}/messages");
    let ic3 = token(a, IC3).await;
    let headers = [("content-type", "application/json")];
    let body = Some(body.to_string().into_bytes());
    let (status, _, answer) = raw(&stage.rig.first, Method::POST, &path, Some(&ic3), &headers, body).await;
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&answer));
    let answer: Value = serde_json::from_slice(&answer).unwrap();
    answer["OriginalArrivalTime"].to_string().trim_matches('"').to_owned()
}

/// `b`'s client got the message over its push channel and shows the link's card: title, description, picture.
async fn the_member_sees_the_card(b: &mut User, chat: &str, id: &str, url: &str) {
    b.pushed(0, chat, id, MessageChange::New).await;
    let stored = b.engine().message(chat, id).await.unwrap().unwrap();
    let properties: Value = serde_json::from_str(&stored.properties.unwrap()).unwrap();
    let previews = link_previews(properties.get("links"));
    assert_eq!(previews.len(), 1, "{properties}");
    let card = &previews[0];
    assert_eq!((card.url.as_str(), card.title.as_str()), (url, "A made-up page"));
    assert_eq!(card.description.as_deref(), Some("What the page says about itself."));
    let src = card.picture.clone().unwrap();
    let picture = b.engine().message_picture(src).await;
    let picture = image::load_from_memory(&picture.unwrap().unwrap().bytes).unwrap();
    assert_eq!((picture.width(), picture.height()), (640, 160));
}

/// The fetcher's own limits: a long page is cut, a large picture and a slow or circling site are refused.
async fn oversized_and_slow_answers_are_refused(node: &Node, ic3: &str, site: &str) {
    let (status, body) = ask(node, Some(ic3), "info", &format!("{site}/long")).await;
    let info: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!((status, info["title"].as_str()), (200, Some("A made-up page")));
    let (_, body) = ask(node, Some(ic3), "info", &format!("{site}/late")).await;
    assert!(
        !String::from_utf8_lossy(&body).contains("A late title"),
        "the page was read past its cap"
    );
    let (status, _) = ask(node, Some(ic3), "image/Thumbnail", &format!("{site}/huge.png")).await;
    assert_eq!(status, 415);
    let (status, _) = ask(node, Some(ic3), "image/Thumbnail", &format!("{site}/page")).await;
    assert_eq!(status, 415);
    a_slow_or_circling_site_is_given_up(node, ic3, site).await;
}

async fn a_slow_or_circling_site_is_given_up(node: &Node, ic3: &str, site: &str) {
    let (status, _) = ask(node, Some(ic3), "info", &format!("{site}/around")).await;
    assert_eq!(status, 502);
    let asked = Instant::now();
    let (status, _) = ask(node, Some(ic3), "info", &format!("{site}/slow")).await;
    assert_eq!(status, 502);
    assert!(asked.elapsed() < Duration::from_secs(4), "{:?}", asked.elapsed());
    let (status, _) = ask(node, None, "info", &format!("{site}/page")).await;
    assert_eq!(status, 401);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_links_preview_reaches_the_other_members_client_as_a_card_with_its_picture() {
    let config = format!(
        "[media]\npreview_allow_private_addresses = true\npreview_timeout = 2\npreview_max_page_bytes = \
         {PAGE_CAP}\npreview_max_picture_bytes = {}\n",
        picture_cap()
    );
    for backend in backends() {
        let stage = Stage::start(backend, &config).await;
        let (a, mut b, c, chat) = stage.people().await;
        let (site, hits) = site().await;
        let ic3 = token(&a, IC3).await;
        for page in ["/page", "/moved"] {
            let url = format!("{site}{page}");
            let (status, body) = ask(&stage.rig.first, Some(&ic3), "info", &url).await;
            assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
            let info: Value = serde_json::from_slice(&body).unwrap();
            let id = send_with_preview(&stage, &a, &chat, &url, &info["preview"]).await;
            the_member_sees_the_card(&mut b, &chat, &id, &url).await;
        }
        assert!(hits.load(Ordering::SeqCst) > 0);
        oversized_and_slow_answers_are_refused(stage.rig.other(), &ic3, &site).await;
        drop((a, b, c));
        stage.stop().await;
    }
}

/// Addresses that are not on the public internet, `port` being the test's own site on loopback.
fn private_addresses(port: &str) -> Vec<String> {
    let mut urls = vec![
        format!("http://127.0.0.1:{port}/page"),
        format!("http://localhost:{port}/page"),
        format!("http://[::1]:{port}/page"),
        format!("http://[::ffff:127.0.0.1]:{port}/page"),
        format!("http://2130706433:{port}/page"),
    ];
    let fixed = [
        "http://localhost/page",
        "http://127.0.0.1/page",
        "http://[::1]/page",
        "http://2130706433/page",
        "http://10.0.0.1/",
        "http://192.168.1.1/",
        "http://172.16.0.1/",
        "http://169.254.169.254/latest/meta-data/",
        "http://100.64.0.1/",
        "http://[fd00::1]/",
        "http://[fe80::1]/",
        "http://0.0.0.0/",
        "ftp://example.com/file",
        "file:///etc/passwd",
        "http://user:secret@example.com/",
    ];
    urls.extend(fixed.map(str::to_owned));
    // this machine's own name: it looks like any host's and resolves to an address of the machine
    let name = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    let name = name.trim();
    let own = (name, 80).to_socket_addrs().map_or(0, Iterator::count);
    if !name.is_empty() && own > 0 {
        urls.push(format!("http://{name}/page"));
        urls.push(format!("http://{name}:{port}/page"));
    }
    urls
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn previews_reach_no_loopback_private_or_link_local_address_and_send_nothing_there() {
    for backend in backends() {
        let stage = Stage::start(backend, "").await;
        let (a, b, c, _chat) = stage.people().await;
        let (site, hits) = site().await;
        let port = site.rsplit(':').next().unwrap();
        let ic3 = token(&a, IC3).await;
        for url in private_addresses(port) {
            for what in ["info", "image/Thumbnail"] {
                let (status, _) = ask(stage.rig.other(), Some(&ic3), what, &url).await;
                assert!(matches!(status, 400 | 403), "{what} {url}: {status}");
            }
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0, "a refused address was reached");
        drop((a, b, c));
        stage.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_previews_switched_off_no_page_is_read_and_the_link_is_still_sent() {
    for backend in backends() {
        let stage = Stage::start(
            backend,
            "[media]\nlink_previews = false\npreview_allow_private_addresses = true\n",
        )
        .await;
        let (a, mut b, c, chat) = stage.people().await;
        let (site, hits) = site().await;
        let ic3 = token(&a, IC3).await;
        for what in ["info", "image/Thumbnail"] {
            let (status, _) = ask(&stage.rig.first, Some(&ic3), what, &format!("{site}/page")).await;
            assert_eq!(status, 404, "{what}");
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        let id = a.send(&chat, &format!("see {site}/page")).await;
        b.pushed(0, &chat, &id, MessageChange::New).await;
        b.shows(&chat, &id, &format!("{site}/page")).await;
        drop((a, b, c));
        stage.stop().await;
    }
}
