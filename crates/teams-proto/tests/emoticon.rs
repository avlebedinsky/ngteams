//! A reaction's animation strip on Teams' CDN ([`ReactionAnimation`], `docs/protocol/chat.md`, "Reaction pictures") on
//! wiremock, through the `test-support` mock origin: a `GET` of `<id>/default/50_anim_f.png` under the emoticons'
//! base with no token, its answer kept only when it is a PNG of at most `MAX_ANIMATION` bytes; an id that isn't one
//! path segment of Teams' form makes no read.
//!
//! The strip's bytes are made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_fence::{Fence, FenceLog, Mri, Roster};
use teams_proto::{
    Client, Credentials, HttpError, HttpTransport, MAX_ANIMATION, ReactionAnimation, Read as _, Service,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LAB_A: &str = "8:orgid:0000000a-0000-4000-8000-00000000000a";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const STRIP_PATH: &str = "/evergreen-assets/personal-expressions/v2/assets/emoticons/1f440_eyes/default/50_anim_f.png";

/// Records every audience a token was asked for: the CDN's read asks for none.
#[derive(Default)]
struct FakeTokens {
    audiences: Mutex<Vec<String>>,
}

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(audience.to_owned());
        future::ready(Ok(Secret::new("placeholder-token".to_owned())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

async fn setup() -> (MockServer, Arc<FakeTokens>, Client<HttpTransport<FakeTokens>>) {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let fence = Arc::new(Fence::new(
        Roster::new([Mri::new(LAB_A)], []),
        FenceLog::from_writer(std::io::sink()),
    ));
    let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), fence, CHAT_BASE, &server.uri()).unwrap();
    (server, tokens, Client::new(transport))
}

#[test]
fn only_an_id_of_teams_form_makes_a_read() {
    let read = ReactionAnimation::of("1f440_eyes").unwrap();
    assert_eq!(read.service(), Service::Emoticons);
    assert_eq!(read.endpoint().segments(), ["1f440_eyes", "default", "50_anim_f.png"]);
    assert!(read.endpoint().query().is_empty());
    for id in ["", "../x", "a/b", "yes-tone2", "Eyes", "a.b", &"a".repeat(65)] {
        assert!(ReactionAnimation::of(id).is_none(), "{id:?}");
    }
}

#[tokio::test]
async fn the_strip_is_a_get_with_no_token_and_only_a_png_is_kept() {
    let (server, tokens, client) = setup().await;
    let strip = b"\x89PNG made-up strip".to_vec();
    Mock::given(method("GET"))
        .and(path(STRIP_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(strip.clone(), "image/png"))
        .mount(&server)
        .await;
    let read = ReactionAnimation::of("1f440_eyes").unwrap();
    let answer = client.read(&read).await.unwrap();
    assert_eq!(read.read_answer(&answer), Some(strip));
    let request = server.received_requests().await.unwrap().pop().unwrap();
    assert!(request.headers.get("authorization").is_none());
    assert!(
        tokens
            .audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty(),
        "no token asked for"
    );

    server.reset().await;
    Mock::given(method("GET"))
        .and(path(STRIP_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"<html/>".to_vec(), "text/html"))
        .mount(&server)
        .await;
    assert_eq!(read.read_answer(&client.read(&read).await.unwrap()), None, "not a PNG");

    server.reset().await;
    Mock::given(method("GET"))
        .and(path(STRIP_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0; MAX_ANIMATION + 1], "image/png"))
        .mount(&server)
        .await;
    assert_eq!(read.read_answer(&client.read(&read).await.unwrap()), None, "too large");

    server.reset().await;
    Mock::given(method("GET"))
        .and(path(STRIP_PATH))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    assert!(matches!(client.read(&read).await, Err(HttpError::Status(_))));
}
