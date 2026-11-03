//! A picture pasted into a message ([`MessagePicture`], `docs/protocol/chat.md`, "Pictures in messages") on wiremock,
//! through the `test-support` mock origin: the view a message's `<img>` names (on the media service's host or the
//! async gateway's) read as a `GET` of `v1/objects/<id>/views/<view>` from the account's AMS base, with its `ic3` token
//! and `x-ms-migration: True`; its answer kept only when it is a picture by its signature; a `src` of any other form
//! makes no read, and a transport without an AMS base refuses it.
//!
//! The object ids and bytes are made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::{
    Client, Credentials, HttpError, HttpTransport, IC3_AUDIENCE, MAX_BODY, MessagePicture, PastedFormat, Read as _,
    Refusal, Service,
};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const AMS_BASE: &str = "https://us-prod.asyncgw.teams.microsoft.com";
const OBJECT: &str = "0-eus-d9-00000000000000000000000000000abc";

fn src(host: &str, view: &str) -> String {
    format!("https://{host}/v1/objects/{OBJECT}/views/{view}")
}

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
    HttpTransport::with_mock_origin(Arc::clone(tokens), CHAT_BASE, &server.uri()).unwrap()
}

#[test]
fn only_a_view_of_an_ams_object_makes_a_read() {
    for host in ["us-api.asm.skype.com", "us-prod.asyncgw.teams.microsoft.com"] {
        for view in ["imgo", "imgpsh_fullsize", "imgpsh_fullsize_anim"] {
            let read = MessagePicture::of(&src(host, view)).unwrap();
            assert_eq!(read.service(), Service::Ams);
            assert_eq!(read.endpoint().segments(), ["v1", "objects", OBJECT, "views", view]);
            assert!(read.endpoint().query().is_empty());
            assert_eq!((read.object(), read.view()), (OBJECT, view));
        }
    }
    for other in [
        // another host, scheme, port, user, query or fragment
        format!("https://media1.giphy.com/v1/objects/{OBJECT}/views/imgo"),
        format!("https://asm.skype.com.example.test/v1/objects/{OBJECT}/views/imgo"),
        format!("https://x.y.asm.skype.com/v1/objects/{OBJECT}/views/imgo"),
        format!("http://us-api.asm.skype.com/v1/objects/{OBJECT}/views/imgo"),
        format!("https://us-api.asm.skype.com:8443/v1/objects/{OBJECT}/views/imgo"),
        format!("https://user@us-api.asm.skype.com/v1/objects/{OBJECT}/views/imgo"),
        format!("{}?x=1", src("us-api.asm.skype.com", "imgo")),
        format!("{}#x", src("us-api.asm.skype.com", "imgo")),
        // another path or view
        format!("https://us-api.asm.skype.com/v1/objects/{OBJECT}/"),
        format!("https://us-api.asm.skype.com/v1/objects/{OBJECT}/views/imgo/status"),
        format!("https://us-api.asm.skype.com/v1/objects/{OBJECT}/content/imgpsh"),
        format!("https://us-api.asm.skype.com/v1/{OBJECT}/objects/{OBJECT}/views/imgo"),
        src("us-api.asm.skype.com", "video"),
        "https://us-api.asm.skype.com/v1/objects/../views/imgo".to_owned(),
        "https://us-api.asm.skype.com/v1/objects/a%2Fb/views/imgo".to_owned(),
        format!("https://us-api.asm.skype.com/v1/objects/{}/views/imgo", "a".repeat(129)),
        "https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/hi/default/20_f.png"
            .to_owned(),
        "not a url".to_owned(),
    ] {
        assert!(MessagePicture::of(&other).is_none(), "{other}");
    }
}

#[test]
fn a_picture_is_known_by_its_signature() {
    let cases: [(&[u8], Option<PastedFormat>); 6] = [
        (b"\x89PNG\r\n\x1a\nmade-up", Some(PastedFormat::Png)),
        (&[0xFF, 0xD8, 0xFF, 0xE0, 0], Some(PastedFormat::Jpeg)),
        (b"GIF89a made-up", Some(PastedFormat::Gif)),
        (b"RIFF\0\0\0\0WEBPVP8 ", Some(PastedFormat::Webp)),
        (b"<html></html>", None),
        (b"", None),
    ];
    for (bytes, format) in cases {
        assert_eq!(PastedFormat::sniff(bytes), format, "{bytes:?}");
    }
}

