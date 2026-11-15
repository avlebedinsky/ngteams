-- Workflows with incoming webhooks: the Teams connections a workflow posts through, the workflows (their Power
-- Automate properties as JSON, the thread they post to and their webhook secret), their owners and runs. Times are
-- Unix milliseconds.

-- An app's row (`kind` 'app') is a principal in chats that never signs in.
ALTER TABLE users ADD COLUMN kind TEXT NOT NULL DEFAULT 'user';
-- The Workflows bot, which posts what a webhook receives: deleted so no listing, sign-in or admin change reaches it,
-- while it still names what it posted.
INSERT INTO users (id, upn, display_name, password_hash, disabled, created_at, updated_at, is_admin,
                   must_change_password, deleted_at, kind)
VALUES ('358f0194-6b0e-4dd3-af35-c24fe8a9ec87', 'workflows@apps.invalid', 'Workflows', '', TRUE, 0, 0, FALSE, FALSE,
        0, 'app');

CREATE TABLE workflow_connections (
    name TEXT PRIMARY KEY NOT NULL,
    owner_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    api TEXT NOT NULL,
    created_at BIGINT NOT NULL
);
CREATE INDEX workflow_connections_owner ON workflow_connections (owner_id);

CREATE TABLE workflows (
    id TEXT PRIMARY KEY NOT NULL,
    creator_id TEXT NOT NULL REFERENCES users (id),
    display_name TEXT NOT NULL,
    enabled BOOLEAN NOT NULL,
    properties TEXT NOT NULL,
    target_thread TEXT NOT NULL,
    secret TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE TABLE workflow_owners (
    workflow_id TEXT NOT NULL REFERENCES workflows (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (workflow_id, user_id)
);
CREATE INDEX workflow_owners_user ON workflow_owners (user_id);

CREATE TABLE workflow_runs (
    workflow_id TEXT NOT NULL REFERENCES workflows (id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    started_at BIGINT NOT NULL,
    ended_at BIGINT NOT NULL,
    status TEXT NOT NULL,
    error TEXT,
    PRIMARY KEY (workflow_id, id)
);
CREATE INDEX workflow_runs_time ON workflow_runs (workflow_id, started_at);
