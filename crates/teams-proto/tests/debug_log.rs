//! The transport in the app's diagnostic log (M2 Task 24a), the log installed at `debug` as the app installs it: each
//! request at `info` by method, host, path (no query), status and latency; at `debug` (`teams_wire::http`) the
//! request and its answer in full (the URL with its query, every header, the body; a picture by its type and size
//! only), the bearer token, a token field of the answer and its cookie redacted by the logger. One test: the logger is
//! the process's.
//!
//! The token here is made up, JWT-shaped as a real one is.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::future::{self, Future};
use std::sync::Arc;

use serde_json::json;
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_fence::{Fence, FenceLog, Mri, Roster};
use teams_proto::{Endpoint, HttpTransport, Read, Service, Transport};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const CONVERSATIONS: &str = "/api/chatsvc/amer/v1/users/ME/conversations";
const PICTURE: &str = "/v1.0/users/00000000-0000-4000-8000-00000000000b/photos/96x96/$value";

/// A JWT-shaped token, made up.
fn jwt(seed: &str) -> String {
    let body = format!("{seed}DeBuGlOgToKeNaBcD0123456789-_").repeat(5);
    format!("eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9.eyJ{body}.{body}")
}

/// The one token every audience gets.
struct OneToken(String);

impl teams_proto::Credentials for OneToken {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }
}

/// A read of the test's choosing.
struct Get(Service, Endpoint);

impl Read for Get {
    fn service(&self) -> Service {
        self.0
    }

    fn endpoint(&self) -> Endpoint {
        self.1.clone()
    }
}

/// The two reads, answered: the conversation list (with a token field, a cookie and some text) and a picture.
async fn read_both(token: &str, skypetoken: &str, cookie: &str) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CONVERSATIONS))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", format!("session={cookie}; Secure").as_str())
                .set_body_json(json!({"conversations": [], "skypetoken": skypetoken, "note": "visible words"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(PICTURE))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0xff, 0xd8, 0xff, 0xe0], "image/jpeg"))
        .mount(&server)
        .await;
    let fence = Arc::new(Fence::new(
        Roster::new([Mri::new("8:orgid:lab-a")], []),
        FenceLog::from_writer(std::io::sink()),
    ));
    let credentials = Arc::new(OneToken(token.to_owned()));
    let transport = HttpTransport::with_mock_origin(credentials, fence, CHAT_BASE, &server.uri()).unwrap();
    let conversations = Get(
        Service::ChatService,
        Endpoint::new(["v1", "users", "ME", "conversations"]).with_query("view", "msnp24Equivalent"),
    );
    assert_eq!(transport.read(&conversations).await.unwrap().status(), 200);
    let picture = Get(
        Service::Graph,
        Endpoint::new([
            "users",
            "00000000-0000-4000-8000-00000000000b",
            "photos",
            "96x96",
            "$value",
        ]),
    );
    assert_eq!(transport.read(&picture).await.unwrap().status(), 200);
}

#[tokio::test]
async fn at_debug_a_request_and_its_answer_are_logged_in_full_and_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let log = teams_log::install(&teams_log::Config {
        dir: dir.path().to_owned(),
        setting: teams_log::Level::Debug,
        env: None,
        rotation: teams_log::Rotation::default(),
    })
    .unwrap();
    let (token, skypetoken, cookie) = (jwt("t"), "SkYpEtOkEnVaLuE-not-a-jwt", "CoOkIeVaLuE42");
    read_both(&token, skypetoken, cookie).await;
    assert!(log.flush_written());
    let text = fs::read_to_string(log.path()).unwrap();
    for kept in [
        "INFO  teams_proto::http: GET teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations -> HTTP 200 in ",
        "DEBUG teams_wire::http: request GET \
         https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations?view=msnp24Equivalent headers {",
        "authorization: [redacted]",
        "x-ms-migration: True",
        "response GET https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations?view=msnp24Equivalent \
         -> 200 in ",
        "set-cookie: [redacted]",
        r#""skypetoken":"[redacted]""#,
        "visible words",
        "[image/jpeg, 4 bytes]",
    ] {
        assert!(text.contains(kept), "the log has {kept:?}:\n{text}");
    }
    for absent in [token.as_str(), skypetoken, cookie] {
        assert!(!text.contains(absent), "the log holds {absent:?}:\n{text}");
    }
    let info: Vec<&str> = text
        .lines()
        .filter(|line| line.contains(" INFO  teams_proto"))
        .collect();
    assert!(
        info.iter().all(|line| !line.contains('?') && !line.contains("visible")),
        "no query or body at info: {info:?}"
    );
}
