//! The pictures messages show, cached across runs, through two engines run one after the other on the same store,
//! against wiremock as AMS, Graph and SharePoint's media host (the transport's `test-support` mock origin): a
//! message's picture and a file's thumbnail read by the first run are shown by the second without a read; a picture
//! the service has none of isn't cached, so the next run reads it again; the cache is a private file beside the store.
//!
//! Tokens, ids and links here are made up; the thumbnails' answer follows the synthetic fixture
//! (`tests/fixtures/graph/thumbnails.json`).

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_fence::{FailClosed, Fence, FenceLog, Mri, Roster};
use teams_proto::{Author, Client, Credentials, FileThumbnails, HttpTransport, PastedFormat};
use teams_store::{PictureCache, Store};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LAB_A: &str = "8:orgid:0000000a-0000-4000-8000-00000000000a";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const AMS_BASE: &str = "https://us-prod.asyncgw.teams.microsoft.com";
const OBJECT: &str = "0-eus-d9-00000000000000000000000000000abc";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nmade-up picture";
const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
const THUMBNAILS: &str = include_str!("../../../tests/fixtures/graph/thumbnails.json");
const SHARE: &str = "https://contoso-my.sharepoint.com/:v:/g/personal/made_up_user_contoso_test/MadeUpShareLink0001";

fn src(view: &str) -> String {
    format!("https://us-api.asm.skype.com/v1/objects/{OBJECT}/views/{view}")
}

fn view_path(view: &str) -> String {
    format!("/v1/objects/{OBJECT}/views/{view}")
}

/// Placeholder tokens for every audience.
struct FakeTokens;

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

/// An engine of lab account a on the store at `store_file`, its reads on `server`.
fn engine(server: &MockServer, store_file: &Path) -> EngineHandle {
    let fence = Arc::new(Fence::new(
        Roster::new([Mri::new(LAB_A)], []),
        FenceLog::from_writer(std::io::sink()),
    ));
    let tokens = Arc::new(FakeTokens);
    let transport =
        || HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &server.uri()).unwrap();
    let client = Arc::new(Client::new(transport().with_ams(AMS_BASE).unwrap()));
    let author = Author::new(Mri::new(LAB_A), "lab-a");
    let outbox = Outbox::new(Arc::new(transport()), Arc::clone(&fence), Arc::new(FailClosed), author);
    engine::start(Parts {
        calls: None,
        store: Store::open(store_file).unwrap(),
        client,
        outbox,
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap()
}

fn store_file(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("store").join("a.sqlite")
}

async fn requests_to(server: &MockServer, wanted: &str) -> usize {
    let requests = server.received_requests().await.unwrap();
    requests.iter().filter(|request| request.url.path() == wanted).count()
}

#[tokio::test]
async fn a_picture_read_once_is_shown_from_the_cache_by_the_next_run() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(view_path("imgo")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(PNG.to_vec(), "image/png"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store_file = store_file(&dir);

    let first = engine(&server, &store_file);
    let read = first.requests().message_picture(src("imgo")).await.unwrap().unwrap();
    assert_eq!((read.format, read.bytes.as_slice()), (PastedFormat::Png, PNG));
    // the same run answers from memory
    let again = first.requests().message_picture(src("imgo")).await.unwrap().unwrap();
    assert_eq!(again.bytes, PNG);
    first.shutdown();
    assert_eq!(requests_to(&server, &view_path("imgo")).await, 1);

    let cache = PictureCache::file_beside(&store_file);
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&cache), 0o600, "the cache is private");
    assert_eq!(mode(cache.parent().unwrap()), 0o700);

    // the next run: the same picture, by a `src` on another of AMS's hosts, without a read
    let second = engine(&server, &store_file);
    let other_host = src("imgo").replace("us-api.asm.skype.com", "us-prod.asyncgw.teams.microsoft.com");
    let cached = second.requests().message_picture(other_host).await.unwrap().unwrap();
    assert_eq!((cached.format, cached.bytes.as_slice()), (PastedFormat::Png, PNG));
    second.shutdown();
    assert_eq!(
        requests_to(&server, &view_path("imgo")).await,
        1,
        "no read in the second run"
    );
}

#[tokio::test]
async fn a_picture_the_service_has_none_of_is_read_again_by_the_next_run() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(view_path("imgpsh_fullsize")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store_file = store_file(&dir);
    for run in 1..=2 {
        let handle = engine(&server, &store_file);
        let answer = handle.requests().message_picture(src("imgpsh_fullsize")).await.unwrap();
        assert!(answer.is_none());
        handle.shutdown();
        assert_eq!(requests_to(&server, &view_path("imgpsh_fullsize")).await, run);
    }
}

#[tokio::test]
async fn a_files_thumbnail_read_once_is_shown_from_the_cache_by_the_next_run() {
    let server = MockServer::start().await;
    let share = FileThumbnails::of(SHARE).unwrap();
    let listed = format!("/v1.0/shares/{}/driveItem/thumbnails", share.share());
    let mut answer: Value = serde_json::from_str::<Value>(THUMBNAILS).unwrap()["answer"].clone();
    answer["value"][0]["large"]["url"] = Value::String(
        "https://westus1-mediap.svc.ms/transform/thumbnail?provider=spo&cs=made-up&docid=made-up&width=800".to_owned(),
    );
    Mock::given(method("GET"))
        .and(path(listed.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(answer.to_string(), "application/json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/transform/thumbnail"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(JPEG.to_vec(), "image/jpeg"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store_file = store_file(&dir);

    let first = engine(&server, &store_file);
    let read = first.requests().file_thumbnail(SHARE).await.unwrap().unwrap();
    assert_eq!((read.format, read.bytes.as_slice()), (PastedFormat::Jpeg, JPEG));
    first.shutdown();

    let second = engine(&server, &store_file);
    let cached = second.requests().file_thumbnail(SHARE).await.unwrap().unwrap();
    assert_eq!(cached.bytes, JPEG);
    second.shutdown();
    assert_eq!(requests_to(&server, &listed).await, 1, "Graph read once");
    assert_eq!(
        requests_to(&server, "/transform/thumbnail").await,
        1,
        "the picture read once"
    );
}
