//! A sticker from Teams' own packs ([`StickerPicture`], `docs/protocol/chat.md`, "Stickers") on wiremock, through the
//! `test-support` mock origin: the GIF a message's `<img>` names on Teams' sticker CDN read as a `GET` of
//! `evergreen-assets/stickerassets/<folder>[/<locale>]/<file>` from that CDN, with no token and no query; its answer
//! kept only when it is a GIF by its signature; a `src` of any other form makes no read.
//!
//! The pack folders, file names and bytes are made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::{
    Client, Credentials, HttpError, HttpTransport, InlinePicture, MAX_BODY, PastedFormat, Read as _, STICKERS_HOST,
    Service, StickerPicture,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";

/// A made-up sticker as a message names it, on the host the web client reads its stickers from.
const SRC: &str = "https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/madeup-250x250/en-us/MadeUp_Hi.gif?v=5";

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

#[test]
fn only_a_gif_in_teams_sticker_assets_makes_a_read() {
    let read = StickerPicture::of(SRC).unwrap();
    assert_eq!(read.service(), Service::Stickers);
    assert_eq!(read.endpoint().segments(), ["madeup-250x250", "en-us", "MadeUp_Hi.gif"]);
    assert!(read.endpoint().query().is_empty(), "the query stays out");
    assert_eq!(
        (read.folder(), read.locale(), read.file()),
        ("madeup-250x250", Some("en-us"), "MadeUp_Hi.gif")
    );
    assert_eq!(InlinePicture::of(SRC), Some(InlinePicture::Sticker(read.clone())));
    // Teams' static CDN host serves the same files, read from the sticker host; a path with no locale too
    let statics =
        "https://statics.teams.cdn.office.net/evergreen-assets/stickerassets/madeup-250x250/en-us/MadeUp_Hi.gif";
    assert_eq!(StickerPicture::of(statics), Some(read));
    let plain = StickerPicture::of(
        "https://statics.teams.cdn.office.net/evergreen-assets/stickerassets/meme-500x500/Made-Up_1.gif?v=5",
    )
    .unwrap();
    assert_eq!(plain.endpoint().segments(), ["meme-500x500", "Made-Up_1.gif"]);
    assert_eq!(plain.locale(), None);
    let base = "evergreen-assets/stickerassets";
    for other in [
        // another host, scheme, port, user or fragment
        format!("https://teams.public.onecdn.static.microsoft.example.test/{base}/a-250x250/A.gif"),
        format!("https://media.giphy.com/{base}/a-250x250/A.gif"),
        format!("http://{STICKERS_HOST}/{base}/a-250x250/A.gif"),
        format!("https://{STICKERS_HOST}:8443/{base}/a-250x250/A.gif"),
        format!("https://user@{STICKERS_HOST}/{base}/a-250x250/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/A.gif#x"),
        // another path, folder, locale or file
        format!("https://{STICKERS_HOST}/evergreen-assets/funstuff/stickers/en-us/assets/A/Preview/A.png"),
        format!("https://{STICKERS_HOST}/{base}/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/A-250x250/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/a_250/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/EN-US/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/e/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/en-us/x/A.gif"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/A.png"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/.gif"),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/A%20B.gif"),
        format!("https://{STICKERS_HOST}/{base}/../A.gif"),
        format!("https://{STICKERS_HOST}/{base}/{}/A.gif", "a".repeat(65)),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/{}/A.gif", "e".repeat(17)),
        format!("https://{STICKERS_HOST}/{base}/a-250x250/{}.gif", "A".repeat(93)),
        "not a url".to_owned(),
    ] {
        assert!(StickerPicture::of(&other).is_none(), "{other}");
    }
}

#[tokio::test]
async fn the_sticker_is_read_from_its_cdn_with_no_token_and_only_a_gif_is_kept() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let client = Client::new(HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap());
    let gif_path = "/evergreen-assets/stickerassets/madeup-250x250/en-us/MadeUp_Hi.gif";
    // past the 4 MB a plain body may take: a sticker is read up to a picture's limit
    let mut gif = b"GIF89a made-up frames".to_vec();
    gif.resize(MAX_BODY + 1, 0);
    Mock::given(method("GET"))
        .and(path(gif_path))
        .respond_with(ResponseTemplate::new(200).set_body_raw(gif.clone(), "image/gif"))
        .mount(&server)
        .await;
    let read = InlinePicture::of(SRC).unwrap();
    let picture = read.read_answer(&client.read(&read).await.unwrap()).unwrap();
    assert_eq!((picture.format, picture.bytes.len()), (PastedFormat::Gif, gif.len()));
    assert!(
        tokens
            .audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty(),
        "no token asked for"
    );
    let request = server.received_requests().await.unwrap().pop().unwrap();
    assert!(request.url.query().is_none(), "no query: {}", request.url);
    assert!(request.headers.get("authorization").is_none(), "no credential");
    assert_eq!(
        request.headers.get("accept").and_then(|accept| accept.to_str().ok()),
        Some("image/gif")
    );

    server.reset().await;
    // a picture that isn't a GIF is none; a missing one an error
    Mock::given(method("GET"))
        .and(path(gif_path))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"\x89PNG\r\n\x1a\nmade-up".to_vec(), "image/png"))
        .mount(&server)
        .await;
    assert_eq!(read.read_answer(&client.read(&read).await.unwrap()), None);
    server.reset().await;
    Mock::given(method("GET"))
        .and(path(gif_path))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    assert!(matches!(client.read(&read).await, Err(HttpError::Status(_))));
}
