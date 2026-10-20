-- Schema 1: the first M1 store (Task 11). Times are milliseconds since the Unix epoch; versions are the chat
-- service's own numbers (a message's `version`, a thread's `version` and `rosterVersion`).

-- One row per process that opened the store: the outbox sends only the rows its own run made or adopted.
CREATE TABLE runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at INTEGER NOT NULL
) STRICT;

CREATE TABLE conversations (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    topic TEXT,
    version INTEGER NOT NULL,
    roster_version INTEGER,
    -- the newest of the service's last-message time and every stored message's arrival
    last_activity INTEGER
) STRICT;

CREATE INDEX conversations_by_activity ON conversations (last_activity DESC);

CREATE TABLE members (
    conversation_id TEXT NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    mri TEXT NOT NULL,
    role TEXT,
    PRIMARY KEY (conversation_id, mri)
) STRICT;

CREATE TABLE messages (
    conversation_id TEXT NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    version INTEGER NOT NULL,
    client_message_id TEXT,
    sender TEXT,
    sender_name TEXT,
    message_type TEXT NOT NULL,
    -- empty once the message is soft-deleted
    content TEXT NOT NULL,
    -- the message's `properties` object as the service sent it (mentions, files, …), JSON text
    properties TEXT,
    arrival_time INTEGER NOT NULL,
    compose_time INTEGER,
    edit_time INTEGER,
    delete_time INTEGER,
    -- the version of the newest whole reaction set merged in: a reaction with no row was absent then, so a change
    -- to it from before then is ignored
    reactions_as_of INTEGER,
    PRIMARY KEY (conversation_id, id)
) STRICT;

CREATE INDEX messages_by_arrival ON messages (conversation_id, arrival_time DESC, id DESC);
CREATE INDEX messages_by_client_id ON messages (conversation_id, client_message_id)
    WHERE client_message_id IS NOT NULL;

-- One row per (message, reaction, user), the latest change winning. A removal stays as a row with `present` 0, so
-- an older change that arrives later can't bring the reaction back. `changed` orders the changes: a per-user change's
-- own time, or the version of the whole set it came in; `time` is when the reaction was set.
CREATE TABLE reactions (
    conversation_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    reaction TEXT NOT NULL,
    user_mri TEXT NOT NULL,
    time INTEGER NOT NULL,
    changed INTEGER NOT NULL,
    present INTEGER NOT NULL CHECK (present IN (0, 1)),
    PRIMARY KEY (conversation_id, message_id, reaction, user_mri),
    FOREIGN KEY (conversation_id, message_id) REFERENCES messages (conversation_id, id) ON DELETE CASCADE
) STRICT;

-- Each member's read horizon (`consumptionhorizon`), newest `updated` wins.
CREATE TABLE read_horizons (
    conversation_id TEXT NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    user_mri TEXT NOT NULL,
    last_read_arrival INTEGER NOT NULL,
    updated INTEGER NOT NULL,
    raw TEXT,
    PRIMARY KEY (conversation_id, user_mri)
) STRICT;

-- Where a conversation's message sync stands: the service's own links, kept as given.
CREATE TABLE conversation_sync (
    conversation_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    sync_link TEXT,
    backward_link TEXT,
    history_complete INTEGER NOT NULL CHECK (history_complete IN (0, 1)),
    updated INTEGER NOT NULL
) STRICT;

-- Where the conversation list's sync stands: a single row.
CREATE TABLE conversation_list_sync (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    sync_link TEXT,
    updated INTEGER NOT NULL
) STRICT;

-- Writes waiting to go out, one row per command, keyed by its `clientmessageid` (kept across retries).
CREATE TABLE outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES runs (id),
    client_message_id TEXT NOT NULL UNIQUE,
    -- none for a chat that doesn't exist yet (chat create)
    conversation_id TEXT,
    kind TEXT NOT NULL,
    -- the command as the core serialized it; the store doesn't read it
    payload TEXT NOT NULL,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    not_before INTEGER,
    last_error TEXT,
    message_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE INDEX outbox_by_state ON outbox (state, run_id);
