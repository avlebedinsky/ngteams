-- The title bar's search over the chat-service messages' plain text: one row per message of a `19:` thread, its
-- text as the reader sees it (empty for one with none: an event, a deleted message) and its words (its runs of
-- letters and digits, lower-cased, as the search's query is split), found by FTS5 over the words
-- (`message_search_fts`, which triggers keep in step). The server writes a row in the message's own transaction; the
-- messages from before this migration are indexed by the server at its start.
CREATE TABLE message_search (
    id INTEGER PRIMARY KEY,
    chat_id TEXT NOT NULL,
    message_id BIGINT NOT NULL,
    text TEXT NOT NULL,
    words TEXT NOT NULL,
    UNIQUE (chat_id, message_id),
    FOREIGN KEY (chat_id, message_id) REFERENCES chat_messages (chat_id, id) ON DELETE CASCADE
);

CREATE VIRTUAL TABLE message_search_fts USING fts5 (
    words,
    content = 'message_search',
    content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);

CREATE TRIGGER message_search_added AFTER INSERT ON message_search BEGIN
    INSERT INTO message_search_fts (rowid, words) VALUES (new.id, new.words);
END;

CREATE TRIGGER message_search_removed AFTER DELETE ON message_search BEGIN
    INSERT INTO message_search_fts (message_search_fts, rowid, words) VALUES ('delete', old.id, old.words);
END;

CREATE TRIGGER message_search_changed AFTER UPDATE ON message_search BEGIN
    INSERT INTO message_search_fts (message_search_fts, rowid, words) VALUES ('delete', old.id, old.words);
    INSERT INTO message_search_fts (rowid, words) VALUES (new.id, new.words);
END;
