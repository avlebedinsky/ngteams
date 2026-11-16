-- The title bar's search over the chat-service messages' plain text: one row per message of a `19:` thread, its
-- text as the reader sees it (empty for one with none: an event, a deleted message), found by Postgres' full-text
-- search on the `simple` configuration. The server writes a row in the message's own transaction; the messages from
-- before this migration are indexed by the server at its start.
CREATE TABLE message_search (
    chat_id TEXT NOT NULL,
    message_id BIGINT NOT NULL,
    text TEXT NOT NULL,
    words TEXT NOT NULL,
    PRIMARY KEY (chat_id, message_id),
    FOREIGN KEY (chat_id, message_id) REFERENCES chat_messages (chat_id, id) ON DELETE CASCADE
);
CREATE INDEX message_search_text ON message_search USING GIN (to_tsvector('simple', words));
