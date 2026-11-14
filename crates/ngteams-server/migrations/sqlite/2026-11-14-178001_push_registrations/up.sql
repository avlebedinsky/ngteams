CREATE TABLE push_registrations (
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    registration_id TEXT NOT NULL,
    app_id TEXT NOT NULL,
    path TEXT NOT NULL,
    expires_at BIGINT NOT NULL,
    PRIMARY KEY (user_id, registration_id, app_id)
);
CREATE INDEX push_registrations_expires_at ON push_registrations (expires_at);
