-- The status an account set by hand (Busy, Do not disturb, Appear away, ...): it shows over the account's endpoints
-- until the account resets it or it expires, over restarts of the nodes and of Redis. Times are Unix seconds.
CREATE TABLE presence_manual (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    availability TEXT NOT NULL,
    expires_at BIGINT NOT NULL
);
