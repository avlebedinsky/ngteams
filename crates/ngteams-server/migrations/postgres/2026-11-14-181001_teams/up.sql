-- Teams and their channels. A team's id is its general channel's thread; every channel is a chat-service thread
-- (a `chats` row), whose `chat_members` are the people who may read and post in it. Times are Unix milliseconds.
CREATE TABLE teams (
    id TEXT PRIMARY KEY NOT NULL REFERENCES chats (id) ON DELETE CASCADE,
    group_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    public BOOLEAN NOT NULL,
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    join_code TEXT UNIQUE,
    settings TEXT NOT NULL,
    tag_settings TEXT NOT NULL,
    picture_etag TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL,
    version BIGINT NOT NULL
);

CREATE TABLE team_members (
    team_id TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id),
    owner BOOLEAN NOT NULL,
    hidden BOOLEAN NOT NULL DEFAULT FALSE,
    joined_at BIGINT NOT NULL,
    PRIMARY KEY (team_id, user_id)
);
CREATE INDEX team_members_user ON team_members (user_id);

CREATE TABLE channels (
    id TEXT PRIMARY KEY NOT NULL REFERENCES chats (id) ON DELETE CASCADE,
    team_id TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    group_id TEXT UNIQUE,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    private BOOLEAN NOT NULL,
    layout TEXT NOT NULL,
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    shown_by_default BOOLEAN NOT NULL DEFAULT FALSE,
    settings TEXT,
    created_at BIGINT NOT NULL,
    version BIGINT NOT NULL
);
CREATE INDEX channels_team ON channels (team_id);

CREATE TABLE channel_roles (
    channel_id TEXT NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id),
    owner BOOLEAN NOT NULL DEFAULT FALSE,
    moderator BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (channel_id, user_id)
);

CREATE TABLE team_join_requests (
    team_id TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id),
    created_at BIGINT NOT NULL,
    PRIMARY KEY (team_id, user_id)
);

CREATE TABLE team_tags (
    id TEXT PRIMARY KEY NOT NULL,
    team_id TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    members TEXT NOT NULL,
    version BIGINT NOT NULL
);
CREATE INDEX team_tags_team ON team_tags (team_id);

CREATE TABLE conversation_folders (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    version BIGINT NOT NULL,
    body TEXT NOT NULL
);

-- The post a channel message belongs to: its own id for a post, the post's for a reply; NULL in a chat.
ALTER TABLE chat_messages ADD COLUMN root_id BIGINT;
CREATE INDEX chat_messages_root ON chat_messages (chat_id, root_id);
