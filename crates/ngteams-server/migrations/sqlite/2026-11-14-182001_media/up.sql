-- Pictures in messages (media objects, read by the members of the chats they were made for) and files sent in
-- chats (an account's drive items, read by the accounts they were shared with), with each account's stored bytes.
CREATE TABLE media_objects (
    id TEXT PRIMARY KEY NOT NULL,
    owner_id TEXT NOT NULL REFERENCES users (id),
    kind TEXT NOT NULL,
    filename TEXT NOT NULL,
    content_type TEXT,
    blob TEXT,
    size BIGINT NOT NULL,
    created_at BIGINT NOT NULL
);

CREATE TABLE media_object_readers (
    object_id TEXT NOT NULL REFERENCES media_objects (id) ON DELETE CASCADE,
    chat_id TEXT NOT NULL REFERENCES chats (id) ON DELETE CASCADE,
    PRIMARY KEY (object_id, chat_id)
);

CREATE TABLE drive_items (
    id TEXT PRIMARY KEY NOT NULL,
    owner_id TEXT NOT NULL REFERENCES users (id),
    name TEXT NOT NULL,
    size BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    UNIQUE (owner_id, name)
);

CREATE TABLE drive_grants (
    item_id TEXT NOT NULL REFERENCES drive_items (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id),
    PRIMARY KEY (item_id, user_id)
);

CREATE TABLE media_usage (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES users (id),
    bytes BIGINT NOT NULL
);
