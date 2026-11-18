//! What a shared file's sharing link lets people do, read through the engine on the file's personal site as "Share
//! this file" reads it, served only on loopback: the link's scope and words; a refusal is unavailable, a read that
//! failed on the way unread, and a link off the account's file service is never asked for. Every link and id here
//! is made up.

#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::{EngineError, Outbox};
use teams_mock::{CHAT_BASE, LAB_A};
use teams_proto::chat::share_token;
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OBJECT: &str = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/Documents/Microsoft%20Teams%20Chat%20Files/made-up.mp4";
const SHARE: &str = "https://contoso-my.sharepoint.com/:v:/g/personal/made_up_user_contoso_test/MadeUpLink";

struct Tokens;

impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("synthetic-site-token")))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

fn running_engine(server: &MockServer, dir: &tempfile::TempDir) -> EngineHandle {
    let tokens = Arc::new(Tokens);
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("share.sqlite")).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "Lab A")),
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap()
}

#[tokio::test]
async fn a_links_permission_is_read_on_the_files_site() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let handle = running_engine(&server, &dir);
    let permission = format!(
        "/personal/made_up_user_contoso_test/_api/v2.0/shares/{}/permission",
        share_token(SHARE)
    );
    Mock::given(method("GET"))
        .and(path(permission.as_str()))
        .and(header("authorization", "Bearer synthetic-site-token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"roles": ["write"],
            "link": {"scope": "anonymous", "type": "edit", "localizedDescription": "Anyone with the link can edit"}})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let read = handle.link_permission(OBJECT, SHARE).await.unwrap().unwrap();
    assert_eq!(read.scope, "anonymous");
    assert_eq!(read.description.as_deref(), Some("Anyone with the link can edit"));
    // no link in the answer, refused, failed on the way (the transport's retry too)
    for (answer, times) in [
        (
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"roles": ["write"]})),
            1,
        ),
        (ResponseTemplate::new(403), 1),
        (ResponseTemplate::new(503), 2),
    ] {
        Mock::given(method("GET"))
            .respond_with(answer)
            .up_to_n_times(times)
            .mount(&server)
            .await;
    }
    assert_eq!(handle.link_permission(OBJECT, SHARE).await.unwrap(), None);
    assert!(matches!(
        handle.link_permission(OBJECT, SHARE).await,
        Err(EngineError::FileUnavailable)
    ));
    assert!(matches!(
        handle.link_permission(OBJECT, SHARE).await,
        Err(EngineError::FileUnread)
    ));
    // a link off the account's file service is never asked for
    let asked = server.received_requests().await.unwrap().len();
    assert!(matches!(
        handle.link_permission(OBJECT, "https://files.example/made-up").await,
        Err(EngineError::FileUnavailable)
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), asked);
    handle.shutdown();
}
