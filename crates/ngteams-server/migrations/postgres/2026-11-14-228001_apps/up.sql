-- Apps and bots: a registered Teams app (its manifest kept whole), the bot it may have (a `users` row of kind 'app'
-- with the bot's id, which never signs in), the bot's messaging endpoint and the SHA-256 of its secret, who may use the
-- app, where it is installed, and the RSA keys activities to bots are signed with. Times are Unix seconds.
CREATE TABLE apps (
    id TEXT PRIMARY KEY NOT NULL,
    version TEXT NOT NULL,
    name TEXT NOT NULL,
    full_name TEXT NOT NULL,
    short_description TEXT NOT NULL,
    full_description TEXT NOT NULL,
    developer TEXT NOT NULL,
    accent_color TEXT NOT NULL,
    manifest TEXT NOT NULL,
    source TEXT NOT NULL,
    icons BOOLEAN NOT NULL,
    everyone BOOLEAN NOT NULL,
    enabled BOOLEAN NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE TABLE app_users (
    app_id TEXT NOT NULL REFERENCES apps (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (app_id, user_id)
);
CREATE INDEX app_users_user ON app_users (user_id);

CREATE TABLE bots (
    id TEXT PRIMARY KEY NOT NULL REFERENCES users (id),
    app_id TEXT NOT NULL UNIQUE REFERENCES apps (id) ON DELETE CASCADE,
    endpoint TEXT,
    secret_hash TEXT NOT NULL,
    auth_mode TEXT NOT NULL,
    scopes TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE TABLE app_installs (
    app_id TEXT NOT NULL REFERENCES apps (id) ON DELETE CASCADE,
    scope TEXT NOT NULL,
    target TEXT NOT NULL,
    installed_by TEXT REFERENCES users (id) ON DELETE SET NULL,
    installed_at BIGINT NOT NULL,
    PRIMARY KEY (app_id, scope, target)
);
CREATE INDEX app_installs_target ON app_installs (scope, target);

CREATE TABLE bot_signing_keys (
    kid TEXT PRIMARY KEY NOT NULL,
    private_der BYTEA NOT NULL,
    created_at BIGINT NOT NULL,
    retired_at BIGINT
);
