//! A meeting's recap, read when its Recap tab opens (`docs/protocol/recap.md`): what the meeting left, from MCPS
//! ([`teams_proto::MeetingContents`]), and a transcript's `WebVTT`, from the personal site that holds it
//! ([`teams_proto::TranscriptLocation`]), handed on as it came once it reads as a transcript. Nothing is stored and
//! nothing is written; the recording itself is played as any shared video is ([`crate::file_content`]).
//!
//! A read that fails, an answer in another shape, or a transcript address the app doesn't read is
//! [`EngineError::RecapUnread`]: the view says so and offers to try again.

use std::sync::Arc;

use teams_proto::transcript::{MAX_TRANSCRIPT, read_vtt};
use teams_proto::{Client, Credentials, HttpTransport, MeetingContent, MeetingContents, TranscriptLocation};

use crate::engine::{EngineError, Responder};

/// The recap reads of one account.
pub(crate) struct Recaps<C> {
    client: Arc<Client<HttpTransport<C>>>,
}

impl<C: Credentials + 'static> Recaps<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>) -> Self {
        Self { client }
    }

    /// What the meeting whose chat is `thread` left, as its recap lists it.
    pub(crate) fn contents(&self, thread: String, reply: Responder<Vec<MeetingContent>>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.read(&MeetingContents::of(thread)).await {
                Ok(answer) => MeetingContents::read_answer(answer.body()).ok_or(EngineError::RecapUnread),
                Err(error) => {
                    log::info!("a meeting's recap contents couldn't be read: {error}");
                    Err(EngineError::RecapUnread)
                }
            };
            let _ = reply.send(answer);
        });
    }

    /// The `WebVTT` of the transcript at `location` (a `TranscriptV2` content's), once it reads as one.
    pub(crate) fn transcript(&self, location: &str, reply: Responder<String>) {
        let Some(location) = TranscriptLocation::of(location) else {
            log::info!("a meeting's transcript is somewhere the app doesn't read");
            let _ = reply.send(Err(EngineError::RecapUnread));
            return;
        };
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.transport().meeting_transcript(&location).await {
                Ok(answer) if answer.body().len() <= MAX_TRANSCRIPT => std::str::from_utf8(answer.body())
                    .ok()
                    .filter(|text| read_vtt(text).is_some())
                    .map(str::to_owned)
                    .ok_or(EngineError::RecapUnread),
                Ok(_) => Err(EngineError::RecapUnread),
                Err(error) => {
                    log::info!("a meeting's transcript couldn't be read: {error}");
                    Err(EngineError::RecapUnread)
                }
            };
            let _ = reply.send(answer);
        });
    }
}
