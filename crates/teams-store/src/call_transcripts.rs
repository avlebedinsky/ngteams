//! Calls' live captions saved on this computer (schema 21): a transcript per call, by the chat the call was in, and its
//! lines, one per utterance. The app collects them from the account's own caption stream (`teams-ui`'s call
//! transcripts) when the user asked for every call's captions to be saved; Teams never sees them. A call back in the
//! same chat soon after one ended goes on in that one's transcript ([`Store::open_call_transcript`]). `Debug` leaves the
//! words out.

use std::fmt;

use rusqlite::{OptionalExtension as _, params};

use crate::{Store, StoreError};

/// A saved transcript, without its lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallTranscript {
    pub id: i64,
    /// The chat the call was in.
    pub conversation_id: String,
    /// The meeting's title, when the call was a meeting's.
    pub title: Option<String>,
    /// When the call connected, ms since the epoch.
    pub started_at: i64,
    /// When its last line was said or the call ended, whichever is later, ms since the epoch.
    pub ended_at: i64,
    /// How many lines it holds.
    pub lines: i64,
}

/// One utterance of a saved transcript.
#[derive(Clone, PartialEq, Eq)]
pub struct TranscriptLine {
    /// The app's key for the utterance: a later final result of the same utterance replaces its text.
    pub utterance: String,
    /// Who spoke, by MRI, as the caption service gave it.
    pub speaker: Option<String>,
    /// Their name as the caption service gave it.
    pub speaker_name: Option<String>,
    /// When it was first heard and when its final result came, ms since the epoch.
    pub started_at: i64,
    pub ended_at: i64,
    pub text: String,
}

impl fmt::Debug for TranscriptLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TranscriptLine")
            .field("utterance", &self.utterance)
            .field("started_at", &self.started_at)
            .field("ended_at", &self.ended_at)
            .field("text_bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}

impl Store {
    /// The transcript a call in `conversation_id` that connected at `at` (ms) saves its lines to: the chat's latest
    /// one when it ended no earlier than `rejoin_ms` before `at` (the same call, dropped and rejoined), else a new one,
    /// titled `title`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn open_call_transcript(
        &mut self,
        conversation_id: &str,
        title: Option<&str>,
        at: i64,
        rejoin_ms: i64,
    ) -> Result<i64, StoreError> {
        let tx = self.conn.transaction()?;
        let latest: Option<i64> = tx
            .query_row(
                "SELECT id FROM call_transcripts WHERE conversation_id = ?1 AND ended_at >= ?2
                 ORDER BY ended_at DESC, id DESC LIMIT 1",
                params![conversation_id, at.saturating_sub(rejoin_ms)],
                |row| row.get(0),
            )
            .optional()?;
        let id = if let Some(id) = latest {
            tx.execute(
                "UPDATE call_transcripts SET ended_at = max(ended_at, ?2), title = coalesce(title, ?3) WHERE id = ?1",
                params![id, at, title],
            )?;
            id
        } else {
            tx.execute(
                "INSERT INTO call_transcripts (conversation_id, title, started_at, ended_at) VALUES (?1, ?2, ?3, ?3)",
                params![conversation_id, title, at],
            )?;
            tx.last_insert_rowid()
        };
        tx.commit()?;
        Ok(id)
    }

    /// Keeps `lines` in transcript `id`, in one transaction: each replaces the line of its utterance, and the
    /// transcript's end moves on to the latest of them. Nothing is kept for a transcript that is gone (deleted
    /// meanwhile).
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails; nothing changed then.
    pub fn put_transcript_lines(&mut self, id: i64, lines: &[TranscriptLine]) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        {
            let mut row = tx.prepare_cached(
                "INSERT INTO call_transcript_lines
                     (transcript_id, utterance, speaker, speaker_name, started_at, ended_at, text)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 WHERE EXISTS (SELECT 1 FROM call_transcripts WHERE id = ?1)
                 ON CONFLICT (transcript_id, utterance) DO UPDATE SET speaker = excluded.speaker,
                     speaker_name = excluded.speaker_name, ended_at = excluded.ended_at, text = excluded.text",
            )?;
            for line in lines {
                row.execute(params![
                    id,
                    line.utterance,
                    line.speaker,
                    line.speaker_name,
                    line.started_at,
                    line.ended_at,
                    line.text,
                ])?;
            }
            if let Some(last) = lines.iter().map(|line| line.ended_at).max() {
                tx.execute(
                    "UPDATE call_transcripts SET ended_at = max(ended_at, ?2) WHERE id = ?1",
                    params![id, last],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Transcript `id`'s call ended at `at` (ms): its end moves on to it.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn end_call_transcript(&self, id: i64, at: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE call_transcripts SET ended_at = max(ended_at, ?2) WHERE id = ?1",
            params![id, at],
        )?;
        Ok(())
    }

    /// The transcripts of the calls in `conversation_id` that hold a line, the earliest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn call_transcripts(&self, conversation_id: &str) -> Result<Vec<CallTranscript>, StoreError> {
        let mut statement = self.conn.prepare_cached(
            "SELECT t.id, t.title, t.started_at, t.ended_at, count(l.utterance) FROM call_transcripts t
             JOIN call_transcript_lines l ON l.transcript_id = t.id
             WHERE t.conversation_id = ?1 GROUP BY t.id ORDER BY t.started_at, t.id",
        )?;
        let found = statement
            .query_map(params![conversation_id], |row| {
                Ok(CallTranscript {
                    id: row.get(0)?,
                    conversation_id: conversation_id.to_owned(),
                    title: row.get(1)?,
                    started_at: row.get(2)?,
                    ended_at: row.get(3)?,
                    lines: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(found)
    }

    /// Transcript `id`'s lines in the order they were said; none for a transcript that is gone.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn transcript_lines(&self, id: i64) -> Result<Vec<TranscriptLine>, StoreError> {
        let mut statement = self.conn.prepare_cached(
            "SELECT utterance, speaker, speaker_name, started_at, ended_at, text FROM call_transcript_lines
             WHERE transcript_id = ?1 ORDER BY started_at, ended_at, utterance",
        )?;
        let found = statement
            .query_map(params![id], |row| {
                Ok(TranscriptLine {
                    utterance: row.get(0)?,
                    speaker: row.get(1)?,
                    speaker_name: row.get(2)?,
                    started_at: row.get(3)?,
                    ended_at: row.get(4)?,
                    text: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(found)
    }

    /// Deletes transcript `id` with its lines; whether there was one.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn delete_call_transcript(&self, id: i64) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .execute("DELETE FROM call_transcripts WHERE id = ?1", params![id])?
            > 0)
    }
}
