-- Schema 12: how far back a conversation's sync answers need reading, and the conversations a catch-up has still to
-- sync again.

-- the `lastCompleteSegmentEndTime` the kept sync link came with (ms): every message that arrived up to then is stored,
-- or kept in the gap; the next sync answer is read back to it and no further. NULL for a state of an older schema.
ALTER TABLE conversation_sync ADD COLUMN sync_until INTEGER;

-- the same for the gap: how far back the cut sync answer it is the rest of had to be read. NULL with no gap, or a gap
-- of an older schema.
ALTER TABLE conversation_sync ADD COLUMN gap_until INTEGER;

-- conversations a catch-up brought and couldn't sync: every later catch-up syncs them again, whether or not the
-- conversation list brings them, so the list's sync link moves on past them
CREATE TABLE unsynced_conversations (
    conversation_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations (id) ON DELETE CASCADE
) STRICT;
