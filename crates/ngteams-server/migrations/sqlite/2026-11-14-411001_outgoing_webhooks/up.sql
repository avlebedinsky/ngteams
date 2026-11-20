-- A team's outgoing webhooks: each is an app account (`users.kind` `app`, its id the webhook's), mentioned as
-- `28:<id>` in the team's standard channels. `security_key` is the HMAC key the token shows once (base64): the server
-- signs with it, so it is kept as the server keeps its other signing keys. `name_key` is the name in lower case, one
-- per team. Times are Unix milliseconds.
CREATE TABLE outgoing_webhooks (
    id TEXT PRIMARY KEY NOT NULL REFERENCES users (id),
    team_id TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    name_key TEXT NOT NULL,
    description TEXT NOT NULL,
    callback_url TEXT NOT NULL,
    security_key TEXT NOT NULL,
    picture_etag TEXT,
    created_by TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX outgoing_webhooks_name ON outgoing_webhooks (team_id, name_key);

-- The outbox of mentions to webhooks, written in the message's own transaction. A row is sent at most once: a node
-- takes it from `pending` to `sending` with a compare-and-set update; a row whose node died stays `sending` until
-- its lease (`locked_until`) runs out, and then fails without a second request.
CREATE TABLE outgoing_webhook_deliveries (
    id TEXT PRIMARY KEY NOT NULL,
    webhook_id TEXT NOT NULL REFERENCES outgoing_webhooks (id) ON DELETE CASCADE,
    chat_id TEXT NOT NULL,
    root_id BIGINT NOT NULL,
    activity TEXT NOT NULL,
    state TEXT NOT NULL,
    locked_by TEXT,
    locked_until BIGINT NOT NULL,
    last_error TEXT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
CREATE INDEX outgoing_webhook_deliveries_due ON outgoing_webhook_deliveries (state, created_at);
