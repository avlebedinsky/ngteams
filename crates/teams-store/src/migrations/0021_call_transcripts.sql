-- Schema 21: calls' live captions saved on this computer (the app's "Save live captions of every call"). Teams never
-- sees them: they are the captions the account's own caption pane receives, kept as a transcript by the chat the call
-- was in. A transcript is one call, or one call and its rejoins (a call back in the same chat soon after it ended goes
-- on in the same transcript). `started_at` and `ended_at` are ms since the epoch; `ended_at` moves on with each line
-- and with the call's end. `conversation_id` is a chat the store may not hold yet (a meeting joined by its link), so it
-- names no row. `title` is the meeting's title when the call was one.
CREATE TABLE call_transcripts (
    id INTEGER PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    title TEXT,
    started_at INTEGER NOT NULL,
    ended_at INTEGER NOT NULL
) STRICT;

CREATE INDEX call_transcripts_by_conversation ON call_transcripts (conversation_id, ended_at);

-- A transcript's lines: one per utterance (`utterance` is the app's key for it, so a later final result of the same
-- utterance replaces its text), with who spoke (their MRI and name as the caption service gave them) and when, ms since
-- the epoch. Lines go with their transcript.
CREATE TABLE call_transcript_lines (
    transcript_id INTEGER NOT NULL REFERENCES call_transcripts (id) ON DELETE CASCADE,
    utterance TEXT NOT NULL,
    speaker TEXT,
    speaker_name TEXT,
    started_at INTEGER NOT NULL,
    ended_at INTEGER NOT NULL,
    text TEXT NOT NULL,
    PRIMARY KEY (transcript_id, utterance)
) STRICT;
