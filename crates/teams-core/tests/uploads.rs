//! Pictures pasted into a message and files attached to one, through the engine's requests, against wiremock as the
//! async gateway (AMS), Graph and SharePoint (the transport's `test-support` mock origin): a picture is sent as an
//! object only its conversation may read, then its bytes; each write is tried once, again only after a short throttle
//! or a rejected token; a read-only app, an oversized picture or an unreadable answer sends nothing more. A file
//! upload is refused by a read-only app and for a file that can't be read, before any request.
//!
//! Tokens, ids and links here are made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::{EngineError, Outbox};
use teams_proto::{Author, Client, Credentials, HttpTransport, MAX_SENT_PICTURE, Mri, PastedFormat, PastedPicture};
use teams_store::Store;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LAB_A: &str = "8:orgid:0000000a-0000-4000-8000-00000000000a";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const AMS_BASE: &str = "https://us-prod.asyncgw.teams.microsoft.com";
const CHAT: &str = "19:made-up-group@thread.v2";
const OBJECT: &str = "0-eus-d9-00000000000000000000000000000abc";
const CREATE: &str = "/v1/objects/";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nmade-up picture";

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

/// How the engine's transports are made.
#[derive(Clone, Copy)]
struct Setup {
    /// The outbox's transport knows the async gateway.
    ams: bool,
    /// The app is read-only.
    read_only: bool,
}

const WRITING: Setup = Setup {
    ams: true,
    read_only: false,
};

/// An engine of lab account a against `server`, made as `setup` says.
fn engine(server: &MockServer, dir: &tempfile::TempDir, setup: Setup) -> EngineHandle {
    let tokens = Arc::new(FakeTokens);
    let transport = || {
        let made = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri())
            .unwrap()
            .for_account(Mri::new(LAB_A));
        let made = if setup.ams {
            made.with_ams(AMS_BASE).unwrap()
        } else {
            made
        };
        if setup.read_only { made.read_only() } else { made }
    };
    let client = Arc::new(Client::new(transport()));
    let outbox = Outbox::new(Arc::new(transport()), Author::new(Mri::new(LAB_A), "lab-a"));
    engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("a.sqlite")).unwrap(),
        client,
        outbox,
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap()
}

fn png() -> PastedPicture {
    PastedPicture {
        format: PastedFormat::Png,
        bytes: PNG.to_vec(),
    }
}

/// The text of an attachment failure (which the log names by its kind alone); any other error fails the test.
fn why(error: EngineError) -> String {
    assert_eq!(error.kind(), "attachment");
    match error {
        EngineError::Attachment { why } => why,
        other => panic!("expected an attachment failure, got {other:?}"),
    }
}

/// Answers the object's creation with `status` and `body`, at most `times` times.
async fn create_answers(server: &MockServer, status: u16, body: &Value, times: u64) {
    Mock::given(method("POST"))
        .and(path(CREATE))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .up_to_n_times(times)
        .with_priority(1)
        .mount(server)
        .await;
}

/// The gateway as it answers a picture's send: the object made, its bytes put into it.
async fn gateway_accepts(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(CREATE))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": OBJECT })))
        .with_priority(5)
        .mount(server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/v1/objects/{OBJECT}/content/imgpsh")))
        .respond_with(ResponseTemplate::new(201))
        .with_priority(5)
        .mount(server)
        .await;
}

async fn sent(server: &MockServer, verb: &str) -> Vec<wiremock::Request> {
    let requests = server.received_requests().await.unwrap();
    requests
        .into_iter()
        .filter(|request| request.method.as_str() == verb)
        .collect()
}

#[tokio::test]
async fn a_pasted_picture_is_sent_as_an_object_only_its_conversation_reads() {
    let server = MockServer::start().await;
    gateway_accepts(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &dir, WRITING);

    let object = handle.requests().upload_picture(CHAT.to_owned(), png()).await.unwrap();
    handle.shutdown();

    assert_eq!(object, OBJECT);
    let created = sent(&server, "POST").await;
    assert_eq!(created.len(), 1, "one object made");
    let body: Value = serde_json::from_slice(&created[0].body).unwrap();
    assert_eq!(body["permissions"], json!({ CHAT: ["read"] }), "only the chat reads it");
    assert_eq!(body["type"], "pish/image");
    assert_eq!(body["filename"], "MicrosoftTeams-image.png");
    let filled = sent(&server, "PUT").await;
    assert_eq!(filled.len(), 1, "the bytes put once");
    assert_eq!(filled[0].body, PNG);
}

#[tokio::test]
async fn a_short_throttle_or_a_rejected_token_is_tried_again_once() {
    let server = MockServer::start().await;
    gateway_accepts(&server).await;
    // the first creation is throttled (the default short wait), the bytes' first put rejects the token
    create_answers(&server, 429, &json!({}), 1).await;
    Mock::given(method("PUT"))
        .and(path(format!("/v1/objects/{OBJECT}/content/imgpsh")))
        .respond_with(ResponseTemplate::new(401))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &dir, WRITING);

    let object = handle.requests().upload_picture(CHAT.to_owned(), png()).await.unwrap();
    handle.shutdown();

    assert_eq!(object, OBJECT);
    assert_eq!(
        sent(&server, "POST").await.len(),
        2,
        "the throttled creation tried again"
    );
    assert_eq!(sent(&server, "PUT").await.len(), 2, "the rejected put tried again");
}

