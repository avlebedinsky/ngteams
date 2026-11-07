//! A meeting's recap read through the engine (`docs/protocol/recap.md`): what the meeting left, from MCPS, and its
//! transcript's `WebVTT`, from the personal site that holds it, served by wiremock through the transport's mock origin.
//! An answer in another shape, a failed read, an app without MCPS and a transcript address the app doesn't read are all
//! [`EngineError::RecapUnread`]. Every link, id and byte here is made up.

#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;

use serde_json::json;
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::{EngineError, Outbox};
use teams_mock::{CHAT_BASE, LAB_A};
use teams_proto::recap::ContentKind;
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MCPS: &str = "https://teams.microsoft.com/api/mcps/amer";
const THREAD: &str = "19:meeting_MadeUpMeeting01@thread.v2";
const RECORDING: &str = "https://contoso-my.sharepoint.com/:v:/g/personal/made_up_user_contoso_test/MadeUpRecording01";
const TRANSCRIPT: &str = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/_api/v2.1/drives/b!madeup/items/ITEM1/versions/current/media/transcripts/t1/content";
const VTT_PATH: &str =
    "/personal/made_up_user_contoso_test/_api/v2.1/drives/b!madeup/items/ITEM1/media/transcripts/t1/streamContent";
const VTT: &str = "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\n<v Lab A>Made-up words.</v>\n";

struct Tokens;

impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("synthetic-recap-token")))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

struct Rig {
    server: MockServer,
    handle: EngineHandle,
    _dir: tempfile::TempDir,
}

/// An engine of lab account a, its reads on wiremock; its client knows MCPS when `mcps` says so.
async fn running_engine(mcps: bool) -> Rig {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let tokens = Arc::new(Tokens);
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    let client = if mcps { http().with_mcps(MCPS).unwrap() } else { http() };
    let handle = engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("recap.sqlite")).unwrap(),
        client: Arc::new(Client::new(client)),
        outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "Lab A")),
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap();
    Rig {
        server,
        handle,
        _dir: dir,
    }
}

/// MCPS answers the meeting's contents read with `response`, once.
async fn contents_answer(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path_regex("^/api/mcps/amer/contents/?$"))
        .and(query_param("threadId", THREAD))
        .respond_with(response)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

/// The personal site answers the transcript's read with `response`, once.
async fn transcript_answer(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(VTT_PATH))
        .respond_with(response)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_meetings_contents_are_its_recording_and_transcript_as_mcps_lists_them() {
    let rig = running_engine(true).await;
    let answer = json!({
        "resources": [
            {"type": "Recording", "location": RECORDING,
             "metadata": {"callId": "call-1", "driveId": "b!madeup", "driveItemId": "ITEM1"}},
            {"type": "TranscriptV2", "location": TRANSCRIPT,
             "metadata": {"callId": "call-1", "webUrl": RECORDING}},
            {"type": "Whiteboard", "location": "https://example.test/board"}
        ]
    });
    contents_answer(&rig.server, ResponseTemplate::new(200).set_body_json(answer)).await;

    let contents = rig.handle.meeting_contents(THREAD).await.unwrap();

    let kinds: Vec<ContentKind> = contents.iter().map(|content| content.kind).collect();
    assert_eq!(
        kinds,
        [ContentKind::Recording, ContentKind::Transcript],
        "a whiteboard isn't shown"
    );
    assert_eq!(contents[0].location, RECORDING);
    assert_eq!(contents[0].item_id.as_deref(), Some("ITEM1"));
    assert_eq!(contents[1].sharing_link(), Some(RECORDING));
}

#[tokio::test]
async fn a_meetings_contents_that_cant_be_read_are_unread() {
    let rig = running_engine(true).await;
    // an answer in another shape, then a failed read
    contents_answer(
        &rig.server,
        ResponseTemplate::new(200).set_body_json(json!({"value": []})),
    )
    .await;
    let error = rig.handle.meeting_contents(THREAD).await.unwrap_err();
    assert!(matches!(error, EngineError::RecapUnread), "{error:?}");
    assert_eq!(error.kind(), "recap unread", "as the log names it");
    contents_answer(&rig.server, ResponseTemplate::new(500)).await;
    let error = rig.handle.meeting_contents(THREAD).await.unwrap_err();
    assert!(matches!(error, EngineError::RecapUnread), "{error:?}");
    assert_eq!(rig.server.received_requests().await.unwrap().len(), 2);

    // an app without MCPS reads nothing
    let without = running_engine(false).await;
    let error = without.handle.meeting_contents(THREAD).await.unwrap_err();
    assert!(matches!(error, EngineError::RecapUnread), "{error:?}");
    assert!(without.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_transcript_is_its_webvtt_as_it_came() {
    let rig = running_engine(false).await;
    transcript_answer(
        &rig.server,
        ResponseTemplate::new(200).set_body_raw(VTT.as_bytes().to_vec(), "text/vtt"),
    )
    .await;

    let text = rig.handle.meeting_transcript(TRANSCRIPT).await.unwrap();

    assert_eq!(text, VTT);
    let requests = rig.server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), VTT_PATH);
}

#[tokio::test]
async fn a_transcript_that_cant_be_read_is_unread() {
    let rig = running_engine(false).await;
    // not a WebVTT file, then not text at all, then a refused read
    let answers = [
        ResponseTemplate::new(200).set_body_raw(b"made-up words".to_vec(), "text/plain"),
        ResponseTemplate::new(200).set_body_raw(vec![0xFF, 0xFE, 0xFD], "text/vtt"),
        ResponseTemplate::new(404),
    ];
    for answer in answers {
        transcript_answer(&rig.server, answer).await;
        let error = rig.handle.meeting_transcript(TRANSCRIPT).await.unwrap_err();
        assert!(matches!(error, EngineError::RecapUnread), "{error:?}");
    }
    assert_eq!(rig.server.received_requests().await.unwrap().len(), 3);

    // an address the app doesn't read: nothing is asked
    let error = rig
        .handle
        .meeting_transcript("https://example.com/no-transcript")
        .await
        .unwrap_err();
    assert!(matches!(error, EngineError::RecapUnread), "{error:?}");
    assert_eq!(rig.server.received_requests().await.unwrap().len(), 3);
}
