//! A meeting recap's reads through the transport, against a local mock: MCPS's `contents/` with the meeting-artifacts
//! token (refused without the region's base, which must be `/api/mcps/<region>` on an AFD host), and a transcript's
//! `WebVTT` on its personal site with that site's token. Every id, host and word here is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::recap::{MCPS_AUDIENCE, MeetingContents, TranscriptLocation};
use teams_proto::{Client, Credentials, HttpError, HttpTransport, Refusal};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const MCPS_BASE: &str = "https://teams.microsoft.com/api/mcps/us";
const THREAD: &str = "19:meeting_bWFkZS11cA@thread.v2";
const LOCATION: &str = "https://madeup-my.sharepoint.com/personal/lab_a/_api/v2.1/drives/b!drive1/items/ITEM1/versions/current/media/transcripts/t1/content";

/// Records every audience a token was asked for.
#[derive(Default)]
struct FakeTokens {
    audiences: Mutex<Vec<String>>,
}

impl FakeTokens {
    fn asked(&self) -> Vec<String> {
        self.audiences.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
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

#[tokio::test]
async fn the_contents_are_read_from_mcps_with_the_meeting_artifacts_token() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let client = Client::new(transport(&server, &tokens).with_mcps(MCPS_BASE).unwrap());
    let answer = serde_json::json!({"resources": [{"type": "Recording", "location": "https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/share1", "metadata": {"driveItemId": "ITEM1"}}]});
    Mock::given(method("GET"))
        .and(path("/api/mcps/us/contents/"))
        .and(query_param("threadId", THREAD))
        .and(header("authorization", "Bearer placeholder-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer))
        .expect(1)
        .mount(&server)
        .await;
    let read = client.read(&MeetingContents::of(THREAD)).await.expect("an answer");
    let contents = MeetingContents::read_answer(read.body()).expect("contents");
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].item_id.as_deref(), Some("ITEM1"));
    assert_eq!(tokens.asked(), [MCPS_AUDIENCE]);
}

#[tokio::test]
async fn without_an_mcps_base_the_read_is_refused_and_only_an_mcps_base_is_taken() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let client = Client::new(transport(&server, &tokens));
    assert!(matches!(
        client.read(&MeetingContents::of(THREAD)).await,
        Err(HttpError::Refused(Refusal::NoMcps))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
    for base in [MCPS_BASE, "https://teams.microsoft.com/api/mcps/emea/"] {
        assert!(HttpTransport::<FakeTokens>::accepts_mcps_base(base), "{base}");
    }
    for base in [
        "http://teams.microsoft.com/api/mcps/us",
        "https://teams.example.test/api/mcps/us",
        "https://teams.microsoft.com/api/mcps",
        "https://teams.microsoft.com/api/mcps/US",
        "https://teams.microsoft.com/api/csa/us",
        "https://teams.microsoft.com/api/mcps/us/contents",
        "https://teams.microsoft.com/api/mcps/us?x",
        "https://teams.microsoft.com:8443/api/mcps/us",
        " https://teams.microsoft.com/api/mcps/us",
    ] {
        assert!(!HttpTransport::<FakeTokens>::accepts_mcps_base(base), "{base}");
    }
}

#[tokio::test]
async fn a_transcript_is_read_on_its_personal_site_with_that_sites_token() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let transport = transport(&server, &tokens);
    Mock::given(method("GET"))
        .and(path(
            "/personal/lab_a/_api/v2.1/drives/b!drive1/items/ITEM1/media/transcripts/t1/streamContent",
        ))
        .and(query_param("is", "1"))
        .and(query_param("applymediaedits", "false"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("WEBVTT\n\nu/1-0\n00:00:01.000 --> 00:00:02.000\n<v Ada Lab>Hi.</v>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let location = TranscriptLocation::of(LOCATION).expect("a location");
    let answer = transport.meeting_transcript(&location).await.expect("the transcript");
    assert!(answer.body().starts_with(b"WEBVTT"));
    assert_eq!(tokens.asked(), ["https://madeup-my.sharepoint.com"]);
}

#[tokio::test]
async fn a_transcript_s_languages_are_read_on_its_personal_site_with_that_sites_token() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let transport = transport(&server, &tokens);
    Mock::given(method("GET"))
        .and(path("/personal/lab_a/_api/v2.1/drives/b!drive1/items/ITEM1"))
        .and(query_param("select", "media/transcripts"))
        .and(query_param("$expand", "media/transcripts"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"media": {"transcripts": [{"id": "t1", "languageTag": "en-US"}]}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let location = TranscriptLocation::of(LOCATION).expect("a location");
    let answer = transport
        .meeting_transcript_languages(&location)
        .await
        .expect("the transcripts");
    assert_eq!(
        teams_proto::recap::transcript_language(answer.body(), location.id()).as_deref(),
        Some("en-US")
    );
    assert_eq!(tokens.asked(), ["https://madeup-my.sharepoint.com"]);
}

#[tokio::test]
async fn a_transcript_the_site_refuses_is_an_error() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let transport = transport(&server, &tokens);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let location = TranscriptLocation::of(LOCATION).expect("a location");
    assert!(matches!(
        transport.meeting_transcript(&location).await,
        Err(HttpError::Status(_))
    ));
}