#[tokio::test]
async fn a_token_rejected_twice_is_the_pictures_failure() {
    let server = MockServer::start().await;
    create_answers(&server, 401, &json!({}), 5).await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &dir, WRITING);

    let error = handle
        .requests()
        .upload_picture(CHAT.to_owned(), png())
        .await
        .unwrap_err();
    handle.shutdown();

    assert_eq!(why(error), "The picture service answered 401. Try again.");
    assert_eq!(
        sent(&server, "POST").await.len(),
        2,
        "one more attempt with a new token, no more"
    );
    assert!(sent(&server, "PUT").await.is_empty(), "no object, no bytes");
}

#[tokio::test]
async fn a_refused_or_unreadable_creation_sends_no_bytes() {
    let cases: [(u16, Value, &str); 2] = [
        (500, json!({}), "The picture service answered 500. Try again."),
        (
            201,
            json!({ "id": "not an object/id" }),
            "The picture service's answer couldn't be read. Try again.",
        ),
    ];
    for (status, body, expected) in cases {
        let server = MockServer::start().await;
        create_answers(&server, status, &body, 5).await;
        let dir = tempfile::tempdir().unwrap();
        let handle = engine(&server, &dir, WRITING);

        let error = handle
            .requests()
            .upload_picture(CHAT.to_owned(), png())
            .await
            .unwrap_err();
        handle.shutdown();

        assert_eq!(why(error), expected);
        assert_eq!(sent(&server, "POST").await.len(), 1, "a {status} isn't tried again");
        assert!(sent(&server, "PUT").await.is_empty());
    }
}

#[tokio::test]
async fn a_failed_put_of_the_bytes_is_the_pictures_failure() {
    let server = MockServer::start().await;
    gateway_accepts(&server).await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(403))
        .with_priority(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &dir, WRITING);

    let error = handle
        .requests()
        .upload_picture(CHAT.to_owned(), png())
        .await
        .unwrap_err();
    handle.shutdown();

    assert_eq!(why(error), "The picture service answered 403. Try again.");
    assert_eq!(sent(&server, "PUT").await.len(), 1);
}

#[tokio::test]
async fn pictures_that_cant_be_sent_send_nothing() {
    let server = MockServer::start().await;
    gateway_accepts(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &dir, WRITING);
    let requests = handle.requests();

    let too_large = PastedPicture {
        format: PastedFormat::Png,
        bytes: vec![0; MAX_SENT_PICTURE + 1],
    };
    let error = requests.upload_picture(CHAT.to_owned(), too_large).await.unwrap_err();
    assert_eq!(
        why(error),
        "This picture is too large to send. Send it as a file instead."
    );
    let error = requests.upload_picture(String::new(), png()).await.unwrap_err();
    assert_eq!(why(error), "This picture can't be sent here.");
    handle.shutdown();

    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_empty_picture_makes_no_object_and_puts_no_bytes() {
    let server = MockServer::start().await;
    gateway_accepts(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &dir, WRITING);
    let empty = PastedPicture {
        format: PastedFormat::Png,
        bytes: Vec::new(),
    };

    let error = handle
        .requests()
        .upload_picture(CHAT.to_owned(), empty)
        .await
        .unwrap_err();
    handle.shutdown();

    assert_eq!(why(error), "This picture can't be sent.");
    assert!(sent(&server, "POST").await.is_empty(), "no object is made for it");
    assert!(sent(&server, "PUT").await.is_empty());
}

#[tokio::test]
async fn a_read_only_app_sends_no_picture_and_uploads_no_file() {
    let server = MockServer::start().await;
    gateway_accepts(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(
        &server,
        &dir,
        Setup {
            ams: true,
            read_only: true,
        },
    );
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, b"made-up notes").unwrap();

    let picture = handle
        .requests()
        .upload_picture(CHAT.to_owned(), png())
        .await
        .unwrap_err();
    let upload = handle.requests().upload_file(file).await.unwrap_err();
    handle.shutdown();

    assert_eq!(why(picture), "This app is read-only.");
    assert_eq!(why(upload), "This app is read-only.");
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "no request at all"
    );
}

#[tokio::test]
async fn without_the_gateway_the_picture_is_not_sent() {
    let server = MockServer::start().await;
    gateway_accepts(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(
        &server,
        &dir,
        Setup {
            ams: false,
            read_only: false,
        },
    );

    let error = handle
        .requests()
        .upload_picture(CHAT.to_owned(), png())
        .await
        .unwrap_err();
    handle.shutdown();

    assert!(!why(error).is_empty(), "the refusal's reason is the failure");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_file_that_cant_be_read_is_not_uploaded() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let handle = engine(&server, &dir, WRITING);
    let requests = handle.requests();

    let missing = requests.upload_file(dir.path().join("missing.txt")).await.unwrap_err();
    let folder = requests.upload_file(dir.path().to_path_buf()).await.unwrap_err();
    let nameless = requests.upload_file(PathBuf::from("/")).await.unwrap_err();
    handle.shutdown();

    assert_eq!(
        why(missing),
        "The file can't be read. Check its permissions and choose it again."
    );
    assert_eq!(why(folder), "Choose a file, rather than a folder.");
    assert_eq!(why(nameless), "The file's name can't be read.");
    assert!(server.received_requests().await.unwrap().is_empty());
}
