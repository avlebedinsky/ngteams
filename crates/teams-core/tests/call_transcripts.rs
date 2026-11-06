//! Calls' captions saved on this computer, kept, read and deleted through a running engine (no service write, nothing
//! on the wire), and read back by a later engine on the same store: what a restart of the app shows. Every id and word
//! here is made up.

#![cfg(test)]

use std::future::{self, Future};
use std::path::Path;
use std::sync::Arc;

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::{Outbox, TranscriptWrite};
use teams_mock::{AB_CHAT, CHAT_BASE, LAB_A, LAB_B};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::{Store, TranscriptLine};
use wiremock::MockServer;

struct Tokens;

impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("synthetic-transcript-token")))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

fn engine_on(server: &MockServer, path: &Path) -> EngineHandle {
    let tokens = Arc::new(Tokens);
    let transport = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    let outbox = Outbox::new(Arc::new(transport()), Author::new(Mri::new(LAB_A), "Lab A"));
    engine::start(Parts {
        calls: None,
        store: Store::open(path).unwrap(),
        client: Arc::new(Client::new(transport())),
        outbox,
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap()
}

fn line(utterance: &str, at: i64) -> TranscriptLine {
    TranscriptLine {
        utterance: utterance.to_owned(),
        speaker: Some(LAB_B.to_owned()),
        speaker_name: Some("Lab B".to_owned()),
        started_at: at,
        ended_at: at + 1_500,
        text: "A synthetic sentence.".to_owned(),
    }
}

#[tokio::test]
async fn saved_captions_are_read_back_by_a_later_engine_and_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcripts.sqlite");
    let server = MockServer::start().await;
    let id = {
        let handle = engine_on(&server, &path);
        let id = handle
            .transcript(TranscriptWrite::Open {
                conversation: AB_CHAT.to_owned(),
                title: None,
                at: 1_000_000,
                rejoin_ms: 300_000,
            })
            .await
            .unwrap();
        let kept = handle
            .transcript(TranscriptWrite::Lines {
                id,
                lines: vec![line("u1", 1_010_000), line("u2", 1_020_000)],
            })
            .await
            .unwrap();
        assert_eq!(kept, id);
        handle
            .transcript(TranscriptWrite::End { id, at: 1_100_000 })
            .await
            .unwrap();
        handle.shutdown();
        id
    };
    let handle = engine_on(&server, &path);
    let listed = handle.call_transcripts(AB_CHAT).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!((listed[0].id, listed[0].ended_at, listed[0].lines), (id, 1_100_000, 2));
    assert_eq!(handle.transcript_lines(id).await.unwrap().len(), 2);
    handle.transcript(TranscriptWrite::Delete { id }).await.unwrap();
    assert!(handle.call_transcripts(AB_CHAT).await.unwrap().is_empty());
    handle.shutdown();
    // nothing of it went to the service
    assert!(server.received_requests().await.unwrap_or_default().is_empty());
}
