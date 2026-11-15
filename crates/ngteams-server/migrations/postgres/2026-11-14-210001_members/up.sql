-- What a person may read of a thread: from `history_from` on (0: all of it), and nothing written after `left_at`.
ALTER TABLE chat_members ADD COLUMN history_from BIGINT NOT NULL DEFAULT 0;
ALTER TABLE chat_members ADD COLUMN left_at BIGINT;
ALTER TABLE chats ADD COLUMN roster_version BIGINT NOT NULL DEFAULT 0;
