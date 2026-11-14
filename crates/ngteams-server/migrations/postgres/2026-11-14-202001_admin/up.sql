-- Account administration: the admin role, a password that must be changed, deleted accounts kept as tombstones,
-- the audit of admin actions, and the row the last-admin guards lock.
ALTER TABLE users ADD COLUMN is_admin BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE users ADD COLUMN must_change_password BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE users ADD COLUMN deleted_at BIGINT;

CREATE TABLE admin_audit (
    id BIGSERIAL PRIMARY KEY,
    at BIGINT NOT NULL,
    actor_id TEXT,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    target_id TEXT,
    target TEXT NOT NULL,
    detail TEXT NOT NULL
);

INSERT INTO server_meta (name, value) VALUES ('admin_lock', '');
