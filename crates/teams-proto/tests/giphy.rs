//! A GIF picked from Teams' GIF picker ([`GiphyPicture`], `docs/protocol/chat.md`, "Pictures in messages") on
//! wiremock, through the `test-support` mock origin: the GIF a message's `<img>` names on Giphy's media host (or a
//! shard of it) read as a `GET` of `media/<id>/<file>` from `media.giphy.com`, with no token and nothing of Giphy's
//! token or the query; its answer kept only when it is a GIF by its signature; a `src` of any other form makes no
//! read. [`InlinePicture`] takes an AMS view or such a GIF.
//!
//! The GIF ids, tokens and bytes are made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_fence::{Fence, FenceLog, Mri, Roster};
use teams_proto::{
    Client, Credentials, GIPHY_HOST, GiphyPicture, HttpError, HttpTransport, InlinePicture, MAX_BODY, MAX_PICTURE_BODY,
    MessagePicture, PastedFormat, Read as _, Service,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LAB_A: &str = "8:orgid:0000000a-0000-4000-8000-00000000000a";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const ID: &str = "MadeUpGif000";
const TOKEN: &str = "v1.Y2lkPW1hZGUtdXAmY3Q9Zw";

/// Records every audience a token was asked for.
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

fn transport(server: &MockServer, tokens: &Arc<FakeTokens>) -> HttpTransport<FakeTokens> {
    let fence = Arc::new(Fence::new(
        Roster::new([Mri::new(LAB_A)], []),
        FenceLog::from_writer(std::io::sink()),
    ));
    HttpTransport::with_mock_origin(Arc::clone(tokens), fence, CHAT_BASE, &server.uri()).unwrap()
}

#[test]
fn only_a_gif_on_giphy_s_media_host_makes_a_read() {
    for src in [
        format!("https://media2.giphy.com/media/{TOKEN}/{ID}/giphy.gif"),
        format!("https://media.giphy.com/media/{ID}/giphy.gif"),
        format!("https://media0.giphy.com/media/{TOKEN}/{ID}/giphy.gif?cid=made-up&rid=giphy.gif"),
        format!("https://media4.giphy.com/media/v1.bWFkZS11cA==/{ID}/giphy.gif"),
    ] {
        let read = GiphyPicture::of(&src).unwrap();
        assert_eq!(read.service(), Service::Giphy, "{src}");
        // the id and the file alone: Giphy's token, the shard and the query stay out
        assert_eq!(read.endpoint().segments(), ["media", ID, "giphy.gif"], "{src}");
        assert!(read.endpoint().query().is_empty());
        assert_eq!((read.id(), read.file()), (ID, "giphy.gif"));
        assert_eq!(InlinePicture::of(&src), Some(InlinePicture::Giphy(read)));
    }
    let renditions = GiphyPicture::of(&format!("https://media.giphy.com/media/{ID}/200w_d.gif")).unwrap();
    assert_eq!(renditions.file(), "200w_d.gif");
    for other in [
        // another host, scheme, port, user or fragment
        format!("https://giphy.com/media/{ID}/giphy.gif"),
        format!("https://i.giphy.com/media/{ID}/giphy.gif"),
        format!("https://media10.giphy.com/media/{ID}/giphy.gif"),
        format!("https://mediax.giphy.com/media/{ID}/giphy.gif"),
        format!("https://media.giphy.com.example.test/media/{ID}/giphy.gif"),
        format!("https://media2.giphy.com.example.test/media/{ID}/giphy.gif"),
        format!("http://media.giphy.com/media/{ID}/giphy.gif"),
        format!("https://media.giphy.com:8443/media/{ID}/giphy.gif"),
        format!("https://user@media.giphy.com/media/{ID}/giphy.gif"),
        format!("https://media.giphy.com/media/{ID}/giphy.gif#x"),
        // another path, id, token or file
        format!("https://media.giphy.com/{ID}/giphy.gif"),
        format!("https://media.giphy.com/media/{ID}/"),
        format!("https://media.giphy.com/media/{ID}/giphy.mp4"),
        format!("https://media.giphy.com/media/{ID}/giphy.webp"),
        format!("https://media.giphy.com/media/{ID}/GIPHY.gif"),
        format!("https://media.giphy.com/media/{ID}/.gif"),
        format!("https://media.giphy.com/media/{ID}/giphy.gif/x"),
        format!("https://media.giphy.com/media/v2.abc/{ID}/giphy.gif"),
        format!("https://media.giphy.com/media/v1./{ID}/giphy.gif"),
        format!("https://media.giphy.com/media/v1.a%2Fb/{ID}/giphy.gif"),
        format!("https://media.giphy.com/media/{TOKEN}/{TOKEN}/{ID}/giphy.gif"),
        "https://media.giphy.com/media/a-b/giphy.gif".to_owned(),
        "https://media.giphy.com/media/../giphy.gif".to_owned(),
        format!("https://media.giphy.com/media/{}/giphy.gif", "a".repeat(65)),
        format!("https://media.giphy.com/media/{ID}/{}.gif", "a".repeat(45)),
        "https://images.example.test/made-up.gif".to_owned(),
        "not a url".to_owned(),
    ] {
        assert!(GiphyPicture::of(&other).is_none(), "{other}");
    }
}

#[test]
fn an_inline_picture_is_an_ams_view_or_a_gif() {
    let view = "https://us-api.asm.skype.com/v1/objects/0-eus-d9-00000000000000000000000000000abc/views/imgo";
    let ams = InlinePicture::of(view).unwrap();
    assert_eq!(ams, InlinePicture::Ams(MessagePicture::of(view).unwrap()));
    assert_eq!(ams.service(), Service::Ams);
    assert!(InlinePicture::of("https://images.example.test/made-up.png").is_none());
}

#[tokio::test]
async fn the_gif_is_read_from_giphy_with_no_token_and_only_a_gif_is_kept() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let client = Client::new(transport(&server, &tokens));
    let gif_path = format!("/media/{ID}/giphy.gif");
    let gif = b"GIF89a made-up frames".to_vec();
    Mock::given(method("GET"))
        .and(path(gif_path.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(gif.clone(), "image/gif"))
        .mount(&server)
        .await;
    let read = InlinePicture::of(&format!(
        "https://media2.giphy.com/media/{TOKEN}/{ID}/giphy.gif?cid=made-up"
    ))
    .unwrap();
    let picture = read.read_answer(&client.read(&read).await.unwrap()).unwrap();
    assert_eq!((picture.format, picture.bytes), (PastedFormat::Gif, gif));
    assert!(
        tokens
            .audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty(),
        "no token asked for"
    );
    let request = server.received_requests().await.unwrap().pop().unwrap();
    assert!(request.body.is_empty());
    assert!(request.url.query().is_none(), "no query: {}", request.url);
    assert!(request.headers.get("authorization").is_none(), "no credential");
    assert_eq!(
        request.headers.get("accept").and_then(|accept| accept.to_str().ok()),
        Some("image/gif")
    );
    assert_eq!(GIPHY_HOST, "media.giphy.com");

    server.reset().await;
    // a picture that isn't a GIF, or no picture, is none
    for (bytes, kind) in [
        (b"\x89PNG\r\n\x1a\nmade-up".to_vec(), "image/png"),
        (b"<html></html>".to_vec(), "text/html"),
    ] {
        Mock::given(method("GET"))
            .and(path(gif_path.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_raw(bytes, kind))
            .mount(&server)
            .await;
        assert_eq!(read.read_answer(&client.read(&read).await.unwrap()), None, "{kind}");
        server.reset().await;
    }

    Mock::given(method("GET"))
        .and(path(gif_path.as_str()))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    assert!(matches!(client.read(&read).await, Err(HttpError::Status(_))));
}

#[tokio::test]
async fn a_gif_larger_than_a_plain_body_is_read_up_to_a_picture_s_limit() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let client = Client::new(transport(&server, &tokens));
    let gif_path = format!("/media/{ID}/giphy.gif");
    let read = GiphyPicture::of(&format!("https://media2.giphy.com/media/{ID}/giphy.gif")).unwrap();
    // a full-size GIF past the 4 MB a plain body may take: read whole
    let mut large = b"GIF89a".to_vec();
    large.resize(MAX_BODY + 1, 0);
    Mock::given(method("GET"))
        .and(path(gif_path.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(large.clone(), "image/gif"))
        .mount(&server)
        .await;
    let picture = read.read_answer(&client.read(&read).await.unwrap()).unwrap();
    assert_eq!(picture.bytes.len(), large.len());

    // past a picture's limit: too large
    server.reset().await;
    large.resize(MAX_PICTURE_BODY + 1, 0);
    Mock::given(method("GET"))
        .and(path(gif_path.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(large, "image/gif"))
        .mount(&server)
        .await;
    assert!(matches!(client.read(&read).await, Err(HttpError::BodyTooLarge)));
}
