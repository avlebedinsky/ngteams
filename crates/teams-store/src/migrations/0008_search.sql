-- Schema 8 (M2 Task 15): local search over the stored messages' plain text. The migration only creates the tables and
-- fills nothing: the store keeps the service's HTML and can't read it as text, so the core hands each message's text
-- as it stores it, and indexes the messages stored before this schema from Rust (its backfill).
--
-- One row per message the index has seen, at the version it was indexed from: a message without one, or with a newer
-- version than its row's, is still to be indexed. Its `id` is the rowid of its text in `message_search`; a message
-- with no text (deleted, not a text message) has a row here and none there.
CREATE TABLE search_entries (
    id INTEGER PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    UNIQUE (conversation_id, message_id),
    FOREIGN KEY (conversation_id, message_id) REFERENCES messages (conversation_id, id) ON DELETE CASCADE
) STRICT;

-- The messages' text, by `search_entries.id`; prefixes of 2 and 3 characters indexed for typed-ahead queries.
CREATE VIRTUAL TABLE message_search USING fts5 (
    text,
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);

-- A message gone (its conversation deleted) takes its text with it.
CREATE TRIGGER search_entries_gone AFTER DELETE ON search_entries BEGIN
    DELETE FROM message_search WHERE rowid = old.id;
END;