#[tokio::test]
async fn the_view_is_read_from_the_ams_base_with_the_ic3_token_and_only_a_picture_is_kept() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let client = Client::new(transport(&server, &tokens).with_ams(AMS_BASE).unwrap());
    let view_path = format!("/v1/objects/{OBJECT}/views/imgo");
    let png = b"\x89PNG\r\n\x1a\nmade-up picture".to_vec();
    Mock::given(method("GET"))
        .and(path(view_path.as_str()))
        .and(header("authorization", "Bearer placeholder-token"))
        .and(header("x-ms-migration", "True"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(png.clone(), "image/png"))
        .mount(&server)
        .await;
    // the message names the media service's host; the read goes to the account's AMS base all the same
    let read = MessagePicture::of(&src("us-api.asm.skype.com", "imgo")).unwrap();
    let picture = read.read_answer(&client.read(&read).await.unwrap()).unwrap();
    assert_eq!((picture.format, picture.bytes), (PastedFormat::Png, png));
    assert_eq!(
        *tokens.audiences.lock().unwrap_or_else(PoisonError::into_inner),
        [IC3_AUDIENCE]
    );
    let request = server.received_requests().await.unwrap().pop().unwrap();
    assert!(request.body.is_empty());
    assert!(
        request
            .headers
            .get("accept")
            .and_then(|accept| accept.to_str().ok())
            .is_some_and(|accept| accept.contains("image/")),
        "an image's Accept"
    );

    server.reset().await;
    Mock::given(method("GET"))
        .and(path(view_path.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"{}".to_vec(), "application/json"))
        .mount(&server)
        .await;
    assert_eq!(
        read.read_answer(&client.read(&read).await.unwrap()),
        None,
        "not a picture"
    );

    server.reset().await;
    Mock::given(method("GET"))
        .and(path(view_path.as_str()))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    assert!(matches!(client.read(&read).await, Err(HttpError::Status(_))));
}

#[tokio::test]
async fn without_an_ams_base_the_read_is_refused_and_only_an_asyncgw_base_is_taken() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let client = Client::new(transport(&server, &tokens));
    let read = MessagePicture::of(&src("us-api.asm.skype.com", "imgo")).unwrap();
    assert!(matches!(
        client.read(&read).await,
        Err(HttpError::Refused(Refusal::NoAms))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());

    for base in [AMS_BASE, "https://eu-prod.asyncgw.teams.microsoft.com/"] {
        assert!(HttpTransport::<FakeTokens>::accepts_ams_base(base), "{base}");
    }
    for base in [
        "http://us-prod.asyncgw.teams.microsoft.com",
        "https://us-api.asm.skype.com",
        "https://asyncgw.teams.microsoft.com",
        "https://us-prod.asyncgw.teams.microsoft.com.example.test",
        "https://us-prod.asyncgw.teams.microsoft.com:8443",
        "https://us-prod.asyncgw.teams.microsoft.com/v1",
        "https://us-prod.asyncgw.teams.microsoft.com?x",
        "https://US_PROD.asyncgw.teams.microsoft.com",
        " https://us-prod.asyncgw.teams.microsoft.com",
    ] {
        assert!(!HttpTransport::<FakeTokens>::accepts_ams_base(base), "{base}");
    }
}

#[tokio::test]
async fn a_picture_larger_than_a_plain_body_is_read_from_ams() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let client = Client::new(transport(&server, &tokens).with_ams(AMS_BASE).unwrap());
    let mut large = b"\x89PNG\r\n\x1a\n".to_vec();
    large.resize(MAX_BODY + 1, 0);
    Mock::given(method("GET"))
        .and(path(format!("/v1/objects/{OBJECT}/views/imgo")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(large.clone(), "image/png"))
        .mount(&server)
        .await;
    let read = MessagePicture::of(&src("us-api.asm.skype.com", "imgo")).unwrap();
    let picture = read.read_answer(&client.read(&read).await.unwrap()).unwrap();
    assert_eq!(
        picture.bytes.len(),
        large.len(),
        "a photo's original past the 4 MB a plain body may take"
    );
}
