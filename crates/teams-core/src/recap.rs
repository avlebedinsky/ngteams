//! A meeting's recap, read when its Recap tab opens (`docs/protocol/recap.md`): what the meeting left, from MCPS
//! ([`teams_proto::MeetingContents`]), and a transcript's `WebVTT`, from the personal site that holds it
//! ([`teams_proto::TranscriptLocation`]), handed on as it came once it reads as a transcript, and its language, from its
//! item's transcripts. Nothing is stored and nothing is written; the recording is streamed (below), and played from
//! its download link as any shared video is ([`crate::file_content`]) only when it can't be streamed.
//!
//! A read that fails, an answer in another shape, or a transcript address the app doesn't read is
//! [`EngineError::RecapUnread`]: the view says so and offers to try again.
//!
//! A recording is streamed as Teams' Recap player streams it ([`teams_proto::recording`]): its DASH presentation read
//! from the personal site that holds it, and its parts from the media transform service, each read when the player
//! asks for it. That works whether or not the account may download the recording (an attendee's view of another's
//! meeting is refused at the download link). A presentation refused or unreadable is [`EngineError::FileUnavailable`],
//! as is a part refused; one whose read failed on the way is [`EngineError::FileUnread`].

use std::sync::Arc;

use teams_proto::recap::transcript_language;
use teams_proto::recording::read_manifest;
use teams_proto::transcript::{MAX_TRANSCRIPT, read_vtt};
use teams_proto::{
    Client, Credentials, HttpError, HttpTransport, MediaSegmentLink, MeetingContent, MeetingContents, Presentation,
    RecordingManifest, TranscriptLocation,
};

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

    /// The DASH presentation of the recording `manifest` names, once it reads as one the player plays.
    pub(crate) fn presentation(&self, manifest: RecordingManifest, reply: Responder<Arc<Presentation>>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.transport().recording_manifest(&manifest).await {
                Ok(answer) => read_manifest(answer.body()).map(Arc::new).ok_or_else(|| {
                    log::info!("a meeting recording's presentation isn't one the player plays");
                    EngineError::FileUnavailable
                }),
                Err(error) => {
                    log::info!("a meeting recording's presentation couldn't be read: {error}");
                    Err(file_error(&error))
                }
            };
            let _ = reply.send(answer);
        });
    }

    /// A part of a recording's presentation, at its address.
    pub(crate) fn segment(&self, part: MediaSegmentLink, reply: Responder<Vec<u8>>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.transport().media_segment(&part).await {
                Ok(answer) => Ok(answer.body().to_vec()),
                Err(error) => {
                    log::info!("a part of a meeting recording couldn't be read: {error}");
                    Err(file_error(&error))
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

    /// The language of the transcript at `location` (a `TranscriptV2` content's), as its item's transcripts give it.
    pub(crate) fn language(&self, location: &str, reply: Responder<Option<String>>) {
        let Some(location) = TranscriptLocation::of(location) else {
            let _ = reply.send(Err(EngineError::RecapUnread));
            return;
        };
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.transport().meeting_transcript_languages(&location).await {
                Ok(answer) => Ok(transcript_language(answer.body(), location.id())),
                Err(error) => {
                    log::info!("a meeting transcript's language couldn't be read: {error}");
                    Err(EngineError::RecapUnread)
                }
            };
            let _ = reply.send(answer);
        });
    }
}

/// A recording's read that failed: refused or too large (it can't be had) or failed on the way (it may be read again).
fn file_error(error: &HttpError) -> EngineError {
    match error {
        HttpError::Status(answer) if !matches!(answer.status(), 408 | 429 | 500..=599) => EngineError::FileUnavailable,
        HttpError::Refused(_) | HttpError::BodyTooLarge => EngineError::FileUnavailable,
        _ => EngineError::FileUnread,
    }
}
