-- The calendar. An event is its organizer's; each account it invites has a row of its own with its answer. A
-- series is one row (recurring) expanded when read; an occurrence changed, answered or cancelled on its own is a
-- row under its series, its id the series' id, a dot and the occurrence's own start. A channel's meeting belongs to its team. Times are Unix milliseconds.
CREATE TABLE calendar_events (
    id TEXT PRIMARY KEY NOT NULL,
    owner_id TEXT NOT NULL REFERENCES users (id),
    team_id TEXT REFERENCES teams (id) ON DELETE CASCADE,
    channel_id TEXT,
    post_id BIGINT,
    series_id TEXT REFERENCES calendar_events (id) ON DELETE CASCADE,
    original_start BIGINT,
    start_ms BIGINT NOT NULL,
    end_ms BIGINT NOT NULL,
    until_ms BIGINT NOT NULL,
    recurring BOOLEAN NOT NULL DEFAULT FALSE,
    changed BOOLEAN NOT NULL DEFAULT FALSE,
    cancelled BOOLEAN NOT NULL DEFAULT FALSE,
    meeting_chat TEXT,
    body TEXT NOT NULL,
    version BIGINT NOT NULL
);
CREATE INDEX calendar_events_owner ON calendar_events (owner_id, start_ms);
CREATE INDEX calendar_events_series ON calendar_events (series_id);
CREATE INDEX calendar_events_chat ON calendar_events (meeting_chat);

CREATE TABLE calendar_attendees (
    event_id TEXT NOT NULL REFERENCES calendar_events (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id),
    position INTEGER NOT NULL,
    optional BOOLEAN NOT NULL DEFAULT FALSE,
    response TEXT NOT NULL,
    responded_at BIGINT,
    removed BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (event_id, user_id)
);
CREATE INDEX calendar_attendees_user ON calendar_attendees (user_id);

-- An online meeting: its chat is a chat-service thread, its code and passcode make its short link.
CREATE TABLE meetings (
    chat_id TEXT PRIMARY KEY NOT NULL REFERENCES chats (id) ON DELETE CASCADE,
    organizer_id TEXT NOT NULL REFERENCES users (id),
    code TEXT NOT NULL UNIQUE,
    passcode TEXT NOT NULL,
    subject TEXT,
    kind TEXT NOT NULL,
    created_at BIGINT NOT NULL
);

-- The meeting options of a meeting's or a group call's chat, as its organizer set them (JSON: option → value).
CREATE TABLE meeting_options (
    chat_id TEXT PRIMARY KEY NOT NULL REFERENCES chats (id) ON DELETE CASCADE,
    body TEXT NOT NULL
);
