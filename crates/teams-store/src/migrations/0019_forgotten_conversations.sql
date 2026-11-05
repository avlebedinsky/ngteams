-- Schema 19: the threads the store forgot as deleted for good (the chat service answered 404 `ThreadNotFound`), each
-- with the version and last activity it had then. A conversation list that still names one adds it back, and reads
-- it, only when it shows a newer version or a later last message. Its row goes when the conversation is stored again.
CREATE TABLE forgotten_conversations (
    id TEXT PRIMARY KEY NOT NULL,
    version INTEGER NOT NULL,
    last_activity INTEGER
) STRICT;
