CREATE TABLE calls (
    id TEXT PRIMARY KEY NOT NULL,
    conversation TEXT NOT NULL UNIQUE,
    caller_id TEXT NOT NULL,
    phase TEXT NOT NULL,
    version BIGINT NOT NULL,
    due_at BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    state TEXT NOT NULL
);
CREATE INDEX calls_due_at ON calls (due_at);
