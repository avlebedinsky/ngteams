-- The outbox of activities to bots: each event a bot is told of is a row, written in the chat write's own
-- transaction, sent by whichever node takes its lease (`locked_until`), oldest first per bot and conversation.
-- `seq` is the chat service's clock at the event; times are Unix milliseconds. A bot's run of failed sends is kept
-- beside it, to pause a bot that takes nothing.
CREATE TABLE bot_deliveries (
    id TEXT PRIMARY KEY NOT NULL,
    bot_id TEXT NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    conversation_id TEXT NOT NULL,
    seq BIGINT NOT NULL,
    activity TEXT NOT NULL,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL,
    max_attempts INTEGER NOT NULL,
    due_at BIGINT NOT NULL,
    locked_until BIGINT NOT NULL,
    locked_by TEXT,
    last_error TEXT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
CREATE INDEX bot_deliveries_due ON bot_deliveries (state, due_at);
CREATE INDEX bot_deliveries_line ON bot_deliveries (bot_id, conversation_id, seq);

CREATE TABLE bot_health (
    bot_id TEXT PRIMARY KEY NOT NULL REFERENCES bots (id) ON DELETE CASCADE,
    failures INTEGER NOT NULL,
    paused_until BIGINT NOT NULL,
    last_error TEXT,
    last_success BIGINT,
    updated_at BIGINT NOT NULL
);
